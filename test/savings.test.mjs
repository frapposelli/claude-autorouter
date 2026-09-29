import test from 'node:test';
import assert from 'node:assert/strict';
import { createSavingsTracker } from '../src/savings.mjs';

const HAIKU = 'claude-haiku-4-5-20251001';
const SONNET = 'claude-sonnet-5';
const OPUS = 'claude-opus-5-5';
const usage = { input_tokens: 1000000, output_tokens: 1000000 };
const event = (name, extra = {}) => ({ event: name, session_id: 'session-a', request_id: 'request-1', ...extra });
const current = tracker => tracker.snapshot()['session-a'];
function complete(tracker, options = {}) {
  const { model = HAIKU, extra = {}, context, status = 200 } = options;
  const tokens = Object.hasOwn(options, 'tokens') ? options.tokens : usage;
  tracker.update(event('request_start', extra));
  tracker.update(event('route', { model, pricing_context: context, ...extra }));
  tracker.update(event('upstream_response', { status, ...extra }));
  tracker.update(event('upstream_model', { model, ...extra }));
  tracker.update(event('upstream_usage', { usage: tokens, ...extra }));
  tracker.update(event('request_complete', extra));
}

test('compares provider-confirmed token usage with Opus and exposes only aggregate estimates', () => {
  const tracker = createSavingsTracker();
  tracker.update(event('request_start', { body: 'SECRET' }));
  tracker.update(event('route', { model: OPUS, requested_model: OPUS, prompt: 'SECRET' }));
  tracker.update(event('upstream_model', { model: HAIKU }));
  tracker.update(event('upstream_usage', { usage: { ...usage, private_content: 'SECRET' } }));
  assert.equal(current(tracker).requests, 0);
  tracker.update(event('request_complete'));
  assert.deepEqual(current(tracker), { baseline_model: OPUS, actual_usd: 6, baseline_usd: 24,
    saved_usd: 18, percent: 75, requests: 1, unpriced_requests: 0 });
  assert.equal(JSON.stringify(tracker.snapshot()).includes('SECRET'), false);
  assert.equal(JSON.stringify(tracker.snapshot()).includes('input_tokens'), false);
});

test('prices cache reads and both write TTLs without counting cached input twice', () => {
  const tracker = createSavingsTracker();
  complete(tracker, { tokens: { input_tokens: 100, output_tokens: 200,
    cache_read_input_tokens: 300, cache_creation_input_tokens: 1500,
    cache_creation: { ephemeral_5m_input_tokens: 500, ephemeral_1h_input_tokens: 1000 } } });
  const result = current(tracker);
  assert.equal(result.actual_usd, 0.003755);
  assert.equal(result.baseline_usd, 0.01496);
  assert.equal(result.saved_usd, 0.011205);
  assert.equal(result.percent, 0.011205 / 0.01496 * 100);
  complete(tracker, { model: SONNET, extra: { request_id: 'read-only' },
    tokens: { input_tokens: 0, output_tokens: 0, cache_read_input_tokens: 1000000 } });
  assert.equal(current(tracker).actual_usd, 0.203755);
  assert.equal(current(tracker).baseline_usd, 0.21496);
  assert.equal(current(tracker).saved_usd, 0.011205);
});

test('supports documented exact aliases, an Opus baseline override, and negative savings', () => {
  for (const model of ['claude-haiku-4-5', HAIKU, SONNET, 'claude-sonnet-5-5', OPUS, 'claude-opus-5']) {
    const tracker = createSavingsTracker();
    complete(tracker, { model });
    assert.equal(current(tracker).requests, 1, model);
  }
  const original = createSavingsTracker({ baselineModel: 'claude-opus-5' });
  complete(original);
  assert.equal(current(original).saved_usd, 24);
  assert.equal(current(original).percent, 80);
  const moreExpensive = createSavingsTracker();
  complete(moreExpensive, { model: 'claude-opus-5' });
  assert.equal(current(moreExpensive).saved_usd, -6);
  assert.equal(current(moreExpensive).percent, -25);
});

