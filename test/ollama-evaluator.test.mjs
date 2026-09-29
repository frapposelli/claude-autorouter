import test from 'node:test';
import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';
import { readConfig } from '../src/config.mjs';
import { Router } from '../src/router.mjs';
import { buildState } from '../src/prompt-state.mjs';
import { buildOllamaState, evaluateOllama, OLLAMA_QUESTIONS } from '../src/ollama-evaluator.mjs';
import { DEFAULT_OLLAMA_MODEL } from '../src/ollama-models.mjs';

const config = overrides => ({ ...readConfig({ AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_AUTH_MODE: 'subscription' }), ...overrides });
const body = (text = 'Fix one typo') => ({ model: 'claude-haiku-4-5-20251001', max_tokens: 4096, messages: [{ role: 'user', content: text }] });
const metadata = () => Response.json({ details: { parameter_size: '9B' }, capabilities: ['completion'] });
const response = tier => Response.json(decisionPayload(tier));
const decisionPayload = (choice = 'haiku', model = DEFAULT_OLLAMA_MODEL) => ({
  model, answers: { tier: { type: 'choice', choice, probabilities: { haiku: 0.1, sonnet: 0.1, opus: 0.1, [choice]: 0.8 }, confidence: 0.418 } },
  usage: { input_tokens: 1234, output_tokens: 1 },
});

test('local routing excludes Claude executor instructions without changing Jev task extraction', () => {
  for (const prompt of ['What does [].length return?', 'Implement paginated results with tests.',
    'Prove a lease protocol prevents stale writes across a network partition.']) {
    const request = body(prompt);
    const withBackground = { ...request, system: [{ type: 'text',
      text: 'You are a coding assistant. Inspect repositories, implement features, review code and run tests. '.repeat(200) }],
    };
    assert.deepEqual(buildOllamaState(withBackground), buildOllamaState(request));
    assert.equal(buildOllamaState(withBackground).current_task, prompt);
    assert.ok(buildState(withBackground).system.includes('coding assistant'));
  }
});

test('excluding local executor instructions still retains follow-up evidence and bounded Unicode excerpts', () => {
  const request = body('Worker A pauses with lease 41; B commits under 42. Storage accepts every issued token.');
  request.system = 'IRRELEVANT_EXECUTOR_BACKGROUND '.repeat(1000);
  request.messages.push({ role: 'assistant', content: 'The storage acceptance rule is suspect.' },
    { role: 'user', content: 'Prove whether both can commit; specify the atomic repair.' },
    { role: 'assistant', content: [{ type: 'tool_use', name: 'Read', id: 'evidence', input: { secret: 'PRIVATE_TOOL_INPUT' } }] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'evidence', is_error: true,
      content: 'EVIDENCE_START ' + '并发🌍'.repeat(2000) + ' STALE_WRITE_ACCEPTED' }] });
  const state = buildOllamaState(request);
  assert.match(state.original_task, /lease 41/);
  assert.equal(state.current_task, 'Prove whether both can commit; specify the atomic repair.');
  assert.ok(state.recent_messages.some(message => message.content.includes('STALE_WRITE_ACCEPTED')));
  assert.ok(Buffer.byteLength(JSON.stringify(state)) <= 3000);
  assert.doesNotMatch(JSON.stringify(state), /IRRELEVANT_EXECUTOR_BACKGROUND|PRIVATE_TOOL_INPUT/);
});

test('Nimble, Tev1, and compatible custom aliases use the same native protocol without chat requests', async () => {
  for (const model of ['nimble', 'library/nimble:9b-q4_K_M', 'registry.ollama.ai/library/nimble:9b-q4_K_M',
    'tev1:0.8b', 'tev1:0.8b-q8_0', 'tev1:4b-q4_K_M', 'team/local-decider:v1']) {
    const calls = [];
    const result = await evaluateOllama(buildOllamaState(body()), config({ ollamaModel: model }), { fetchImpl: async (url, options) => {
      calls.push(new URL(url).pathname);
      const payload = JSON.parse(options.body);
      assert.equal(payload.model, model);
      if (url.endsWith('/api/show')) return metadata();
      assert.equal(new URL(url).pathname, '/v1/systemone');
      assert.deepEqual(Object.keys(payload).sort(), ['keep_alive', 'model', 'questions', 'state']);
      return Response.json(decisionPayload('sonnet', model));
    } });
    assert.equal(result.choice, 'sonnet');
    assert.deepEqual(calls, ['/api/show', '/v1/systemone']);
  }
});

