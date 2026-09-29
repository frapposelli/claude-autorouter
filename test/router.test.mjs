import test from 'node:test';
import assert from 'node:assert/strict';
import { readConfig } from '../src/config.mjs';
import { Router, buildState, contextSizeBytes } from '../src/router.mjs';

const config = () => readConfig({ TYPESAFE_API_KEY: 'test-jev', ANTHROPIC_API_KEY: 'test-anthropic' });
const request = text => ({ model: 'claude-sonnet-5', max_tokens: 4096, messages: [{ role: 'user', content: text }] });
const result = (choice, confidence = 0.98) => Response.json({ answers: { tier: { choice, confidence } } });

test('routes all three tiers using the documented Jev request and caches identical requests', async () => {
  for (const tier of ['haiku', 'sonnet', 'opus']) {
    let calls = 0;
    const router = new Router(config(), { fetchImpl: async (url, options) => {
      calls++;
      assert.equal(url, 'https://api.typesafe.ai/v1/systemone');
      assert.equal(options.headers.authorization, 'Bearer test-jev');
      const payload = JSON.parse(options.body);
      assert.equal(payload.questions.tier.type, 'choice');
      assert.equal(payload.state.original_task, 'Task');
      return result(tier);
    } });
    assert.equal((await router.route(request('Task'))).model, config().models[tier]);
    assert.equal((await router.route(request('Task'))).source, 'cache');
    assert.equal(calls, 1);
  }
});

test('uncertainty and invalid classifier responses cannot downgrade Opus', async () => {
  for (const response of [() => result('haiku', 0.1), () => result('invalid'), () => result('haiku', '0.9'), () => Response.json({})]) {
    const router = new Router(config(), { fetchImpl: async () => response() });
    const body = { ...request('Task'), model: config().models.opus };
    assert.equal((await router.route(body)).model, config().models.opus);
  }
});

test('timeouts fall back promptly and failures are not cached', async () => {
  let calls = 0;
  const router = new Router({ ...config(), jevTimeoutMs: 20 }, { fetchImpl: async (_, { signal }) => {
    calls++;
    if (calls > 1) return result('haiku');
    return new Promise((resolve, reject) => {
      const keepAlive = setTimeout(resolve, 2000);
      signal.addEventListener('abort', () => { clearTimeout(keepAlive); reject(signal.reason); }, { once: true });
    });
  } });
  const start = performance.now();
  const first = await router.route(request('Task'));
  assert.equal(first.source, 'fallback');
  assert.equal(first.model, config().models.sonnet);
  assert.ok(performance.now() - start < 1000);
  assert.equal((await router.route(request('Task'))).model, config().models.haiku);
});

test('tool continuations are still evaluated but stay with the originating model, per agent', async () => {
  let calls = 0;
  const router = new Router(config(), { fetchImpl: async () => result(++calls === 1 ? 'opus' : 'haiku') });
  const body = request('Debug a race condition');
  assert.equal((await router.route(body, { scope: 'session/agent1' })).model, config().models.opus);
  const continuation = { ...body, messages: [...body.messages,
    { role: 'assistant', content: [{ type: 'tool_use', id: 'a', name: 'Read', input: {} }] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'a', content: 'file contents' }] },
  ] };
  const decision = await router.route(continuation, { scope: 'session/agent1' });
  assert.equal(calls, 2);
  assert.equal(decision.model, config().models.opus);
  assert.equal(decision.reason, 'tool_turn_pinned');
  assert.equal((await router.route(continuation, { scope: 'session/agent2' })).model, body.model);
  const newTurn = { ...continuation, messages: [...continuation.messages, { role: 'assistant', content: 'Done' }, { role: 'user', content: 'Fix a typo' }] };
  assert.equal((await router.route(newTurn, { scope: 'session/agent1' })).model, config().models.haiku);
});

test('thinking history keeps the actual model of the previous routed turn', async () => {
  const router = new Router(config(), { fetchImpl: async () => result('opus') });
  const body = request('Hard task');
  await router.route(body);
  const next = { ...body, messages: [...body.messages,
    { role: 'assistant', content: [{ type: 'thinking', thinking: 'secret', signature: 'signature' }, { type: 'text', text: 'Done' }] },
    { role: 'user', content: 'Follow up' },
  ] };
  assert.equal((await router.route(next)).model, config().models.opus);
});

