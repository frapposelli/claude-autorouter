// @ts-check
import { redactSensitive } from './redaction.mjs';
// Shared, bounded telemetry contract. Never copy request bodies, headers, raw
// errors or arbitrary provider fields into status snapshots or saved history.
export const TELEMETRY_SCHEMA_VERSION = 2;
export const UNPRICED_REASONS = Object.freeze(['request_failed', 'request_cancelled', 'request_evicted',
  'unknown_baseline', 'unknown_model', 'missing_model', 'missing_usage', 'invalid_usage',
  'conflicting_usage', 'mixed_models', 'unsupported_pricing', 'invalid_telemetry',
  'unknown_pricing_version', 'unconfirmed_completion', 'incomplete_usage', 'missing_outcome']);
const EVENTS = new Set(['request_start', 'route', 'upstream_response', 'upstream_model', 'upstream_usage',
  'upstream_error', 'request_complete', 'request_error', 'request_cancelled']);
const SOURCES = new Set(['jev', 'ollama', 'cache', 'fallback', 'passthrough']);
const ERRORS = new Set(['invalid_request_error', 'authentication_error', 'billing_error', 'permission_error',
  'not_found_error', 'request_too_large', 'rate_limit_error', 'api_error', 'overloaded_error', 'timeout_error',
  'unknown_error', 'http_error', 'request_error']);
const CLASSIFIER_ERRORS = new Set(['timeout', 'http_error', 'invalid_response', 'network_error', 'capacity_exhausted']);
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
export const sanitizeIdentifier = value => typeof value === 'string' && /^[A-Za-z0-9_.:-]{1,200}$/.test(value) ? value : undefined;
export const sanitizeModel = value => typeof value === 'string' && /^[A-Za-z0-9_.:/-]{1,120}$/.test(value) ? value : undefined;
const code = value => typeof value === 'string' && /^[a-z][a-z0-9_]{0,79}$/.test(value) ? value : undefined;
const count = value => Number.isSafeInteger(value) && value >= 0;
const latency = value => typeof value === 'number' && Number.isFinite(value) && value >= 0;
const httpStatus = value => Number.isInteger(value) && value >= 100 && value <= 599;
const absent = value => value === undefined || value === null || value === '';
const assign = (row, field, value) => { if (value !== undefined) row[field] = value; };

export function normalizePricingContext(value) {
  if (value === undefined) return;
  if (!object(value)) return { pricing_unsupported: true };
  const row = {};
  for (const [key, allowed] of Object.entries({ speed: ['standard', 'fast'],
    inference_geo: ['global', 'us', 'not_available'], service_tier: ['auto', 'standard_only', 'standard', 'priority', 'batch'] })) {
    if (!Object.hasOwn(value, key)) continue;
    if (allowed.includes(value[key])) row[key] = value[key];
    else row.pricing_unsupported = true;
  }
  for (const key of ['pricing_unsupported', 'unsupported']) {
    if (Object.hasOwn(value, key) && value[key] !== false) row.pricing_unsupported = true;
  }
  return row;
}

export function normalizeUsageTelemetry(value) {
  if (value === undefined) return;
  if (!object(value)) return { pricing_unsupported: true };
  /** @type {Record<string,any>} */
  const row = { ...normalizePricingContext(value) };
  for (const key of ['input_tokens', 'output_tokens', 'cache_read_input_tokens', 'cache_creation_input_tokens']) {
    if (!Object.hasOwn(value, key)) continue;
    if (count(value[key])) row[key] = value[key];
    else row.pricing_unsupported = true;
  }
  if (Object.hasOwn(value, 'cache_creation')) {
    if (value.cache_creation === null && value.cache_creation_input_tokens === 0) { /* no cache writes */ }
    else if (!object(value.cache_creation)) row.pricing_unsupported = true;
    else {
      row.cache_creation = {};
      for (const key of ['ephemeral_5m_input_tokens', 'ephemeral_1h_input_tokens']) {
        if (!Object.hasOwn(value.cache_creation, key)) continue;
        if (count(value.cache_creation[key])) row.cache_creation[key] = value.cache_creation[key];
        else row.pricing_unsupported = true;
      }
    }
  }
  return row;
}

