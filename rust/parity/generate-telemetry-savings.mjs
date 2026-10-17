// Synthetic metadata, privacy, pricing and bounded-state differential cases.
let sequence = 0;
const emit = (op, input) => process.stdout.write(`${JSON.stringify({ id: `accounting-${sequence++}`, op, input })}\n`);
const model = 'claude-haiku-4-5-20251001';
const opus = 'claude-opus-5-5';
const usage = { input_tokens: 1000000, output_tokens: 1000000 };
const values = [null, '', 'private canary', '__proto__', 'main', true, false, 0, 1, -1, 1.5, 200, 599,
  Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER + 1, {}, [], ['claude-sonnet-5']];
const entry = { event: 'route', request_id: 'r1', session_id: 's1', requested_model: model, selected_model: opus,
  confirmed_model: model, status: 'completed', completion_confirmed: true, baseline_model: opus,
  pricing_version: '2026-09-29.1', usage, prompt_excerpt: 'DB_PASSWORD=synthetic-value email person@example.com' };
for (const field of ['schema_version', 'request_id', 'session_id', 'agent_id', 'prompt_id', 'request_class', 'timestamp',
  'requested_model', 'selected_model', 'confirmed_model', 'model', 'baseline_model', 'pricing_version', 'reason',
  'compatibility_reason', 'continuity_state', 'source', 'evaluator', 'tier', 'classified_tier', 'latency_ms',
  'evaluation_latency_ms', 'routing_latency_ms', 'http_status', 'classifier_status', 'classifier_error', 'error_type',
  'context_check', 'counted_input_tokens', 'model_transitions', 'model_transitions_truncated', 'usage', 'pricing_context',
  'usage_complete', 'pricing_eligible', 'completion_confirmed', 'unpriced_reason', 'savings', 'savings_coverage',
  'prompt_excerpt', 'prompt_truncated', 'status']) {
  for (const value of values) {
    const mutated = { ...entry, [field]: value };
    emit('normalize_telemetry', mutated);
    for (const include_prompts of [true, false]) emit('normalize_session', { entry: { ...mutated, event: 'decision' }, include_prompts });
    emit('normalize_session', { entry: { ...mutated, event: 'outcome' } });
    emit('estimate_savings', { ...mutated, event: 'outcome' });
  }
}
for (const timestamp of ['0000-01-01T00:00:00.000Z', '2026-02-30T00:00:00.000Z', '2026-04-31T00:00:00.000Z',
  '2026-01-01T24:00:00.000Z', '2026-01-01T24:00:00.001Z', '2026-13-01T00:00:00.000Z',
  '2026-01-00T00:00:00.000Z', '2026-01-01T00:00:60.000Z']) emit('normalize_telemetry', { ...entry, timestamp });
const usages = [undefined, null, {}, usage];
for (const field of ['input_tokens', 'output_tokens', 'cache_read_input_tokens', 'cache_creation_input_tokens',
  'cache_creation', 'speed', 'inference_geo', 'service_tier', 'unsupported', 'pricing_unsupported']) {
  for (const value of values) usages.push({ ...usage, [field]: value });
}
for (const amount of [0, 1, 1000000, Number.MAX_SAFE_INTEGER]) {
  usages.push({ ...usage, cache_creation: { ephemeral_5m_input_tokens: amount, ephemeral_1h_input_tokens: amount } },
    { ...usage, cache_creation_input_tokens: amount, cache_creation: { ephemeral_5m_input_tokens: amount } });
}
usages.push({ ...usage, cache_creation_input_tokens: 0, cache_creation: null }, { ...usage, inference_geo: 'not_available' });
const contexts = [undefined, null, {}, { speed: 'standard' }, { speed: 'fast' }, { speed: null },
  { inference_geo: 'global' }, { inference_geo: 'us' }, { inference_geo: 'not_available' },
  { service_tier: 'standard' }, { service_tier: 'priority' }, { unsupported: false }, { unsupported: true }];
for (const tokens of usages) if (tokens !== undefined) emit('normalize_usage', tokens);
for (const context of contexts) if (context !== undefined) emit('normalize_pricing', context);
const event = (event, extra = {}) => ({ event, request_id: 'r1', session_id: 's1', ...extra });
const events = (confirmed, tokens, context) => [event('request_start'), event('route', { pricing_context: context }),
  event('upstream_response', { status: 200 }), event('upstream_model', { model: confirmed }),
  event('upstream_usage', { usage: tokens }), event('request_complete')];
for (const confirmed of [model, 'claude-haiku-4-5', 'claude-sonnet-5', 'claude-sonnet-5-5', 'claude-opus-5', opus, 'unknown', null, undefined]) {
  for (const tokens of usages) {
    emit('savings_tracker', { events: events(confirmed, tokens) });
    emit('estimate_savings', { ...entry, event: 'outcome', confirmed_model: confirmed, usage: tokens });
  }
  for (const context of contexts) emit('savings_tracker', { events: events(confirmed, usage, context) });
}
for (const baseline_model of [undefined, null, '', 'private canary', 'claude-sonnet-5', opus, 'claude-opus-5']) {
  for (const status of [undefined, null, 0, 199, 200, 299, 300, 429, 500, '200', 200.5]) {
    const batch = events(model, usage); batch[2].status = status;
    emit('savings_tracker', { baseline_model, events: batch });
  }
}
for (const session_id of [undefined, null, '', '__proto__', 'constructor', 'private/invalid', {}, 42]) {
  for (const duplicate of [event('upstream_model', { model: 'claude-sonnet-5' }),
    event('upstream_usage', { usage: { ...usage, output_tokens: 2 } }), event('request_error'),
    event('request_cancelled'), event('request_complete', { completion_confirmed: false })]) {
    const batch = events(model, usage); batch.splice(5, 0, duplicate);
    emit('savings_tracker', { events: batch.map(event => ({ ...event, session_id })) });
  }
}
for (const length of [100, 101, 1000, 1001, 10000, 10001]) {
  const batch = [];
  for (let i = 0; i < length; i++) {
    if (length < 102) batch.push(event('request_start', { session_id: `s${i}` }));
    else if (length < 1002) batch.push(event('request_start', { request_id: `r${i}` }));
    else batch.push(...events(model, usage).map(event => ({ ...event, request_id: `r${i}` })));
  }
  emit('savings_tracker', { events: batch });
}
emit('savings_tracker', { steps: [...events(model, usage).map(event => ({ op: 'update', event })),
  { op: 'snapshot' }, { op: 'clear' }, { op: 'snapshot' },
  ...events(model, usage).map(event => ({ op: 'update', event })), { op: 'snapshot' }] });