test('capability guards preserve bodies and avoid unsupported Haiku downgrades', async () => {
  const router = new Router(config(), { fetchImpl: async () => result('haiku') });
  for (const extra of [{ thinking: { type: 'adaptive' } }, { output_config: { effort: 'high' } }, { max_tokens: 100000 }, { context_management: { edits: [] } }, { tools: [{ type: 'computer_20250124', name: 'computer' }] }]) {
    const body = { ...request('Simple task'), ...extra };
    const before = structuredClone(body);
    assert.equal((await router.route(body)).model, config().models.sonnet);
    assert.deepEqual(body, before);
  }
  const body = request('x'.repeat(160000));
  assert.equal((await router.route(body)).reason, 'large_or_multimodal_request');
});

test('Jev receives bounded excerpts without attachments or signed thinking', () => {
  const body = request('task');
  body.messages.push({ role: 'assistant', content: [{ type: 'thinking', thinking: 'PRIVATE_THINKING' }] });
  body.messages.push({ role: 'user', content: [{ type: 'image', source: { data: 'PRIVATE_BASE64' } }, { type: 'text', text: '"\\\n'.repeat(100000) }] });
  const state = JSON.stringify(buildState(body));
  assert.ok(state.length <= 12000);
  assert.ok(!state.includes('PRIVATE_THINKING'));
  assert.ok(!state.includes('PRIVATE_BASE64'));
});

test('the excerpt budget includes escaping in initial system and task text', () => {
  const body = { ...request('\u0000'.repeat(2000)), system: '\u0000'.repeat(1000) };
  for (const limit of [12000, 2000]) {
    const state = buildState(body, limit);
    assert.ok(JSON.stringify(state).length <= limit);
    assert.ok(state.system.length > 0);
    assert.ok(state.original_task.length > 0);
  }
});

test('configuration rejects invalid budgets and insecure remote endpoints', () => {
  for (const env of [{ AUTOROUTER_JEV_TIMEOUT_MS: 'NaN' }, { AUTOROUTER_MIN_CONFIDENCE: '2' }, { AUTOROUTER_PORT: '3.5' }, { AUTOROUTER_JEV_URL: 'http://example.com' }]) {
    assert.throws(() => readConfig(env));
  }
});

test('trailing system messages preserve a capable model and establish the human turn for later tool results', async () => {
  const router = new Router(config(), { fetchImpl: async () => result('haiku') });
  const initial = { ...request('Task'), model: config().models.opus };
  const system = { role: 'system', clear_at: 'next_user_message', content: [{ type: 'text', text: 'Turn instructions' }] };
  const body = { ...initial, messages: [...initial.messages, system] };
  const before = structuredClone(body);
  const decision = await router.route(body);
  assert.equal(decision.model, config().models.opus);
  assert.equal(decision.reason, 'mid_conversation_system');
  assert.deepEqual(body, before);
  assert.ok(buildState(body).recent_messages.some(m => m.role === 'system'));
  // Some clients retain cleared system entries; others omit them. The model
  // choice remains pinned when the next request omits the trailing reminder.
  const continuation = { ...initial, messages: [...initial.messages,
    { role: 'assistant', content: [{ type: 'tool_use', id: 'a', name: 'Read', input: {} }] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'a', content: 'result' }] },
  ] };
  const next = await router.route(continuation);
  assert.equal(next.model, config().models.opus);
  assert.equal(next.reason, 'tool_turn_pinned');
});

test('a system capability introduced during a tool turn updates its pinned model', async () => {
  const router = new Router(config(), { fetchImpl: async () => result('haiku') });
  const initial = { ...request('Task'), model: config().models.opus };
  assert.equal((await router.route(initial)).model, config().models.haiku);
  const continuation = { ...initial, messages: [...initial.messages,
    { role: 'assistant', content: 'Working' },
    { role: 'system', content: 'New instruction'.repeat(12000) },
  ] };
  assert.equal((await router.route(continuation)).model, config().models.opus);
  continuation.messages.pop();
  const next = await router.route(continuation);
  assert.equal(next.model, config().models.opus);
  assert.equal(next.reason, 'tool_turn_pinned');
});

