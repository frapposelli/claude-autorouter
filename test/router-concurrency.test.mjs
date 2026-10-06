import test from 'node:test';
import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';
import { Router, CLASSIFICATION_LIMITS } from '../src/router.mjs';
import { readConfig } from '../src/config.mjs';

const config = () => readConfig({ AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: 'synthetic-key', AUTOROUTER_AUTH_MODE: 'subscription' });
const body = (text = 'Synthetic task') => ({ model: 'claude-haiku-4-5-20251001', max_tokens: 16, messages: [{ role: 'user', content: text }] });
const answer = (tier = 'sonnet', confidence = 1) => Response.json({ answers: { tier: { choice: tier, confidence } } });
const deferred = () => { let resolve; const promise = new Promise(done => { resolve = done; }); return { promise, resolve }; };
function heldRouter(c = config()) {
  const requests = [];
  const router = new Router(c, { fetchImpl: async (_url, options) => {
    const gate = deferred(); requests.push({ ...options, ...gate });
    return gate.promise;
  } });
  return { router, requests };
}
const settle = async () => { await Promise.resolve(); await Promise.resolve(); };

test('eight identical concurrent requests share one evaluation while policy still isolates agents', async () => {
  const { router, requests } = heldRouter();
  const pending = Array.from({ length: 8 }, (_, agent) => router.route(body(), { scope: `agent-${agent}`, promptId: `prompt-${agent}` }));
  await settle();
  assert.equal(requests.length, 1);
  requests[0].resolve(answer('haiku'));
  const decisions = await Promise.all(pending);
  assert.ok(decisions.every(row => row.source === 'jev' && row.classified_tier === 'haiku'));
  decisions[0].tier = 'opus';
  assert.equal(decisions[1].tier, 'haiku');
  assert.equal((await router.classify(body())).tier, 'haiku');
  assert.equal(router.pendingEvaluations.size, 0);
  assert.equal(router.evaluationSubscribers, 0);
  assert.equal(router.turns.records.size, 8);
});

test('shared evaluation does not merge the confirmed continuation model of different agents', async () => {
  let calls = 0;
  const router = new Router(config(), { fetchImpl: async () => { calls++; await delay(1); return answer('haiku'); } });
  for (const [scope, model] of [['a', 'claude-opus-5-5'], ['b', 'claude-sonnet-5']]) {
    await router.route(body(), { scope, promptId: 'same', requestId: scope });
    router.complete(scope, { continuation_model: model, tool_uses: [{ id: 'read', model }] });
  }
  const continuation = { ...body(), messages: [...body().messages,
    { role: 'assistant', content: [{ type: 'tool_use', id: 'read', name: 'Read', input: {} }] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'read', content: 'Synthetic result' }] }] };
  const before = calls;
  const results = await Promise.all(['a', 'b'].map(scope => router.route(continuation, { scope, promptId: 'same' })));
  assert.equal(calls - before, 1);
  assert.deepEqual(results.map(row => row.model), ['claude-opus-5-5', 'claude-sonnet-5']);
  assert.ok(results.every(row => row.reason === 'tool_turn_pinned'));
});

test('one cancelled subscriber rejects independently without aborting the shared evaluator', async () => {
  const { router, requests } = heldRouter();
  const controller = new AbortController(), reason = new Error('only first caller cancelled');
  const first = router.classify(body(), controller.signal), second = router.classify(body());
  const rejected = assert.rejects(first, error => error === reason);
  await settle(); controller.abort(reason); await rejected;
  assert.equal(requests[0].signal.aborted, false);
  assert.equal(router.evaluationSubscribers, 1);
  requests[0].resolve(answer('opus'));
  assert.equal((await second).tier, 'opus');
  assert.equal((await router.classify(body())).source, 'cache');
  assert.equal(requests.length, 1);
});

test('last cancellation aborts work, prevents late cache writes, and permits an independent retry', async () => {
  const { router, requests } = heldRouter();
  const controllers = [new AbortController(), new AbortController()];
  const pending = controllers.map(controller => router.classify(body(), controller.signal));
  const rejected = Promise.all(pending.map(promise => assert.rejects(promise, /cancelled/)));
  await settle(); controllers.forEach(controller => controller.abort(new Error('cancelled'))); await rejected;
  assert.equal(requests[0].signal.aborted, true);
  assert.equal(router.pendingEvaluations.size, 0);
  const retry = router.classify(body()); await settle();
  assert.equal(requests.length, 2);
  requests[0].resolve(answer('opus')); await settle();
  assert.equal(router.pendingEvaluations.size, 1);
  requests[1].resolve(answer('haiku'));
  assert.equal((await retry).tier, 'haiku');
  assert.equal((await router.classify(body())).tier, 'haiku');
});

test('already-cancelled cache hits and immediate cancellation never send evaluation requests', async () => {
  let calls = 0;
  const router = new Router(config(), { fetchImpl: async () => { calls++; return answer(); } });
  await router.classify(body());
  const controller = new AbortController(); controller.abort(new Error('already cancelled'));
  await assert.rejects(router.classify(body(), controller.signal), /already cancelled/);
  const immediate = new AbortController();
  const pending = router.classify(body('new'), immediate.signal);
  immediate.abort(new Error('cancel now'));
  await assert.rejects(pending, /cancel now/);
  assert.equal(calls, 1);
  assert.equal(router.pendingEvaluations.size, 0);
});