test('includes main, subagent and auxiliary calls and settles overlapping requests independently', () => {
  const tracker = createSavingsTracker();
  const calls = [
    { request_id: 'main', model: HAIKU, request_class: 'main' },
    { request_id: 'subagent', model: SONNET, agent_id: 'agent-a', request_class: 'subagent' },
    { request_id: 'aux', model: OPUS, request_class: 'auxiliary' },
  ];
  for (const call of calls) {
    tracker.update(event('request_start', call));
    tracker.update(event('upstream_model', call));
    tracker.update(event('upstream_usage', { ...call, usage }));
  }
  tracker.update(event('request_complete', calls[1]));
  tracker.update(event('request_complete', calls[2]));
  tracker.update(event('request_complete', calls[0]));
  assert.equal(current(tracker).requests, 3);
  assert.equal(current(tracker).actual_usd, 42);
  assert.equal(current(tracker).baseline_usd, 72);
  assert.equal(current(tracker).saved_usd, 30);
});

test('keeps sessions isolated and rejects malformed IDs without prototype pollution', () => {
  const tracker = createSavingsTracker();
  for (const session_id of ['session-a', '', '__proto__', 'constructor']) complete(tracker, { extra: { session_id } });
  complete(tracker, { model: SONNET, extra: { session_id: 'session-b' } });
  complete(tracker, { extra: { session_id: 'bad/session' } });
  complete(tracker, { extra: { request_id: 'bad/request' } });
  const snapshot = tracker.snapshot();
  assert.equal(Object.keys(snapshot).length, 5);
  assert.equal(snapshot['session-b'].actual_usd, 12);
  assert.equal(snapshot.__proto__.actual_usd, 6);
  assert.equal({}.actual_usd, undefined);
  assert.equal(current(tracker).requests, 1);
});

test('duplicate events and terminal notifications do not count the same request twice', () => {
  const tracker = createSavingsTracker();
  tracker.update(event('request_start'));
  tracker.update(event('upstream_model', { model: HAIKU }));
  tracker.update(event('upstream_usage', { usage }));
  tracker.update(event('request_start'));
  tracker.update(event('upstream_usage', { usage }));
  tracker.update(event('request_complete'));
  complete(tracker);
  for (const name of ['request_complete', 'request_error', 'request_cancelled', 'upstream_error']) tracker.update(event(name));
  assert.equal(current(tracker).requests, 1);
  assert.equal(current(tracker).unpriced_requests, 0);
  const detached = tracker.snapshot();
  detached['session-a'].actual_usd = 999;
  assert.equal(current(tracker).actual_usd, 6);
});

test('unknown models or a custom non-Opus baseline remain unpriced', () => {
  for (const model of ['claude-haiku-4-5-20990101', 'claude-opus-99', 'custom-model', undefined]) {
    const tracker = createSavingsTracker();
    tracker.update(event('request_start'));
    tracker.update(event('route', { model: HAIKU }));
    tracker.update(event('upstream_model', { model }));
    tracker.update(event('upstream_usage', { usage }));
    tracker.update(event('request_complete'));
    assert.equal(current(tracker).requests, 0);
    assert.equal(current(tracker).unpriced_requests, 1);
  }
  for (const baselineModel of [SONNET, 'private-opus', '\u001b[31msecret']) {
    const tracker = createSavingsTracker({ baselineModel });
    complete(tracker);
    assert.equal(current(tracker).unpriced_requests, 1);
    assert.equal(current(tracker).baseline_usd, 0);
  }
});

