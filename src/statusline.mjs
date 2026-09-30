const PHASES = new Set(['routing', 'connecting', 'streaming', 'ready', 'error', 'cancelled']);
const REASONS = {
  tool_turn_pinned: 'turn pinned', prompt_turn_pinned: 'prompt pinned', goal_turn_pinned: 'goal pinned',
  thinking_history: 'thinking pinned', unknown_continuation: 'continuation pinned',
  mid_conversation_system: 'system features', requires_sonnet_capabilities: 'capability guard',
  model_specific_features: 'model features', large_or_multimodal_request: 'large request',
  context_capacity: 'large context',
  internal_request: 'internal request', unknown_model: 'custom model', low_confidence: 'low confidence',
  auto_mode_floor: 'Auto mode floor', auto_mode_safeguards: 'Auto safety', auto_mode_incompatible: 'Auto model guard',
};
const CLASSIFIER_ERRORS = {
  timeout: 'timeout', http_error: 'HTTP error', invalid_response: 'invalid response', network_error: 'network error',
};
const clean = (value, limit = 64) => typeof value === 'string' ? value
  .replace(/\x1b\][\s\S]*?(?:\x07|\x1b\\)/g, '')
  .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '')
  .replace(/[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/g, '')
  .replace(/\s+/g, ' ').trim().slice(0, limit) : '';
const object = value => value && typeof value === 'object' && !Array.isArray(value);
const width = value => [...value].reduce((sum, char) => sum + (/[^\u0000-\u10ff\u2000-\u2e7f]/u.test(char) ? 2 : 1), 0);
function shorten(value, limit) {
  if (width(value) <= limit) return value;
  let text = '';
  for (const char of value) { if (width(text + char) + 1 > limit) break; text += char; }
  return text + '…';
}
function modelName(value) {
  const name = clean(value, 48);
  const match = /^claude-(haiku|sonnet|opus)-(\d+)(?:-(\d{1,2})(?=-|$))?/i.exec(name);
  return match ? `${match[1][0].toUpperCase()}${match[1].slice(1).toLowerCase()} ${match[2]}${match[3] ? '.' + match[3] : ''}` : name;
}

// Only unambiguous native windows are listed. Custom IDs and models whose
// subscription window depends on an opt-in cannot safely imply a capacity.
const CONTEXT_WINDOWS = new Map([
  ...['claude-haiku-4-5', 'claude-haiku-4-5-20251001', 'claude-sonnet-4-5', 'claude-sonnet-4-5-20250929',
    'claude-opus-4-5', 'claude-opus-4-5-20251101'].map(model => [model, 200000]),
  ...['claude-sonnet-5', 'claude-sonnet-5-5', 'claude-opus-4-7', 'claude-opus-4-8',
    'claude-opus-5', 'claude-opus-5-5'].map(model => [model, 1000000]),
]);
const capacityLabel = value => value % 1000000 === 0 ? `${value / 1000000}M` : value % 1000 === 0 ? `${value / 1000}K` : String(value);

function contextLabel(input, state) {
  const client = object(input?.context_window) ? input.context_window : undefined;
  const percent = client?.used_percentage;
  const clientCapacity = Number.isSafeInteger(client?.context_window_size) && client.context_window_size > 0
    ? client.context_window_size : undefined;
  const cli = typeof percent === 'number' && Number.isFinite(percent) && percent >= 0 && percent <= 100
    ? `CLI ctx ${Math.round(percent)}%${clientCapacity ? `/${capacityLabel(clientCapacity)}` : ''}` : '';
  // Claude resets current_usage to null after /compact. Do not revive the
  // previous request's count while that reset is waiting for a fresh response.
  if (client && Object.hasOwn(client, 'current_usage') && client.current_usage === null) return cli;
  const current = object(state?.context_usage) && state.context_usage.model === state.actual_model
    && ['streaming', 'ready'].includes(state.phase) ? state.context_usage : undefined;
  const historical = !current && ['routing', 'connecting', 'streaming', 'error', 'cancelled'].includes(state?.phase)
    && object(state.last_context_usage) ? state.last_context_usage : undefined;
  const usage = current ?? historical;
  const capacity = CONTEXT_WINDOWS.get(usage?.model);
  if (!capacity || !Number.isSafeInteger(usage.input_tokens) || usage.input_tokens < 0) return cli;
  const api = `${historical ? 'last ' : ''}API ctx ${Math.round(usage.input_tokens * 100 / capacity)}%/${capacityLabel(capacity)}`;
  // Keep the client limit visible when it differs: the API window does not
  // change Claude Code's own compaction threshold or remaining-context UI.
  return cli && clientCapacity !== capacity ? `${api} · ${cli}` : api;
}

function savingsLabels(entry) {
  if (!object(entry)) return;
  const count = value => Number.isSafeInteger(value) && value >= 0;
  const dollars = value => typeof value === 'number' && Number.isFinite(value) && value >= 0;
  const unavailable = { full: 'savings unavailable', compact: 'savings unavailable', color: '33' };
  if (!count(entry.requests) || !count(entry.unpriced_requests)) return unavailable;
  if (entry.requests === 0) return entry.unpriced_requests > 0 ? unavailable : undefined;
  if (!dollars(entry.actual_usd) || !dollars(entry.baseline_usd)
    || typeof entry.saved_usd !== 'number' || !Number.isFinite(entry.saved_usd)) return unavailable;
  const extra = entry.saved_usd < 0;
  const amount = Math.abs(entry.saved_usd);
  const money = amount > 0 && amount < 0.01 ? '<$0.01' : `$${amount.toFixed(2)}`;
  const percent = entry.baseline_usd > 0 && typeof entry.percent === 'number' && Number.isFinite(entry.percent)
    ? `${Math.round(Math.abs(entry.percent))}%` : '';
  const partial = entry.partial === true || entry.unpriced_requests > 0;
  const label = `est ${extra ? 'extra' : 'saved'}`;
  const ending = ` vs Opus${partial ? ' partial' : ''}`;
  return {
    full: `${label} ${money}${percent ? ` (${percent})` : ''}${ending}`,
    compact: percent ? `${label} ${percent}${ending}` : `${label} ${money}${ending}`,
    color: extra || partial ? '33' : '32',
  };
}

export function renderStatusLine(input, snapshot, { now = Date.now(), color = true, columns = process.env.COLUMNS, alive } = {}) {
  const paint = (value, code) => color ? `\x1b[${code}m${value}\x1b[0m` : value;
  const numericColumns = Number(columns);
  const available = Number.isFinite(numericColumns) && numericColumns > 0 ? Math.max(1, Math.floor(numericColumns)) : 120;
  let online = object(snapshot) && snapshot.version === 1 && Number.isSafeInteger(snapshot.pid) && snapshot.pid > 0
    && Number.isFinite(snapshot.heartbeat_at) && snapshot.heartbeat_at > 0
    && snapshot.heartbeat_at <= now + 5000 && now - snapshot.heartbeat_at <= 20000;
  if (online && alive !== undefined) {
    try { online = typeof alive === 'function' ? alive(snapshot.pid) !== false : alive !== false; } catch { online = false; }
  }
  if (!online) {
    if (width('● AutoRouter offline') > available) return paint(shorten('AutoRouter offline', available), '2;31');
    return `${paint('●', '31')} ${paint('AutoRouter', '1')} ${paint('offline', '2;31')}`;
  }

  const sessionId = object(input) && (input.session_id === undefined || typeof input.session_id === 'string') ? input.session_id ?? '' : undefined;
  const candidate = sessionId !== undefined && object(snapshot.sessions) && Object.hasOwn(snapshot.sessions, sessionId) ? snapshot.sessions[sessionId] : undefined;
  const savingsEntry = sessionId !== undefined && object(snapshot.savings) && Object.hasOwn(snapshot.savings, sessionId) ? snapshot.savings[sessionId] : undefined;
  const savings = savingsLabels(savingsEntry);
  const state = object(candidate) && PHASES.has(candidate.phase) ? candidate : undefined;
  const phase = state?.phase;
  let status = phase ?? 'awaiting request';
  let model = '';
  let prefix = '';
  let suffix = '';
  let confirmed = false;
  const last = modelName(state?.last_model);
  const actual = modelName(state?.actual_model);
  const selected = modelName(state?.selected_model);
  if (phase === 'streaming' && actual) { model = actual; confirmed = true; }
  else if (phase === 'connecting' && selected) { model = selected; suffix = ' selected'; }
  else if (phase === 'streaming' && selected) { model = selected; suffix = ' unconfirmed'; }
  else if (phase === 'ready' && actual) { model = actual; prefix = 'last '; confirmed = true; }
  else if (phase === 'ready' && selected) { model = selected; suffix = ' unconfirmed'; }
  else if (['ready', 'error', 'cancelled'].includes(phase) && (last || actual)) { model = last || actual; prefix = 'last '; confirmed = true; }
  else if (last) { model = last; prefix = 'last '; confirmed = true; }
  if (phase === 'ready' && !model) status = 'awaiting request';
  const errorType = clean(state?.error_type, 32);
  if (phase === 'error') status = Number.isInteger(state.status) && state.status >= 400 && state.status <= 599
    ? `error ${state.status}` : errorType ? `error ${errorType}` : 'error';

  const details = [];
  const source = ['jev', 'ollama', 'cache', 'fallback', 'passthrough'].includes(state?.source) ? state.source : undefined;
  const evaluator = ['jev', 'ollama'].includes(state?.evaluator) ? state.evaluator : undefined;
  const evaluatorLabel = evaluator === 'ollama' ? 'Ollama' : evaluator === 'jev' ? 'Jev' : '';
  let fallbackCause = '';
  let fallbackPhase = '';
  let compactFallback = false;
  if (source) {
    const sourceLabel = source === 'passthrough' ? 'pass-through' : source === 'jev' ? 'Jev' : source === 'ollama' ? 'Ollama'
      : evaluatorLabel ? `${evaluatorLabel} ${source}` : source;
    const timing = Number.isFinite(state.latency_ms) && state.latency_ms >= 0 ? ` ${Math.round(Math.min(state.latency_ms, 999999))}ms` : '';
    const classified = ['haiku', 'sonnet', 'opus'].includes(state.classified_tier) ? state.classified_tier : undefined;
    const chosenFamily = /^claude-(haiku|sonnet|opus)-/.exec(state.selected_model ?? '')?.[1];
    const override = source !== 'fallback' && classified && chosenFamily && classified !== chosenFamily
      ? `→${classified[0].toUpperCase()}${classified.slice(1)}` : '';
    details.push(`${sourceLabel}${override}${timing}`);
  }
  if (source === 'fallback') {
    // Snapshots normally contain allowlisted categories, but the renderer also
    // rejects raw error messages so paths and provider response text stay out.
    fallbackCause = Object.hasOwn(CLASSIFIER_ERRORS, state.classifier_error) ? CLASSIFIER_ERRORS[state.classifier_error] : '';
    if (state.classifier_error === 'http_error' && Number.isInteger(state.classifier_status)
      && state.classifier_status >= 100 && state.classifier_status <= 599) fallbackCause = `HTTP ${state.classifier_status}`;
    if (fallbackCause) details.push(fallbackCause);
  }
  if (Object.hasOwn(REASONS, state?.reason)) details.push(state.reason === 'context_capacity' && state.context_check === 'count_unavailable'
    ? 'size unverified' : REASONS[state.reason]);
  if (phase === 'error' && errorType && !status.includes(errorType)) details.push(errorType);
  let context = contextLabel(input, state);
  let detail = details.join(' · ');
  let saving = savings?.full ?? '';
  let brand = '● AutoRouter';
  const modelLabel = () => model ? prefix + model + suffix : '';
  const plain = () => [brand, modelLabel(), status, detail, saving, context].filter(Boolean).join(' · ');
  if (width(plain()) > available) context = '';
  // Routine routing details can yield to the estimate, while errors and
  // fallback explanations have priority over savings. Never shorten away
  // "est" or "partial": hide the whole metric when its qualifiers cannot fit.
  if (width(plain()) > available && source !== 'fallback' && phase !== 'error') detail = '';
  if (width(plain()) > available && saving) saving = savings.compact;
  if (width(plain()) > available) saving = '';
  if (width(plain()) > available && source === 'fallback') {
    // A successful Claude response can still follow evaluator failure. When
    // space is tight, retain that cause instead of an ordinary "ready" phase,
    // evaluator timing, or the guard details that followed the fallback.
    compactFallback = true;
    fallbackPhase = ['error', 'cancelled'].includes(phase) ? status : '';
    status = [fallbackPhase, `${evaluatorLabel ? `${evaluatorLabel} ` : ''}fallback${fallbackCause ? `: ${fallbackCause}` : ''}`].filter(Boolean).join(' · ');
    detail = '';
  }
  if (width(plain()) > available) detail = '';
  if (width(plain()) > available) brand = '● AR';
  if (width(plain()) > available) brand = '';
  if (width(plain()) > available && compactFallback) {
    status = [fallbackPhase, `fallback${fallbackCause ? `: ${fallbackCause}` : ''}`].filter(Boolean).join(' · ');
  }
  if (width(plain()) > available && model) {
    if (phase === 'streaming' && !compactFallback) status = 'stream';
    const room = available - width(prefix + suffix + status) - 3;
    if (room >= 1) model = shorten(model, room);
    else {
      // A selection's qualifier is essential. If it cannot fit, show only the
      // router phase instead of making an unconfirmed model look confirmed.
      model = ''; prefix = ''; suffix = '';
      if (width('● AutoRouter · ' + status) <= available) brand = '● AutoRouter';
      else if (width('● AR · ' + status) <= available) brand = '● AR';
    }
  }
  if (width(plain()) > available && compactFallback && !fallbackPhase && available >= width('fallback')
    && available < width('fallback: ') + 2) status = 'fallback';
  if (width(plain()) > available) status = shorten(status, Math.max(1, available - width(modelLabel()) - (model ? 3 : 0)));
  const attention = phase === 'error' ? '31' : source === 'fallback' ? '33' : phase === 'cancelled' ? '2' : phase === 'streaming' || phase === 'ready' ? '32' : '36';
  const chunks = [];
  if (brand) chunks.push(`${paint('●', attention)} ${paint(brand.slice(2), '1')}`);
  if (model) chunks.push(paint(modelLabel(), confirmed ? '37' : '33'));
  chunks.push(paint(status, attention));
  if (detail) chunks.push(paint(detail, source === 'fallback' ? '33' : '2'));
  if (saving) chunks.push(paint(saving, savings.color));
  if (context) chunks.push(paint(context, '2'));
  return chunks.join(paint(' · ', '2'));
}