test('shared failures never poison a later retry or another key', async () => {
  const { router, requests } = heldRouter();
  const pending = [router.classify(body()), router.classify(body()), router.classify(body('different'))];
  await settle();
  assert.equal(requests.length, 2);
  requests[0].resolve(new Response('PRIVATE_FAILURE', { status: 503 }));
  requests[1].resolve(answer('haiku'));
  const values = await Promise.all(pending);
  assert.deepEqual(values.map(value => value.source), ['fallback', 'fallback', 'jev']);
  assert.equal(values[0].classifier_status, 503);
  const retry = router.classify(body()); await settle(); requests[2].resolve(answer('opus'));
  assert.equal((await retry).tier, 'opus');
  assert.equal(router.evaluationSubscribers, 0);
});

test('full body, requested floor and live evaluator configuration all participate in cache identity', async () => {
  const c = config();
  let calls = 0;
  const router = new Router(c, { fetchImpl: async () => { calls++; return answer('haiku', 0.5); } });
  assert.equal((await router.classify(body())).tier, 'sonnet');
  assert.equal((await router.classify({ ...body(), model: 'claude-opus-5-5' })).tier, 'opus');
  await router.classify({ ...body(), opaque_provider_field: 'new context' });
  c.minConfidence = 0;
  assert.equal((await router.classify(body())).tier, 'haiku');
  for (const [key, value] of [['jevModel', 'other-synthetic-model'], ['jevKey', 'other-synthetic-key'],
    ['jevEndpoint', 'https://example.invalid/v1/systemone'], ['jevTimeoutMs', 900], ['stateChars', 1000]]) {
    c[key] = value; await router.classify(body());
  }
  assert.equal(calls, 9);
  assert.equal((await router.classify(body())).source, 'cache');
});

test('pending evaluations and their subscribers are independently bounded and recover after cancellation', async () => {
  for (const dimension of ['pending', 'subscribers']) {
    const { router, requests } = heldRouter();
    const controllers = Array.from({ length: CLASSIFICATION_LIMITS[dimension] }, () => new AbortController());
    const pending = controllers.map((controller, index) => router.classify(body(dimension === 'pending' ? String(index) : 'same'), controller.signal));
    const outcomes = Promise.allSettled(pending);
    const extra = await router.classify(body('overflow'));
    assert.equal(extra.source, 'fallback');
    assert.equal(extra.classifier_error, 'capacity_exhausted');
    assert.equal(router.pendingEvaluations.size, dimension === 'pending' ? CLASSIFICATION_LIMITS.pending : 1);
    assert.equal(router.evaluationSubscribers, CLASSIFICATION_LIMITS[dimension]);
    controllers.forEach(controller => controller.abort()); await outcomes;
    assert.equal(router.pendingEvaluations.size, 0);
    assert.equal(router.evaluationSubscribers, 0);
    assert.ok(requests.every(request => request.signal.aborted));
    const retry = router.classify(body('overflow')); await settle();
    requests.at(-1).resolve(answer());
    assert.equal((await retry).source, 'jev');
    requests.forEach(request => request.resolve(answer()));
    await settle();
  }
});

test('Jev response bodies are bounded and its deadline continues after response headers arrive', { timeout: 2000 }, async () => {
  for (const mode of ['oversize', 'malformed', 'stalled', 'http_error']) {
    let cancelled = false, calls = 0;
    const router = new Router({ ...config(), jevTimeoutMs: 20 }, { fetchImpl: async () => {
      calls++;
      if (mode === 'malformed') return new Response('PRIVATE malformed JSON');
      return new Response(new ReadableStream({ start(controller) {
        if (mode === 'oversize') controller.enqueue(new Uint8Array(65537));
      }, cancel() { cancelled = true; return new Promise(() => {}); } }), { status: mode === 'http_error' ? 503 : 200 });
    } });
    const keepAlive = setTimeout(() => {}, 1000);
    try {
      for (let attempt = 0; attempt < 2; attempt++) {
        const result = await router.classify(body());
        assert.equal(result.source, 'fallback');
        assert.equal(result.classifier_error, mode === 'stalled' ? 'timeout' : mode === 'http_error' ? 'http_error' : 'invalid_response');
        assert.ok(!JSON.stringify(result).includes('PRIVATE'));
      }
      assert.equal(calls, 2);
      assert.equal(router.pendingEvaluations.size, 0);
      if (mode !== 'malformed') assert.equal(cancelled, true);
    } finally { clearTimeout(keepAlive); }
  }
});

test('Ollama zero-deadline coalescing retains one metadata gate and independent cancellation', { timeout: 1000 }, async () => {
  const c = { ...readConfig({ AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_AUTH_MODE: 'subscription' }), ollamaTimeoutMs: 0 };
  const ready = deferred(), release = deferred(), paths = [];
  let evaluatorSignal;
  const router = new Router(c, { fetchImpl: async (url, options) => {
    paths.push(new URL(url).pathname); evaluatorSignal = options.signal;
    if (url.endsWith('/api/show')) return Response.json({ details: { parameter_size: '9B' } });
    ready.resolve(); await release.promise;
    return Response.json({ model: c.ollamaModel, answers: { tier: { type: 'choice', choice: 'haiku', confidence: 1,
      probabilities: { haiku: 1, sonnet: 0, opus: 0 } } }, usage: { input_tokens: 100, output_tokens: 1 } });
  } });
  const controller = new AbortController();
  const first = router.classify(body(), controller.signal), second = router.classify(body());
  const rejected = assert.rejects(first, /cancelled/);
  await ready.promise; controller.abort(new Error('first cancelled')); await rejected;
  assert.equal(evaluatorSignal.aborted, false);
  release.resolve(); assert.equal((await second).source, 'ollama');
  assert.deepEqual(paths, ['/api/show', '/v1/systemone']);
});