test('moving API cache markers preserves a tool turn without erasing tool input', async () => {
  const router = new Router(config(), { fetchImpl: async () => result('opus') });
  const cache_control = { type: 'ephemeral' };
  const initial = { ...request('Task'),
    system: [{ type: 'text', text: 'System', cache_control }],
    tools: [{ name: 'Read', input_schema: { type: 'object' }, cache_control }],
    messages: [
      { role: 'user', content: 'Earlier task' },
      { role: 'assistant', content: [{ type: 'tool_use', id: 'old', name: 'Read', input: { cache_control: 'literal tool data' } }] },
      { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'old', content: [{ type: 'text', text: 'Earlier result', cache_control }] }] },
      { role: 'assistant', content: 'Done' },
      { role: 'user', content: [{ type: 'text', text: 'New task', cache_control }] },
    ],
  };
  await router.route(initial);
  const continuation = structuredClone(initial);
  delete continuation.system[0].cache_control;
  delete continuation.tools[0].cache_control;
  delete continuation.messages[2].content[0].content[0].cache_control;
  delete continuation.messages[4].content[0].cache_control;
  continuation.messages.push(
    { role: 'assistant', content: [{ type: 'tool_use', id: 'new', name: 'Read', input: {} }] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'new', content: 'New result', cache_control }] },
  );
  const before = structuredClone(continuation);
  const next = await router.route(continuation);
  assert.equal(next.model, config().models.opus);
  assert.equal(next.reason, 'tool_turn_pinned');
  assert.deepEqual(continuation, before);

  // This field belongs to the user's tool input, so it must remain part of the
  // identity instead of being stripped by a recursive key-name filter.
  continuation.messages[1].content[0].input.cache_control = 'different tool data';
  assert.equal((await router.route(continuation)).reason, 'unknown_continuation');
});

test('gateway prompt IDs keep tool continuations pinned when available tools change, scoped by agent', async () => {
  const router = new Router(config(), { fetchImpl: async () => result('opus') });
  const initial = request('Discover tools and implement task');
  await router.route(initial, { scope: 'session/agent1', promptId: 'prompt-1' });
  const continuation = { ...initial,
    tools: [{ name: 'NewlyDiscoveredTool', input_schema: { type: 'object' } }],
    messages: [...initial.messages,
      { role: 'assistant', content: [{ type: 'tool_use', id: 'a', name: 'ToolSearch', input: {} }] },
      { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'a', content: 'New tool available' }] },
    ],
  };
  const next = await router.route(continuation, { scope: 'session/agent1', promptId: 'prompt-1' });
  assert.equal(next.model, config().models.opus);
  assert.equal(next.reason, 'tool_turn_pinned');
  assert.equal((await router.route(continuation, { scope: 'session/agent2', promptId: 'prompt-1' })).reason, 'unknown_continuation');
  const followingTurn = { ...continuation, messages: [...continuation.messages,
    { role: 'assistant', content: [{ type: 'thinking', thinking: 'private', signature: 'signed' }, { type: 'text', text: 'Done' }] },
    { role: 'user', content: 'Next task' },
  ] };
  const following = await router.route(followingTurn, { scope: 'session/agent1', promptId: 'prompt-2' });
  assert.equal(following.model, config().models.opus);
  assert.equal(following.reason, 'thinking_history');
});

test('thinking history recovers the preceding model across different gateway prompt IDs', async () => {
  const router = new Router(config(), { fetchImpl: async () => result('opus') });
  const initial = request('Hard task');
  await router.route(initial, { scope: 'session', promptId: 'prompt-1' });
  const next = { ...initial, messages: [...initial.messages,
    { role: 'assistant', content: [{ type: 'thinking', thinking: 'private', signature: 'signed' }, { type: 'text', text: 'Done' }] },
    { role: 'user', content: 'Follow up' },
  ] };
  const decision = await router.route(next, { scope: 'session', promptId: 'prompt-2' });
  assert.equal(decision.model, config().models.opus);
  assert.equal(decision.reason, 'thinking_history');
});

