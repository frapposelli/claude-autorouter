#!/usr/bin/env node
// Opt-in local inference only. Importing this module never contacts a service.
import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { readConfig, TIERS } from '../src/config.mjs';
import { Router } from '../src/router.mjs';
import { buildOllamaState, OLLAMA_QUESTIONS } from '../src/ollama-evaluator.mjs';
import { validateOllamaEndpoint, validateOllamaModel } from '../src/ollama-models.mjs';
import { setupOllama } from '../src/ollama-setup.mjs';

const FIXTURES = new URL('../test/fixtures/ollama-integration.json', import.meta.url);
const digest = value => createHash('sha256').update(value).digest('hex');
const normalizedModel = model => {
  const value = model.replace(/^registry\.ollama\.ai\//, '').replace(/^library\//, '');
  return value.slice(value.lastIndexOf('/') + 1).includes(':') ? value : `${value}:latest`;
};
const failure = message => Object.assign(new Error(message), { code: 'ROUTING_TEST_ERROR' });

export async function readRoutingFixtures() {
  const text = await readFile(FIXTURES, 'utf8');
  const cases = JSON.parse(text);
  if (!Array.isArray(cases) || !cases.length || new Set(cases.map(item => item.id)).size !== cases.length
    || cases.some(item => !/^[a-z0-9-]+$/.test(item.id) || !TIERS.includes(item.expected) || typeof item.prompt !== 'string')) {
    throw failure('Invalid checked-in routing fixtures.');
  }
  return { cases, sha256: digest(text) };
}

export function buildRoutingRequest(item, config) {
  // Synthetic background resembles the captured Claude request shape. None of
  // this text comes from Anthropic instructions, a repository, or a transcript.
  const background = 'Synthetic coding assistant guidance: explain changes clearly, inspect relevant code, and verify the requested behavior. ';
  return {
    model: config.models.haiku, max_tokens: 32000, stream: false,
    thinking: { type: 'disabled' }, tools: [],
    system: [{ type: 'text', text: background.repeat(110) }],
    messages: [...structuredClone(item.history ?? []), { role: 'user', content: [
      { type: 'text', text: '<system-reminder>SYNTHETIC_REMINDER_ONLY: environment and session metadata. This background is unrelated to the current task.</system-reminder>' },
      { type: 'text', text: '<available-deferred-tools>SYNTHETIC_REMINDER_ONLY: no tools are enabled.</available-deferred-tools>' },
      { type: 'text', text: item.prompt },
    ] }],
  };
}

async function residentModels(config, fetchImpl, signal) {
  const timeout = AbortSignal.timeout(5000);
  const requestSignal = signal ? AbortSignal.any([signal, timeout]) : timeout;
  const response = await fetchImpl(`${config.ollamaEndpoint}/api/ps`, { redirect: 'error', signal: requestSignal });
  if (!response.ok || response.redirected || !response.body) {
    await response.body?.cancel();
    throw failure('Cannot inspect resident models on local Ollama.');
  }
  const reader = response.body.getReader();
  const abort = () => { void reader.cancel().catch(() => {}); };
  requestSignal.addEventListener('abort', abort, { once: true });
  let length = 0;
  const chunks = [];
  try {
    for (;;) {
      requestSignal.throwIfAborted();
      const { done, value } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (length > 1024 * 1024) throw failure('Ollama returned an oversized resident model list.');
      chunks.push(value);
    }
    requestSignal.throwIfAborted();
    let payload;
    try { payload = JSON.parse(Buffer.concat(chunks).toString('utf8')); }
    catch { throw failure('Ollama returned an invalid resident model list.'); }
    if (!Array.isArray(payload?.models) || payload.models.some(item => typeof (item?.name ?? item?.model) !== 'string')) {
      throw failure('Ollama returned an invalid resident model list.');
    }
    return payload.models.map(item => item.name ?? item.model);
  } finally {
    requestSignal.removeEventListener('abort', abort);
    await reader.cancel().catch(() => {}); reader.releaseLock();
  }
}

export async function runRoutingTests(config, { fetchImpl = fetch, signal, write = console.log, cases, fixtureHash } = {}) {
  if (config.evaluator !== 'ollama') throw failure('This test requires the local Ollama evaluator.');
  validateOllamaEndpoint(config.ollamaEndpoint);
  validateOllamaModel(config.ollamaModel);
  if (!cases) ({ cases, sha256: fixtureHash } = await readRoutingFixtures());
  const resident = await residentModels(config, fetchImpl, signal);
  if (resident.some(model => normalizedModel(model) !== normalizedModel(config.ollamaModel))) {
    throw failure('Other Ollama models are resident. Rerun when only the selected model or no model is loaded; this test leaves existing models running.');
  }
  const started = performance.now();
  // This checks version, installation and local metadata before warming. An
  // absent model is an error; setup is never allowed to download it here.
  await setupOllama(config, { pull: false, warm: true, fetchImpl, signal, write });
  const report = {
    type: 'ollama_router_integration', timestamp: new Date().toISOString(),
    evaluator_model: config.ollamaModel, timeout_ms: config.ollamaTimeoutMs,
    protocol: '/v1/systemone', fixture_sha256: fixtureHash,
    questions_sha256: digest(JSON.stringify(OLLAMA_QUESTIONS)),
    resident_before: resident.length > 0, warmup_ms: Math.round(performance.now() - started),
    paid_provider_calls: 0, downloads: 0, model_unloaded_by_test: false,
    keep_alive: config.ollamaKeepAlive, rows: [],
  };
  for (const item of cases) {
    signal?.throwIfAborted();
    const body = buildRoutingRequest(item, config);
    const state = buildOllamaState(body, config.ollamaStateChars);
    // A fresh router prevents a repeated case from succeeding from its cache.
    const router = new Router(config, { fetchImpl });
    const result = await router.route(body, { scope: `synthetic-${item.id}`, requestClass: 'main', signal });
    const category = result.source === 'fallback' ? 'fallback'
      : result.source !== 'ollama' ? 'unexpected_source'
      : result.classified_tier !== item.expected ? 'classification_mismatch'
      : result.model !== config.models[item.expected] || result.reason !== 'classified' ? 'guard_override' : 'pass';
    const row = {
      case: item.id, expected: item.expected, classified_tier: result.classified_tier,
      selected_model: result.model, source: result.source, reason: result.reason,
      classifier_error: result.classifier_error, classifier_status: result.classifier_status,
      latency_ms: result.latency_ms, state_bytes: Buffer.byteLength(JSON.stringify(state)),
      current_task_matches: state.current_task === item.prompt, result: category,
    };
    if (!row.current_task_matches) row.result = 'task_extraction_mismatch';
    report.rows.push(row);
    write(`${row.case}: ${row.result}; evaluator=${row.classified_tier ?? 'none'}, selected=${row.selected_model}, source=${row.source}, reason=${row.reason}${row.classifier_error ? `, error=${row.classifier_error}` : ''}, ${row.latency_ms}ms`);
  }
  const covered = new Set(report.rows.filter(row => row.result === 'pass').map(row => row.classified_tier));
  report.missing_tiers = TIERS.filter(tier => !covered.has(tier));
  report.passed = report.rows.every(row => row.result === 'pass') && report.missing_tiers.length === 0;
  return report;
}

function parseArgs(args) {
  const options = {};
  for (let index = 0; index < args.length; index++) {
    const flag = args[index];
    if (flag === '--help') { options.help = true; continue; }
    if (!['--model', '--timeout-ms', '--output'].includes(flag) || !args[index + 1] || args[index + 1].startsWith('--')) {
      throw failure('Unknown or incomplete option; use --help.');
    }
    options[flag.slice(2)] = args[++index];
  }
  if (!options.help && !options.model) throw failure('--model is required; select one installed local model explicitly.');
  return options;
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help) {
    console.log('Usage: node scripts/test-ollama-routing.mjs --model TAG [--timeout-ms N] [--output artifacts/path.json]\nRuns synthetic Claude-shaped requests through the actual router and local native evaluator. Requires Ollama 0.35+ and an already installed local model. Uses the production model deadline unless configured or overridden. Warms once, tests all three tiers and new human turns, and fails on mismatches, fallback or missing tier coverage. Sends no requests to Anthropic or Jev, downloads nothing, and changes no user configuration. Refuses other resident models and never explicitly unloads the selected model; its configured keep-alive controls residency.');
    return;
  }
  const env = { AUTOROUTER_AUTH_MODE: 'subscription', AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_OLLAMA_MODEL: validateOllamaModel(options.model) };
  for (const key of ['AUTOROUTER_OLLAMA_URL', 'AUTOROUTER_OLLAMA_TIMEOUT_MS', 'AUTOROUTER_OLLAMA_KEEP_ALIVE']) {
    if (process.env[key] !== undefined) env[key] = process.env[key];
  }
  if (options['timeout-ms'] !== undefined) env.AUTOROUTER_OLLAMA_TIMEOUT_MS = options['timeout-ms'];
  const config = readConfig(env);
  const controller = new AbortController();
  const cancel = () => controller.abort(new Error('Routing test cancelled.'));
  for (const name of ['SIGINT', 'SIGTERM']) process.once(name, cancel);
  try {
    const report = await runRoutingTests(config, { signal: controller.signal });
    if (options.output) {
      const path = resolve(options.output);
      await mkdir(dirname(path), { recursive: true });
      await writeFile(path, `${JSON.stringify(report, null, 2)}\n`, { mode: 0o600 });
      console.log(`Saved ${path}`);
    }
    console.log(`${report.passed ? 'PASS' : 'FAIL'}: ${report.rows.filter(row => row.result === 'pass').length}/${report.rows.length} cases; missing tiers: ${report.missing_tiers.join(', ') || 'none'}.`);
    if (!report.passed) process.exitCode = 1;
  } finally {
    for (const name of ['SIGINT', 'SIGTERM']) process.removeListener(name, cancel);
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => {
    const safe = error.code === 'ROUTING_TEST_ERROR' || error.code?.startsWith('OLLAMA_')
      ? error.message : 'Local routing test could not complete. Check the options, local Ollama, and output path.';
    console.error(`Ollama routing test failed: ${safe}`); process.exitCode = 1;
  });
}
