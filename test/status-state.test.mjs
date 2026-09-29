import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';
import { createStatusState } from '../src/status-state.mjs';

const event = (name, extra = {}) => ({ event: name, request_id: 'request-1', session_id: 'session-a', ...extra });
const read = state => JSON.parse(readFileSync(state.path, 'utf8'));
function fixture(t, options) {
  const state = createStatusState(options);
  t.after(() => state.close());
  assert.ok(state.path);
  return state;
}

// These snapshots are a subprocess boundary consumed while inference is live,
// so permissions, isolation, privacy and race handling are behavior contracts.
test('creates an immediate private atomic snapshot and cleans up idempotently', t => {
  const state = fixture(t);
  const snapshot = read(state);
  assert.equal(snapshot.version, 1);
  assert.equal(snapshot.pid, process.pid);
  assert.ok(Math.abs(snapshot.heartbeat_at - Date.now()) < 1000);
  assert.deepEqual(snapshot.sessions, {});
  assert.equal(statSync(dirname(state.path)).mode & 0o777, 0o700);
  assert.equal(statSync(state.path).mode & 0o777, 0o600);
  state.update(event('request_start'));
  state.update(event('route', { model: 'claude-sonnet-5' }));
  state.flush();
  assert.equal(statSync(state.path).mode & 0o777, 0o600);
  assert.deepEqual(readdirSync(dirname(state.path)), ['state.json']);
  state.close();
  state.close();
  assert.equal(existsSync(dirname(state.path)), false);
  assert.doesNotThrow(() => { state.update(event('request_start')); state.flush(); });
});

test('tracks requested, selected and confirmed models distinctly and retains last confirmed model', t => {
  const state = fixture(t);
  state.update(event('request_start', { prompt_id: 'prompt-a' }));
  state.flush();
  assert.equal(read(state).sessions['session-a'].phase, 'routing');
  state.update(event('route', { requested_model: 'claude-haiku-4-5', model: 'claude-sonnet-5', source: 'jev', reason: 'context_capacity', latency_ms: 213.4,
    classified_tier: 'haiku', context_check: 'over_budget', counted_input_tokens: 227338 }));
  state.flush();
  let current = read(state).sessions['session-a'];
  assert.equal(current.phase, 'connecting');
  assert.equal(current.requested_model, 'claude-haiku-4-5');
  assert.equal(current.selected_model, 'claude-sonnet-5');
  assert.equal(current.classified_tier, 'haiku');
  assert.equal(current.context_check, 'over_budget');
  assert.equal(current.counted_input_tokens, 227338);
  assert.equal(current.actual_model, undefined);
  assert.equal(current.last_model, undefined);
  state.update(event('upstream_response', { status: 200 }));
  state.update(event('upstream_model', { model: 'claude-sonnet-5' }));
  state.flush();
  current = read(state).sessions['session-a'];
  assert.equal(current.phase, 'streaming');
  assert.equal(current.actual_model, 'claude-sonnet-5');
  assert.equal(current.last_model, 'claude-sonnet-5');
  state.update(event('request_complete'));
  state.flush();
  assert.equal(read(state).sessions['session-a'].phase, 'ready');
  state.update(event('request_start', { request_id: 'request-2' }));
  state.flush();
  current = read(state).sessions['session-a'];
  assert.equal(current.phase, 'routing');
  assert.equal(current.actual_model, undefined);
  assert.equal(current.selected_model, undefined);
  assert.equal(current.requested_model, undefined);
  assert.equal(current.prompt_id, undefined);
  assert.equal(current.last_model, 'claude-sonnet-5');
});

