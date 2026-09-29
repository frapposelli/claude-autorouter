import test from 'node:test';
import assert from 'node:assert/strict';
import { readConfig, requireKeys } from '../src/config.mjs';
import { Router } from '../src/router.mjs';
import { buildState } from '../src/prompt-state.mjs';
import { buildOllamaState, evaluateOllama } from '../src/ollama-evaluator.mjs';

const config = overrides => ({ ...readConfig({ AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_AUTH_MODE: 'subscription' }), ...overrides });
const body = (text = 'Fix one typo') => ({ model: 'claude-haiku-4-5-20251001', max_tokens: 4096, messages: [{ role: 'user', content: text }] });
const metadata = () => Response.json({ details: { parameter_size: '1.7B' }, capabilities: ['completion'] });
const response = tier => Response.json({ done: true, done_reason: 'stop', message: { role: 'assistant', content: JSON.stringify({ tier }) } });

test('multibyte local context stays inside the byte budget and excludes private thinking and images', () => {
  const request = body('你好🌍'.repeat(2000));
  request.system = '背景'.repeat(5000);
  request.messages.push({ role: 'assistant', content: [{ type: 'thinking', thinking: 'SECRET_THINKING' }, { type: 'image', source: { data: 'SECRET_BASE64' } }] });
  const state = buildOllamaState(request);
  const serialized = JSON.stringify(state);
  assert.ok(Buffer.byteLength(serialized) <= 3000);
  assert.ok(state.current_task.length > 0);
  assert.ok(!serialized.includes('SECRET'));
});

test('Jev remains default; Ollama subscription requires no evaluator key and API billing still requires one', () => {
  assert.equal(readConfig({}).evaluator, 'jev');
  assert.throws(() => requireKeys(readConfig({ AUTOROUTER_AUTH_MODE: 'subscription' })), /TYPESAFE_API_KEY/);
  assert.doesNotThrow(() => requireKeys(config()));
  assert.throws(() => requireKeys(readConfig({ AUTOROUTER_EVALUATOR: 'ollama' })), /ANTHROPIC_API_KEY/);
  assert.doesNotThrow(() => requireKeys(readConfig({ AUTOROUTER_EVALUATOR: 'ollama', ANTHROPIC_API_KEY: 'test-api' })));
  for (const env of [{ AUTOROUTER_EVALUATOR: 'auto' }, { AUTOROUTER_OLLAMA_URL: 'https://example.com' },
    { AUTOROUTER_OLLAMA_URL: 'http://127.0.0.1:11434/redirect' }, { AUTOROUTER_OLLAMA_MODEL: 'qwen3:cloud' },
    { AUTOROUTER_OLLAMA_TIMEOUT_MS: '0' }, { AUTOROUTER_OLLAMA_KEEP_ALIVE: '-1' }]) assert.throws(() => readConfig(env));
});

test('Ollama routes every tier locally with bounded input, fixed resource settings, and no invented confidence', async () => {
  for (const tier of ['haiku', 'sonnet', 'opus']) {
    const calls = [];
    const router = new Router(config({ jevKey: 'never-send-jev', anthropicKey: 'never-send-anthropic' }), { fetchImpl: async (url, options) => {
      calls.push(url);
      assert.ok(url.startsWith('http://127.0.0.1:11434/api/'));
      assert.equal(options.redirect, 'error');
      assert.deepEqual(options.headers, { 'content-type': 'application/json' });
      const payload = JSON.parse(options.body);
      if (url.endsWith('/show')) return metadata();
      assert.equal(url, 'http://127.0.0.1:11434/api/chat');
      assert.equal(payload.think, false);
      assert.equal(payload.stream, false);
      assert.equal(payload.options.num_ctx, 4096);
      assert.equal(payload.options.num_predict, 32);
      assert.equal(payload.options.temperature, 0);
      assert.equal(payload.keep_alive, '5m');
      assert.equal(payload.format.additionalProperties, false);
      const state = JSON.parse(payload.messages[1].content);
      assert.equal(state.current_task, 'Fix one typo');
      assert.ok(payload.messages[1].content.length <= 3000);
      return response(tier);
    } });
    const routed = await router.route(body());
    assert.equal(routed.model, config().models[tier]);
    assert.equal(routed.source, 'ollama');
    assert.equal(routed.evaluator, 'ollama');
    assert.equal(routed.confidence, undefined);
    assert.equal(routed.classified_tier, tier);
    const cached = await router.route(body());
    assert.equal(cached.source, 'cache');
    assert.equal(cached.evaluator, 'ollama');
    assert.equal(calls.length, 2, 'Cached decisions must not call either provider');
  }
});

