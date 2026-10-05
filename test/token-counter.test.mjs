import test from 'node:test';
import assert from 'node:assert/strict';
import { createTokenCounter } from '../src/token-counter.mjs';

const config = { upstream: 'https://api.anthropic.com', tokenCountTimeoutMs: 50 };
const model = 'claude-haiku-4-5-20251001';
const request = () => ({ model: 'claude-sonnet-5', max_tokens: 4096, stream: true,
  system: [{ type: 'text', text: 'full system context', cache_control: { type: 'ephemeral' } }],
  messages: [{ role: 'user', content: 'Simple question' }],
  tools: [{ name: 'Read', description: 'complete schema', input_schema: { type: 'object' } }],
  thinking: { type: 'disabled' }, metadata: { user_id: 'attribution' } });
const headers = { authorization: 'Bearer request-owned-test-credential', 'anthropic-beta': 'oauth-2025-04-20', 'anthropic-version': '2023-06-01' };

test('counts the complete input for the selected model with request-owned authentication', async () => {
  const body = { ...request(), output_config: { format: { type: 'json_schema', schema: { type: 'object' } } },
    tool_choice: { type: 'auto' }, context_management: { edits: [] }, cache_control: { type: 'ephemeral', ttl: '1h' } };
  const target = 'claude-opus-5';
  const before = structuredClone(body);
  let calls = 0;
  const count = createTokenCounter(config, { fetchImpl: async (url, options) => {
    calls++;
    assert.equal(url, 'https://api.anthropic.com/v1/messages/count_tokens?beta=true');
    assert.equal(options.method, 'POST');
    assert.equal(options.redirect, 'error');
    assert.equal(options.headers.get('authorization'), headers.authorization);
    assert.equal(options.headers.get('anthropic-beta'), headers['anthropic-beta']);
    assert.equal(options.headers.get('content-type'), 'application/json');
    assert.equal(options.headers.get('content-length'), null);
    const { max_tokens, stream, metadata, ...expected } = body;
    assert.deepEqual(JSON.parse(options.body), { ...expected, model: target, thinking: { type: 'adaptive' } });
    return Response.json({ input_tokens: 87654 });
  } });
  assert.equal(await count(body, target, { headers: { ...headers, 'content-length': '99999' }, search: '?beta=true' }), 87654);
  assert.equal(calls, 1);
  assert.deepEqual(body, before);
});

test('counting uses target-model thinking requirements without changing other input or explicit native requests', async () => {
  for (const [source, target, expectedThinking] of [
    [model, 'claude-sonnet-5-5', 'between_tools'],
    [model, 'claude-opus-5-5', 'adaptive'],
    [model, 'claude-sonnet-5', 'disabled'],
    ['claude-sonnet-5-5', 'claude-sonnet-5-5', 'disabled'],
  ]) {
    const body = { ...request(), model: source, output_config: { effort: 'medium' }, tool_choice: { type: 'auto' } };
    const before = structuredClone(body);
    const count = createTokenCounter(config, { fetchImpl: async (_url, options) => {
      const { max_tokens, stream, metadata, ...context } = body;
      assert.deepEqual(JSON.parse(options.body), { ...context, model: target, thinking: { type: expectedThinking } });
      return Response.json({ input_tokens: 12000 });
    } });
    assert.equal(await count(body, target, { headers }), 12000);
    assert.deepEqual(body, before);
  }
});

test('count cache keys reflect the adapted thinking mode and preserve high-effort settings', async () => {
  const target = 'claude-sonnet-5-5';
  const body = { ...request(), model };
  const payloads = [];
  const count = createTokenCounter(config, { fetchImpl: async (_url, options) => {
    payloads.push(JSON.parse(options.body));
    return Response.json({ input_tokens: payloads.length });
  } });
  assert.equal(await count(body, target, { headers }), 1);
  assert.deepEqual(payloads[0].thinking, { type: 'between_tools' });
  assert.equal(await count({ ...body, max_tokens: 20 }, target, { headers }), 1);
  assert.equal(await count({ ...body, model: target, thinking: { type: 'between_tools' } }, target, { headers }), 1);

  const highEffort = { ...body, output_config: { effort: 'xhigh' } };
  assert.equal(await count(highEffort, target, { headers }), 2);
  assert.deepEqual(payloads[1].thinking, { type: 'adaptive' });
  assert.deepEqual(payloads[1].output_config, highEffort.output_config);
  assert.equal(await count({ ...highEffort, thinking: { type: 'adaptive' } }, target, { headers }), 2);
  assert.equal(await count(body, target, { headers }), 1);
  assert.equal(payloads.length, 2);
});

test('cache separates tokenizer, context modifiers, API features, and request credentials', async () => {
  let calls = 0;
  const count = createTokenCounter(config, { fetchImpl: async () => Response.json({ input_tokens: ++calls }) });
  const body = request();
  assert.equal(await count(body, model, { headers }), 1);
  assert.equal(await count({ ...body, max_tokens: 128, stream: false }, model, { headers }), 1);
  assert.equal(await count(body, 'claude-sonnet-5', { headers }), 2);
  assert.equal(await count({ ...body, thinking: { type: 'adaptive' } }, body.model, { headers }), 3);
  assert.equal(await count({ ...body, output_config: { effort: 'low' } }, body.model, { headers }), 4);
  assert.equal(await count(body, model, { headers: { ...headers, authorization: 'Bearer refreshed-test-credential' } }), 5);
  assert.equal(await count(body, model, { headers: { ...headers, 'anthropic-beta': 'different-feature' } }), 6);
  assert.equal(await count(body, model, { headers: { ...headers, 'anthropic-workspace-id': 'workspace-2' } }), 7);
  assert.equal(await count(body, model, { headers }), 1);
});