test('large internal requests preserve their requested model instead of the human turn model', async () => {
  const router = new Router(config(), { fetchImpl: async () => result('haiku') });
  const initial = { ...request('Task'), model: config().models.opus };
  await router.route(initial);
  const internal = { ...initial, messages: [...initial.messages, { role: 'assistant', content: 'Large history'.repeat(15000) }] };
  for (const requestClass of ['compaction', 'auxiliary']) {
    const decision = await router.route(internal, { requestClass });
    assert.equal(decision.model, initial.model);
    assert.equal(decision.reason, 'internal_request');
  }
});

test('unknown requested models are preserved even when a preceding routed turn is known', async () => {
  const router = new Router(config(), { fetchImpl: async () => result('haiku') });
  const initial = request('Task');
  await router.route(initial);
  const unknown = { ...initial, model: 'custom-experimental-model', messages: [...initial.messages,
    { role: 'assistant', content: 'Done' },
    { role: 'user', content: 'Large task'.repeat(20000) },
  ] };
  const decision = await router.route(unknown);
  assert.equal(decision.model, unknown.model);
  assert.equal(decision.reason, 'unknown_model');
});

test('large and multimodal requests can upgrade a compatible Haiku client, but never downgrade Opus', async () => {
  for (const content of ['large'.repeat(32000), [{ type: 'image', source: { type: 'base64', data: 'fake-test-image' } }, { type: 'text', text: 'Analyze this' }]]) {
    const body = { model: config().models.haiku, max_tokens: 4096, messages: [{ role: 'user', content }] };
    const strong = new Router(config(), { fetchImpl: async () => result('opus') });
    assert.equal((await strong.route(body)).model, config().models.opus);
    const weak = new Router(config(), { fetchImpl: async () => result('haiku') });
    const decision = await weak.route({ ...body, model: config().models.opus });
    assert.equal(decision.model, config().models.opus);
    assert.equal(decision.reason, 'large_or_multimodal_request');
  }
});

test('large system prompts and tool schemas raise a simple Haiku choice to a model with enough context', async () => {
  for (const extra of [
    { system: 'system context '.repeat(12000) },
    { tools: [{ name: 'Read', description: 'tool documentation '.repeat(9000), input_schema: { type: 'object' } }] },
  ]) {
    let evaluations = 0;
    const router = new Router(config(), { fetchImpl: async (_, options) => {
      evaluations++;
      assert.ok(JSON.stringify(JSON.parse(options.body).state).length <= 12000);
      return result('haiku');
    } });
    const body = { ...request('What is in this repository?'), model: config().models.haiku, ...extra };
    const before = structuredClone(body);
    const decision = await router.route(body);
    assert.equal(decision.model, config().models.sonnet);
    assert.equal(decision.reason, 'context_capacity');
    assert.equal(evaluations, 1); // No token-counting network request.
    assert.deepEqual(body, before);
  }
});

const deferredRequest = () => ({
  ...request('What is in this repository?'), model: config().models.haiku,
  tools: [
    { name: 'ToolSearch', description: 'Find tools to use', input_schema: { type: 'object', properties: { query: { type: 'string' } } } },
    { name: 'mcp__example__lookup', defer_loading: true, input_schema: {
      type: 'object', properties: { query: { type: 'string', description: 'schema documentation '.repeat(8000) } },
    } },
  ],
});

test('unused deferred schemas do not force a simple prompt away from Haiku and remain unchanged on the wire', async () => {
  const router = new Router(config(), { fetchImpl: async () => result('haiku') });
  const body = deferredRequest();
  const before = structuredClone(body);
  assert.ok(Buffer.byteLength(JSON.stringify(body)) > 150000);
  assert.ok(contextSizeBytes(body) < 1000);
  const decision = await router.route(body);
  assert.equal(decision.model, config().models.haiku);
  assert.equal(decision.reason, 'classified');
  assert.deepEqual(body, before);
});

