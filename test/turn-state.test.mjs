import test from 'node:test';
import assert from 'node:assert/strict';
import { TurnState } from '../src/turn-state.mjs';
import { Router } from '../src/router.mjs';
import { readConfig } from '../src/config.mjs';

const pin = model => ({ model, requestedModel: 'claude-haiku-4-5-20251001' });
const done = (model, tool_uses = []) => ({ model, continuation_model: model, tool_uses });

test('pending tools survive elapsed TTL and cache pressure without evicting active tasks', () => {
  let now = 0;
  const state = new TurnState({ limit: 2, idleTtlMs: 10, now: () => now });
  state.select(['one', 'alias'], pin('opus'), { scope: 'a', requestId: 'a1', sequence: 1 });
  assert.equal(state.get('one'), undefined, 'selection is not a confirmed execution');
  assert.equal(state.complete('a1', done('opus', [{ id: 'tool', model: 'opus' }])), true);
  now = 1000000;
  state.select(['two'], pin('sonnet'), { scope: 'b', requestId: 'b1', sequence: 2 });
  state.complete('b1', done('sonnet'));
  assert.equal(state.select(['three'], pin('haiku'), { scope: 'c', requestId: 'c1', sequence: 3 }), false);
  assert.equal(state.get('alias').model, 'opus');
  assert.equal(state.records.size, 2);
  assert.equal(state.attempts.size, 0);
});

test('only idle retired tasks expire; active task and tool ownership remain scoped', () => {
  let now = 0;
  const state = new TurnState({ idleTtlMs: 10, now: () => now });
  state.select(['one'], pin('opus'), { scope: 'a', requestId: 'a1', sequence: 1 });
  state.complete('a1', done('opus'));
  state.select(['two'], pin('haiku'), { scope: 'a', requestId: 'a2', sequence: 2 });
  state.complete('a2', done('haiku'));
  now = 11;
  assert.equal(state.get('one'), undefined);
  assert.equal(state.get('two').model, 'haiku');
});

test('failed and superseded attempts never replace a confirmed model', () => {
  const state = new TurnState();
  state.select(['one'], pin('opus'), { requestId: 'initial', sequence: 1 });
  state.complete('initial', done('opus'));
  state.select(['one'], pin('haiku'), { requestId: 'old', sequence: 2 });
  state.select(['one'], pin('sonnet'), { requestId: 'new', sequence: 3 });
  assert.equal(state.complete('old', done('haiku')), false);
  state.complete('new');
  assert.equal(state.get('one').model, 'opus');
  state.select(['one'], pin('sonnet'), { requestId: 'retry', sequence: 4 });
  state.complete('retry', done('sonnet'));
  assert.equal(state.get('one').model, 'sonnet');
  assert.equal(state.complete('retry', done('haiku')), false);
});

test('unconfirmed cancelled work and ambiguous tool ownership are discarded', () => {
  const state = new TurnState();
  state.select(['one'], pin('opus'), { requestId: 'a', sequence: 1 });
  state.complete('a');
  assert.equal(state.records.size, 0);
  state.select(['one'], pin('opus'), { requestId: 'b', sequence: 2 });
  assert.equal(state.complete('b', done('opus', [{ id: 'x', model: 'sonnet' }, { id: 'y', model: 'opus' }])), false);
  assert.equal(state.get('one'), undefined);
  assert.equal(state.attempts.size, 0);
});

test('gateway routing commits actual provider fallback, retains it after expiry, then permits a new task to downgrade', async () => {
  const config = { ...readConfig({ AUTOROUTER_EVALUATOR: 'jev' }), cacheEntries: 1, turnTtlMs: 1 };
  let now = 0;
  const router = new Router(config, { now: () => now,
    fetchImpl: async () => Response.json({ answers: { tier: { choice: 'haiku', confidence: 0.99 } } }) });
  const body = { model: config.models.haiku, messages: [{ role: 'user', content: 'Read and fix fixture' }], max_tokens: 1024 };
  const identity = { scope: 'session/agent', promptId: 'task' };
  await router.route(body, { ...identity, requestId: 'initial' });
  router.complete('initial', done(config.models.opus, [{ id: 'read', model: config.models.opus }]));
  now = 1000000;
  const continuation = { ...body, messages: [...body.messages,
    { role: 'assistant', content: [{ type: 'tool_use', id: 'read', name: 'Read', input: {} }] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'read', content: 'File' }] },
  ] };
  const next = await router.route(continuation, { ...identity, requestId: 'next' });
  assert.equal(next.model, config.models.opus);
  assert.equal(next.continuity_state, 'confirmed');
  router.complete('next', done(config.models.opus));
  const fresh = await router.route({ ...continuation, messages: [...continuation.messages,
    { role: 'assistant', content: 'Finished' }, { role: 'user', content: 'Return [].length' },
  ] }, { scope: identity.scope, promptId: 'fresh', requestId: 'fresh' });
  assert.equal(fresh.model, config.models.haiku);
  router.complete('fresh');
});

