// Acceptance gates are synthetic and never execute an evaluator or Claude.
let sequence = 0;
const emit = (op, input) => process.stdout.write(`${JSON.stringify({ id: `evaluation-${sequence++}`, op, input })}\n`);
const tiers = ['haiku', 'sonnet', 'opus'];
const values = [undefined, null, '', 'haiku', 'sonnet', 'opus', 'unknown', false, true, 0, 1, -1, 0.5, 2, {}, [], ['haiku']];
for (const value of [...values, '.5', '0.75', '0', '1', '1.', '01', '1e-1', '0x0', 'NaN', 'Infinity', '0.5 ', '1\n', '\t0.5', '9'.repeat(400)]) {
  if (value !== undefined) emit('quality_threshold', value);
}
for (const field of ['profile', 'requiredTiers', 'minAgreement', 'maxUnderRouteRate']) {
  for (const value of values) emit('evaluation_policy', { [field]: value });
}
const rows = () => tiers.map(tier => ({ expected: tier, classified_tier: tier, selected_model: `claude-${tier}-5`, source: 'jev', evaluator: 'jev', reason: 'classified' }));
for (const profile of ['compatible', 'native', 'auto']) {
  for (const classifierOnly of [false, true]) for (const evaluator of ['jev', 'ollama', 'invalid']) {
    for (const field of ['expected', 'classified_tier', 'selected_tier', 'selected_model', 'source', 'evaluator', 'expected_selected_tier', 'expected_reason', 'reason']) {
      for (const value of values) {
        const modified = rows(); modified[0][field] = value;
        emit('routing_report', { rows: modified, options: { evaluator, classifierOnly, policy: { profile } } });
      }
    }
    emit('routing_report', { rows: [], options: { evaluator, classifierOnly, policy: { profile } } });
  }
}
for (const minAgreement of [0, 0.5, 0.75, 1]) for (const maxUnderRouteRate of [0, 0.25, 0.5, 1]) {
  for (const predictions of [['haiku', 'sonnet', 'opus'], ['sonnet', 'sonnet', 'sonnet'], ['opus', 'sonnet', 'haiku']]) {
    emit('routing_report', { rows: rows().map((row, i) => ({ ...row, classified_tier: predictions[i] })), options: { evaluator: 'jev', policy: { minAgreement, maxUnderRouteRate } } });
  }
}
const checks = { claude_success: true, expected_result: true, requests_reached_router: true, upstream_model_evidence: true,
  no_upstream_api_errors: true, no_proxy_errors: true, original_tests_preserved: true, independent_tests_pass: true,
  read_edit_bash_exercised: true, no_permission_denials: true };
const route = { source: 'jev', evaluator: 'jev', classified_tier: 'sonnet', expected_classified_tier: 'sonnet', model: 'claude-sonnet-5', request_class: 'main' };
for (const expectOutage of [false, true]) for (const evaluator of ['jev', 'ollama', 'invalid']) {
  for (const expectedClassifiedTier of values) {
    for (const source of ['jev', 'ollama', 'cache', 'fallback', 'passthrough']) {
      emit('live_case', { checks, routes: [{ ...route, source }, { ...route, request_class: 'auxiliary' }], evaluator, expectOutage, expectedClassifiedTier });
    }
  }
  for (const field of Object.keys(checks)) for (const value of values) emit('live_case', { checks: { ...checks, [field]: value }, routes: [route], evaluator, expectOutage });
  emit('live_case', { checks: {}, routes: [], evaluator, expectOutage });
}
for (const profile of ['compatible', 'native', 'auto']) for (const expectOutage of [false, true]) {
  for (const request_class of values) for (const source of ['jev', 'ollama', 'cache', 'fallback', 'passthrough']) {
    const cases = [{ passed: true, routes: [route, { ...route, model: 'claude-opus-5' }, { ...route, source, request_class, model: 'claude-haiku-4-5' }] }];
    emit('live_report', { cases, options: { policy: { profile }, expectOutage } });
  }
  for (const passed of values) emit('live_report', { cases: [{ passed, routes: [route] }], options: { policy: { profile }, expectOutage } });
  emit('live_report', { cases: [], options: { policy: { profile }, expectOutage } });
}