test('deferred schemas count when discovered through direct or nested references or historical calls', async () => {
  const reference = { type: 'tool_reference', tool_name: 'mcp__example__lookup' };
  for (const content of [
    [reference],
    [{ type: 'tool_result', tool_use_id: 'search', content: [reference] }],
    [{ type: 'tool_search_tool_result', tool_use_id: 'search', content: { type: 'tool_search_tool_search_result', tool_references: [reference] } }],
    [{ type: 'tool_use', id: 'lookup', name: 'mcp__example__lookup', input: {} }],
    [{ type: 'future_wrapper', payload: { nested: [reference] } }],
  ]) {
    const router = new Router(config(), { fetchImpl: async () => result('haiku') });
    const body = deferredRequest();
    body.messages.push({ role: 'assistant', content });
    const before = structuredClone(body);
    assert.ok(contextSizeBytes(body) > 150000);
    const decision = await router.route(body);
    assert.equal(decision.model, config().models.sonnet);
    assert.equal(decision.reason, 'context_capacity');
    assert.deepEqual(body, before);
  }
});

test('repeated references count repeated schema expansions across conversation history', async () => {
  const body = deferredRequest();
  body.tools[1].input_schema.properties.query.description = 'schema '.repeat(12000);
  const reference = { type: 'tool_reference', tool_name: body.tools[1].name };
  body.messages.push({ role: 'user', content: [{ type: 'tool_result', tool_use_id: 'first', content: [reference] }] });
  assert.ok(contextSizeBytes(body) < 150000);
  body.messages.push({ role: 'user', content: [{ type: 'tool_result', tool_use_id: 'second', content: [reference] }] });
  assert.ok(contextSizeBytes(body) > 150000);
  const router = new Router(config(), { fetchImpl: async () => result('haiku') });
  assert.equal((await router.route(body)).model, config().models.sonnet);
});

test('deferral never excludes unknown or malformed tool shapes or models from the context guard', () => {
  for (const change of [
    body => { body.model = 'custom-haiku-model'; },
    body => { body.tools[1].defer_loading = 'true'; },
    body => { body.tools[1].input_schema.type = 'array'; },
    body => { body.tools[1].future_option = true; },
    body => { body.tools[1].strict = 'true'; },
    body => { body.tools[1].description = { unknown: 'format' }; },
    body => { body.tools[1].allowed_callers = [null]; },
    body => { body.tools[1].cache_control = { type: 'ephemeral' }; },
    body => { body.tools[0].defer_loading = true; },
    body => { body.messages.push({ role: 'user', content: [{ type: 'tool_reference', tool_name: null }] }); },
  ]) {
    const body = deferredRequest();
    change(body);
    assert.equal(contextSizeBytes(body), Buffer.byteLength(JSON.stringify(body)));
  }
});

test('deferral only affects top-level eligible schemas and keeps other request context in the guard', async () => {
  for (const change of [
    body => { body.system = 'system '.repeat(23000); },
    body => { delete body.tools[1].defer_loading; body.tools[1].input_schema.properties.defer_loading = { const: true }; },
  ]) {
    const body = deferredRequest();
    change(body);
    assert.ok(contextSizeBytes(body) > 150000);
    const router = new Router(config(), { fetchImpl: async () => result('haiku') });
    assert.equal((await router.route(body)).model, config().models.sonnet);
  }
});

test('deferred server tool types still preserve the existing model-specific compatibility lock', async () => {
  const body = deferredRequest();
  body.tools[1].type = 'computer_20250124';
  assert.equal(contextSizeBytes(body), Buffer.byteLength(JSON.stringify(body)));
  const router = new Router(config(), { fetchImpl: async () => result('opus') });
  const decision = await router.route(body);
  assert.equal(decision.model, body.model);
  assert.equal(decision.reason, 'model_specific_features');
});