test('context uses the latest foreground response input including cache without summing the session', t => {
  const state = fixture(t);
  state.update(event('request_start'));
  state.update(event('upstream_model', { model: 'claude-sonnet-5' }));
  const usage = { input_tokens: 4891, cache_creation_input_tokens: 20000, cache_read_input_tokens: 200000,
    output_tokens: 900000, private_extra: 'PRIVATE prompt', cache_creation: { ephemeral_1h_input_tokens: 20000 } };
  state.update(event('upstream_usage', { usage }));
  state.update(event('request_complete'));
  state.flush();
  const context = { model: 'claude-sonnet-5', input_tokens: 224891 };
  assert.deepEqual(read(state).sessions['session-a'].context_usage, context);
  assert.ok(!readFileSync(state.path, 'utf8').includes('PRIVATE'));
  state.update(event('request_start', { request_id: 'request-2' }));
  state.flush();
  assert.equal(read(state).sessions['session-a'].context_usage, undefined);
  assert.deepEqual(read(state).sessions['session-a'].last_context_usage, context);
  // Neither old requests nor subagents may replace foreground context.
  state.update(event('upstream_usage', { usage: { input_tokens: 999999 } }));
  state.update(event('upstream_usage', { request_id: 'request-2', agent_id: 'agent-1', usage: { input_tokens: 999999 } }));
  state.update(event('upstream_model', { request_id: 'request-2', model: 'claude-haiku-4-5-20251001' }));
  state.update(event('upstream_usage', { request_id: 'request-2', usage: { input_tokens: 1000, output_tokens: 5 } }));
  state.update(event('request_complete', { request_id: 'request-2' }));
  state.flush();
  assert.deepEqual(read(state).sessions['session-a'].context_usage, { model: 'claude-haiku-4-5-20251001', input_tokens: 1000 });
  assert.deepEqual(read(state).sessions['session-a'].last_context_usage, context);
});

test('unconfirmed, invalid or interrupted usage cannot become a current API context claim', t => {
  const state = fixture(t);
  for (const usage of [null, [], {}, { input_tokens: -1 }, { input_tokens: 1.5 }, { input_tokens: Infinity },
    { input_tokens: 100, cache_read_input_tokens: null }, { input_tokens: Number.MAX_SAFE_INTEGER, cache_creation_input_tokens: 1 },
    { input_tokens: 100, pricing_unsupported: true }]) {
    state.update(event('request_start'));
    state.update(event('upstream_model', { model: 'claude-sonnet-5' }));
    state.update(event('upstream_usage', { usage }));
    state.flush();
    assert.equal(read(state).sessions['session-a'].context_usage, undefined);
  }
  state.update(event('request_start'));
  state.update(event('route', { model: 'claude-sonnet-5' }));
  state.update(event('upstream_usage', { usage: { input_tokens: 100 } }));
  state.flush();
  assert.equal(read(state).sessions['session-a'].context_usage, undefined);
  for (const failure of [event('upstream_error'), event('request_error'), event('request_cancelled'), event('upstream_response', { status: 400 })]) {
    state.update(event('request_start'));
    state.update(event('upstream_model', { model: 'claude-sonnet-5' }));
    state.update(event('upstream_usage', { usage: { input_tokens: 100 } }));
    state.update(failure);
    state.update(event('upstream_usage', { usage: { input_tokens: 200 } }));
    state.flush();
    assert.equal(read(state).sessions['session-a'].context_usage, undefined);
  }
});

test('latest-started request wins under overlapping same-session requests', t => {
  const state = fixture(t);
  state.update(event('request_start'));
  state.update(event('upstream_model', { model: 'claude-haiku-4-5' }));
  state.update(event('request_start', { request_id: 'request-2' }));
  state.update(event('route', { request_id: 'request-2', model: 'claude-opus-5-5' }));
  for (const name of ['route', 'upstream_response', 'upstream_model', 'upstream_error', 'request_complete', 'request_error', 'request_cancelled']) {
    state.update(event(name, { model: 'claude-sonnet-5', status: 500, error_type: 'api_error' }));
  }
  state.flush();
  const current = read(state).sessions['session-a'];
  assert.equal(current.request_id, 'request-2');
  assert.equal(current.phase, 'connecting');
  assert.equal(current.selected_model, 'claude-opus-5-5');
  assert.equal(current.last_model, 'claude-haiku-4-5');
  assert.equal(current.status, undefined);
  assert.equal(current.error_type, undefined);
});

test('keeps sessions and anonymous requests isolated without prototype pollution', t => {
  const state = fixture(t);
  for (const sessionId of ['session-a', 'session-b', '', '__proto__', 'constructor']) {
    state.update(event('request_start', { session_id: sessionId }));
    state.update(event('route', { session_id: sessionId, model: `model-${sessionId || 'anonymous'}` }));
  }
  state.update(event('request_start', { session_id: 'invalid/session' }));
  state.update(event('route', { session_id: 'not-started', model: 'claude-opus-5-5' }));
  state.flush();
  const sessions = read(state).sessions;
  assert.equal(Object.keys(sessions).length, 5);
  assert.equal(sessions[''].selected_model, 'model-anonymous');
  assert.equal(sessions['session-b'].selected_model, 'model-session-b');
  assert.equal(sessions.__proto__.selected_model, 'model-__proto__');
  assert.equal({}.selected_model, undefined);
});

