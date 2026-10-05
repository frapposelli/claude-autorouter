import test from 'node:test';
import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';
import { readConfig } from '../src/config.mjs';
import { runLocalDiagnostic, formatLocalDiagnostic, LOCAL_DIAGNOSTIC_CASES } from '../src/local-diagnostic.mjs';

const configFor = (extra = {}) => readConfig({ AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_OLLAMA_MODEL: 'tev1:4b-q4_K_M', ...extra });
const tiers = ['haiku', 'sonnet', 'opus'];

function fixture(config, { initiallyResident = false, others = [], installed = true, version = '0.35.0', remote = false,
  choice, respond, residentResponse } = {}) {
  let resident = initiallyResident;
  let decisions = 0;
  const calls = [];
  const fetchImpl = async (url, options = {}) => {
    assert.equal(new URL(url).origin, config.ollamaEndpoint);
    assert.equal(options.redirect, 'error');
    assert.ok(!Object.keys(options.headers ?? {}).some(key => ['authorization', 'x-api-key'].includes(key.toLowerCase())));
    const path = new URL(url).pathname;
    const body = options.body ? JSON.parse(options.body) : undefined;
    calls.push({ path, body, signal: options.signal });
    if (path === '/api/version') return Response.json({ version });
    if (path === '/api/tags') return Response.json({ models: installed ? [{ name: config.ollamaModel }] : [] });
    if (path === '/api/show') return Response.json({ details: { parameter_size: '4B' }, ...(remote ? { remote_host: 'https://PRIVATE_HOST' } : {}) });
    if (path === '/api/ps') return residentResponse?.(calls) ?? Response.json({ models: [
      ...(resident ? [{ name: config.ollamaModel }] : []), ...others.map(name => ({ name })),
    ] });
    assert.equal(path, '/v1/systemone', 'No cloud, chat, downloads, unloads, or service control');
    assert.equal(body.model, config.ollamaModel);
    assert.equal(body.keep_alive, config.ollamaKeepAlive);
    assert.notEqual(body.keep_alive, '0');
    assert.ok(!JSON.stringify(body).includes('PRIVATE_KEY'));
    assert.ok(!JSON.stringify(body.state).includes('SYNTHETIC_REMINDER_ONLY'));
    assert.ok(!JSON.stringify(body.state).includes('Synthetic coding assistant guidance'));
    const index = decisions++;
    resident = true;
    const custom = await respond?.({ index, options, body });
    if (custom) return custom;
    const selected = index === 0 ? 'haiku' : choice ?? LOCAL_DIAGNOSTIC_CASES.find(item => item.prompt === body.state.current_task)?.expected;
    assert.ok(tiers.includes(selected), 'Each synthetic task survived the actual state extraction');
    return Response.json({ model: config.ollamaModel, answers: { tier: { type: 'choice', choice: selected, confidence: 1,
      probabilities: Object.fromEntries(tiers.map(tier => [tier, tier === selected ? 1 : 0])) } }, usage: { input_tokens: 900, output_tokens: 1 } });
  };
  return { calls, fetchImpl };
}

test('diagnostic uses packaged synthetic all-tier cases without cloud credentials and keeps observations separate from claims', async () => {
  const config = { ...configFor(), jevKey: 'PRIVATE_KEY_JEV', anthropicKey: 'PRIVATE_KEY_ANTHROPIC' };
  const local = fixture(config);
  const progress = [];
  const report = await runLocalDiagnostic(config, { fetchImpl: local.fetchImpl, onProgress: row => progress.push(row) });
  assert.equal(report.passed, true);
  assert.equal(report.fixture_version, 1);
  assert.match(report.fixture_sha256, /^[a-f0-9]{64}$/);
  assert.match(report.questions_sha256, /^[a-f0-9]{64}$/);
  assert.equal(report.residency_before, 'not_resident');
  assert.equal(report.startup.residency_before, 'not_resident');
  assert.equal(report.startup.timeout_ms, 60000);
  assert.equal(report.runtime_timeout_ms, 15000);
  assert.equal(report.gates.transport.passed, null);
  assert.equal(report.gates.policy.passed, null);
  assert.equal(report.gates.task.passed, null);
  assert.deepEqual(report.gates.coverage.missing, []);
  assert.deepEqual(report.rows.map(row => [row.case, row.classified_tier]), LOCAL_DIAGNOSTIC_CASES.map(item => [item.id, item.expected]));
  assert.ok(report.rows.every(row => row.residency_before === 'resident' && row.source === 'ollama' && row.current_task_matches && row.state_bytes <= 3000));
  assert.equal(local.calls.filter(call => call.path === '/v1/systemone').length, LOCAL_DIAGNOSTIC_CASES.length + 1);
  assert.equal(local.calls.filter(call => call.path === '/api/ps').length, LOCAL_DIAGNOSTIC_CASES.length + 1);
  assert.equal(progress.filter(row => row.event === 'case_complete').length, LOCAL_DIAGNOSTIC_CASES.length);
  assert.equal(report.paid_provider_calls, 0);
  assert.equal(report.models_unloaded, 0);
  assert.equal(report.downloads, 0);
  assert.equal(report.configuration_changed, false);
  assert.ok(!JSON.stringify(report).includes('PRIVATE_'));
  assert.ok(!JSON.stringify(report).includes(LOCAL_DIAGNOSTIC_CASES[0].prompt));
  assert.match(formatLocalDiagnostic(report).join('\n'), /PASS: 6\/6.*\nResidency is observed/);
});

