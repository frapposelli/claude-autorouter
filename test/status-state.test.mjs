import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';
import * as fileSystem from 'node:fs/promises';
import { Readable, Writable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { createStatusState } from '../src/status-state.mjs';
import { renderStatusLine } from '../src/statusline.mjs';

const event = (name, extra = {}) => ({ event: name, request_id: 'request-1', session_id: 'session-a', ...extra });
const read = state => JSON.parse(readFileSync(state.path, 'utf8'));
const deferred = () => {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
};

test('a protocol-unconfirmed response retains its observed model but displays unknown completion and no savings', async t => {
  const state = createStatusState();
  t.after(() => state.close());
  await state.ready;
  state.update(event('request_start'));
  state.update(event('route', { model: 'claude-sonnet-5', source: 'jev' }));
  state.update(event('upstream_model', { model: 'claude-sonnet-5' }));
  state.update(event('upstream_usage', { usage: { input_tokens: 1000, output_tokens: 10 } }));
  state.update(event('request_complete', { completion_confirmed: false }));
  await state.flush();
  const saved = read(state);
  assert.equal(saved.sessions['session-a'].actual_model, 'claude-sonnet-5');
  assert.equal(saved.sessions['session-a'].completion_confirmed, false);
  assert.equal(saved.savings['session-a'].requests, 0);
  assert.deepEqual(saved.savings['session-a'].unpriced_reasons, { unconfirmed_completion: 1 });
  const line = renderStatusLine({ session_id: 'session-a' }, saved, { color: false, columns: 40 });
  assert.match(line, /Sonnet 5.*completion unknown/);
  assert.doesNotMatch(line, /ready/);
});
function blockedStorage(blockWrite = 2) {
  const entered = deferred(), release = deferred();
  const snapshots = [];
  let active = 0, maximum = 0;
  return { entered, release, snapshots, get maximum() { return maximum; }, fileSystem: {
    ...fileSystem,
    async open(...args) {
      const handle = await fileSystem.open(...args);
      const original = handle.writeFile.bind(handle);
      handle.writeFile = async value => {
        active++; maximum = Math.max(maximum, active);
        snapshots.push(JSON.parse(value));
        try {
          if (snapshots.length === blockWrite) { entered.resolve(); await release.promise; }
          await original(value);
        } finally { active--; }
      };
      return handle;
    },
  } };
}
async function fixture(t, options) {
  const state = createStatusState(options);
  t.after(() => state.close());
  await state.ready;
  assert.ok(state.path);
  return state;
}

// These snapshots are a subprocess boundary consumed while inference is live,
// so permissions, isolation, privacy and race handling are behavior contracts.
test('readiness creates a private atomic snapshot and cleans up idempotently', async t => {
  const state = await fixture(t);
  const snapshot = read(state);
  assert.equal(snapshot.version, 1);
  assert.equal(snapshot.pid, process.pid);
  assert.ok(Math.abs(snapshot.heartbeat_at - Date.now()) < 1000);
  assert.deepEqual(snapshot.sessions, {});
  assert.equal(statSync(dirname(state.path)).mode & 0o777, 0o700);
  assert.equal(statSync(state.path).mode & 0o777, 0o600);
  state.update(event('request_start'));
  state.update(event('route', { model: 'claude-sonnet-5' }));
  await state.flush();
  assert.equal(statSync(state.path).mode & 0o777, 0o600);
  assert.deepEqual(readdirSync(dirname(state.path)), ['state.json']);
  await state.close();
  await state.close();
  assert.equal(existsSync(dirname(state.path)), false);
  await assert.doesNotReject(async () => { state.update(event('request_start')); await state.flush(); });
});

test('tracks requested, selected and confirmed models distinctly and retains last confirmed model', async t => {
  const state = await fixture(t);
  state.update(event('request_start', { prompt_id: 'prompt-a' }));
  await state.flush();
  assert.equal(read(state).sessions['session-a'].phase, 'routing');
  state.update(event('route', { requested_model: 'claude-haiku-4-5', model: 'claude-sonnet-5', source: 'jev', reason: 'context_capacity', latency_ms: 213.4,
    classified_tier: 'haiku', context_check: 'over_budget', counted_input_tokens: 227338 }));
  await state.flush();
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
  await state.flush();
  current = read(state).sessions['session-a'];
  assert.equal(current.phase, 'streaming');
  assert.equal(current.actual_model, 'claude-sonnet-5');
  assert.equal(current.last_model, 'claude-sonnet-5');
  state.update(event('request_complete'));
  await state.flush();
  assert.equal(read(state).sessions['session-a'].phase, 'ready');
  state.update(event('request_start', { request_id: 'request-2' }));
  await state.flush();
  current = read(state).sessions['session-a'];
  assert.equal(current.phase, 'routing');
  assert.equal(current.actual_model, undefined);
  assert.equal(current.selected_model, undefined);
  assert.equal(current.requested_model, undefined);
  assert.equal(current.prompt_id, undefined);
  assert.equal(current.last_model, 'claude-sonnet-5');
});

test('shared telemetry retains safe compatibility, continuity and distinct timing evidence for the current request', async t => {
  const state = await fixture(t);
  state.update(event('request_start'));
  state.update(event('route', { selected_model: 'claude-sonnet-5-5', requested_model: 'claude-haiku-4-5-20251001',
    source: 'jev', classified_tier: 'haiku', reason: 'model_incompatible', compatibility_reason: 'forced_tool_choice',
    continuity_state: 'unknown', evaluation_latency_ms: 5000000, routing_latency_ms: 5000005,
    latency_ms: 5000005, decision_latency_ms: 5000005, private_metadata: 'PRIVATE_REQUEST' }));
  state.update(event('upstream_response', { status: 200, first_response_ms: 5000020 }));
  state.update(event('upstream_model', { confirmed_model: 'claude-sonnet-5-5' }));
  state.update(event('request_complete', { total_latency_ms: 5000120, completion_confirmed: true }));
  await state.flush();
  const current = read(state).sessions['session-a'];
  assert.equal(current.selected_model, 'claude-sonnet-5-5');
  assert.equal(current.actual_model, 'claude-sonnet-5-5');
  assert.equal(current.compatibility_reason, 'forced_tool_choice');
  assert.equal(current.continuity_state, 'unknown');
  assert.equal(current.evaluation_latency_ms, 5000000, 'A disabled evaluator deadline can exceed an hour');
  assert.equal(current.routing_latency_ms, 5000005);
  assert.equal(current.first_response_ms, 5000020);
  assert.equal(current.total_latency_ms, 5000120);
  assert.equal(current.completion_confirmed, true);
  assert.ok(!readFileSync(state.path, 'utf8').includes('PRIVATE_REQUEST'));

  state.update(event('request_start', { request_id: 'new' }));
  state.update(event('route', { evaluation_latency_ms: 1, routing_latency_ms: 2, continuity_state: 'confirmed' }));
  state.update(event('route', { request_id: 'new', evaluation_latency_ms: Infinity, routing_latency_ms: -1,
    continuity_state: 'PRIVATE state', compatibility_reason: 'PRIVATE reason' }));
  await state.flush();
  const next = read(state).sessions['session-a'];
  for (const field of ['evaluation_latency_ms', 'routing_latency_ms', 'first_response_ms', 'total_latency_ms', 'completion_confirmed', 'continuity_state', 'compatibility_reason']) {
    assert.equal(next[field], undefined, `${field} must not leak from another request`);
  }
});

test('invalid agent and class identities cannot collapse into foreground or anonymous status', async t => {
  const state = await fixture(t);
  state.update(event('request_start'));
  for (const extra of [{ agent_id: 'bad/agent' }, { request_class: 'bad class' }, { session_id: 'bad/session' }, { prompt_id: 'bad/prompt' }]) {
    state.update(event('request_start', { request_id: 'foreign', ...extra }));
    state.update(event('upstream_model', { model: 'claude-opus-5-5', ...extra }));
  }
  await state.flush();
  assert.deepEqual(Object.keys(read(state).sessions), ['session-a']);
  assert.equal(read(state).sessions['session-a'].request_id, 'request-1');
  assert.equal(read(state).sessions['session-a'].actual_model, undefined);
});

test('Auto safety pass-through is visible without stale evaluator decisions from the previous request', async t => {
  const state = await fixture(t);
  state.update(event('request_start'));
  state.update(event('route', { model: 'claude-sonnet-5', source: 'jev', evaluator: 'jev', classified_tier: 'haiku', reason: 'auto_mode_floor' }));
  await state.flush();
  const render = () => renderStatusLine({ session_id: 'session-a' }, read(state), { color: false, columns: 240 });
  assert.match(render(), /Jev→Haiku · Auto floor from Haiku/);
  state.update(event('request_complete'));
  state.update(event('request_start', { request_id: 'request-2' }));
  state.update(event('route', { request_id: 'request-2', model: 'claude-sonnet-5', source: 'passthrough', reason: 'auto_mode_safeguards' }));
  await state.flush();
  assert.match(render(), /Sonnet 5 selected · connecting · pass-through · Auto safety/);
  assert.ok(!/Jev|Ollama|Haiku/.test(render()));
  assert.equal(read(state).sessions['session-a'].classified_tier, undefined);
});

test('context uses the latest foreground response input including cache without summing the session', async t => {
  const state = await fixture(t);
  state.update(event('request_start'));
  state.update(event('upstream_model', { model: 'claude-sonnet-5' }));
  const usage = { input_tokens: 4891, cache_creation_input_tokens: 20000, cache_read_input_tokens: 200000,
    output_tokens: 900000, private_extra: 'PRIVATE prompt', cache_creation: { ephemeral_1h_input_tokens: 20000 } };
  state.update(event('upstream_usage', { usage }));
  state.update(event('request_complete'));
  await state.flush();
  const context = { model: 'claude-sonnet-5', input_tokens: 224891 };
  assert.deepEqual(read(state).sessions['session-a'].context_usage, context);
  assert.ok(!readFileSync(state.path, 'utf8').includes('PRIVATE'));
  state.update(event('request_start', { request_id: 'request-2' }));
  await state.flush();
  assert.equal(read(state).sessions['session-a'].context_usage, undefined);
  assert.deepEqual(read(state).sessions['session-a'].last_context_usage, context);
  // Neither old requests nor subagents may replace foreground context.
  state.update(event('upstream_usage', { usage: { input_tokens: 999999 } }));
  state.update(event('upstream_usage', { request_id: 'request-2', agent_id: 'agent-1', usage: { input_tokens: 999999 } }));
  state.update(event('upstream_model', { request_id: 'request-2', model: 'claude-haiku-4-5-20251001' }));
  state.update(event('upstream_usage', { request_id: 'request-2', usage: { input_tokens: 1000, output_tokens: 5 } }));
  state.update(event('request_complete', { request_id: 'request-2' }));
  await state.flush();
  assert.deepEqual(read(state).sessions['session-a'].context_usage, { model: 'claude-haiku-4-5-20251001', input_tokens: 1000 });
  assert.deepEqual(read(state).sessions['session-a'].last_context_usage, context);
});

test('unconfirmed, invalid or interrupted usage cannot become a current API context claim', async t => {
  const state = await fixture(t);
  for (const usage of [null, [], {}, { input_tokens: -1 }, { input_tokens: 1.5 }, { input_tokens: Infinity },
    { input_tokens: 100, cache_read_input_tokens: null }, { input_tokens: Number.MAX_SAFE_INTEGER, cache_creation_input_tokens: 1 },
    { input_tokens: 100, pricing_unsupported: true }]) {
    state.update(event('request_start'));
    state.update(event('upstream_model', { model: 'claude-sonnet-5' }));
    state.update(event('upstream_usage', { usage }));
    await state.flush();
    assert.equal(read(state).sessions['session-a'].context_usage, undefined);
  }
  state.update(event('request_start'));
  state.update(event('route', { model: 'claude-sonnet-5' }));
  state.update(event('upstream_usage', { usage: { input_tokens: 100 } }));
  await state.flush();
  assert.equal(read(state).sessions['session-a'].context_usage, undefined);
  for (const failure of [event('upstream_error'), event('request_error'), event('request_cancelled'), event('upstream_response', { status: 400 })]) {
    state.update(event('request_start'));
    state.update(event('upstream_model', { model: 'claude-sonnet-5' }));
    state.update(event('upstream_usage', { usage: { input_tokens: 100 } }));
    state.update(failure);
    state.update(event('upstream_usage', { usage: { input_tokens: 200 } }));
    await state.flush();
    assert.equal(read(state).sessions['session-a'].context_usage, undefined);
  }
});

test('latest-started request wins under overlapping same-session requests', async t => {
  const state = await fixture(t);
  state.update(event('request_start'));
  state.update(event('upstream_model', { model: 'claude-haiku-4-5' }));
  state.update(event('request_start', { request_id: 'request-2' }));
  state.update(event('route', { request_id: 'request-2', model: 'claude-opus-5-5' }));
  for (const name of ['route', 'upstream_response', 'upstream_model', 'upstream_error', 'request_complete', 'request_error', 'request_cancelled']) {
    state.update(event(name, { model: 'claude-sonnet-5', status: 500, error_type: 'api_error' }));
  }
  await state.flush();
  const current = read(state).sessions['session-a'];
  assert.equal(current.request_id, 'request-2');
  assert.equal(current.phase, 'connecting');
  assert.equal(current.selected_model, 'claude-opus-5-5');
  assert.equal(current.last_model, 'claude-haiku-4-5');
  assert.equal(current.status, undefined);
  assert.equal(current.error_type, undefined);
});

test('keeps sessions and anonymous requests isolated without prototype pollution', async t => {
  const state = await fixture(t);
  for (const sessionId of ['session-a', 'session-b', '', '__proto__', 'constructor']) {
    state.update(event('request_start', { session_id: sessionId }));
    state.update(event('route', { session_id: sessionId, model: `model-${sessionId || 'anonymous'}` }));
  }
  state.update(event('request_start', { session_id: 'invalid/session' }));
  state.update(event('route', { session_id: 'not-started', model: 'claude-opus-5-5' }));
  await state.flush();
  const sessions = read(state).sessions;
  assert.equal(Object.keys(sessions).length, 5);
  assert.equal(sessions[''].selected_model, 'model-anonymous');
  assert.equal(sessions['session-b'].selected_model, 'model-session-b');
  assert.equal(sessions.__proto__.selected_model, 'model-__proto__');
  assert.equal({}.selected_model, undefined);
});

test('ignores agent events and every non-main request class', async t => {
  const state = await fixture(t);
  state.update(event('request_start'));
  for (const extra of [{ agent_id: 'agent-1' }, ...['auxiliary', 'compaction', 'background', 'subagent', 'workflow', 'future_request_class'].map(request_class => ({ request_class }))]) {
    state.update(event('request_start', { request_id: 'ignored-request', ...extra }));
    state.update(event('upstream_model', { model: 'claude-opus-5-5', ...extra }));
    state.update(event('request_error', { status: 500, ...extra }));
  }
  await state.flush();
  const current = read(state).sessions['session-a'];
  assert.equal(current.request_id, 'request-1');
  assert.equal(current.phase, 'routing');
  assert.equal(current.actual_model, undefined);
  state.update(event('route', { request_class: 'main', model: 'claude-sonnet-5' }));
  await state.flush();
  assert.equal(read(state).sessions['session-a'].selected_model, 'claude-sonnet-5');
});

test('preserves HTTP, SSE and transport errors through completion', async t => {
  const state = await fixture(t);
  const examples = [
    event('upstream_response', { status: 429 }),
    event('upstream_error', { error_type: 'overloaded_error' }),
    event('request_error', { status: 502 }),
  ];
  for (const failure of examples) {
    state.update(event('request_start'));
    state.update(failure);
    state.update(event('request_complete'));
    await state.flush();
    const current = read(state).sessions['session-a'];
    assert.equal(current.phase, 'error');
    assert.ok(current.error_type);
  }
  state.update(event('request_start'));
  state.update(event('request_cancelled'));
  state.update(event('request_complete'));
  await state.flush();
  assert.equal(read(state).sessions['session-a'].phase, 'cancelled');
});

test('classifier fallback diagnostics do not imply an upstream error', async t => {
  const state = await fixture(t);
  state.update(event('request_start'));
  state.update(event('route', { model: 'claude-sonnet-5', source: 'fallback', reason: 'classifier_unavailable', classifier_error: 'http_error', classifier_status: 503, latency_ms: 200 }));
  state.update(event('upstream_response', { status: 200 }));
  state.update(event('request_complete'));
  await state.flush();
  const current = read(state).sessions['session-a'];
  assert.equal(current.phase, 'ready');
  assert.equal(current.source, 'fallback');
  assert.equal(current.classifier_error, 'http_error');
  assert.equal(current.classifier_status, 503);
  assert.equal(current.status, 200);
});

test('evaluator identity survives local, cached and fallback routes without retaining arbitrary metadata', async t => {
  const state = await fixture(t);
  for (const source of ['ollama', 'cache', 'fallback']) {
    state.update(event('request_start'));
    state.update(event('route', { model: 'claude-sonnet-5', source, evaluator: 'ollama', classified_tier: 'haiku',
      classifier_error: source === 'fallback' ? 'timeout' : undefined,
      evaluator_model: 'PRIVATE model metadata', evaluator_url: 'http://PRIVATE', evaluator_prompt: 'PRIVATE prompt' }));
    await state.flush();
    const current = read(state).sessions['session-a'];
    assert.equal(current.source, source);
    assert.equal(current.evaluator, 'ollama');
    assert.equal(current.classified_tier, 'haiku');
    assert.equal(current.phase, 'connecting');
    assert.ok(!readFileSync(state.path, 'utf8').includes('PRIVATE'));
  }
  state.update(event('request_start', { request_id: 'request-2' }));
  state.update(event('route', { evaluator: 'ollama', source: 'ollama' }));
  state.update(event('route', { request_id: 'request-2', agent_id: 'agent-1', evaluator: 'ollama', source: 'ollama' }));
  state.update(event('route', { request_id: 'request-2', evaluator: 'PRIVATE evaluator', source: 'PRIVATE source' }));
  await state.flush();
  assert.equal(read(state).sessions['session-a'].evaluator, undefined);
  assert.equal(read(state).sessions['session-a'].source, undefined);
  assert.ok(!readFileSync(state.path, 'utf8').includes('PRIVATE'));
  state.update(event('route', { request_id: 'request-2', evaluator: 'jev', source: 'jev' }));
  await state.flush();
  assert.equal(read(state).sessions['session-a'].evaluator, 'jev');
});

test('bounds retained sessions and model metadata and discards sensitive extras', async t => {
  const state = await fixture(t);
  for (let i = 0; i < 102; i++) state.update(event('request_start', { session_id: `session-${i}` }));
  const secret = 'SECRET prompt body Bearer key';
  state.update(event('route', { session_id: 'session-101', model: 'x'.repeat(1000), requested_model: '\u001b[31mred',
    source: secret, reason: secret, classifier_error: secret, classifier_status: -1, latency_ms: Infinity,
    body: secret, headers: { authorization: secret }, error: new Error(secret), message: secret }));
  state.update(event('upstream_error', { session_id: 'session-101', error_type: secret, error: secret }));
  await state.flush();
  const snapshot = read(state);
  assert.equal(Object.keys(snapshot.sessions).length, 100);
  assert.equal(snapshot.sessions['session-0'], undefined);
  assert.equal(snapshot.sessions['session-1'], undefined);
  assert.equal(snapshot.sessions['session-101'].selected_model, undefined);
  assert.equal(snapshot.sessions['session-101'].requested_model, undefined);
  assert.equal(snapshot.sessions['session-101'].error_type, 'unknown_error');
  assert.ok(!readFileSync(state.path, 'utf8').includes('SECRET'));
  state.update(event('upstream_error', { session_id: 'session-101', error_type: 'token_secret_without_spaces' }));
  await state.flush();
  assert.ok(!readFileSync(state.path, 'utf8').includes('token_secret_without_spaces'));
});

test('coalesces updates and refreshes heartbeat without inference traffic', async t => {
  t.mock.timers.enable({ apis: ['Date', 'setInterval'], now: 1000000 });
  const state = await fixture(t);
  state.update(event('request_start'));
  state.update(event('route', { model: 'claude-sonnet-5' }));
  assert.deepEqual(read(state).sessions, {});
  await state.flush();
  assert.equal(read(state).sessions['session-a'].phase, 'connecting');
  t.mock.timers.tick(5000);
  await state.flush();
  assert.equal(read(state).heartbeat_at, 1005000);
});

test('separate instances use isolated directories under the requested parent', async t => {
  const parent = mkdtempSync(join(tmpdir(), 'autorouter-status-parent-'));
  t.after(() => rmSync(parent, { recursive: true, force: true }));
  const first = await fixture(t, { directory: parent });
  const second = await fixture(t, { directory: parent });
  assert.notEqual(first.path, second.path);
  assert.equal(dirname(dirname(first.path)), parent);
  first.update(event('request_start'));
  await first.flush();
  assert.deepEqual(read(second).sessions, {});
  await first.close();
  assert.ok(existsSync(second.path));
});

test('unavailable storage and malformed telemetry never throw', async t => {
  const parent = mkdtempSync(join(tmpdir(), 'autorouter-status-fail-'));
  t.after(() => rmSync(parent, { recursive: true, force: true }));
  const notDirectory = join(parent, 'file');
  writeFileSync(notDirectory, 'not a directory');
  const unavailable = createStatusState({ directory: notDirectory });
  t.after(() => unavailable.close());
  await unavailable.ready;
  assert.equal(unavailable.path, null);
  await assert.doesNotReject(async () => { unavailable.update(event('request_start')); await unavailable.flush(); await unavailable.close(); });
  let invalidOptions;
  assert.doesNotThrow(() => { invalidOptions = createStatusState({ get directory() { throw new Error('bad directory'); } }); });
  await invalidOptions.ready;
  assert.equal(invalidOptions.path, null);
  await invalidOptions.close();
  const state = await fixture(t, { directory: parent });
  rmSync(dirname(state.path), { recursive: true });
  await assert.doesNotReject(async () => {
    state.update(null);
    state.update({ get event() { throw new Error('bad telemetry'); } });
    state.update(event('request_start'));
    await state.flush();
    await state.close();
  });
});

test('session savings include overlapping and background calls without changing the foreground model', async t => {
  const state = await fixture(t, { baselineModel: 'claude-opus-5-5' });
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
  await state.flush();
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
  await state.flush();
  snapshot = read(state);
  assert.deepEqual(snapshot.savings['session-a'], savings);
});

test('a blocked snapshot leaves inference streaming and coalesces a large burst into one latest snapshot', async t => {
  const storage = blockedStorage();
  t.after(() => storage.release.resolve());
  const state = await fixture(t, { fileSystem: storage.fileSystem });
  state.update(event('request_start'));
  const flushed = state.flush();
  await storage.entered.promise;
  assert.deepEqual(read(state).sessions, {}, 'Atomic replacement preserves the previous complete snapshot');
  for (let i = 0; i < 2000; i++) {
    state.update(event('request_start', { request_id: `burst-${i}` }));
    state.update(event('route', { request_id: `burst-${i}`, model: 'claude-sonnet-5-5' }));
  }
  const delivered = [];
  await pipeline(Readable.from((async function* () {
    for (let i = 0; i < 10; i++) {
      await new Promise(resolve => setImmediate(resolve));
      yield Buffer.from(`chunk-${i}`);
    }
  })()), new Writable({ write(chunk, _encoding, done) { delivered.push(chunk.toString()); done(); } }));
  assert.equal(delivered.length, 10, 'The whole mock inference stream finishes while disk I/O is blocked');
  assert.equal(storage.snapshots.length, 2, 'No snapshot copies or writes accumulate behind the blocked writer');
  assert.equal(storage.maximum, 1);
  storage.release.resolve();
  await flushed;
  assert.equal(storage.snapshots.length, 3);
  assert.equal(read(state).sessions['session-a'].request_id, 'burst-1999');
  assert.equal(read(state).sessions['session-a'].selected_model, 'claude-sonnet-5-5');
  assert.deepEqual(readdirSync(dirname(state.path)), ['state.json']);
});

test('close drains the accepted latest state before deletion and cannot recreate status files', async t => {
  const storage = blockedStorage();
  t.after(() => storage.release.resolve());
  const state = await fixture(t, { fileSystem: storage.fileSystem });
  const directory = dirname(state.path);
  state.update(event('request_start'));
  const flushed = state.flush();
  await storage.entered.promise;
  state.update(event('route', { model: 'claude-opus-5-5' }));
  const closing = state.close();
  assert.equal(state.close(), closing, 'Concurrent shutdown callers share the same drain');
  state.update(event('request_start', { request_id: 'too-late' }));
  let closed = false;
  closing.then(() => { closed = true; });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(closed, false);
  assert.equal(existsSync(directory), true);
  storage.release.resolve();
  await Promise.all([flushed, closing]);
  assert.equal(storage.snapshots.at(-1).sessions['session-a'].selected_model, 'claude-opus-5-5');
  assert.equal(storage.snapshots.at(-1).sessions['session-a'].request_id, 'request-1');
  assert.equal(storage.maximum, 1);
  assert.equal(existsSync(directory), false);
  await state.flush();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(existsSync(directory), false);
});

test('readiness fails soft on stalled storage while close retains ownership of the late write', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const storage = blockedStorage(1);
  const parent = mkdtempSync(join(tmpdir(), 'autorouter-status-stalled-'));
  const state = createStatusState({ directory: parent, fileSystem: storage.fileSystem });
  t.after(async () => { storage.release.resolve(); await state.close(); rmSync(parent, { recursive: true, force: true }); });
  assert.equal(state.path, null, 'No partial initial snapshot is exposed');
  await storage.entered.promise;
  t.mock.timers.tick(1000);
  await state.ready;
  assert.equal(state.path, null);
  state.update(event('request_start'));
  const closing = state.close();
  storage.release.resolve();
  await closing;
  assert.deepEqual(readdirSync(parent), []);
  assert.equal(storage.snapshots.length, 1);
});

test('write failure cleans the temporary file and a later snapshot can recover without rejecting', async t => {
  let fail = false;
  const state = await fixture(t, { fileSystem: { ...fileSystem, async rename(...args) {
    if (fail) throw new Error('synthetic storage failure');
    return fileSystem.rename(...args);
  } } });
  fail = true;
  state.update(event('request_start'));
  await state.flush();
  assert.deepEqual(read(state).sessions, {});
  assert.deepEqual(readdirSync(dirname(state.path)), ['state.json']);
  fail = false;
  state.update(event('route', { model: 'claude-sonnet-5-5' }));
  await state.flush();
  assert.equal(read(state).sessions['session-a'].selected_model, 'claude-sonnet-5-5');
});
