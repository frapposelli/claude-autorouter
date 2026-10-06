import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createEvaluationPolicy, evaluateRoutingReport, evaluateLiveCase, evaluateLiveReport } from '../src/evaluation-report.mjs';
import { readConfig } from '../src/config.mjs';
import { Router } from '../src/router.mjs';
import { parseEvaluationArgs, runEvaluation } from '../scripts/evaluate.mjs';
import { parseOllamaEvaluationArgs } from '../scripts/evaluate-ollama.mjs';

const TIERS = ['haiku', 'sonnet', 'opus'];
const MODELS = { haiku: 'claude-haiku-4-5-20251001', sonnet: 'claude-sonnet-5', opus: 'claude-opus-5-5' };
const rows = () => TIERS.map(tier => ({ expected: tier, classified_tier: tier, selected_model: MODELS[tier],
  source: 'jev', evaluator: 'jev', reason: 'classified' }));
const checks = () => ({ claude_success: true, expected_result: true, requests_reached_router: true,
  upstream_model_evidence: true, no_upstream_api_errors: true, no_proxy_errors: true, no_permission_denials: true });
const liveRoute = (extra = {}) => ({ source: 'jev', evaluator: 'jev', classified_tier: 'sonnet', model: MODELS.sonnet, request_class: 'main', ...extra });

test('routing gates separate rubric, policy and tier coverage from unmeasured transport/task quality', () => {
  const report = evaluateRoutingReport(rows(), { evaluator: 'jev' });
  assert.equal(report.passed, true);
  for (const name of ['evaluator', 'rubric', 'policy', 'coverage']) assert.equal(report.gates[name].passed, true);
  assert.equal(report.gates.transport.passed, null);
  assert.equal(report.gates.task.passed, null);
  assert.equal(evaluateRoutingReport([], { evaluator: 'jev' }).passed, false);
});

test('all-fallback and cached-only results cannot masquerade as successful fresh evaluation', () => {
  for (const source of ['fallback', 'cache', 'passthrough']) {
    const report = evaluateRoutingReport(rows().map(row => ({ ...row, source })), { evaluator: 'jev' });
    assert.equal(report.gates.evaluator.passed, false, source);
    assert.equal(report.passed, false);
  }
  const mixed = rows();
  mixed[1].source = 'fallback';
  assert.equal(evaluateRoutingReport(mixed, { evaluator: 'jev' }).gates.evaluator.passed, false);
});

test('missing tier coverage fails even when permissive rubric thresholds permit every prediction', () => {
  const collapsed = rows().map(row => ({ ...row, classified_tier: 'sonnet', selected_model: MODELS.sonnet }));
  const report = evaluateRoutingReport(collapsed, { evaluator: 'jev', policy: createEvaluationPolicy({ minAgreement: 0, maxUnderRouteRate: 1 }) });
  assert.equal(report.gates.evaluator.passed, true);
  assert.equal(report.gates.rubric.passed, true);
  assert.equal(report.gates.coverage.passed, false);
  assert.deepEqual(report.gates.coverage.missing, ['haiku', 'opus']);
  assert.equal(report.passed, false);
});

test('Auto policy permits a Haiku classifier verdict only with a Sonnet floor and requires Sonnet/Opus coverage', () => {
  const policy = createEvaluationPolicy({ profile: 'auto' });
  const correct = rows();
  correct[0].selected_model = MODELS.sonnet;
  const report = evaluateRoutingReport(correct, { evaluator: 'jev', policy });
  assert.equal(report.passed, true);
  assert.deepEqual(report.gates.coverage.required, ['sonnet', 'opus']);
  assert.equal(report.gates.rubric.agreement, 1);
  assert.equal(evaluateRoutingReport(rows(), { evaluator: 'jev', policy }).gates.policy.passed, false);
});

test('a declared compatibility override is checked independently of the classifier rubric', () => {
  const policy = createEvaluationPolicy({ requiredTiers: ['sonnet'] });
  const constrained = [{ ...rows()[0], selected_model: MODELS.sonnet, expected_selected_tier: 'sonnet',
    reason: 'requires_sonnet_capabilities', expected_reason: 'requires_sonnet_capabilities' }];
  assert.equal(evaluateRoutingReport(constrained, { evaluator: 'jev', policy }).passed, true);
  constrained[0].reason = 'classified';
  const wrongReason = evaluateRoutingReport(constrained, { evaluator: 'jev', policy });
  assert.equal(wrongReason.gates.rubric.passed, true);
  assert.equal(wrongReason.gates.policy.passed, false);
});