function reasons(value) {
  if (!object(value)) return;
  return Object.fromEntries(UNPRICED_REASONS.filter(key => count(value[key])).map(key => [key, value[key]]));
}

function normalizeSavings(value) {
  if (!object(value)) return;
  const row = {};
  assign(row, 'baseline_model', sanitizeModel(value.baseline_model));
  for (const key of ['actual_usd', 'baseline_usd', 'saved_usd', 'percent']) {
    if (typeof value[key] === 'number' && Number.isFinite(value[key]) && Math.abs(value[key]) <= 1e18
      && (!['actual_usd', 'baseline_usd'].includes(key) || value[key] >= 0)) row[key] = value[key];
  }
  for (const key of ['requests', 'priced_requests', 'unpriced_requests']) if (count(value[key])) row[key] = value[key];
  if (value.partial === true) row.partial = true;
  if (typeof value.pricing_version === 'string' && /^[A-Za-z0-9_.-]{1,40}$/.test(value.pricing_version)) row.pricing_version = value.pricing_version;
  if (typeof value.pricing_date === 'string' && /^\d{4}-\d\d-\d\d$/.test(value.pricing_date)) row.pricing_date = value.pricing_date;
  // Provenance is a maintained public URL, never an arbitrary URL that might
  // contain credentials or a private endpoint.
  if (value.pricing_source === 'https://platform.claude.com/docs/en/about-claude/pricing') row.pricing_source = value.pricing_source;
  assign(row, 'unpriced_reasons', reasons(value.unpriced_reasons));
  return row;
}

function timestamp(value) {
  return typeof value === 'string' && /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/.test(value)
    && Number.isFinite(Date.parse(value)) ? value : new Date().toISOString();
}

/**
 * @template {import('./contracts.mjs').LifecycleName|'decision'|'outcome'} T
 * @param {any} entry
 * @param {T} event
 * @returns {(import('./contracts.mjs').TelemetryFields & {event:T,request_id:string})|undefined}
 */
