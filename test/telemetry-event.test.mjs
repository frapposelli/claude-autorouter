import test from 'node:test';
import assert from 'node:assert/strict';
import { normalizeTelemetryEvent, normalizeSessionRecord, normalizeUsageTelemetry, normalizePricingContext,
  TELEMETRY_SCHEMA_VERSION } from '../src/telemetry-event.mjs';

const MODEL = 'claude-sonnet-5';
const fixture = { event: 'route', timestamp: '2026-10-05T10:00:00.000Z', request_id: 'request-1', session_id: 'session-1',
  requested_model: 'claude-haiku-4-5-20251001', selected_model: MODEL, model: MODEL, source: 'jev', evaluator: 'jev',
  classified_tier: 'haiku', reason: 'context_capacity', compatibility_reason: 'context_limit', continuity_state: 'confirmed',
  evaluation_latency_ms: 12.1, routing_latency_ms: 14, decision_latency_ms: 12.1, first_response_ms: 20, total_latency_ms: 50,
  context_check: 'over_budget', counted_input_tokens: 210000 };

test('one versioned allowlist retains decision evidence and excludes bodies, credentials and raw errors', () => {
  const entry = { ...fixture, authorization: 'SECRET', headers: { api_key: 'SECRET' }, body: { text: 'SECRET' },
    error: new Error('SECRET'), error_type: 'SECRET', prompt_excerpt: 'SECRET', unknown: 'SECRET' };
  assert.deepEqual(normalizeTelemetryEvent(entry), { schema_version: TELEMETRY_SCHEMA_VERSION, ...fixture });
  assert.equal(JSON.stringify(normalizeTelemetryEvent(entry)).includes('SECRET'), false);
  assert.equal(entry.body.text, 'SECRET');
});

test('all internal lifecycle names normalize and public terminal aliases retain their meaning', () => {
  for (const event of ['request_start', 'route', 'upstream_response', 'upstream_model', 'upstream_usage', 'upstream_error',
    'request_complete', 'request_error', 'request_cancelled']) assert.equal(normalizeTelemetryEvent({ ...fixture, event }).event, event);
  assert.equal(normalizeTelemetryEvent({ ...fixture, event: 'error' }).event, 'request_error');
  assert.equal(normalizeTelemetryEvent({ ...fixture, event: 'cancelled' }).event, 'request_cancelled');
  assert.equal(normalizeTelemetryEvent({ ...fixture, event: 'private' }), undefined);
});

test('invalid explicit identities cannot collapse into anonymous sessions or foreground requests', () => {
  for (const key of ['session_id', 'request_id', 'agent_id', 'prompt_id', 'request_class']) {
    for (const value of ['a/b invalid', {}, 123, 'x'.repeat(201)]) assert.equal(normalizeTelemetryEvent({ ...fixture, [key]: value }), undefined, key);
  }
  assert.equal(normalizeTelemetryEvent({ ...fixture, session_id: '' }).session_id, undefined);
  assert.equal(normalizeTelemetryEvent({ ...fixture, request_class: 'future_background' }).request_class, 'future_background');
  assert.equal(normalizeTelemetryEvent({ ...fixture, session_id: '__proto__' }).session_id, '__proto__');
  assert.equal(normalizeTelemetryEvent({ ...fixture, request_id: '' }), undefined);
});

test('timings, enums, counts and model transitions are bounded without retaining invalid payloads', () => {
  const result = normalizeTelemetryEvent({ ...fixture, status: 599, http_status: 200, classifier_status: 429,
    latency_ms: Infinity, routing_latency_ms: -1, total_latency_ms: Infinity, counted_input_tokens: '1',
    reason: 'unsafe reason', error_type: 'api_error', classifier_error: 'timeout',
    model_transitions: Array(1000).fill(MODEL), unknown: { enormous: 'x'.repeat(1000000) } });
  assert.equal(result.status, 599);
  assert.equal(result.error_type, 'api_error');
  for (const key of ['latency_ms', 'routing_latency_ms', 'total_latency_ms', 'counted_input_tokens', 'reason', 'unknown']) assert.equal(result[key], undefined);
  assert.equal(result.model_transitions.length, 16);
  assert.equal(result.model_transitions_truncated, true);
  assert.ok(JSON.stringify(result).length < 3000);
  assert.equal(normalizeTelemetryEvent({ ...fixture, model_transitions: [MODEL, { secret: 'SECRET' }] }).model_transitions_truncated, true);
});