test('ignores agent events and every non-main request class', t => {
  const state = fixture(t);
  state.update(event('request_start'));
  for (const extra of [{ agent_id: 'agent-1' }, ...['auxiliary', 'compaction', 'background', 'subagent', 'workflow', 'future_request_class'].map(request_class => ({ request_class }))]) {
    state.update(event('request_start', { request_id: 'ignored-request', ...extra }));
    state.update(event('upstream_model', { model: 'claude-opus-5-5', ...extra }));
    state.update(event('request_error', { status: 500, ...extra }));
  }
  state.flush();
  const current = read(state).sessions['session-a'];
  assert.equal(current.request_id, 'request-1');
  assert.equal(current.phase, 'routing');
  assert.equal(current.actual_model, undefined);
  state.update(event('route', { request_class: 'main', model: 'claude-sonnet-5' }));
  state.flush();
  assert.equal(read(state).sessions['session-a'].selected_model, 'claude-sonnet-5');
});

test('preserves HTTP, SSE and transport errors through completion', t => {
  const state = fixture(t);
  const examples = [
    event('upstream_response', { status: 429 }),
    event('upstream_error', { error_type: 'overloaded_error' }),
    event('request_error', { status: 502 }),
  ];
  for (const failure of examples) {
    state.update(event('request_start'));
    state.update(failure);
    state.update(event('request_complete'));
    state.flush();
    const current = read(state).sessions['session-a'];
    assert.equal(current.phase, 'error');
    assert.ok(current.error_type);
  }
  state.update(event('request_start'));
  state.update(event('request_cancelled'));
  state.update(event('request_complete'));
  state.flush();
  assert.equal(read(state).sessions['session-a'].phase, 'cancelled');
});

test('classifier fallback diagnostics do not imply an upstream error', t => {
  const state = fixture(t);
  state.update(event('request_start'));
  state.update(event('route', { model: 'claude-sonnet-5', source: 'fallback', reason: 'classifier_unavailable', classifier_error: 'http_error', classifier_status: 503, latency_ms: 200 }));
  state.update(event('upstream_response', { status: 200 }));
  state.update(event('request_complete'));
  state.flush();
  const current = read(state).sessions['session-a'];
  assert.equal(current.phase, 'ready');
  assert.equal(current.source, 'fallback');
  assert.equal(current.classifier_error, 'http_error');
  assert.equal(current.classifier_status, 503);
  assert.equal(current.status, 200);
});

test('bounds retained sessions and model metadata and discards sensitive extras', t => {
  const state = fixture(t);
  for (let i = 0; i < 102; i++) state.update(event('request_start', { session_id: `session-${i}` }));
  const secret = 'SECRET prompt body Bearer key';
  state.update(event('route', { session_id: 'session-101', model: 'x'.repeat(1000), requested_model: '\u001b[31mred',
    source: secret, reason: secret, classifier_error: secret, classifier_status: -1, latency_ms: Infinity,
    body: secret, headers: { authorization: secret }, error: new Error(secret), message: secret }));
  state.update(event('upstream_error', { session_id: 'session-101', error_type: secret, error: secret }));
  state.flush();
  const snapshot = read(state);
  assert.equal(Object.keys(snapshot.sessions).length, 100);
  assert.equal(snapshot.sessions['session-0'], undefined);
  assert.equal(snapshot.sessions['session-1'], undefined);
  assert.equal(snapshot.sessions['session-101'].selected_model, undefined);
  assert.equal(snapshot.sessions['session-101'].requested_model, undefined);
  assert.equal(snapshot.sessions['session-101'].error_type, 'unknown_error');
  assert.ok(!readFileSync(state.path, 'utf8').includes('SECRET'));
  state.update(event('upstream_error', { session_id: 'session-101', error_type: 'token_secret_without_spaces' }));
  state.flush();
  assert.ok(!readFileSync(state.path, 'utf8').includes('token_secret_without_spaces'));
});