test('a growing unsigned Haiku tool turn upgrades once and retains the new model within its scope', async () => {
  const router = new Router(config(), { fetchImpl: async () => result('haiku') });
  const initial = { ...request('Read and summarize this repository'), model: config().models.haiku };
  const options = { scope: 'session/agent1', promptId: 'prompt-1' };
  assert.equal((await router.route(initial, options)).model, config().models.haiku);
  const continuation = { ...initial, messages: [...initial.messages,
    { role: 'assistant', content: [{ type: 'tool_use', id: 'read', name: 'Read', input: {} }] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'read', content: 'file contents '.repeat(12000) }] },
  ] };
  const before = structuredClone(continuation);
  const upgrade = await router.route(continuation, options);
  assert.equal(upgrade.model, config().models.sonnet);
  assert.equal(upgrade.reason, 'context_capacity');
  assert.deepEqual(continuation, before);
  // Even if old tool output gets shortened, the same turn remains on Sonnet.
  continuation.messages[2].content[0].content = 'shortened output';
  const next = await router.route(continuation, options);
  assert.equal(next.model, config().models.sonnet);
  assert.equal(next.reason, 'tool_turn_pinned');
  const other = await router.route(continuation, { ...options, scope: 'session/agent2' });
  assert.equal(other.model, config().models.haiku);
  assert.equal(other.reason, 'unknown_continuation');
});

test('nested tool attachments trigger the capacity floor even with a small request body', async () => {
  for (const type of ['image', 'document']) {
    const router = new Router(config(), { fetchImpl: async () => result('haiku') });
    const body = { ...request('Inspect the attachment'), model: config().models.haiku };
    body.messages.push(
      { role: 'assistant', content: [{ type: 'tool_use', id: 'fetch', name: 'Fetch', input: {} }] },
      { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'fetch', content: [
        { type, source: { type: 'url', url: 'https://example.test/attachment' } },
      ] }] },
    );
    assert.ok(Buffer.byteLength(JSON.stringify(body)) < 150000);
    const before = structuredClone(body);
    const decision = await router.route(body);
    assert.equal(decision.model, config().models.sonnet);
    assert.equal(decision.reason, 'context_capacity');
    assert.deepEqual(body, before);
  }
});

test('capacity upgrades never override thinking or model-specific locks hidden behind a turn pin', async () => {
  for (const extra of [
    { thinking: { type: 'enabled', budget_tokens: 1024 } },
    { context_management: { edits: [] } },
    { speed: 'standard' },
    { container: {} },
    { mcp_servers: [] },
    { tools: [{ name: 'computer', type: 'computer_20250124' }] },
    { history: { type: 'thinking', thinking: 'private', signature: 'signed' } },
    { history: { type: 'redacted_thinking', data: 'signed' } },
  ]) {
    const router = new Router(config(), { fetchImpl: async () => result('haiku') });
    const initial = { ...request('Task'), model: config().models.haiku };
    const options = { scope: 'session/agent', promptId: 'prompt' };
    await router.route(initial, options);
    const { history, ...fields } = extra;
    const body = { ...initial, ...fields, system: 'large system '.repeat(13000), messages: [...initial.messages,
      { role: 'assistant', content: [...(history ? [history] : []), { type: 'tool_use', id: 'read', name: 'Read', input: {} }] },
      { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'read', content: 'result' }] },
    ] };
    const before = structuredClone(body);
    const decision = await router.route(body, options);
    assert.equal(decision.model, config().models.haiku);
    assert.equal(decision.reason, 'tool_turn_pinned');
    assert.deepEqual(body, before);
  }
});

test('large internal Haiku requests can upgrade without changing the foreground turn pin', async () => {
  for (const requestClass of ['compaction', 'auxiliary']) {
    const router = new Router(config(), { fetchImpl: async () => result('haiku') });
    const initial = { ...request('Task'), model: config().models.haiku };
    const options = { scope: 'session/agent', promptId: 'prompt' };
    await router.route(initial, options);
    const continuation = { ...initial, messages: [...initial.messages, { role: 'assistant', content: 'Working' }] };
    const internal = { ...continuation, system: 'large system '.repeat(13000) };
    const decision = await router.route(internal, { ...options, requestClass });
    assert.equal(decision.model, config().models.sonnet);
    assert.equal(decision.reason, 'context_capacity');
    assert.equal((await router.route(continuation, options)).model, config().models.haiku);
  }
});