test('Ollama scores all three tiers natively with the frozen policy and no Jev confidence threshold', async () => {
  for (const tier of ['haiku', 'sonnet', 'opus']) {
    const calls = [];
    const fetchImpl = async (url, options) => {
      calls.push(url);
      assert.equal(options.redirect, 'error');
      assert.deepEqual(options.headers, { 'content-type': 'application/json' });
      assert.ok(!options.body.includes('PRIVATE_'));
      if (url.endsWith('/api/show')) return metadata();
      assert.equal(url, 'http://127.0.0.1:11434/v1/systemone');
      const payload = JSON.parse(options.body);
      assert.deepEqual(Object.keys(payload).sort(), ['keep_alive', 'model', 'questions', 'state']);
      assert.equal(payload.model, DEFAULT_OLLAMA_MODEL);
      assert.equal(payload.keep_alive, '5m');
      assert.equal(payload.state.current_task, 'Fix one typo');
      assert.ok(Buffer.byteLength(JSON.stringify(payload.state)) <= 3000);
      assert.deepEqual(payload.questions, OLLAMA_QUESTIONS);
      return Response.json(decisionPayload(tier));
    };
    const router = new Router(config({ jevKey: 'PRIVATE_JEV_KEY', anthropicKey: 'PRIVATE_ANTHROPIC_KEY', minConfidence: 1 }), { fetchImpl });
    const routed = await router.classify(body());
    assert.equal(routed.tier, tier);
    assert.equal(routed.source, 'ollama');
    assert.equal(routed.confidence, undefined);
    assert.equal(routed.probabilities, undefined);
    assert.equal((await router.classify(body())).source, 'cache');
    assert.deepEqual(calls, ['http://127.0.0.1:11434/api/show', 'http://127.0.0.1:11434/v1/systemone']);
  }
  const result = await evaluateOllama(buildOllamaState(body()), config(), {
    fetchImpl: async url => url.endsWith('/show') ? metadata() : Response.json(decisionPayload()),
  });
  assert.deepEqual(result, { choice: 'haiku', metrics: { input_tokens: 1234, output_tokens: 1 } });
});

test('malformed native answers fall back safely without caching or consulting Jev', async () => {
  const mutations = [
    value => { value.model = 'other-model'; },
    value => { value.answers.tier.type = 'score'; },
    value => { value.answers.tier.choice = 'unknown'; },
    value => { value.answers.tier.choice = 'opus'; },
    value => { value.answers.tier.confidence = '0.8'; },
    value => { value.answers.tier.confidence = 1.1; },
    value => { delete value.answers.tier.probabilities.opus; },
    value => { value.answers.tier.probabilities.opus = -0.1; },
    value => { value.answers.tier.probabilities.opus = 0.2; },
    value => { value.answers.extra = 'PRIVATE_RESPONSE'; },
    value => { value.usage.input_tokens = -1; },
    value => { delete value.usage; },
  ];
  for (const mutate of mutations) {
    let classifications = 0;
    const calls = [];
    const router = new Router(config(), { fetchImpl: async url => {
      calls.push(new URL(url).pathname);
      if (url.endsWith('/show')) return metadata();
      const value = decisionPayload();
      if (++classifications === 1) mutate(value);
      return Response.json(value);
    } });
    const request = { ...body(), model: config().models.opus };
    const failed = await router.classify(request);
    assert.equal(failed.source, 'fallback');
    assert.equal(failed.classifier_error, 'invalid_response');
    assert.equal(failed.tier, 'opus');
    assert.ok(!JSON.stringify(failed).includes('PRIVATE_'));
    assert.equal((await router.classify(request)).source, 'ollama');
    assert.deepEqual(calls, ['/api/show', '/v1/systemone', '/api/show', '/v1/systemone']);
  }
});

test('Ollama preserves the local metadata gate and gives safe version guidance only for its missing endpoint', async () => {
  const calls = [];
  await assert.rejects(evaluateOllama(buildOllamaState(body()), config(), { fetchImpl: async url => {
    calls.push(new URL(url).pathname);
    return Response.json({ remote_host: 'PRIVATE_HOST', details: { parameter_size: '9B' } });
  } }), /classifier_invalid_response/);
  assert.deepEqual(calls, ['/api/show']);
  for (const missing of ['/api/show', '/v1/systemone']) {
    await assert.rejects(evaluateOllama(buildOllamaState(body()), config(), { fetchImpl: async url => {
      if (new URL(url).pathname === missing) return new Response('PRIVATE_PROVIDER_ERROR', { status: 404 });
      return metadata();
    } }), error => {
      assert.equal(error.classifierStatus, 404);
      assert.equal(error.code, missing === '/v1/systemone' ? 'OLLAMA_VERSION' : undefined);
      if (missing === '/v1/systemone') assert.match(error.message, /Ollama 0\.35 or newer/);
      assert.ok(!error.message.includes('PRIVATE_'));
      return true;
    });
  }
});