test('missing, invalid or contradictory usage never becomes a guessed estimate', () => {
  const examples = [undefined, {}, { input_tokens: 1 }, { output_tokens: 1 },
    { ...usage, input_tokens: -1 }, { ...usage, output_tokens: 1.5 }, { ...usage, output_tokens: Infinity },
    { ...usage, input_tokens: Number.MAX_SAFE_INTEGER + 1 }, { ...usage, input_tokens: '100' },
    { ...usage, cache_read_input_tokens: -1 }, { ...usage, cache_read_input_tokens: null },
    { ...usage, cache_creation_input_tokens: 100 },
    { ...usage, cache_creation_input_tokens: 100, cache_creation: { ephemeral_5m_input_tokens: 99 } },
    { ...usage, cache_creation_input_tokens: 0, cache_creation: { ephemeral_1h_input_tokens: 1 } },
    { ...usage, cache_creation_input_tokens: null }, { ...usage, cache_creation: null },
    { ...usage, cache_creation: { ephemeral_5m_input_tokens: null } },
    { ...usage, cache_creation: { ephemeral_1h_input_tokens: '1' } },
  ];
  for (let index = 0; index < examples.length; index++) {
    const tracker = createSavingsTracker();
    complete(tracker, { tokens: examples[index] });
    assert.equal(current(tracker).unpriced_requests, 1, `invalid usage ${index}`);
    assert.equal(current(tracker).actual_usd, 0);
  }
});

test('accepts explicit zero usage and a complete TTL split without an aggregate field', () => {
  const tracker = createSavingsTracker();
  complete(tracker, { tokens: { input_tokens: 0, output_tokens: 0 } });
  assert.equal(current(tracker).percent, 0);
  assert.equal(current(tracker).requests, 1);
  complete(tracker, { extra: { request_id: 'split-only' }, tokens: { input_tokens: 0, output_tokens: 0,
    cache_creation: { ephemeral_1h_input_tokens: 1000000 } } });
  assert.equal(current(tracker).actual_usd, 2);
  assert.equal(current(tracker).baseline_usd, 8);
});

test('unsupported request or actual usage pricing modifiers remain unpriced', () => {
  const modifiers = [{ speed: 'fast' }, { speed: null }, { inference_geo: 'us' }, { service_tier: 'priority' },
    { service_tier: 'batch' }, { service_tier: 'unknown' }, { pricing_unsupported: true }, { unsupported: true }];
  for (const context of modifiers) {
    const tracker = createSavingsTracker();
    complete(tracker, { context });
    complete(tracker, { extra: { request_id: 'actual-modifier' }, tokens: { ...usage, ...context } });
    assert.equal(current(tracker).unpriced_requests, 2, JSON.stringify(context));
    assert.equal(current(tracker).requests, 0);
  }
  const tracker = createSavingsTracker();
  complete(tracker, { context: { speed: 'standard', inference_geo: 'global', service_tier: 'standard_only' },
    tokens: { ...usage, speed: 'standard', inference_geo: 'global', service_tier: 'standard' } });
  assert.equal(current(tracker).requests, 1);
});

test('prices Haiku 4.5 unavailable geography at its documented standard rate only', () => {
  for (const model of [HAIKU, 'claude-haiku-4-5']) {
    const tracker = createSavingsTracker();
    complete(tracker, { model, tokens: { ...usage, inference_geo: 'not_available' } });
    assert.equal(current(tracker).requests, 1);
    assert.equal(current(tracker).actual_usd, 6);
    assert.equal(current(tracker).baseline_usd, 24);
    assert.equal(current(tracker).percent, 75);
  }
  for (const model of [SONNET, 'claude-sonnet-5-5', OPUS, 'claude-opus-5', 'claude-haiku-4-5-20990101']) {
    const tracker = createSavingsTracker();
    complete(tracker, { model, tokens: { ...usage, inference_geo: 'not_available' } });
    assert.equal(current(tracker).requests, 0, model);
    assert.equal(current(tracker).unpriced_requests, 1, model);
  }
  const tracker = createSavingsTracker();
  complete(tracker, { context: { inference_geo: 'not_available' } });
  complete(tracker, { extra: { request_id: 'null-geo' }, tokens: { ...usage, inference_geo: null } });
  complete(tracker, { extra: { request_id: 'unknown-geo' }, tokens: { ...usage, inference_geo: 'unknown' } });
  assert.equal(current(tracker).requests, 0);
  assert.equal(current(tracker).unpriced_requests, 3);
});

