import { createHash } from 'node:crypto';
import { Router } from './router.mjs';
import { buildOllamaState, OLLAMA_QUESTIONS } from './ollama-evaluator.mjs';
import { inspectOllama } from './ollama-setup.mjs';
import { validateOllamaEndpoint, validateOllamaModel } from './ollama-models.mjs';
import { createEvaluationPolicy, evaluateRoutingReport } from './evaluation-report.mjs';

// Packaged synthetic fixtures: no repository, transcript, or account data is
// read. Labels test this rubric on six examples, not general task quality.
export const LOCAL_DIAGNOSTIC_VERSION = 1;
export const LOCAL_DIAGNOSTIC_CASES = Object.freeze([
  { id: 'literal', expected: 'haiku', prompt: 'Print the literal word READY exactly. Do not add any explanation.' },
  { id: 'array-length', expected: 'haiku', prompt: 'What does the JavaScript expression `[].length` evaluate to? Reply with only the integer.' },
  { id: 'bounded-feature', expected: 'sonnet', prompt: 'Add pagination to this REST endpoint using `page` and `pageSize`. Validate the parameters, preserve existing filtering, and add tests for empty results and out-of-range pages.' },
  { id: 'distributed-fencing', expected: 'opus', prompt: "Review this distributed locking design: worker A's lease expires while paused. Worker B acquires a newer fencing token and writes successfully. A resumes and writes using its old token. The database checks only whether a token was ever issued. Explain the failure sequence and design the minimum atomic database check that prevents stale writes, including duplicate retries." },
  { id: 'new-mechanical-task', expected: 'haiku', history: [
    { role: 'user', content: 'Prove the safety of a distributed ledger during failover and concurrent retries.' },
    { role: 'assistant', content: 'The proof and regression tests are complete.' },
  ], prompt: "Separate task: replace the exact text 'recieve' with 'receive' in a label. Make no other changes." },
  { id: 'new-difficult-task', expected: 'opus', history: [
    { role: 'user', content: 'What is the value of [].length?' },
    { role: 'assistant', content: '0' },
  ], prompt: 'New task: diagnose nondeterministic deadlocks between several processes after a rolling deployment. Reconcile conflicting traces, identify the broken ordering invariant, and prove that the repair cannot introduce message loss.' },
].map(item => Object.freeze({ ...item, ...(item.history ? { history: Object.freeze(item.history.map(Object.freeze)) } : {}) })));