function base(entry, event) {
  const requestId = sanitizeIdentifier(entry.request_id);
  if (!requestId) return;
  /** @type {import('./contracts.mjs').TelemetryFields & {event:T,request_id:string}} */
  const row = { schema_version: TELEMETRY_SCHEMA_VERSION, event, timestamp: timestamp(entry.timestamp), request_id: requestId };
  for (const key of ['session_id', 'agent_id', 'prompt_id']) {
    if (absent(entry[key])) continue;
    const value = sanitizeIdentifier(entry[key]);
    // Reject invalid explicit identities instead of merging them into the
    // anonymous session or treating an unknown agent as the foreground UI.
    if (!value) return;
    row[key] = value;
  }
  if (!absent(entry.request_class)) {
    const requestClass = code(entry.request_class);
    if (!requestClass) return;
    row.request_class = requestClass;
  }
  for (const key of ['requested_model', 'selected_model', 'confirmed_model', 'model', 'baseline_model']) assign(row, key, sanitizeModel(entry[key]));
  if (typeof entry.pricing_version === 'string' && /^[A-Za-z0-9_.-]{1,40}$/.test(entry.pricing_version)) row.pricing_version = entry.pricing_version;
  for (const key of ['reason', 'compatibility_reason', 'continuity_state']) assign(row, key, code(entry[key]));
  if (SOURCES.has(entry.source)) row.source = entry.source;
  if (['jev', 'ollama'].includes(entry.evaluator)) row.evaluator = entry.evaluator;
  for (const key of ['tier', 'classified_tier']) if (['haiku', 'sonnet', 'opus'].includes(entry[key])) row[key] = entry[key];
  for (const key of ['latency_ms', 'evaluation_latency_ms', 'routing_latency_ms', 'decision_latency_ms',
    'first_response_ms', 'upstream_latency_ms', 'total_latency_ms']) if (latency(entry[key])) row[key] = entry[key];
  if (CLASSIFIER_ERRORS.has(entry.classifier_error)) row.classifier_error = entry.classifier_error;
  if (httpStatus(entry.classifier_status)) row.classifier_status = entry.classifier_status;
  if (ERRORS.has(entry.error_type)) row.error_type = entry.error_type;
  if (httpStatus(entry.http_status)) row.http_status = entry.http_status;
  if (['within_budget', 'over_budget', 'count_unavailable'].includes(entry.context_check)) row.context_check = entry.context_check;
  if (count(entry.counted_input_tokens)) row.counted_input_tokens = entry.counted_input_tokens;
  if (Array.isArray(entry.model_transitions)) {
    const transitions = entry.model_transitions.slice(0, 16).map(sanitizeModel).filter(Boolean);
    row.model_transitions = transitions;
    if (entry.model_transitions.length > 16 || transitions.length !== entry.model_transitions.length) row.model_transitions_truncated = true;
  } else if (entry.model_transitions !== undefined) row.model_transitions_truncated = true;
  if (entry.model_transitions_truncated === true) row.model_transitions_truncated = true;
  assign(row, 'usage', normalizeUsageTelemetry(entry.usage));
  assign(row, 'pricing_context', normalizePricingContext(entry.pricing_context));
  for (const key of ['usage_complete', 'pricing_eligible', 'completion_confirmed']) if (typeof entry[key] === 'boolean') row[key] = entry[key];
  if (UNPRICED_REASONS.includes(entry.unpriced_reason)) row.unpriced_reason = entry.unpriced_reason;
  assign(row, 'savings', normalizeSavings(entry.savings));
  assign(row, 'savings_coverage', normalizeSavings(entry.savings_coverage));
  return row;
}

/** @param {any} entry @returns {import('./contracts.mjs').LifecycleEvent|undefined} */
export function normalizeTelemetryEvent(entry) {
  try {
    if (!object(entry)) return;
    const event = entry.event === 'error' ? 'request_error' : entry.event === 'cancelled' ? 'request_cancelled' : entry.event;
    if (!EVENTS.has(event)) return;
    const row = base(entry, event);
    if (row && httpStatus(entry.status)) row.status = entry.status;
    return row;
  } catch { return; }
}

function excerpt(value) {
  let text = '', length = 0;
  if (typeof value !== 'string') return { text, truncated: false };
  // Defense in depth, and it also covers records written before redaction
  // existed when they are read back. Already-redacted text is unchanged.
  value = redactSensitive(value);
  for (const character of value) {
    if (length++ === 500) return { text: text.toWellFormed(), truncated: true };
    text += character;
  }
  return { text: text.toWellFormed(), truncated: false };
}

/**
 * @param {any} entry
 * @param {{includePrompts?:boolean}} [options]
 * @returns {import('./contracts.mjs').SessionRecord|undefined}
 */
export function normalizeSessionRecord(entry, { includePrompts = true } = {}) {
  try {
    if (!object(entry) || !['decision', 'outcome'].includes(entry.event)) return;
    if (entry.schema_version !== undefined && ![1, 2].includes(entry.schema_version)) return;
    const row = base(entry, entry.event);
    if (!row) return;
    if (entry.event === 'decision') {
      if (!row.requested_model || !row.selected_model) return;
      if (includePrompts) {
        const foreground = !row.request_class || row.request_class === 'main';
        const prompt = foreground ? excerpt(entry.prompt_excerpt) : { text: '', truncated: false };
        row.prompt_excerpt = prompt.text;
        row.prompt_truncated = prompt.truncated || (foreground && entry.prompt_truncated === true);
      }
    } else {
      if (!['completed', 'error', 'cancelled'].includes(entry.status)) return;
      row.status = entry.status;
    }
    return row;
  } catch { return; }
}