test('an already resident model is reported honestly and Auto profile still checks all classifier tiers', async () => {
  const config = configFor({ AUTOROUTER_CLIENT_PROFILE: 'auto' });
  const report = await runLocalDiagnostic(config, { fetchImpl: fixture(config, { initiallyResident: true }).fetchImpl });
  assert.equal(report.passed, true);
  assert.equal(report.startup.residency_before, 'resident');
  assert.deepEqual(report.gates.coverage.required, tiers);
  assert.match(formatLocalDiagnostic(report)[2], /model resident before call/);
  assert.ok(!('confirmed_model' in report.rows[0]), 'No Claude model executed');
});

test('unrelated resident models are never evicted to manufacture a cold or warm measurement', async () => {
  const config = configFor();
  for (const initiallyResident of [false, true]) {
    const local = fixture(config, { initiallyResident, others: ['PRIVATE_OTHER_MODEL:latest'] });
    const report = await runLocalDiagnostic(config, { fetchImpl: local.fetchImpl });
    assert.equal(report.passed, false);
    assert.equal(report.error.code, 'unrelated_models_resident');
    assert.equal(local.calls.filter(call => call.path === '/v1/systemone').length, 0);
    assert.ok(!JSON.stringify(report).includes('PRIVATE_OTHER_MODEL'));
  }
});

test('a newly resident unrelated model stops the diagnostic before another inference', async () => {
  const config = configFor();
  let inspections = 0;
  const local = fixture(config, { residentResponse: () => Response.json({ models: ++inspections < 3
    ? [] : [{ name: 'unrelated:latest' }] }) });
  const report = await runLocalDiagnostic(config, { fetchImpl: local.fetchImpl });
  assert.equal(report.passed, false);
  assert.equal(report.error.code, 'unrelated_models_resident');
  assert.equal(report.rows.length, 1);
  assert.equal(local.calls.filter(call => call.path === '/v1/systemone').length, 2);
  assert.equal(report.gates.cases.passed, false);
});

test('missing, cloud, and unsupported local installations fail before sending synthetic tasks', async () => {
  const config = configFor();
  for (const [options, code] of [
    [{ installed: false }, 'model_missing'], [{ remote: true }, 'OLLAMA_CLOUD'], [{ version: '0.34.9' }, 'OLLAMA_VERSION'],
  ]) {
    const local = fixture(config, options);
    const report = await runLocalDiagnostic(config, { fetchImpl: local.fetchImpl });
    assert.equal(report.passed, false);
    assert.equal(report.error.code, code);
    assert.equal(local.calls.filter(call => call.path === '/v1/systemone').length, 0);
    assert.ok(!JSON.stringify(report).includes('PRIVATE_'));
  }
});

test('invalid settings, other backends and keep-alive zero are rejected without requests', async () => {
  const config = configFor();
  for (const [changes, code] of [
    [{ evaluator: 'jev' }, 'evaluator_required'],
    [{ ollamaEndpoint: 'https://PRIVATE_HOST/v1' }, 'invalid_configuration'],
    [{ ollamaModel: 'nimble:cloud' }, 'invalid_configuration'],
    [{ ollamaTimeoutMs: NaN }, 'invalid_configuration'],
    [{ ollamaKeepAlive: '0' }, 'positive_keep_alive_required'],
  ]) {
    const report = await runLocalDiagnostic({ ...config, ...changes }, { fetchImpl: () => assert.fail('Invalid configuration must not make requests') });
    assert.equal(report.passed, false);
    assert.equal(report.error.code, code);
    assert.ok(!JSON.stringify(report).includes('PRIVATE_'));
  }
});

test('production timeout zero is preserved while initial preparation keeps its separate bound', async () => {
  const config = configFor({ AUTOROUTER_OLLAMA_TIMEOUT_MS: '0' });
  const controller = new AbortController();
  const local = fixture(config, { respond: async ({ index, options }) => {
    if (index > 0) { await delay(6); options.signal.throwIfAborted(); }
  } });
  const report = await runLocalDiagnostic(config, { fetchImpl: local.fetchImpl, signal: controller.signal });
  assert.equal(report.passed, true);
  assert.equal(report.runtime_timeout_ms, 0);
  const inference = local.calls.filter(call => call.path === '/v1/systemone');
  assert.notEqual(inference[0].signal, controller.signal, 'Preparation has its own 60s budget');
  assert.ok(inference.slice(1).every(call => !call.signal.aborted));
  assert.ok(report.rows.every(row => row.source === 'ollama' && row.latency_ms >= 4),
    'Unlimited measured calls complete beyond the neighboring finite-deadline fixture');
  assert.match(formatLocalDiagnostic(report)[1], /Runtime deadline: disabled/);
});

