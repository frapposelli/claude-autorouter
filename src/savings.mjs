import { normalizeSessionRecord, UNPRICED_REASONS } from './telemetry-event.mjs';

// Published standard, global API prices in cents per million tokens. These are
// API-equivalent estimates, not subscription charges. Exact IDs only: an
// unfamiliar model must not inherit another model's price from its name.
// These facts retain the existing reviewed rates; a table change must publish
// a new version so historical usage never silently receives today's prices.
export const PRICING_VERSION = '2026-09-29.1';
export const PRICING_DATE = '2026-09-29';
export const PRICING_SOURCE = 'https://platform.claude.com/docs/en/about-claude/pricing';
const HAIKU_45 = Object.freeze({ input: 100, output: 500, write5m: 125, write1h: 200, read: 10 });
const SONNET_5 = Object.freeze({ input: 200, output: 1000, write5m: 250, write1h: 400, read: 20 });
const OPUS_5 = Object.freeze({ input: 500, output: 2500, write5m: 625, write1h: 1000, read: 50 });
const OPUS_55 = Object.freeze({ input: 400, output: 2000, write5m: 500, write1h: 800, read: 20 });
const PRICES = new Map([
  ['claude-haiku-4-5', HAIKU_45], ['claude-haiku-4-5-20251001', HAIKU_45],
  ['claude-sonnet-5', SONNET_5], ['claude-sonnet-5-5', SONNET_5],
  ['claude-opus-5', OPUS_5], ['claude-opus-5-5', OPUS_55],
]);
export const PRICING_FACTS = Object.freeze({ version: PRICING_VERSION, date: PRICING_DATE, source: PRICING_SOURCE,
  currency: 'USD', unit: 'cents_per_million_tokens', models: Object.freeze(Object.fromEntries(PRICES)) });
const OPUS_MODELS = new Set(['claude-opus-5', 'claude-opus-5-5']);
const EVENTS = new Set(['request_start', 'route', 'upstream_response', 'upstream_model', 'upstream_usage',
  'upstream_error', 'request_complete', 'request_error', 'request_cancelled']);
const MAX_SESSIONS = 100;
const MAX_INFLIGHT = 1000;
const MAX_HISTORY = 10000;
const identifier = value => typeof value === 'string' && /^[\w.:-]{1,200}$/.test(value) ? value : undefined;
const count = value => Number.isSafeInteger(value) && value >= 0;
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);

function standardPricing(context, allowUnavailableGeo = false) {
  if (context === undefined) return true;
  if (!object(context)) return false;
  return (context.pricing_unsupported === undefined || context.pricing_unsupported === false)
    && (context.unsupported === undefined || context.unsupported === false)
    && (context.speed === undefined || context.speed === 'standard')
    && (context.inference_geo === undefined || context.inference_geo === 'global'
      || (allowUnavailableGeo && context.inference_geo === 'not_available'))
    && (context.service_tier === undefined || ['auto', 'standard_only', 'standard'].includes(context.service_tier));
}

function normalizeUsage(usage, prices) {
  // Haiku 4.5 predates inference geography controls: Anthropic reports
  // "not_available" and standard pricing applies. Do not extend this exception
  // to newer models, whose unknown geography could carry a different rate.
  // https://platform.claude.com/docs/en/manage-claude/data-residency#pricing
  if (!object(usage) || !count(usage.input_tokens) || !count(usage.output_tokens)
    || !standardPricing(usage, prices === HAIKU_45)) return;
  const read = usage.cache_read_input_tokens ?? 0;
  // Missing counters are optional; explicit null or malformed counters are not.
  if (!count(read) || usage.cache_read_input_tokens === null || usage.cache_creation_input_tokens === null) return;
  const creation = usage.cache_creation_input_tokens;
  if (creation !== undefined && !count(creation)) return;
  let write5m = 0, write1h = 0;
  if (usage.cache_creation !== undefined) {
    if (!object(usage.cache_creation)) return;
    write5m = usage.cache_creation.ephemeral_5m_input_tokens ?? 0;
    write1h = usage.cache_creation.ephemeral_1h_input_tokens ?? 0;
    if (!count(write5m) || !count(write1h)
      || usage.cache_creation.ephemeral_5m_input_tokens === null || usage.cache_creation.ephemeral_1h_input_tokens === null) return;
    if (!count(write5m + write1h) || (creation !== undefined && creation !== write5m + write1h)) return;
  } else if (creation > 0) {
    // The aggregate alone cannot distinguish a five-minute from a one-hour
    // cache write, so it cannot establish the price.
    return;
  }
  return { input: usage.input_tokens, output: usage.output_tokens, read, write5m, write1h };
}

