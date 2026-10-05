// Pure acceptance rules shared by the opt-in evaluation harnesses. Thresholds
// are validated before any provider calls; passing transport is not evidence
// of evaluator availability, routing correctness, or completed task quality.
const TIERS = ['haiku', 'sonnet', 'opus'];
const PROFILES = ['compatible', 'native', 'auto'];
const gate = (passed, details = {}) => ({ passed, ...details });
const allPassed = gates => Object.values(gates).every(value => value.passed !== false);

export function modelTier(model) {
  return typeof model === 'string' ? /^claude-(haiku|sonnet|opus)(?:-|$)/.exec(model)?.[1] : undefined;
}

export function profileTier(tier, profile) {
  return profile === 'auto' && tier === 'haiku' ? 'sonnet' : tier;
}

export function parseQualityThreshold(value) {
  if (typeof value !== 'string' || !/^(?:\d+(?:\.\d+)?|\.\d+)$/.test(value)) throw new Error('Quality thresholds must be decimal numbers between 0 and 1');
  const number = Number(value);
  if (!Number.isFinite(number) || number < 0 || number > 1) throw new Error('Quality thresholds must be between 0 and 1');
  return number;
}

export function createEvaluationPolicy({ profile = 'compatible', requiredTiers,
  minAgreement = 1, maxUnderRouteRate = 0 } = {}) {
  if (!PROFILES.includes(profile)) throw new Error('Evaluation profile must be compatible, native, or auto');
  for (const [name, value] of [['minAgreement', minAgreement], ['maxUnderRouteRate', maxUnderRouteRate]]) {
    if (typeof value !== 'number' || !Number.isFinite(value) || value < 0 || value > 1) throw new Error(`${name} must be between 0 and 1`);
  }
  const tiers = requiredTiers ?? (profile === 'native' ? [] : profile === 'auto' ? ['sonnet', 'opus'] : TIERS);
  if (!Array.isArray(tiers) || tiers.some(tier => !TIERS.includes(tier)) || new Set(tiers).size !== tiers.length
    || (profile === 'auto' && tiers.includes('haiku'))) throw new Error('Invalid required tier coverage for evaluation profile');
  return Object.freeze({ profile, minAgreement, maxUnderRouteRate, requiredTiers: Object.freeze([...tiers]) });
}

function evaluatorGate(rows, evaluator, expectOutage) {
  if (!['jev', 'ollama'].includes(evaluator)) throw new Error('Unknown evaluation backend');
  const active = rows.filter(row => row.source !== 'passthrough');
  const fresh = active.filter(row => row.source === evaluator).length;
  const fallbacks = active.filter(row => row.source === 'fallback').length;
  const passed = active.length > 0 && (expectOutage ? fallbacks === active.length
    : fresh > 0 && active.every(row => row.source === evaluator || (row.source === 'cache' && row.evaluator === evaluator)));
  return gate(passed, { expected: expectOutage ? 'fallback' : evaluator, evaluated_requests: active.length,
    successful_evaluations: fresh, fallbacks });
}

export function tierCoverage(tiers, policy) {
  const observed = [...new Set(tiers.filter(tier => TIERS.includes(tier)))].sort();
  const missing = policy.requiredTiers.filter(tier => !observed.includes(tier));
  return gate(missing.length === 0, { required: [...policy.requiredTiers], observed, missing });
}