test('quality thresholds apply to predictions before selection floors and include every case in agreement', () => {
  const incorrect = [...rows(), { ...rows()[2], classified_tier: 'sonnet', selected_model: MODELS.sonnet }];
  const strict = evaluateRoutingReport(incorrect, { evaluator: 'jev' });
  assert.equal(strict.gates.rubric.agreement, 0.75);
  assert.equal(strict.gates.rubric.under_route_rate, 0.25);
  assert.equal(strict.passed, false);
  const allowed = createEvaluationPolicy({ minAgreement: 0.75, maxUnderRouteRate: 0.25 });
  assert.equal(evaluateRoutingReport(incorrect, { evaluator: 'jev', policy: allowed }).passed, true);
  assert.equal(evaluateRoutingReport(incorrect, { evaluator: 'jev', policy: createEvaluationPolicy({ minAgreement: 0.75 }) }).passed, false);
});

test('classifier-only reports do not claim that a Claude model was selected or a task completed', () => {
  const classified = rows().map(row => ({ ...row, selected_model: null, confirmed_model: null }));
  const report = evaluateRoutingReport(classified, { evaluator: 'jev', classifierOnly: true });
  assert.equal(report.passed, true);
  assert.equal(report.gates.policy.passed, null);
  assert.equal(report.gates.task.passed, null);
});

test('normal live transport/task success cannot pass on fallback, passthrough, or another evaluator', () => {
  for (const source of ['fallback', 'passthrough', 'ollama']) {
    const report = evaluateLiveCase({ checks: checks(), routes: [liveRoute({ source })], evaluator: 'jev' });
    assert.equal(report.gates.transport.passed, true);
    assert.equal(report.gates.task.passed, true);
    assert.equal(report.gates.evaluator.passed, false, source);
    assert.equal(report.passed, false);
  }
  assert.equal(evaluateLiveCase({ checks: {}, routes: [], evaluator: 'jev' }).passed, false);
});

test('explicit live outages require fallback while allowing auxiliary safety pass-through', () => {
  const routes = [liveRoute({ source: 'fallback', classified_tier: undefined }), liveRoute({ source: 'passthrough', request_class: 'auxiliary' })];
  const report = evaluateLiveCase({ checks: checks(), routes, evaluator: 'jev', expectOutage: true, expectedClassifiedTier: 'haiku' });
  assert.equal(report.passed, true);
  assert.equal(report.gates.rubric.passed, null);
  assert.equal(evaluateLiveCase({ checks: checks(), routes: [liveRoute()], evaluator: 'jev', expectOutage: true }).passed, false);
  const failedTask = evaluateLiveCase({ checks: { ...checks(), expected_result: false }, routes, evaluator: 'jev', expectOutage: true });
  assert.equal(failedTask.gates.evaluator.passed, true);
  assert.equal(failedTask.gates.task.passed, false);
  assert.equal(failedTask.passed, false);
});

test('live rubric failures stay distinct from transport and independently verified task failures', () => {
  const report = evaluateLiveCase({ checks: checks(), routes: [liveRoute()], evaluator: 'jev', expectedClassifiedTier: 'opus' });
  assert.equal(report.gates.rubric.passed, false);
  assert.equal(report.gates.transport.passed, true);
  assert.equal(report.gates.task.passed, true);
  const failed = evaluateLiveCase({ checks: { ...checks(), independent_tests_pass: false }, routes: [liveRoute()], evaluator: 'jev' });
  assert.equal(failed.gates.evaluator.passed, true);
  assert.equal(failed.gates.task.passed, false);
});

test('live profile coverage uses main routes only and explicitly scoped subsets do not claim full coverage', () => {
  const cases = [{ passed: true, routes: [liveRoute(), liveRoute({ model: MODELS.opus }), liveRoute({ model: MODELS.haiku, request_class: 'auxiliary' })] }];
  const compatible = evaluateLiveReport(cases);
  assert.equal(compatible.passed, false);
  assert.deepEqual(compatible.gates.coverage.missing, ['haiku']);
  assert.equal(evaluateLiveReport(cases, { policy: createEvaluationPolicy({ profile: 'auto' }) }).passed, true);
  assert.equal(evaluateLiveReport(cases, { policy: createEvaluationPolicy({ requiredTiers: ['sonnet'] }) }).passed, true);
  assert.equal(evaluateLiveReport(cases, { expectOutage: true }).gates.coverage.passed, null);
  assert.equal(evaluateLiveReport([]).passed, false);
});

