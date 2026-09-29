import test from 'node:test';
import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';
import { readConfig } from '../src/config.mjs';
import { Router } from '../src/router.mjs';
import { buildRoutingRequest, readRoutingFixtures, runRoutingTests } from '../scripts/test-ollama-routing.mjs';

const { cases, sha256 } = await readRoutingFixtures();
const byId = id => cases.find(item => item.id === id);
const configFor = model => readConfig({ AUTOROUTER_AUTH_MODE: 'subscription', AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_OLLAMA_MODEL: model });
const MODELS = ['nimble:9b-q4_K_M', 'tev1:0.8b-q8_0', 'tev1:4b-q4_K_M', 'tev1:4b-q8_0'];

function localFixture(config, { answer, resident = [], installed = true } = {}) {
  const calls = [];
  const fixture = { calls, mode: 'normal', fetchImpl: async (url, options) => {
    assert.equal(new URL(url).origin, config.ollamaEndpoint);
    assert.equal(options.redirect, 'error');
    assert.equal(options.headers?.authorization, undefined);
    assert.equal(options.headers?.['x-api-key'], undefined);
    const path = new URL(url).pathname;
    const payload = options.body ? JSON.parse(options.body) : undefined;
    calls.push({ path, payload });
    if (path === '/api/ps') return Response.json({ models: resident.map(name => ({ name })) });
    if (path === '/api/version') return Response.json({ version: '0.35.0' });
    if (path === '/api/tags') return Response.json({ models: installed ? [{ name: config.ollamaModel }] : [] });
    assert.equal(payload.model, config.ollamaModel);
    if (path === '/api/show') return Response.json({ details: { parameter_size: '4B' } });
    assert.equal(path, '/v1/systemone', 'No downloads, unloads, Jev, chat or Anthropic calls are allowed');
    const warm = payload.state.current_task === 'Return the literal word ready.';
    if (!warm && fixture.mode === 'timeout') { await delay(20); options.signal.throwIfAborted(); }
    if (!warm && fixture.mode === 'invalid') return Response.json({ error: 'SYNTHETIC_PRIVATE_RESPONSE' });
    const choice = warm ? 'haiku' : answer ?? cases.find(item => item.prompt === payload.state.current_task)?.expected;
    assert.ok(choice, 'The actual human task must survive Claude-shaped background');
    assert.ok(!JSON.stringify(payload.state).includes('SYNTHETIC_REMINDER_ONLY'));
    assert.ok(!JSON.stringify(payload.state).includes('PRIVATE_THINKING'));
    return Response.json({ model: config.ollamaModel, answers: { tier: {
      type: 'choice', choice, probabilities: { haiku: 0, sonnet: 0, opus: 0, [choice]: 1 }, confidence: 1,
    } }, usage: { input_tokens: 900, output_tokens: 1 } });
  } };
  return fixture;
}

test('Claude-shaped requests preserve native Haiku, Sonnet and Opus decisions for every supported model family', async () => {
  for (const model of MODELS) {
    const config = configFor(model);
    const fixture = localFixture(config);
    for (const item of cases) {
      const router = new Router(config, { fetchImpl: fixture.fetchImpl });
      const result = await router.route(buildRoutingRequest(item, config), { scope: item.id, requestClass: 'main' });
      assert.equal(result.model, config.models[item.expected], `${model}/${item.id}`);
      assert.equal(result.classified_tier, item.expected);
      assert.equal(result.source, 'ollama');
      assert.equal(result.reason, 'classified');
      assert.equal(result.classifier_error, undefined);
    }
    assert.equal(fixture.calls.filter(call => call.path === '/v1/systemone').length, cases.length);
  }
});

test('new human prompts in one session may move from Sonnet to Opus to Haiku without stale turn pinning', async () => {
  const config = configFor(MODELS[1]);
  const fixture = localFixture(config);
  const router = new Router(config, { fetchImpl: fixture.fetchImpl });
  let body;
  for (const [index, id] of ['bounded-feature', 'distributed-fencing', 'array-length'].entries()) {
    const item = byId(id);
    const current = buildRoutingRequest(item, config);
    body = body ? { ...current, messages: [...body.messages, { role: 'assistant', content: 'Synthetic task completed.' }, current.messages.at(-1)] } : current;
    const result = await router.route(body, { scope: 'one-session', promptId: `human-${index}`, requestClass: 'main' });
    assert.equal(result.source, 'ollama');
    assert.equal(result.classified_tier, item.expected);
    assert.equal(result.model, config.models[item.expected]);
    assert.equal(result.reason, 'classified');
  }
  assert.equal(fixture.calls.filter(call => call.path === '/v1/systemone').length, 3);
});

test('legitimate Sonnet, timeout fallback, recovery and a compatibility override remain distinguishable', async () => {
  const config = configFor(MODELS[2]);
  const fixture = localFixture(config);
  const router = new Router({ ...config, ollamaTimeoutMs: 5 }, { fetchImpl: fixture.fetchImpl });
  const sonnet = await router.route(buildRoutingRequest(byId('bounded-feature'), config));
  assert.deepEqual([sonnet.source, sonnet.classified_tier, sonnet.model, sonnet.reason], ['ollama', 'sonnet', config.models.sonnet, 'classified']);

  fixture.mode = 'timeout';
  const body = buildRoutingRequest(byId('array-length'), config);
  const fallback = await router.route(body);
  assert.equal(fallback.model, config.models.sonnet);
  assert.equal(fallback.source, 'fallback');
  assert.equal(fallback.classified_tier, undefined);
  assert.equal(fallback.classifier_error, 'timeout');
  fixture.mode = 'normal';
  const recovered = await router.route(body);
  assert.equal(recovered.model, config.models.haiku);
  assert.equal(recovered.source, 'ollama', 'Timeout fallbacks must not be cached');

  const guarded = await router.route({ ...body, thinking: { type: 'adaptive' } });
  assert.equal(guarded.source, 'ollama');
  assert.equal(guarded.classified_tier, 'haiku');
  assert.equal(guarded.model, config.models.sonnet);
  assert.equal(guarded.reason, 'requires_sonnet_capabilities');
  assert.equal(guarded.classifier_error, undefined);
});

