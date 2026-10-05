import test from 'node:test';
import assert from 'node:assert/strict';
import { LIVE_CASES, liveExpectedTier } from '../scripts/live-validation.mjs';
import { evaluateLiveCase, evaluateLiveReport } from '../src/evaluation-report.mjs';

test('all bundled live tasks declare rubric labels before provider invocation, including easy thinking follow-ups', () => {
  for (const [name, scenario] of Object.entries(LIVE_CASES)) {
    for (const [index, prompt] of scenario.prompts.entries()) {
      const body = { messages: scenario.prompts.slice(0, index).flatMap(content => [
        { role: 'user', content }, { role: 'assistant', content: 'Synthetic completed reply.' },
      ]).concat({ role: 'user', content: prompt }) };
      assert.ok(['haiku', 'sonnet', 'opus'].includes(liveExpectedTier(scenario, body)), name);
    }
  }
  assert.equal(liveExpectedTier(LIVE_CASES.thinking_continuation, { messages: [
    { role: 'user', content: LIVE_CASES.thinking_continuation.prompts[0] },
    { role: 'assistant', content: 'Synthetic reasoning.' },
    { role: 'user', content: LIVE_CASES.thinking_continuation.prompts[1] },
  ] }), 'haiku');
});

test('a live tier permutation cannot pass merely because aggregate tier coverage is complete', () => {
  const cases = ['haiku', 'sonnet', 'opus'].map((expected, index, tiers) => {
    const actual = tiers[(index + 1) % tiers.length];
    const routes = [{ request_class: 'main', source: 'jev', evaluator: 'jev', classified_tier: actual,
      expected_classified_tier: expected, model: `claude-${actual}-5` }];
    const accepted = evaluateLiveCase({ checks: { claude_success: true, expected_result: true }, routes, evaluator: 'jev' });
    assert.equal(accepted.gates.rubric.passed, false);
    return { ...accepted, routes };
  });
  const report = evaluateLiveReport(cases);
  assert.equal(report.gates.coverage.passed, true);
  assert.equal(report.passed, false);
  assert.equal(evaluateLiveCase({ checks: { claude_success: true, expected_result: true }, evaluator: 'jev',
    routes: [{ source: 'jev', classified_tier: 'haiku' }] }).gates.rubric.passed, false);
});