test('runtime timeout fallback fails the gate and cannot pass as a Sonnet classification', async () => {
  const config = configFor({ AUTOROUTER_OLLAMA_TIMEOUT_MS: '2' });
  const local = fixture(config, { respond: async ({ index, options }) => {
    if (index > 0) { await delay(6); options.signal.throwIfAborted(); }
  } });
  const report = await runLocalDiagnostic(config, { fetchImpl: local.fetchImpl });
  assert.equal(report.startup.source, 'ollama');
  assert.equal(report.passed, false);
  assert.equal(report.gates.evaluator.fallbacks, 6);
  assert.ok(report.rows.every(row => row.source === 'fallback' && row.classifier_error === 'timeout' && row.classified_tier === undefined && row.tier === 'sonnet'));
  assert.deepEqual(report.gates.coverage.missing, tiers);
});

test('wrong labels fail agreement and coverage even when every local call succeeds', async () => {
  const config = configFor();
  const report = await runLocalDiagnostic(config, { fetchImpl: fixture(config, { choice: 'sonnet' }).fetchImpl });
  assert.equal(report.passed, false);
  assert.equal(report.gates.evaluator.passed, true);
  assert.equal(report.gates.rubric.passed, false);
  assert.equal(report.gates.coverage.passed, false);
  assert.deepEqual(report.gates.coverage.missing, ['haiku', 'opus']);
});

test('failed preparation returns safe error metadata and skips measured cases', async () => {
  const config = configFor();
  const local = fixture(config, { respond: () => new Response('PRIVATE_PROVIDER_ERROR', { status: 503 }) });
  const report = await runLocalDiagnostic(config, { fetchImpl: local.fetchImpl });
  assert.equal(report.passed, false);
  assert.equal(report.error.code, 'startup_failed');
  assert.equal(report.startup.classifier_error, 'http_error');
  assert.equal(report.startup.classifier_status, 503);
  assert.equal(report.rows.length, 0);
  assert.equal(local.calls.filter(call => call.path === '/v1/systemone').length, 1);
  assert.ok(!JSON.stringify(report).includes('PRIVATE_PROVIDER_ERROR'));
  assert.match(formatLocalDiagnostic(report).join('\n'), /http_error 503/);
});

test('invalid and oversized residency metadata is bounded and prevents evaluation', async () => {
  const config = configFor();
  for (const residentResponse of [
    () => Response.json({ models: [null] }),
    () => new Response('PRIVATE_BAD_JSON'),
    () => new Response('x'.repeat(1024 * 1024 + 1)),
    () => new Response('PRIVATE_ERROR', { status: 500 }),
  ]) {
    const local = fixture(config, { residentResponse });
    const report = await runLocalDiagnostic(config, { fetchImpl: local.fetchImpl });
    assert.equal(report.passed, false);
    assert.equal(report.error.code, 'residency_unavailable');
    assert.equal(local.calls.filter(call => call.path === '/v1/systemone').length, 0);
    assert.ok(!JSON.stringify(report).includes('PRIVATE_'));
  }
});

test('caller cancellation propagates before I/O and during timeout-disabled runtime evaluation', async () => {
  const config = configFor({ AUTOROUTER_OLLAMA_TIMEOUT_MS: '0' });
  const before = new AbortController();
  before.abort(new Error('cancelled-before'));
  await assert.rejects(runLocalDiagnostic(config, { signal: before.signal, fetchImpl: () => assert.fail('already cancelled') }), /cancelled-before/);
  const controller = new AbortController();
  const local = fixture(config, { respond: async ({ index, options }) => {
    if (index > 0) {
      controller.abort(new Error('cancelled-during'));
      options.signal.throwIfAborted();
    }
  } });
  await assert.rejects(runLocalDiagnostic(config, { fetchImpl: local.fetchImpl, signal: controller.signal }), /cancelled-during/);
  assert.equal(local.calls.filter(call => call.path === '/v1/systemone').length, 2);
});

test('cancelling a stalled residency read releases its reader and sends no task text', async () => {
  const config = configFor();
  const controller = new AbortController();
  let cancelled = false;
  const local = fixture(config, { residentResponse: () => new Response(new ReadableStream({
    start() { queueMicrotask(() => controller.abort(new Error('cancelled-residency'))); },
    cancel() { cancelled = true; },
  })) });
  await assert.rejects(runLocalDiagnostic(config, { fetchImpl: local.fetchImpl, signal: controller.signal }), /cancelled-residency/);
  assert.equal(cancelled, true);
  assert.equal(local.calls.filter(call => call.path === '/v1/systemone').length, 0);
});

test('progress callback throws or rejected promises cannot prevent diagnostic completion', async () => {
  const config = configFor();
  for (const onProgress of [() => { throw new Error('unavailable'); }, async () => { throw new Error('unavailable'); }]) {
    const report = await runLocalDiagnostic(config, { fetchImpl: fixture(config).fetchImpl, onProgress });
    assert.equal(report.passed, true);
  }
});
