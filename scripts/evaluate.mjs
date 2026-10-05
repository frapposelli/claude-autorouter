import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { readConfig, TIERS } from '../src/config.mjs';
import { Router } from '../src/router.mjs';
import { createEvaluationPolicy, evaluateRoutingReport, modelTier, profileTier, parseQualityThreshold } from '../src/evaluation-report.mjs';

export async function runEvaluation({ config, cases, policy = createEvaluationPolicy({ profile: config.clientProfile }),
  routerFactory = () => new Router(config) } = {}) {
  policy = createEvaluationPolicy(policy);
  if (policy.profile !== config.clientProfile) throw new Error('Evaluation policy must match the configured client profile');
  if (!Array.isArray(cases) || cases.some(item => !item || !TIERS.includes(item.expected)
    || (typeof item.prompt !== 'string' && !item.request))) throw new Error('Invalid evaluation fixtures');
  const rows = [];
  for (const item of cases) {
    const body = item.request ?? { model: config.models.sonnet, max_tokens: 4096, messages: [{ role: 'user', content: item.prompt }] };
    const router = routerFactory(); // Fresh classifier/turn cache for each case.
    const result = await router.route(body, { requestClass: 'main', scope: `evaluation-${item.name}` });
    const requestedTier = TIERS.find(tier => config.models[tier] === body.model) ?? modelTier(body.model);
    const confidenceTier = config.evaluator === 'jev' && result.confidence < config.minConfidence
      ? TIERS[Math.max(1, TIERS.indexOf(requestedTier), TIERS.indexOf(result.classified_tier))] : result.classified_tier;
    rows.push({ case: item.name, expected: item.expected, classified_tier: result.classified_tier ?? null,
      selected_model: result.model, selected_tier: TIERS.find(tier => config.models[tier] === result.model) ?? modelTier(result.model),
      confirmed_model: null, expected_selected_tier: item.expected_selected_tier ?? profileTier(confidenceTier, policy.profile),
      ...(item.expected_reason ? { expected_reason: item.expected_reason } : {}),
      source: result.source, evaluator: result.evaluator, reason: result.reason, confidence: result.confidence ?? null, ms: result.latency_ms });
  }
  const acceptance = evaluateRoutingReport(rows, { evaluator: config.evaluator, policy });
  const times = rows.map(row => row.ms).filter(Number.isFinite).sort((a, b) => a - b);
  const percentile = p => times.length ? times[Math.ceil(p * times.length) - 1] : null;
  return { ...acceptance, evaluator: config.evaluator, rows, rubric_agreement: acceptance.gates.rubric.agreement,
    fallback_count: acceptance.gates.evaluator.fallbacks, routing_p50_ms: percentile(0.5), routing_p95_ms: percentile(0.95) };
}

export function parseEvaluationArgs(args) {
  const options = {};
  for (let index = 0; index < args.length; index++) {
    const key = args[index];
    if (key === '--help') { options.help = true; continue; }
    if (!['--min-agreement', '--max-under-route-rate', '--profile'].includes(key) || !args[index + 1] || args[index + 1].startsWith('--')) {
      throw new Error('Unknown or incomplete evaluation option; use --help');
    }
    const value = args[++index];
    if (key === '--profile') options.profile = value;
    else options[key === '--min-agreement' ? 'minAgreement' : 'maxUnderRouteRate'] = parseQualityThreshold(value);
  }
  createEvaluationPolicy(options); // Reject thresholds before any provider calls.
  return options;
}

async function main() {
  const options = parseEvaluationArgs(process.argv.slice(2));
  if (options.help) {
    console.log('Usage: node scripts/evaluate.mjs [--profile compatible|native|auto] [--min-agreement 1] [--max-under-route-rate 0]\nExplicit evaluator calls only: Jev is paid; Ollama is local. Defaults require exact label agreement and no under-routing. Compatible coverage requires all three selected tiers; Auto requires Sonnet and Opus. Set thresholds before running. This measures synthetic rubric agreement and routing policy, not Claude task quality or net savings.');
    return;
  }
  const config = readConfig(options.profile ? { ...process.env, AUTOROUTER_CLIENT_PROFILE: options.profile } : process.env);
  const policy = createEvaluationPolicy({ ...options, profile: config.clientProfile });
  if (config.evaluator === 'jev' && !config.jevKey) throw new Error('Set TYPESAFE_API_KEY to run the Jev evaluation');
  const fixtureText = await readFile(new URL('../test/fixtures/routing.json', import.meta.url), 'utf8');
  const report = await runEvaluation({ config, cases: JSON.parse(fixtureText), policy });
  console.table(report.rows);
  console.log(JSON.stringify({ type: 'routing_evaluation', fixture_sha256: createHash('sha256').update(fixtureText).digest('hex'), ...report }, null, 2));
  console.log('Labels are subjective rubric judgments. Transport and Claude task completion are not measured.');
  if (!report.passed) process.exitCode = 1;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(() => { console.error('Evaluation failed. Check configuration, fixtures, and --help.'); process.exitCode = 1; });
}