test('signed thinking preserves the earlier model while exposing the fresh classifier choice', async () => {
  const config = configFor(MODELS[0]);
  const fixture = localFixture(config);
  const router = new Router(config, { fetchImpl: fixture.fetchImpl });
  const initial = buildRoutingRequest(byId('bounded-feature'), config);
  await router.route(initial, { scope: 'thinking-session' });
  const simple = buildRoutingRequest(byId('array-length'), config);
  const result = await router.route({ ...simple, messages: [...initial.messages,
    { role: 'assistant', content: [{ type: 'thinking', thinking: 'PRIVATE_THINKING', signature: 'PRIVATE_THINKING_SIGNATURE' }, { type: 'text', text: 'Done.' }] },
    simple.messages.at(-1),
  ] }, { scope: 'thinking-session' });
  assert.deepEqual([result.source, result.classified_tier, result.model, result.reason], ['ollama', 'haiku', config.models.sonnet, 'thinking_history']);
});

test('the opt-in live harness warms once, routes every case, and reports only metadata', async () => {
  const config = configFor(MODELS[3]);
  const fixture = localFixture(config, { resident: [config.ollamaModel] });
  const report = await runRoutingTests(config, { fetchImpl: fixture.fetchImpl, write: () => {}, cases, fixtureHash: sha256 });
  assert.equal(report.passed, true);
  assert.deepEqual(report.missing_tiers, []);
  assert.equal(report.timeout_ms, config.ollamaTimeoutMs);
  assert.equal(report.resident_before, true);
  assert.equal(report.rows.length, cases.length);
  assert.ok(report.rows.every(row => row.source === 'ollama' && row.current_task_matches));
  assert.equal(fixture.calls.filter(call => call.path === '/v1/systemone').length, cases.length + 1);
  for (const item of cases) assert.ok(!JSON.stringify(report).includes(item.prompt));
  assert.ok(!JSON.stringify(report).includes('Synthetic coding assistant guidance'));
});

test('the live harness fails honestly when native answers collapse to Sonnet or become fallbacks', async () => {
  const config = configFor(MODELS[1]);
  const collapsed = localFixture(config, { answer: 'sonnet' });
  const report = await runRoutingTests(config, { fetchImpl: collapsed.fetchImpl, write: () => {}, cases });
  assert.equal(report.passed, false);
  assert.deepEqual(report.missing_tiers, ['haiku', 'opus']);
  assert.equal(report.rows.find(row => row.case === 'bounded-feature').result, 'pass');
  assert.ok(report.rows.filter(row => row.expected !== 'sonnet').every(row => row.result === 'classification_mismatch' && row.source === 'ollama'));

  const invalid = localFixture(config);
  invalid.mode = 'invalid';
  const fallback = await runRoutingTests(config, { fetchImpl: invalid.fetchImpl, write: () => {}, cases: [byId('array-length')] });
  assert.equal(fallback.passed, false);
  assert.equal(fallback.rows[0].result, 'fallback');
  assert.equal(fallback.rows[0].classifier_error, 'invalid_response');
  assert.deepEqual(fallback.missing_tiers, ['haiku', 'sonnet', 'opus']);
  assert.ok(!JSON.stringify(fallback).includes('SYNTHETIC_PRIVATE_RESPONSE'));
});

test('duplicate live cases still perform native evaluations instead of passing from the router cache', async () => {
  const config = configFor(MODELS[1]);
  const fixture = localFixture(config);
  const original = byId('literal');
  const report = await runRoutingTests(config, { fetchImpl: fixture.fetchImpl, write: () => {}, cases: [original, { ...original, id: 'literal-repeat' }] });
  assert.equal(fixture.calls.filter(call => call.path === '/v1/systemone').length, 3);
  assert.ok(report.rows.every(row => row.source === 'ollama'));
  assert.equal(report.passed, false, 'Two Haiku decisions alone do not establish all-tier coverage');
});

test('live preflight never unloads another resident model or downloads a missing selected model', async () => {
  const config = configFor(MODELS[0]);
  const busy = localFixture(config, { resident: ['other-local-model:latest'] });
  await assert.rejects(runRoutingTests(config, { fetchImpl: busy.fetchImpl, write: () => {}, cases }), /Other Ollama models are resident/);
  assert.deepEqual(busy.calls.map(call => call.path), ['/api/ps']);
  const missing = localFixture(config, { installed: false });
  await assert.rejects(runRoutingTests(config, { fetchImpl: missing.fetchImpl, write: () => {}, cases }), error => error.code === 'OLLAMA_MODEL_MISSING');
  assert.deepEqual(missing.calls.map(call => call.path), ['/api/ps', '/api/version', '/api/tags']);
});