test('usage and pricing normalizers preserve valid counts and never erase malformed pricing into standard prices', () => {
  const usage = { input_tokens: 100, output_tokens: 3, cache_read_input_tokens: 20, cache_creation_input_tokens: 40,
    cache_creation: { ephemeral_5m_input_tokens: 10, ephemeral_1h_input_tokens: 30 }, inference_geo: 'global' };
  assert.deepEqual(normalizeUsageTelemetry({ ...usage, secret: 'SECRET' }), usage);
  for (const value of [null, [], 'SECRET', { ...usage, input_tokens: 'SECRET' }, { ...usage, cache_creation: null },
    { ...usage, cache_creation: { ephemeral_5m_input_tokens: -1 } }, { ...usage, service_tier: 'SECRET' }]) {
    const safe = normalizeUsageTelemetry(value);
    assert.equal(safe.pricing_unsupported, true);
    assert.equal(JSON.stringify(safe).includes('SECRET'), false);
  }
  assert.deepEqual(normalizePricingContext({ speed: 'fast', inference_geo: 'us', service_tier: 'priority', authorization: 'SECRET' }),
    { speed: 'fast', inference_geo: 'us', service_tier: 'priority' });
  assert.deepEqual(normalizePricingContext({ speed: null }), { pricing_unsupported: true });
  assert.deepEqual(normalizeUsageTelemetry({ input_tokens: 1, output_tokens: 1, cache_creation_input_tokens: 0, cache_creation: null }),
    { input_tokens: 1, output_tokens: 1, cache_creation_input_tokens: 0 });
});

test('legacy decisions upgrade without claiming success and foreground excerpts use 500 Unicode code points', () => {
  const result = normalizeSessionRecord({ ...fixture, schema_version: 1, event: 'decision', prompt_excerpt: '🦊'.repeat(501) });
  assert.equal(result.schema_version, 2);
  assert.equal(result.event, 'decision');
  assert.equal(result.status, undefined);
  assert.equal([...result.prompt_excerpt].length, 500);
  assert.equal(result.prompt_truncated, true);
  assert.equal(normalizeSessionRecord({ ...fixture, event: 'decision', schema_version: 99 }), undefined);
  assert.equal(normalizeSessionRecord({ ...fixture, event: 'decision', selected_model: undefined }), undefined);
});

test('metadata-only and background records never persist prompt text', () => {
  const entry = { ...fixture, event: 'decision', prompt_excerpt: 'SECRET', prompt_truncated: true };
  const metadata = normalizeSessionRecord(entry, { includePrompts: false });
  assert.equal(metadata.prompt_excerpt, undefined);
  assert.equal(metadata.prompt_truncated, undefined);
  for (const extra of [{ agent_id: 'agent-1', request_class: 'subagent' }, { request_class: 'auxiliary' }, { request_class: 'future_background' }]) {
    const result = normalizeSessionRecord({ ...entry, ...extra });
    assert.equal(result.prompt_excerpt, '');
    assert.equal(result.prompt_truncated, false);
  }
  assert.equal(normalizeSessionRecord({ ...entry, agent_id: 'main-agent', request_class: 'main' }).prompt_excerpt, 'SECRET');
  assert.equal(JSON.stringify(metadata).includes('SECRET'), false);
});

test('outcomes require explicit completion status and preserve confirmed models separately from selection', () => {
  const result = normalizeSessionRecord({ ...fixture, event: 'outcome', status: 'completed', confirmed_model: 'claude-opus-5-5',
    completion_confirmed: true, baseline_model: 'claude-opus-5-5', pricing_version: '2026-09-29.1', prompt_excerpt: 'SECRET',
    model_transitions: [MODEL, 'claude-opus-5-5'], usage: { input_tokens: 1, output_tokens: 1 }, usage_complete: true });
  assert.equal(result.selected_model, MODEL);
  assert.equal(result.confirmed_model, 'claude-opus-5-5');
  assert.equal(result.status, 'completed');
  assert.equal(result.completion_confirmed, true);
  assert.equal(result.pricing_version, '2026-09-29.1');
  assert.equal(result.prompt_excerpt, undefined);
  for (const status of [200, undefined, 'complete']) assert.equal(normalizeSessionRecord({ ...fixture, event: 'outcome', status }), undefined);
  assert.equal(normalizeSessionRecord({ event: 'outcome', request_id: 'early-error', status: 'error' }).selected_model, undefined);
  assert.equal(normalizeSessionRecord({ ...fixture, event: 'outcome', status: 'completed', total_latency_ms: 86400001 }).total_latency_ms, 86400001);
});

test('savings metadata and coverage retain only finite amounts and bounded known reasons', () => {
  const safe = normalizeSessionRecord({ ...fixture, event: 'outcome', status: 'completed', savings: { baseline_model: MODEL,
    actual_usd: 1, saved_usd: -1, percent: -10, requests: 2, unpriced_requests: 1, partial: true,
    pricing_version: '2026-09-29.1', pricing_date: '2026-09-29', pricing_source: 'https://private/?secret=SECRET',
    unpriced_reasons: { mixed_models: 1, invalid_usage: -1, secret: 'SECRET' }, secret: 'SECRET' } });
  assert.deepEqual(safe.savings.unpriced_reasons, { mixed_models: 1 });
  assert.equal(safe.savings.saved_usd, -1);
  assert.equal(safe.savings.pricing_source, undefined);
  assert.equal(JSON.stringify(safe).includes('SECRET'), false);
});

test('malformed inputs cannot throw or traverse arbitrary objects', () => {
  const circular = {}; circular.self = circular;
  for (const value of [undefined, null, [], 1, circular, { get event() { throw new Error('SECRET'); } }]) {
    assert.equal(normalizeTelemetryEvent(value), undefined);
    assert.equal(normalizeSessionRecord(value), undefined);
  }
  assert.equal(normalizeTelemetryEvent({ ...fixture, get usage() { throw new Error('SECRET'); } }), undefined);
});