test('invalid gate options are rejected before any evaluations and policy objects cannot mutate mid-run', async () => {
  for (const options of [{ minAgreement: NaN }, { minAgreement: -1 }, { minAgreement: 1.1 }, { maxUnderRouteRate: Infinity },
    { maxUnderRouteRate: '0' }, { requiredTiers: ['haiku', 'haiku'] }, { profile: 'auto', requiredTiers: ['haiku'] }, { profile: 'other' }]) {
    assert.throws(() => createEvaluationPolicy(options));
  }
  for (const parse of [parseEvaluationArgs, parseOllamaEvaluationArgs]) {
    for (const flag of ['--min-agreement', '--max-under-route-rate']) {
      for (const value of ['NaN', '2', ' ', '\t', '0x0', '0.5 ', '1e-1']) assert.throws(() => parse([flag, value]));
    }
    assert.equal(parse(['--min-agreement', '0.9', '--max-under-route-rate', '0.1']).minAgreement, 0.9);
  }
  const policy = createEvaluationPolicy();
  assert.throws(() => policy.requiredTiers.push('other'));
  assert.throws(() => { policy.minAgreement = 0; });
  let calls = 0;
  const config = readConfig({ AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: 'synthetic-only' });
  await assert.rejects(runEvaluation({ config, cases: [], policy: { minAgreement: 2 }, routerFactory: () => { calls++; } }));
  assert.equal(calls, 0);
});

test('hardware benchmark accepts a disabled warm deadline with a bounded cold load', () => {
  const options = parseOllamaEvaluationArgs(['--timeout-ms', '0', '--cold-timeout-ms', '60000']);
  assert.equal(options.timeoutMs, 0);
  assert.equal(options.coldTimeoutMs, 60000);
  for (const flag of ['--rounds', '--stress-rounds', '--timeout-ms', '--cold-timeout-ms']) {
    for (const value of [' ', '1e3', '0x10', '1.0', '-1']) assert.throws(() => parseOllamaEvaluationArgs([flag, value]));
  }
  assert.throws(() => parseOllamaEvaluationArgs(['--cold-timeout-ms', '0']));
});

test('the existing synthetic corpus characterizes actual compatible and Auto policy with mocked evaluator answers', async () => {
  const cases = JSON.parse(await readFile(new URL('./fixtures/routing.json', import.meta.url), 'utf8'));
  for (const profile of ['compatible', 'auto']) {
    const config = readConfig({ AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: 'synthetic-only', AUTOROUTER_CLIENT_PROFILE: profile });
    let calls = 0;
    const routerFactory = () => new Router(config, { fetchImpl: async () => {
      return Response.json({ answers: { tier: { choice: cases[calls++].expected, confidence: 0.99 } } });
    } });
    const report = await runEvaluation({ config, cases, routerFactory });
    assert.equal(report.passed, true, JSON.stringify(report.gates));
    assert.equal(calls, cases.length);
    assert.ok(report.rows.every(row => row.confirmed_model === null));
    if (profile === 'auto') assert.ok(report.rows.filter(row => row.classified_tier === 'haiku').every(row => row.selected_tier === 'sonnet'));
  }
});

test('actual router characterization verifies declared thinking override without changing its request', async () => {
  const config = readConfig({ AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: 'synthetic-only' });
  const request = { model: config.models.sonnet, max_tokens: 4096, thinking: { type: 'adaptive' }, messages: [{ role: 'user', content: 'Fix a typo.' }] };
  const before = structuredClone(request);
  const report = await runEvaluation({ config, cases: [{ name: 'thinking-capability', expected: 'haiku',
    expected_selected_tier: 'sonnet', expected_reason: 'requires_sonnet_capabilities', request }],
  policy: createEvaluationPolicy({ requiredTiers: ['sonnet'] }), routerFactory: () => new Router(config, {
    fetchImpl: async () => Response.json({ answers: { tier: { choice: 'haiku', confidence: 0.99 } } }),
  }) });
  assert.equal(report.passed, true);
  assert.deepEqual(request, before);
});

test('the executable evaluator harness rejects broken real-router runs instead of returning a green connectivity report', async () => {
  const config = readConfig({ AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: 'synthetic-only' });
  const cases = TIERS.map(expected => ({ name: expected, expected, prompt: `Synthetic ${expected} fixture` }));
  for (const mode of ['outage', 'collapsed']) {
    const report = await runEvaluation({ config, cases, routerFactory: () => new Router(config, { fetchImpl: async () => {
      if (mode === 'outage') throw new TypeError('Synthetic evaluator unavailable');
      return Response.json({ answers: { tier: { choice: 'sonnet', confidence: 0.99 } } });
    } }) });
    assert.equal(report.passed, false);
    assert.equal(report.gates.coverage.passed, false);
    assert.equal(report.gates.evaluator.passed, mode !== 'outage');
    assert.equal(report.gates.rubric.passed, false);
  }
});