function cost(usage, prices) {
  // Integer arithmetic avoids floating-point drift over a long session. A unit
  // here is 1/100,000,000 of a dollar (one cent per million tokens).
  return Object.keys(usage).reduce((total, key) => total + BigInt(usage[key]) * BigInt(prices[key]), 0n);
}

export function createSavingsTracker({ baselineModel = 'claude-opus-5-5' } = {}) {
  const baseline = OPUS_MODELS.has(baselineModel) ? PRICES.get(baselineModel) : undefined;
  const baselineName = typeof baselineModel === 'string' && /^[\w.:/-]{1,120}$/.test(baselineModel) ? baselineModel : 'unknown';
  const sessions = new Map();
  const inflight = new Map();
  const settled = new Map();
  const evictedSessions = new Set();
  let historyPartial = false;

  function remember(key) {
    settled.set(key, true);
    while (settled.size > MAX_HISTORY) {
      settled.delete(settled.keys().next().value);
      historyPartial = true;
    }
  }

  function finish(key, failed = false, partial = false, failureReason = 'request_failed') {
    const request = inflight.get(key);
    if (!request) return;
    inflight.delete(key);
    remember(key);
    const session = sessions.get(request.sessionId);
    if (!session) return;
    if (partial) session.partial = true;
    if (failed || request.failed || request.invalid || !request.prices || !request.usage || !baseline) {
      session.unpriced_requests++;
      const reason = failed ? failureReason : request.failed ? 'request_failed' : request.unpricedReason
        ?? (!baseline ? 'unknown_baseline' : !request.prices ? 'missing_model' : 'missing_usage');
      session.unpriced_reasons[reason] = Math.min(Number.MAX_SAFE_INTEGER, (session.unpriced_reasons[reason] ?? 0) + 1);
      return;
    }
    session.actual += cost(request.usage, request.prices);
    session.baseline += cost(request.usage, baseline);
    session.requests++;
  }

  function sessionFor(sessionId) {
    let session = sessions.get(sessionId);
    if (session) {
      sessions.delete(sessionId);
      sessions.set(sessionId, session);
      return session;
    }
    session = { actual: 0n, baseline: 0n, requests: 0, unpriced_requests: 0, unpriced_reasons: {},
      partial: historyPartial || evictedSessions.has(sessionId) };
    sessions.set(sessionId, session);
    while (sessions.size > MAX_SESSIONS) {
      const oldest = sessions.keys().next().value;
      sessions.delete(oldest);
      evictedSessions.add(oldest);
      for (const [key, request] of inflight) {
        if (request.sessionId === oldest) { inflight.delete(key); remember(key); }
      }
      if (evictedSessions.size > MAX_HISTORY) {
        evictedSessions.delete(evictedSessions.values().next().value);
        historyPartial = true;
      }
    }
    return session;
  }

  function update(event) {
    try {
      if (!event || !EVENTS.has(event.event)) return;
      const sessionId = event.session_id == null || event.session_id === '' ? '' : identifier(event.session_id);
      const requestId = identifier(event.request_id);
      if (sessionId === undefined || requestId === undefined) return;
      const key = `${sessionId}\0${requestId}`;
      if (event.event === 'request_start') {
        if (inflight.has(key) || settled.has(key)) return;
        sessionFor(sessionId);
        while (inflight.size >= MAX_INFLIGHT) finish(inflight.keys().next().value, true, true, 'request_evicted');
        const invalid = !standardPricing(event.pricing_context);
        inflight.set(key, { sessionId, invalid, ...(invalid ? { unpricedReason: 'unsupported_pricing' } : {}) });
        return;
      }
      const request = inflight.get(key);
      if (!request) return;
      switch (event.event) {
        case 'route':
          if (!standardPricing(event.pricing_context)) { request.invalid = true; request.unpricedReason ??= 'unsupported_pricing'; }
          break;
        case 'upstream_response':
          if (!Number.isInteger(event.status) || event.status < 200 || event.status >= 300) request.failed = true;
          break;
        case 'upstream_model': {
          const prices = PRICES.get(event.model);
          if (!prices) { request.invalid = true; request.unpricedReason ??= 'unknown_model'; }
          // A fallback can mix usage even between models with identical rates.
          // No aggregate can prove the attribution of all billed tokens.
          if (request.model !== undefined && request.model !== event.model) {
            request.invalid = true; request.unpricedReason = 'mixed_models';
          }
          request.model = event.model;
          request.prices = prices;
          break;
        }
        case 'upstream_usage': {
          const usage = normalizeUsage(event.usage, request.prices);
          if (!usage || !standardPricing(event.pricing_context)) {
            request.invalid = true;
            request.unpricedReason ??= !standardPricing(event.pricing_context) || !standardPricing(event.usage, request.prices === HAIKU_45)
              ? 'unsupported_pricing' : 'invalid_usage';
          }
          else if (request.usage && Object.keys(usage).some(key => usage[key] !== request.usage[key])) {
            request.invalid = true; request.unpricedReason ??= 'conflicting_usage';
          }
          else request.usage = usage;
          break;
        }
        case 'upstream_error':
          request.failed = true;
          break;
        case 'request_complete':
          finish(key, event.completion_confirmed === false, false, 'unconfirmed_completion');
          break;
        case 'request_error':
          finish(key, true);
          break;
        case 'request_cancelled':
          finish(key, true, false, 'request_cancelled');
          break;
      }
    } catch {
      // Telemetry must never interfere with inference. A malformed event must
      // not make partially understood usage look billable.
      try {
        const sessionId = event?.session_id == null ? '' : identifier(event.session_id);
        const requestId = identifier(event?.request_id);
        const request = inflight.get(`${sessionId}\0${requestId}`);
        if (request) { request.invalid = true; request.unpricedReason ??= 'invalid_telemetry'; }
      } catch {}
    }
  }

  function snapshot() {
    return Object.fromEntries([...sessions].map(([sessionId, session]) => {
      const saved = session.baseline - session.actual;
      return [sessionId, {
        baseline_model: baselineName,
        pricing_version: PRICING_VERSION,
        pricing_date: PRICING_DATE,
        pricing_source: PRICING_SOURCE,
        actual_usd: Number(session.actual) / 100000000,
        baseline_usd: Number(session.baseline) / 100000000,
        saved_usd: Number(saved) / 100000000,
        percent: session.baseline === 0n ? 0 : Number(saved) / Number(session.baseline) * 100,
        requests: session.requests,
        unpriced_requests: session.unpriced_requests,
        unpriced_reasons: Object.fromEntries(UNPRICED_REASONS.filter(reason => session.unpriced_reasons[reason])
          .map(reason => [reason, session.unpriced_reasons[reason]])),
        ...(session.partial || historyPartial ? { partial: true } : {}),
      }];
    }));
  }

  function clear() {
    sessions.clear(); inflight.clear(); settled.clear(); evictedSessions.clear(); historyPartial = false;
  }

  return { update, snapshot, clear };
}