test('unknown safeguards report rejected turn admission without replacing active continuity', async () => {
  const config = { ...readConfig({ AUTOROUTER_EVALUATOR: 'jev' }), turnEntries: 1 };
  let evaluatorCalls = 0;
  const router = new Router(config, { fetchImpl: async () => {
    evaluatorCalls++;
    return Response.json({ answers: { tier: { choice: 'opus', confidence: 0.99 } } });
  } });
  const initial = { model: config.models.haiku, messages: [{ role: 'user', content: 'Investigate a fixture' }] };
  await router.route(initial, { scope: 'occupied', promptId: 'first', requestId: 'first' });
  router.complete('first', done(config.models.opus, [{ id: 'pending', model: config.models.opus }]));
  const protectedBody = { ...initial, safeguards: [{ type: 'future_contract' }] };
  const next = await router.route(protectedBody, { scope: 'other', promptId: 'other', requestId: 'other' });
  assert.equal(next.model, protectedBody.model);
  assert.equal(next.source, 'passthrough');
  assert.equal(next.continuity_state, 'capacity_exhausted');
  assert.equal(evaluatorCalls, 1);
  assert.equal(router.turns.records.size, 1);
  assert.equal(router.turns.attempts.size, 0);
  const continuation = { ...initial, messages: [...initial.messages,
    { role: 'assistant', content: [{ type: 'tool_use', id: 'pending', name: 'Read', input: {} }] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'pending', content: 'fixture' }] },
  ] };
  const retained = await router.route(continuation, { scope: 'occupied', promptId: 'first', requestId: 'retained' });
  assert.equal(retained.model, config.models.opus);
  assert.equal(retained.continuity_state, 'confirmed');
  router.complete('retained');
});

test('out-of-order task completion cannot retire a newer active human task', () => {
  let now = 0;
  const state = new TurnState({ idleTtlMs: 10, now: () => now });
  state.select(['older'], pin('opus'), { scope: 's', requestId: 'a', sequence: 1 });
  state.select(['newer'], pin('haiku'), { scope: 's', requestId: 'b', sequence: 2 });
  state.complete('b', done('haiku'));
  state.complete('a', done('opus'));
  now = 100;
  assert.equal(state.get('older'), undefined);
  assert.equal(state.get('newer').model, 'haiku');
});

test('rejected admission cannot supersede accepted work or install aliases', () => {
  const state = new TurnState({ limit: 1 });
  state.select(['one'], pin('opus'), { requestId: 'first', sequence: 1 });
  state.complete('first', done('opus'));
  assert.equal(state.select(['one'], pin('sonnet'), { requestId: 'accepted', sequence: 2 }), true);
  assert.equal(state.select(['one', 'unaccepted-alias'], pin('haiku'), { requestId: 'rejected', sequence: 3 }), false);
  assert.equal(state.complete('accepted', done('sonnet')), true);
  assert.equal(state.get('one').model, 'sonnet');
  assert.equal(state.get('unaccepted-alias'), undefined);
});

test('failed old retries release idle retention after a newer task commits', () => {
  let now = 0;
  const state = new TurnState({ idleTtlMs: 10, now: () => now });
  state.select(['old'], pin('opus'), { scope: 's', requestId: 'initial', sequence: 1 });
  state.complete('initial', done('opus'));
  state.select(['old'], pin('opus'), { scope: 's', requestId: 'retry', sequence: 2 });
  state.select(['new'], pin('haiku'), { scope: 's', requestId: 'new', sequence: 3 });
  state.complete('new', done('haiku'));
  state.complete('retry');
  now = 11;
  assert.equal(state.get('old'), undefined);
  assert.equal(state.get('new').model, 'haiku');
});

test('completion rejects malformed or inconsistent tool ownership without throwing', () => {
  const state = new TurnState();
  for (const [index, tools] of [{}, [null], [{ id: 'wrong', model: 'sonnet' }]].entries()) {
    const id = `attempt-${index}`;
    state.select(['task'], pin('opus'), { requestId: id, sequence: index });
    assert.equal(state.complete(id, done('opus', tools)), false);
    assert.equal(state.get('task'), undefined);
  }
});