test('coalesces updates and refreshes heartbeat without inference traffic', async t => {
  t.mock.timers.enable({ apis: ['Date', 'setInterval'], now: 1000000 });
  const state = fixture(t);
  state.update(event('request_start'));
  state.update(event('route', { model: 'claude-sonnet-5' }));
  assert.deepEqual(read(state).sessions, {});
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(read(state).sessions['session-a'].phase, 'connecting');
  t.mock.timers.tick(5000);
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(read(state).heartbeat_at, 1005000);
});

test('separate instances use isolated directories under the requested parent', t => {
  const parent = mkdtempSync(join(tmpdir(), 'autorouter-status-parent-'));
  t.after(() => rmSync(parent, { recursive: true, force: true }));
  const first = fixture(t, { directory: parent });
  const second = fixture(t, { directory: parent });
  assert.notEqual(first.path, second.path);
  assert.equal(dirname(dirname(first.path)), parent);
  first.update(event('request_start'));
  first.flush();
  assert.deepEqual(read(second).sessions, {});
  first.close();
  assert.ok(existsSync(second.path));
});

test('unavailable storage and malformed telemetry never throw', t => {
  const parent = mkdtempSync(join(tmpdir(), 'autorouter-status-fail-'));
  t.after(() => rmSync(parent, { recursive: true, force: true }));
  const notDirectory = join(parent, 'file');
  writeFileSync(notDirectory, 'not a directory');
  const unavailable = createStatusState({ directory: notDirectory });
  t.after(() => unavailable.close());
  assert.equal(unavailable.path, null);
  assert.doesNotThrow(() => { unavailable.update(event('request_start')); unavailable.flush(); unavailable.close(); });
  let invalidOptions;
  assert.doesNotThrow(() => { invalidOptions = createStatusState({ get directory() { throw new Error('bad directory'); } }); });
  assert.equal(invalidOptions.path, null);
  invalidOptions.close();
  const state = fixture(t, { directory: parent });
  rmSync(dirname(state.path), { recursive: true });
  assert.doesNotThrow(() => {
    state.update(null);
    state.update({ get event() { throw new Error('bad telemetry'); } });
    state.update(event('request_start'));
    state.flush();
    state.close();
  });
});

test('session savings include overlapping and background calls without changing the foreground model', t => {
  const state = fixture(t, { baselineModel: 'claude-opus-5-5' });
  for (const extra of [{ request_id: 'older' }, { request_id: 'latest' }, { request_id: 'background', agent_id: 'agent', request_class: 'subagent' }]) {
    state.update(event('request_start', extra));
  }
  const finish = (request_id, model, extra = {}) => {
    for (const [name, fields] of [
      ['route', { model, pricing_context: { speed: 'standard', inference_geo: 'global', service_tier: 'auto' } }],
      ['upstream_response', { status: 200 }], ['upstream_model', { model }],
      ['upstream_usage', { usage: { input_tokens: 1000, output_tokens: 100, cache_creation_input_tokens: 0, cache_read_input_tokens: 0 } }],
      ['request_complete', {}],
    ]) state.update(event(name, { request_id, ...fields, ...extra }));
  };
  finish('latest', 'claude-opus-5-5');
  finish('background', 'claude-haiku-4-5-20251001', { agent_id: 'agent', request_class: 'subagent' });
  finish('older', 'claude-sonnet-5');
  state.flush();
  let snapshot = read(state);
  assert.equal(snapshot.sessions['session-a'].actual_model, 'claude-opus-5-5');
  assert.equal(snapshot.sessions['session-a'].request_id, 'latest');
  const savings = snapshot.savings['session-a'];
  assert.equal(savings.requests, 3);
  assert.equal(savings.unpriced_requests, 0);
  assert.ok(Math.abs(savings.actual_usd - 0.0105) < 1e-12);
  assert.ok(Math.abs(savings.baseline_usd - 0.018) < 1e-12);
  assert.ok(Math.abs(savings.saved_usd - 0.0075) < 1e-12);
  state.update(event('request_start', { request_id: 'new-turn' }));
  state.flush();
  snapshot = read(state);
  assert.deepEqual(snapshot.savings['session-a'], savings);
});