test('capacity upgrades use a known large-window Opus when configured Sonnet only has 200K', async () => {
  const c = config();
  c.models.sonnet = 'claude-sonnet-4-5-20250929';
  const router = new Router(c, { fetchImpl: async () => result('haiku') });
  const decision = await router.route({ ...request('Simple task'), model: c.models.haiku, system: 'x'.repeat(160000) });
  assert.equal(decision.model, c.models.opus);
  assert.equal(decision.reason, 'context_capacity');
  c.models.opus = 'claude-opus-4-5-20251101';
  const noCapacity = new Router(c, { fetchImpl: async () => result('haiku') });
  const unchanged = await noCapacity.route({ ...request('Simple task'), model: c.models.haiku, system: 'x'.repeat(160000) });
  assert.equal(unchanged.model, c.models.haiku);
  assert.equal(unchanged.reason, 'large_or_multimodal_request');
});

test('capacity floor preserves unknown models and mid-conversation system compatibility', async () => {
  const router = new Router(config(), { fetchImpl: async () => result('haiku') });
  const base = { ...request('Task'), system: 'x'.repeat(160000) };
  for (const model of ['custom-experimental-model', 'custom-haiku-model', 'custom-sonnet-experimental-model', 'custom-opus-model']) {
    const unknown = await router.route({ ...base, model }, { scope: model });
    assert.equal(unknown.model, model);
    assert.notEqual(unknown.reason, 'context_capacity');
  }
  const body = { ...base, model: config().models.haiku, messages: [...base.messages, { role: 'system', content: 'Turn-scoped instructions' }] };
  const decision = await router.route(body);
  assert.equal(decision.model, config().models.haiku);
  assert.equal(decision.reason, 'mid_conversation_system');
});

test('a large serialized request that fits Haiku stays on Haiku after token counting', async () => {
  for (const requestedModel of [config().models.haiku, config().models.sonnet]) {
    const router = new Router(config(), { fetchImpl: async () => result('haiku') });
    const body = { ...request('What does [].length evaluate to?'), model: requestedModel, system: 'Tool catalog context. '.repeat(10000) };
    const before = structuredClone(body);
    let calls = 0;
    const decision = await router.route(body, { countTokens: async (received, model) => {
      calls++;
      assert.deepEqual(received, before);
      assert.equal(model, config().models.haiku);
      return 54481;
    } });
    assert.equal(decision.model, config().models.haiku);
    assert.equal(decision.reason, 'classified');
    assert.equal(decision.classified_tier, 'haiku');
    assert.equal(decision.context_check, 'within_budget');
    assert.equal(decision.counted_input_tokens, 54481);
    assert.equal(calls, 1);
    assert.deepEqual(body, before);
  }
});

test('near-limit, overflowing, and uncountable large requests keep the capacity floor', async () => {
  const body = { ...request('Short task'), model: config().models.haiku, system: 'x'.repeat(200000) };
  for (const count of [190001, 227338, undefined, -1, NaN, Infinity]) {
    const router = new Router(config(), { fetchImpl: async () => result('haiku') });
    const decision = await router.route(body, { countTokens: async () => count });
    assert.equal(decision.model, config().models.sonnet);
    assert.equal(decision.reason, 'context_capacity');
    assert.equal(decision.context_check, Number.isSafeInteger(count) && count >= 0 ? 'over_budget' : 'count_unavailable');
  }
});

test('token checking overlaps classification, while small requests and hard locks need no count call', async () => {
  let counted = false;
  const router = new Router(config(), { fetchImpl: async () => {
    assert.equal(counted, true);
    return result('haiku');
  } });
  const body = { ...request('Simple task'), model: config().models.haiku, system: 'x'.repeat(160000) };
  const decision = await router.route(body, { countTokens: async () => { counted = true; return 40000; } });
  assert.equal(decision.model, config().models.haiku);
  const noCount = new Router(config(), { fetchImpl: async () => result('haiku') });
  const countTokens = async () => assert.fail('Must not add a counting request');
  await noCount.route(request('Tiny task'), { countTokens });
  await noCount.route({ ...body, thinking: { type: 'enabled', budget_tokens: 1024 } }, { countTokens });
});