test('identical task content cannot merge different prompt identities or overwrite pending tools', () => {
  const state = new TurnState();
  state.select(['prompt-one', 'same-content'], pin('opus'), { scope: 'session', requestId: 'one', sequence: 1 });
  state.complete('one', done('opus', [{ id: 'pending', model: 'opus' }]));
  state.select(['prompt-two', 'same-content'], pin('haiku'), { scope: 'session', requestId: 'two', sequence: 2 });
  state.complete('two', done('haiku'));
  assert.equal(state.get('prompt-one').model, 'opus');
  assert.equal(state.get('prompt-two').model, 'haiku');
  assert.equal(state.get('same-content'), undefined, 'shared active content alone cannot identify the task');
});

test('tool discovery aliases are bounded and never displace the primary task identity', () => {
  const state = new TurnState();
  for (let index = 0; index < 100; index++) {
    state.select(['prompt', `discovery-${index}`], pin('opus'), { sequence: index });
  }
  assert.equal(state.records.size, 1);
  assert.equal(state.aliases.size, 8);
  assert.equal(state.get('prompt').model, 'opus');
  assert.equal(state.get('discovery-99').model, 'opus');
});

test('an old tool continuation arriving later cannot retire the newer human task', () => {
  let now = 0;
  const state = new TurnState({ idleTtlMs: 10, now: () => now });
  state.select(['old'], pin('opus'), { scope: 's', requestId: 'old', sequence: 1 });
  state.complete('old', done('opus', [{ id: 'slow', model: 'opus' }]));
  state.select(['new'], pin('haiku'), { scope: 's', requestId: 'new', sequence: 2 });
  state.complete('new', done('haiku'));
  state.select(['old'], pin('opus'), { scope: 's', requestId: 'tool-result', sequence: 3 });
  state.complete('tool-result', done('opus'));
  now = 11;
  assert.equal(state.get('old'), undefined);
  assert.equal(state.get('new').model, 'haiku');
});

test('failed same-content tasks do not destroy an earlier confirmed content alias', () => {
  const state = new TurnState();
  state.select(['a', 'content'], pin('opus'), { scope: 's', requestId: 'a', sequence: 1 });
  state.complete('a', done('opus', [{ id: 'tool-a', model: 'opus' }]));
  state.select(['b', 'content'], pin('haiku'), { scope: 's', requestId: 'b', sequence: 2 });
  state.complete('b');
  assert.equal(state.get('content').model, 'opus');
});

test('headerless continuations recover exact scoped tool ownership despite same-content tasks', async () => {
  const config = readConfig({ AUTOROUTER_EVALUATOR: 'jev' });
  const router = new Router(config, { fetchImpl: async () => Response.json({ answers: { tier: { choice: 'haiku', confidence: 0.99 } } }) });
  const initial = { model: config.models.haiku, max_tokens: 1024, messages: [{ role: 'user', content: 'Read the file' }] };
  for (const [promptId, model] of [['a', config.models.opus], ['b', config.models.haiku]]) {
    await router.route(initial, { scope: 's', promptId, requestId: promptId });
    router.complete(promptId, done(model, [{ id: `tool-${promptId}`, model }]));
  }
  const result = { ...initial, messages: [...initial.messages,
    { role: 'assistant', content: [{ type: 'tool_use', id: 'tool-a', name: 'Read', input: {} }] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'tool-a', content: 'File' }] },
  ] };
  const decision = await router.route(result, { scope: 's', requestId: 'continue' });
  assert.equal(decision.model, config.models.opus);
  assert.equal(decision.continuity_state, 'confirmed');
  router.complete('continue', done(config.models.opus));
  assert.equal((await router.route(result, { scope: 'different' })).continuity_state, 'unknown');
});

test('ambiguous headerless continuity never stages an overwrite of either confirmed task', async () => {
  const config = readConfig({ AUTOROUTER_EVALUATOR: 'jev' });
  const router = new Router(config, { fetchImpl: async () => Response.json({ answers: { tier: { choice: 'haiku', confidence: 0.99 } } }) });
  const initial = { model: config.models.haiku, messages: [{ role: 'user', content: 'Same task' }] };
  for (const [promptId, model] of [['a', config.models.opus], ['b', config.models.haiku]]) {
    await router.route(initial, { scope: 's', promptId, requestId: promptId });
    router.complete(promptId, done(model, [{ id: `tool-${promptId}`, model }]));
  }
  const body = { ...initial, messages: [...initial.messages,
    { role: 'assistant', content: 'Context is incomplete' },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'unobserved', content: 'Result' }] },
  ] };
  const decision = await router.route(body, { scope: 's', requestId: 'ambiguous' });
  assert.equal(decision.reason, 'unknown_continuation');
  assert.equal(decision.continuity_state, 'unknown');
  assert.equal(router.complete('ambiguous', done(config.models.sonnet)), false);
  assert.equal(router.turns.toolOwner('s', ['tool-a']).pin.model, config.models.opus);
  assert.equal(router.turns.toolOwner('s', ['tool-b']).pin.model, config.models.haiku);
});