test('cache is bounded and expires successful counts', async () => {
  let calls = 0;
  const count = createTokenCounter({ ...config, tokenCountCacheEntries: 2, tokenCountCacheTtlMs: 10 }, {
    fetchImpl: async () => Response.json({ input_tokens: ++calls }),
  });
  const body = request();
  assert.equal(await count(body, 'claude-haiku-4-5-20251001'), 1);
  assert.equal(await count(body, 'claude-sonnet-5'), 2);
  assert.equal(await count(body, 'claude-haiku-4-5-20251001'), 1); // Refresh LRU position.
  assert.equal(await count(body, 'claude-opus-5'), 3);
  assert.equal(await count(body, 'claude-sonnet-5'), 4); // b was evicted.
  await new Promise(resolve => setTimeout(resolve, 20));
  assert.equal(await count(body, 'claude-sonnet-5'), 5);
});

test('unsupported server tools, MCP, remote attachments, and unknown input fields skip the API', async () => {
  const count = createTokenCounter(config, { fetchImpl: async () => assert.fail('Unsupported input reached count API') });
  const attachment = (type, source) => ({ messages: [{ role: 'user', content: [{ type: 'tool_result', tool_use_id: 'read', content: [
    { type, source: { type: source, url: 'https://example.test/file', file_id: 'file_test' } },
  ] }] }] });
  for (const extra of [
    { tools: [{ type: 'web_search_20250305', name: 'web_search' }] },
    { tools: [{ type: 'tool_search_tool_regex_20251119', name: 'tool_search_tool_regex' }] },
    { tools: [{ type: 'code_execution_20260120', name: 'code_execution' }] },
    { mcp_servers: [] }, { container: 'container-id' }, { future_input_context: 'not counted' },
    attachment('image', 'url'), attachment('document', 'file'),
  ]) assert.equal(await count({ ...request(), ...extra }, model, { headers }), undefined);
});

test('base64 attachments, client tools, advisor tools and beta count modifiers are preserved', async () => {
  const body = { ...request(), model, compaction: { type: 'summarize' }, speed: 'standard',
    output_format: { type: 'json_schema', schema: { type: 'object' } },
    tools: [{ name: 'bash', type: 'bash_20250124' }, { name: 'advisor', type: 'advisor_20260301', model: 'claude-opus-5-5' }],
    messages: [{ role: 'user', content: [{ type: 'image', source: { type: 'base64', media_type: 'image/png', data: 'test-image' } }] }] };
  const count = createTokenCounter(config, { fetchImpl: async (_, options) => {
    const { max_tokens, stream, metadata, ...expected } = body;
    assert.deepEqual(JSON.parse(options.body), { ...expected, model });
    return Response.json({ input_tokens: 1000 });
  } });
  assert.equal(await count(body, model, { headers }), 1000);
});

test('invalid counts and upstream failures return unknown and are never cached', async () => {
  for (const failed of [
    () => Response.json({ input_tokens: 1 }, { status: 429 }),
    () => Response.json({ input_tokens: '100' }),
    () => Response.json({ input_tokens: -1 }),
    () => Response.json({ input_tokens: 1.5 }),
    () => Response.json({ input_tokens: Number.MAX_SAFE_INTEGER + 1 }),
    () => Response.json({}), () => new Response('not-json'),
    () => new Response('x'.repeat(65537)), () => { throw new Error('private upstream error'); },
  ]) {
    let calls = 0;
    const count = createTokenCounter(config, { fetchImpl: async () => ++calls === 1 ? failed() : Response.json({ input_tokens: 0 }) });
    assert.equal(await count(request(), model, { headers }), undefined);
    assert.equal(await count(request(), model, { headers }), 0);
    assert.equal(await count(request(), model, { headers }), 0);
    assert.equal(calls, 2);
  }
});

test('timeouts include fetch and response body consumption without caching the failure', async () => {
  for (const phase of ['fetch', 'body']) {
    let calls = 0;
    let observedSignal;
    const count = createTokenCounter({ ...config, tokenCountTimeoutMs: 15 }, { fetchImpl: async (_, { signal }) => {
      observedSignal = signal;
      if (++calls > 1) return Response.json({ input_tokens: 123 });
      if (phase === 'fetch') return new Promise(() => {});
      return new Response(new ReadableStream({ start() {} }));
    } });
    const start = performance.now();
    assert.equal(await count(request(), model, { headers }), undefined);
    assert.ok(performance.now() - start < 500);
    assert.equal(observedSignal.aborted, true);
    assert.equal(await count(request(), model, { headers }), 123);
  }
});

test('client cancellation aborts counting promptly and already-cancelled calls skip even cached counts', async () => {
  const controller = new AbortController();
  let observedSignal;
  let entered;
  const started = new Promise(resolve => { entered = resolve; });
  const count = createTokenCounter({ ...config, tokenCountTimeoutMs: 1000 }, { fetchImpl: async (_, { signal }) => {
    observedSignal = signal;
    entered();
    return new Promise(() => {});
  } });
  const pending = count(request(), model, { headers, signal: controller.signal });
  await started;
  controller.abort();
  assert.equal(await pending, undefined);
  assert.equal(observedSignal.aborted, true);
  const cached = createTokenCounter(config, { fetchImpl: async () => Response.json({ input_tokens: 123 }) });
  assert.equal(await cached(request(), model, { headers }), 123);
  assert.equal(await cached(request(), model, { headers, signal: controller.signal }), undefined);
});