const STARTUP_TIMEOUT_MS = 60000;
const METADATA_TIMEOUT_MS = 5000;
const MAX_METADATA_BYTES = 1024 * 1024;
const digest = value => createHash('sha256').update(JSON.stringify(value)).digest('hex');
const rounded = value => Math.round(value * 100) / 100;
const identity = model => {
  const name = model.replace(/^registry\.ollama\.ai\//, '').replace(/^library\//, '');
  return name.slice(name.lastIndexOf('/') + 1).includes(':') ? name : `${name}:latest`;
};

const MESSAGES = Object.freeze({
  evaluator_required: 'Local evaluation requires AUTOROUTER_EVALUATOR=ollama; the diagnostic does not change your configuration.',
  invalid_configuration: 'Check the local Ollama endpoint, model, and runtime deadline configuration.',
  positive_keep_alive_required: 'The local diagnostic requires a positive keep-alive, such as AUTOROUTER_OLLAMA_KEEP_ALIVE=5m. Normal routing still supports 0.',
  model_missing: 'The configured local model is not installed. Install it explicitly before rerunning this diagnostic.',
  unrelated_models_resident: 'Other Ollama models are resident. Retry when only the selected model, or no model, is loaded; this diagnostic leaves them running.',
  residency_unavailable: 'Could not safely inspect local Ollama model residency. No further evaluation was attempted.',
  OLLAMA_VERSION: 'Local evaluation requires Ollama 0.35 or newer with /v1/systemone.',
  OLLAMA_CLOUD: 'The selected Ollama model uses a remote service. Select an installed local model.',
  OLLAMA_TIMEOUT: 'Local Ollama metadata inspection timed out.',
  OLLAMA_HTTP: 'Local Ollama rejected metadata inspection. Check its version and selected model.',
  OLLAMA_RESPONSE: 'Local Ollama returned invalid or oversized metadata.',
  OLLAMA_UNAVAILABLE: 'Could not reach local Ollama. Start the existing service and retry.',
  startup_failed: 'The initial synthetic evaluation failed; measured cases were not run.',
});
const failure = code => Object.assign(new Error(MESSAGES[code]), { code });

function requestBody(item) {
  return { model: 'claude-haiku-4-5-20251001', max_tokens: 32000, tools: [],
    system: [{ type: 'text', text: 'Synthetic coding assistant guidance: inspect relevant code and verify changes. '.repeat(110) }],
    messages: [...structuredClone(item.history ?? []), { role: 'user', content: [
      { type: 'text', text: '<system-reminder>SYNTHETIC_REMINDER_ONLY: unrelated environment metadata.</system-reminder>' },
      { type: 'text', text: item.prompt },
    ] }],
  };
}

async function residency(config, fetchImpl, signal) {
  const timeout = AbortSignal.timeout(METADATA_TIMEOUT_MS);
  const combined = signal ? AbortSignal.any([signal, timeout]) : timeout;
  const response = await fetchImpl(`${config.ollamaEndpoint}/api/ps`, { redirect: 'error', signal: combined });
  if (!response.ok || response.redirected || !response.body) {
    await response.body?.cancel();
    throw failure('residency_unavailable');
  }
  const reader = response.body.getReader();
  const chunks = [];
  let size = 0;
  const abort = () => { void reader.cancel(combined.reason).catch(() => {}); };
  combined.addEventListener('abort', abort, { once: true });
  try {
    for (;;) {
      combined.throwIfAborted();
      const { value, done } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > MAX_METADATA_BYTES) throw failure('residency_unavailable');
      chunks.push(value);
    }
    combined.throwIfAborted();
    const payload = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    if (!Array.isArray(payload?.models) || payload.models.some(item => typeof (item?.name ?? item?.model) !== 'string')) {
      throw failure('residency_unavailable');
    }
    const models = payload.models.map(item => identity(item.name ?? item.model));
    if (models.some(model => model !== identity(config.ollamaModel))) throw failure('unrelated_models_resident');
    return models.length ? 'resident' : 'not_resident';
  } catch (error) {
    signal?.throwIfAborted();
    throw error?.code === 'unrelated_models_resident' ? error : failure('residency_unavailable');
  } finally {
    combined.removeEventListener('abort', abort);
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

function finish(report) {
  const evaluation = evaluateRoutingReport(report.rows, { evaluator: 'ollama', classifierOnly: true,
    policy: createEvaluationPolicy({ profile: 'compatible' }) });
  report.gates = { ...evaluation.gates,
    preflight: { passed: report.preflight_passed },
    startup: { passed: report.startup?.source === 'ollama' },
    cases: { passed: report.rows.length === LOCAL_DIAGNOSTIC_CASES.length, expected: LOCAL_DIAGNOSTIC_CASES.length, completed: report.rows.length },
  };
  report.passed = !report.error && Object.values(report.gates).every(gate => gate.passed !== false);
  return report;
}

// No warmup helper with pull/unload controls is reachable from this diagnostic.
// Caller cancellation is preserved, including when runtime timeout is zero.
export async function runLocalDiagnostic(config, { fetchImpl = fetch, signal, onProgress = () => {} } = {}) {
  const report = { schema_version: 1, type: 'local_evaluator_diagnostic', fixture_version: LOCAL_DIAGNOSTIC_VERSION,
    fixture_sha256: digest(LOCAL_DIAGNOSTIC_CASES), questions_sha256: digest(OLLAMA_QUESTIONS),
    evaluator: 'ollama', startup_timeout_ms: STARTUP_TIMEOUT_MS, preflight_passed: false,
    residency_before: 'unknown', startup: null, rows: [], paid_provider_calls: 0, downloads: 0,
    models_unloaded: 0, configuration_changed: false };
  const progress = event => { try { Promise.resolve(onProgress(event)).catch(() => {}); } catch {} };
  try {
    signal?.throwIfAborted();
    if (config?.evaluator !== 'ollama') throw failure('evaluator_required');
    try {
      validateOllamaEndpoint(config.ollamaEndpoint);
      validateOllamaModel(config.ollamaModel);
      if (!Number.isSafeInteger(config.ollamaTimeoutMs) || config.ollamaTimeoutMs < 0 || config.ollamaTimeoutMs > 30000) throw new Error();
    } catch { throw failure('invalid_configuration'); }
    if (typeof config.ollamaKeepAlive !== 'string' || !/^[1-9]\d{0,3}[smh]$/.test(config.ollamaKeepAlive)) throw failure('positive_keep_alive_required');
    report.evaluator_model = config.ollamaModel;
    report.runtime_timeout_ms = config.ollamaTimeoutMs;
    report.keep_alive = config.ollamaKeepAlive;
    progress({ event: 'preflight' });
    const inspection = await inspectOllama(config, { fetchImpl, signal });
    if (!inspection.installed) throw failure('model_missing');
    report.residency_before = await residency(config, fetchImpl, signal);
    report.preflight_passed = true;
    progress({ event: 'startup', residency_before: report.residency_before, timeout_ms: STARTUP_TIMEOUT_MS });
    const initialStart = performance.now();
    const startup = await new Router({ ...config, ollamaTimeoutMs: STARTUP_TIMEOUT_MS }, { fetchImpl })
      .classify(requestBody({ prompt: 'Return the literal word ready.' }), signal);
    report.startup = { latency_ms: rounded(performance.now() - initialStart), source: startup.source,
      classified_tier: startup.classified_tier, classifier_error: startup.classifier_error, classifier_status: startup.classifier_status,
      residency_before: report.residency_before, timeout_ms: STARTUP_TIMEOUT_MS };
    progress({ event: 'startup_complete', ...report.startup });
    if (startup.source !== 'ollama') throw failure('startup_failed');
    for (const item of LOCAL_DIAGNOSTIC_CASES) {
      signal?.throwIfAborted();
      const resident = await residency(config, fetchImpl, signal);
      progress({ event: 'case_start', case: item.id, residency_before: resident });
      const body = requestBody(item);
      const state = buildOllamaState(body, config.ollamaStateChars);
      // A new classifier prevents cache hits from appearing as local inference.
      const start = performance.now();
      const decision = await new Router(config, { fetchImpl }).classify(body, signal);
      const row = { case: item.id, expected: item.expected, classified_tier: decision.classified_tier,
        tier: decision.tier, source: decision.source, evaluator: decision.evaluator, reason: decision.reason,
        classifier_error: decision.classifier_error, classifier_status: decision.classifier_status,
        latency_ms: rounded(performance.now() - start), residency_before: resident,
        state_bytes: Buffer.byteLength(JSON.stringify(state)), current_task_matches: state.current_task === item.prompt };
      report.rows.push(row);
      progress({ event: 'case_complete', ...row });
    }
  } catch (error) {
    signal?.throwIfAborted();
    const code = Object.hasOwn(MESSAGES, error?.code) ? error.code : 'OLLAMA_UNAVAILABLE';
    report.error = { code, message: MESSAGES[code] };
  }
  const result = finish(report);
  // Correct labels alone cannot conceal broken task extraction.
  if (report.rows.some(row => !row.current_task_matches)) {
    result.gates.cases.passed = false;
    result.passed = false;
  }
  return result;
}

export function formatLocalDiagnostic(report) {
  const cause = row => row.classifier_error ? ` (${row.classifier_error}${row.classifier_status ? ` ${row.classifier_status}` : ''})` : '';
  const lines = [`Local evaluator diagnostic: ${report.evaluator_model ?? 'not configured'}`];
  if (report.runtime_timeout_ms !== undefined) lines.push(`Runtime deadline: ${report.runtime_timeout_ms === 0 ? 'disabled' : `${report.runtime_timeout_ms} ms`}; initial preparation bound: ${report.startup_timeout_ms} ms.`);
  if (report.startup) lines.push(`Initial synthetic call: ${report.startup.latency_ms} ms; model ${report.startup.residency_before === 'resident' ? 'resident' : 'not resident'} before call; ${report.startup.source}${cause(report.startup)}.`);
  for (const row of report.rows) lines.push(`${row.case}: expected ${row.expected}, got ${row.classified_tier ?? 'no verdict'}; ${row.source}${cause(row)}; ${row.latency_ms} ms; ${row.residency_before} before call.`);
  if (report.error) lines.push(report.error.message);
  lines.push(`${report.passed ? 'PASS' : 'FAIL'}: ${report.rows.filter(row => row.source === 'ollama' && row.classified_tier === row.expected).length}/${LOCAL_DIAGNOSTIC_CASES.length} synthetic cases; missing tiers: ${report.gates.coverage.missing.join(', ') || 'none'}.`);
  lines.push('Residency is observed, not a guarantee of cold or warm caches. This checks six classifier labels, not Claude task completion or general accuracy.');
  return lines;
}