test('a cloud alias or unknown metadata is rejected before any prompt text is sent', async () => {
  for (const data of [{ remote_model: 'cloud-model', details: { parameter_size: '1B' } },
    { remote_host: 'https://ollama.com', details: { parameter_size: '1B' } }, {}, null]) {
    const calls = [];
    const router = new Router(config(), { fetchImpl: async (url, options) => {
      calls.push(url);
      assert.ok(url.endsWith('/api/show'));
      assert.ok(!options.body.includes('private-prompt-sentinel'));
      return Response.json(data);
    } });
    const decision = await router.route(body('private-prompt-sentinel'));
    assert.equal(decision.source, 'fallback');
    assert.equal(decision.classifier_error, 'invalid_response');
    assert.equal(decision.evaluator, 'ollama');
    assert.equal(calls.length, 1);
  }
});

test('large local model metadata is accepted within its separate bound while oversized metadata is rejected', async () => {
  for (const [size, expected] of [[80000, 'ollama'], [1024 * 1024 + 1, 'fallback']]) {
    let chats = 0;
    const router = new Router(config(), { fetchImpl: async url => {
      if (url.endsWith('/show')) return Response.json({ details: { parameter_size: '4B' }, tensors: 'x'.repeat(size) });
      chats++;
      return response('haiku');
    } });
    const decision = await router.classify(body());
    assert.equal(decision.source, expected);
    assert.equal(chats, expected === 'ollama' ? 1 : 0);
  }
});

test('Ollama failures never contact Jev, are not cached, and retain an existing Opus request', async () => {
  for (const failure of [() => { throw new Error('private upstream detail'); },
    () => new Response('private provider error', { status: 503 }),
    () => Response.json({ done: false, message: { role: 'assistant', content: '{"tier":"haiku"}' } }),
    () => Response.json({ done: true, message: { role: 'assistant', content: '{"tier":"haiku","confidence":1}' } }),
    () => Response.json({ done: true, done_reason: 'length', message: { role: 'assistant', content: '{"tier":"haiku"}' } })]) {
    let attempts = 0;
    const router = new Router(config(), { fetchImpl: async url => {
      assert.ok(url.startsWith(config().ollamaEndpoint));
      if (url.endsWith('/show')) return metadata();
      attempts++;
      return attempts === 1 ? failure() : response('sonnet');
    } });
    const request = { ...body(), model: config().models.opus };
    const first = await router.classify(request);
    assert.equal(first.source, 'fallback');
    assert.equal(first.tier, 'opus');
    assert.equal(first.evaluator, 'ollama');
    assert.ok(!JSON.stringify(first).includes('private'));
    assert.equal((await router.classify(request)).source, 'ollama');
  }
});

test('Ollama response size and full-body deadline are bounded', async () => {
  for (const kind of ['oversize', 'stalled']) {
    let cancelled = false;
    const router = new Router(config({ ollamaTimeoutMs: 30 }), { fetchImpl: async url => {
      if (url.endsWith('/show')) return metadata();
      return new Response(new ReadableStream({ start(controller) {
        if (kind === 'oversize') controller.enqueue(new Uint8Array(65537));
      }, cancel() { cancelled = true; } }));
    } });
    const keepAlive = setTimeout(() => {}, 2000);
    const start = performance.now();
    try {
      const decision = await router.classify(body());
      assert.equal(decision.classifier_error, kind === 'oversize' ? 'invalid_response' : 'timeout');
      assert.equal(decision.tier, 'sonnet');
      assert.ok(performance.now() - start < 1000);
      assert.equal(cancelled, true);
    } finally { clearTimeout(keepAlive); }
  }
});

test('caller cancellation propagates and local classifier keeps continuity and capability guards', async () => {
  const controller = new AbortController();
  controller.abort(new Error('cancelled'));
  await assert.rejects(evaluateOllama(buildState(body()), config(), { signal: controller.signal, fetchImpl: async () => metadata() }), /cancelled/);
  let tier = 'opus';
  const router = new Router(config(), { fetchImpl: async url => url.endsWith('/show') ? metadata() : response(tier) });
  assert.equal((await router.route(body('Investigate a difficult race'))).tier, 'opus');
  tier = 'haiku';
  const continuation = { ...body('Investigate a difficult race'), messages: [...body('Investigate a difficult race').messages,
    { role: 'assistant', content: [{ type: 'tool_use', name: 'Read', id: 'a', input: {} }] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'a', content: 'Evidence' }] }] };
  const next = await router.route(continuation);
  assert.equal(next.reason, 'tool_turn_pinned');
  assert.equal(next.model, config().models.opus);
  const guarded = await router.route({ ...body('Fix another typo'), thinking: { type: 'adaptive' } });
  assert.equal(guarded.model, config().models.sonnet);
  assert.equal(guarded.classified_tier, 'haiku');
});