test('HTTP, streaming, transport and cancelled failures never appear as savings', () => {
  for (const failure of [event('upstream_response', { status: 429 }), event('upstream_error'),
    event('request_error'), event('request_cancelled')]) {
    const tracker = createSavingsTracker();
    tracker.update(event('request_start'));
    tracker.update(event('upstream_model', { model: HAIKU }));
    tracker.update(event('upstream_usage', { usage }));
    tracker.update(failure);
    tracker.update(event('request_complete'));
    tracker.update(event('request_complete'));
    assert.equal(current(tracker).requests, 0);
    assert.equal(current(tracker).unpriced_requests, 1);
    assert.equal(current(tracker).saved_usd, 0);
  }
});

test('conflicting model or final usage observations invalidate the request', () => {
  for (const duplicate of [event('upstream_model', { model: SONNET }),
    event('upstream_usage', { usage: { ...usage, output_tokens: 2 } }), event('upstream_usage', { usage: null })]) {
    const tracker = createSavingsTracker();
    tracker.update(event('request_start'));
    tracker.update(event('upstream_model', { model: HAIKU }));
    tracker.update(event('upstream_usage', { usage }));
    tracker.update(duplicate);
    tracker.update(event('request_complete'));
    assert.equal(current(tracker).unpriced_requests, 1);
  }
});

test('bounds in-flight work and marks evicted requests as unpriced and partial', () => {
  const tracker = createSavingsTracker();
  for (let index = 0; index < 1001; index++) tracker.update(event('request_start', { request_id: `request-${index}` }));
  assert.equal(current(tracker).unpriced_requests, 1);
  assert.equal(current(tracker).partial, true);
  complete(tracker, { extra: { request_id: 'request-0' } });
  complete(tracker, { extra: { request_id: 'request-1000' } });
  assert.equal(current(tracker).unpriced_requests, 1);
  assert.equal(current(tracker).requests, 1);
});

test('bounds sessions, ignores late evicted work and marks returning sessions partial', () => {
  const tracker = createSavingsTracker();
  for (let index = 0; index < 102; index++) tracker.update(event('request_start', { session_id: `session-${index}` }));
  assert.equal(Object.keys(tracker.snapshot()).length, 100);
  complete(tracker, { extra: { session_id: 'session-0' } });
  assert.equal(tracker.snapshot()['session-0'], undefined);
  complete(tracker, { extra: { session_id: 'session-0', request_id: 'new-request' } });
  assert.equal(tracker.snapshot()['session-0'].requests, 1);
  assert.equal(tracker.snapshot()['session-0'].partial, true);
  assert.equal(Object.keys(tracker.snapshot()).length, 100);
});

test('retains long-session totals while marking bounded duplicate history as partial', () => {
  const tracker = createSavingsTracker();
  for (let index = 0; index < 10001; index++) complete(tracker, { extra: { request_id: `request-${index}` } });
  assert.equal(current(tracker).requests, 10001);
  assert.equal(current(tracker).actual_usd, 60006);
  assert.equal(current(tracker).unpriced_requests, 0);
  assert.equal(current(tracker).partial, true);
});

test('malformed telemetry never throws, and clearing removes totals and request history', () => {
  const tracker = createSavingsTracker();
  assert.doesNotThrow(() => {
    tracker.update(null);
    tracker.update({ get event() { throw new Error('private error'); } });
    tracker.update(event('request_complete'));
  });
  assert.deepEqual(tracker.snapshot(), {});
  complete(tracker);
  tracker.clear();
  assert.deepEqual(tracker.snapshot(), {});
  complete(tracker);
  assert.equal(current(tracker).requests, 1);
});