test('Ollama shares one deadline across metadata and native response, cancels bodies, and bounds output', async () => {
  for (const mode of ['metadata', 'native', 'oversize']) {
    let cancelled = false;
    const signals = [];
    const paths = [];
    const router = new Router(config({ ollamaTimeoutMs: 40 }), { fetchImpl: async (url, options) => {
      paths.push(new URL(url).pathname);
      signals.push(options.signal);
      if (url.endsWith('/show') && mode !== 'metadata') {
        await new Promise(resolve => setTimeout(resolve, 10));
        return metadata();
      }
      return new Response(new ReadableStream({ start(controller) {
        if (mode === 'oversize') controller.enqueue(new Uint8Array(65537));
      }, cancel() { cancelled = true; } }));
    } });
    const keepAlive = setTimeout(() => {}, 1000);
    try {
      const decision = await router.classify(body());
      assert.equal(decision.source, 'fallback');
      assert.equal(decision.classifier_error, mode === 'oversize' ? 'invalid_response' : 'timeout');
      assert.equal(cancelled, true);
      assert.deepEqual(paths, mode === 'metadata' ? ['/api/show'] : ['/api/show', '/v1/systemone']);
      assert.ok(signals.every(signal => signal === signals[0]));
    } finally { clearTimeout(keepAlive); }
  }
});

test('a disabled deadline accepts delayed metadata, inference, and body reads with or without a caller signal', { timeout: 2000 }, async () => {
  for (const signal of [undefined, new AbortController().signal]) {
    const fetchImpl = async (url, options) => {
      await delay(15, undefined, { signal: options.signal });
      if (url.endsWith('/show')) return metadata();
      let timer;
      return new Response(new ReadableStream({
        start(controller) {
          timer = setTimeout(() => {
            controller.enqueue(new TextEncoder().encode(JSON.stringify(decisionPayload('opus'))));
            controller.close();
          }, 30);
        },
        cancel() { clearTimeout(timer); },
      }));
    };
    const result = await evaluateOllama(buildOllamaState(body()), config({ ollamaTimeoutMs: 0 }), { signal, fetchImpl });
    assert.equal(result.choice, 'opus');
    // The same response still exceeds an explicitly configured positive budget.
    await assert.rejects(evaluateOllama(buildOllamaState(body()), config({ ollamaTimeoutMs: 20 }), { signal, fetchImpl }),
      error => error.name === 'AbortError' || error.name === 'TimeoutError');
  }
});

test('caller cancellation with no deadline stops metadata and inference body reads without caching a fallback', { timeout: 2000 }, async () => {
  for (const phase of ['/api/show', '/v1/systemone']) {
    const controller = new AbortController();
    const reason = new Error('Caller cancelled local evaluation');
    let started;
    const reading = new Promise(resolve => { started = resolve; });
    let cancelled = false;
    let attempt = 1;
    const paths = [];
    const router = new Router(config({ ollamaTimeoutMs: 0 }), { fetchImpl: async url => {
      const path = new URL(url).pathname;
      paths.push(path);
      if (attempt === 1 && path === phase) {
        return new Response(new ReadableStream({ pull() { started(); }, cancel() { cancelled = true; } }));
      }
      return path === '/api/show' ? metadata() : response('haiku');
    } });
    const pending = router.classify(body(), controller.signal);
    const rejected = assert.rejects(pending, error => error === reason);
    await reading;
    await delay(0);
    controller.abort(reason);
    await rejected;
    assert.equal(cancelled, true);
    assert.deepEqual(paths, phase === '/api/show' ? ['/api/show'] : ['/api/show', '/v1/systemone']);
    attempt++;
    const retried = await router.classify(body());
    assert.equal(retried.source, 'ollama');
    assert.equal(retried.tier, 'haiku');
  }
});

test('an already cancelled request with no deadline never contacts Ollama', async () => {
  const controller = new AbortController();
  const reason = new Error('Already cancelled');
  controller.abort(reason);
  await assert.rejects(evaluateOllama(buildOllamaState(body()), config({ ollamaTimeoutMs: 0 }), {
    signal: controller.signal, fetchImpl: () => assert.fail('Cancelled evaluation must not send metadata or prompt requests'),
  }), error => error === reason);
});

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
    let decisions = 0;
    const router = new Router(config(), { fetchImpl: async url => {
      if (url.endsWith('/show')) return Response.json({ details: { parameter_size: '4B' }, tensors: 'x'.repeat(size) });
      decisions++;
      return response('haiku');
    } });
    const decision = await router.classify(body());
    assert.equal(decision.source, expected);
    assert.equal(decisions, expected === 'ollama' ? 1 : 0);
  }
});

test('Ollama failures with a disabled deadline never contact Jev, are not cached, and retain an existing Opus request', async () => {
  for (const failure of [() => { throw new Error('private upstream detail'); },
    () => new Response('private provider error', { status: 503 }),
    () => new Response('private invalid JSON'),
    () => Response.json({ error: 'private response detail' })]) {
    let attempts = 0;
    const router = new Router(config({ ollamaTimeoutMs: 0 }), { fetchImpl: async url => {
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