export function evaluateRoutingReport(rows, { evaluator, policy = createEvaluationPolicy(), classifierOnly = false } = {}) {
  policy = createEvaluationPolicy(policy);
  if (!Array.isArray(rows) || rows.some(row => !row || !TIERS.includes(row.expected))) throw new Error('Invalid evaluation rows');
  const valid = rows.filter(row => TIERS.includes(row.classified_tier)
    && (row.source === evaluator || (row.source === 'cache' && row.evaluator === evaluator)));
  const agreement = rows.length ? valid.filter(row => row.classified_tier === row.expected).length / rows.length : 0;
  const underRoutes = valid.filter(row => TIERS.indexOf(row.classified_tier) < TIERS.indexOf(row.expected)).length;
  const underRouteRate = rows.length ? underRoutes / rows.length : 0;
  const eligibleTiers = valid.map(row => classifierOnly ? row.classified_tier : row.selected_tier ?? modelTier(row.selected_model));
  const gates = {
    transport: gate(null, { reason: 'Claude transport not exercised' }),
    evaluator: evaluatorGate(rows, evaluator, false),
    rubric: gate(rows.length > 0 && valid.length === rows.length && agreement >= policy.minAgreement
      && underRouteRate <= policy.maxUnderRouteRate, { agreement, under_routes: underRoutes, under_route_rate: underRouteRate,
      min_agreement: policy.minAgreement, max_under_route_rate: policy.maxUnderRouteRate }),
    policy: gate(classifierOnly ? null : rows.length > 0 && rows.every(row => {
      const selected = row.selected_tier ?? modelTier(row.selected_model);
      return TIERS.includes(selected) && (policy.profile !== 'auto' || selected !== 'haiku')
        && (row.expected_selected_tier === undefined || selected === row.expected_selected_tier)
        && (row.expected_reason === undefined || row.reason === row.expected_reason);
    }), classifierOnly ? { reason: 'Classifier-only benchmark; routing policy not exercised' } : {}),
    coverage: tierCoverage(eligibleTiers, policy),
    task: gate(null, { reason: 'No Claude task completion measured' }),
  };
  return { schema_version: 1, policy, gates, passed: allPassed(gates), requests: rows.length };
}

const TRANSPORT_CHECKS = new Set(['claude_success', 'requests_reached_router', 'upstream_model_evidence', 'no_upstream_api_errors', 'no_proxy_errors']);
const TASK_CHECKS = new Set(['expected_result', 'original_tests_preserved', 'independent_tests_pass', 'read_edit_bash_exercised']);
function checkGate(checks, names) {
  const selected = Object.fromEntries(Object.entries(checks).filter(([name]) => names.has(name)));
  return gate(Object.keys(selected).length ? Object.values(selected).every(value => value === true) : null, { checks: selected });
}

export function evaluateLiveCase({ checks, routes, evaluator, expectOutage = false, expectedClassifiedTier } = {}) {
  const policyChecks = new Set(Object.keys(checks).filter(name => !TRANSPORT_CHECKS.has(name) && !TASK_CHECKS.has(name)));
  const main = routes.filter(row => ['main', 'unspecified', undefined, ''].includes(row.request_class));
  const labels = main.map(row => expectedClassifiedTier ?? row.expected_classified_tier);
  const rubric = expectOutage ? gate(null, { reason: 'Explicit evaluator outage' })
    : gate(main.length > 0 && labels.every(tier => TIERS.includes(tier))
      && main.every((row, index) => row.classified_tier === labels[index]),
    { expected: expectedClassifiedTier ?? labels, ...(labels.some(tier => !TIERS.includes(tier)) ? { reason: 'Missing declared live classifier label' } : {}) });
  const gates = {
    transport: checkGate(checks, TRANSPORT_CHECKS),
    evaluator: evaluatorGate(routes, evaluator, expectOutage),
    rubric,
    policy: checkGate(checks, policyChecks),
    task: checkGate(checks, TASK_CHECKS),
  };
  // Empty or incomplete harness evidence can never be a successful live case.
  if (!routes.length || gates.transport.passed === null || gates.task.passed === null) gates.transport.passed = false;
  return { gates, passed: allPassed(gates) };
}

export function evaluateLiveReport(cases, { policy = createEvaluationPolicy(), expectOutage = false } = {}) {
  policy = createEvaluationPolicy(policy);
  const tiers = cases.flatMap(item => (item.routes ?? []).filter(row =>
    ['main', 'unspecified', undefined, ''].includes(row.request_class)
      && ['jev', 'ollama', 'cache'].includes(row.source)).map(row => modelTier(row.model)));
  const coverage = expectOutage ? gate(null, { reason: 'Explicit evaluator outage' }) : tierCoverage(tiers, policy);
  return { policy, gates: { coverage }, passed: cases.length > 0 && cases.every(item => item.passed === true) && coverage.passed !== false };
}