// Saved history must use the recorded baseline and reviewed table version,
// never retroactively assume the current baseline or a successful response.
export function estimateOutcomeSavings(value) {
  const outcome = normalizeSessionRecord(value, { includePrompts: false });
  const unpriced = unpriced_reason => ({ priced: false, unpriced_reason });
  if (!outcome || outcome.event !== 'outcome') return unpriced('invalid_telemetry');
  if (outcome.status === 'cancelled') return unpriced('request_cancelled');
  if (outcome.status === 'error') return unpriced('request_failed');
  if (outcome.completion_confirmed !== true) return unpriced('unconfirmed_completion');
  if (outcome.pricing_version !== PRICING_VERSION) return unpriced('unknown_pricing_version');
  if (!OPUS_MODELS.has(outcome.baseline_model)) return unpriced('unknown_baseline');
  if (outcome.usage_complete === false) return unpriced('incomplete_usage');
  if (outcome.model_transitions_truncated || new Set([outcome.confirmed_model, ...(outcome.model_transitions ?? [])].filter(Boolean)).size > 1) return unpriced('mixed_models');
  if (outcome.pricing_eligible === false) return unpriced(outcome.unpriced_reason ?? 'unsupported_pricing');
  const tracker = createSavingsTracker({ baselineModel: outcome.baseline_model });
  const identity = { request_id: 'outcome', session_id: 'outcome' };
  tracker.update({ ...identity, event: 'request_start', pricing_context: outcome.pricing_context });
  if (outcome.http_status !== undefined) tracker.update({ ...identity, event: 'upstream_response', status: outcome.http_status });
  if (outcome.confirmed_model) tracker.update({ ...identity, event: 'upstream_model', model: outcome.confirmed_model });
  if (outcome.usage !== undefined) tracker.update({ ...identity, event: 'upstream_usage', usage: outcome.usage });
  tracker.update({ ...identity, event: 'request_complete' });
  const result = tracker.snapshot().outcome;
  if (result.unpriced_requests) return unpriced(Object.keys(result.unpriced_reasons)[0]);
  return { priced: true, actual_usd: result.actual_usd, baseline_usd: result.baseline_usd,
    saved_usd: result.saved_usd, percent: result.percent, baseline_model: result.baseline_model, pricing_version: result.pricing_version };
}
