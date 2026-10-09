#!/usr/bin/env node
// Development-only oracle. Never reads saved configuration or inherited keys.
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { readFile, writeFile, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const repository = dirname(dirname(fileURLToPath(import.meta.url)));
const MAX_INPUT_BYTES = 64 * 1024 * 1024;
const MAX_LINE_BYTES = 8 * 1024 * 1024;
const identifier = value => typeof value === 'string' && /^[A-Za-z0-9_.:-]{1,160}$/.test(value);
const FIXTURE_NOW = '2026-10-09T12:00:00.000Z';
const timestamped = entry => {
  if (!entry || typeof entry !== 'object' || Array.isArray(entry)) return entry;
  const value = entry.timestamp;
  const valid = typeof value === 'string' && /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/.test(value) && Number.isFinite(Date.parse(value));
  return valid ? entry : { ...entry, timestamp: FIXTURE_NOW };
};
const utf16Values = value => {
  if (typeof value === 'string') return { utf16: Array.from({ length: value.length }, (_, index) => value.charCodeAt(index)) };
  if (Array.isArray(value)) return value.map(utf16Values);
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, utf16Values(item)]));
  return value;
};

export async function verifyBaseline(root = repository) {
  const manifest = JSON.parse(await readFile(resolve(repository, 'rust/parity/baseline.json'), 'utf8'));
  for (const entry of manifest.files) {
    const bytes = await readFile(resolve(root, entry.path));
    if (bytes.length !== entry.bytes || createHash('sha256').update(bytes).digest('hex') !== entry.sha256) {
      throw new Error(`Frozen baseline mismatch: ${entry.path}`);
    }
  }
  return { baseline_commit: manifest.baseline_commit, files_verified: manifest.files.length };
}

export async function createReference(root = repository) {
  await verifyBaseline(root);
  const load = name => import(pathToFileURL(resolve(root, 'src', `${name}.mjs`)).href);
  const [catalog, preparation, compatibility, validation, configuration, turns, observation, redaction, telemetry, savings, prompts, status, statusline, routing, ollama, evaluation, history, authentication, policy] = await Promise.all([
    load('model-catalog'), load('model-request'), load('auto-routing'), load('request-validation'), load('config'), load('turn-state'), load('response-observer'), load('redaction'), load('telemetry-event'), load('savings'), load('prompt-state'), load('status-state'), load('statusline'), load('router'), load('ollama-evaluator'), load('evaluation-report'), load('session-history'), load('auth'), load('policy'),
  ]);
  return async fixture => {
    if (!identifier(fixture?.id) || !identifier(fixture?.op) || !Object.hasOwn(fixture, 'input')) {
      throw new Error('Invalid fixture envelope');
    }
    const { id, op, input } = fixture;
    const original = process.cwd();
    try {
      let result;
      switch (op) {
        case 'guard_contract_json': {
          const body = JSON.parse(Buffer.from(input.bytes).toString('utf8'));
          const before = JSON.stringify(body);
          const guarded = input.guard === 'safeguards' ? compatibility.hasRoutableSafeguards(body)
            : input.guard === 'auto' ? compatibility.canRouteAutoRequest(body, input.target)
              : input.guard === 'target' ? compatibility.targetCompatibility(body, input.target, { autoMode: input.auto_mode ?? false })
                : (() => { throw Error('Invalid guard contract fixture'); })();
          result = { result: guarded, before, after: JSON.stringify(body) }; break;
        }
        case 'prepare_request_json': {
          const prepared = preparation.prepareRequest(JSON.parse(Buffer.from(input.bytes).toString('utf8')), input.target);
          result = { serialized: [...Buffer.from(JSON.stringify(prepared.request))], adjustments: prepared.adjustments }; break;
        }
        case 'target_compatibility_json': result = compatibility.targetCompatibility(JSON.parse(Buffer.from(input.bytes).toString('utf8')), input.target, { autoMode: input.auto_mode ?? false }); break;
        case 'can_route_auto_json': result = compatibility.canRouteAutoRequest(JSON.parse(Buffer.from(input.bytes).toString('utf8')), input.target); break;
        case 'safeguards_json': result = compatibility.hasRoutableSafeguards(JSON.parse(Buffer.from(input.bytes).toString('utf8'))); break;
        case 'history_session': {
          const directory = await mkdtemp(resolve(tmpdir(), 'autorouter-parity-history-'));
          const compare = String.prototype.localeCompare;
          String.prototype.localeCompare = function(other) { return compare.call(this, other, input.locale ?? 'en-US'); };
          try {
            await writeFile(resolve(directory, 'autorouter-session-fixture.jsonl'), Buffer.from(input.bytes), { mode: 0o600 });
            result = await history.readSessionHistory(directory, { id: 'autorouter-session-fixture', limits: input.limits ?? {} });
            if (input.order) result.selected_order = Object.keys(result.summary.selected_models);
            if (input.text && !Object.keys(input.limits ?? {}).length) {
              const lines = [];
              await history.sessionsCommand(['show', 'autorouter-session-fixture'], { env: { AUTOROUTER_CONFIG: resolve(directory, 'missing-config.json'), AUTOROUTER_SESSION_LOG_DIR: directory }, write: line => lines.push(line) });
              result.text = lines;
            }
          } finally { String.prototype.localeCompare = compare; await rm(directory, { recursive: true, force: true }); }
          break;
        }
        case 'evaluation_policy': result = evaluation.createEvaluationPolicy(input); break;
        case 'quality_threshold': result = evaluation.parseQualityThreshold(input); break;
        case 'routing_report': result = evaluation.evaluateRoutingReport(input.rows, input.options ?? {}); break;
        case 'live_case': result = evaluation.evaluateLiveCase(input); break;
        case 'live_report': result = evaluation.evaluateLiveReport(input.cases, input.options ?? {}); break;
        case 'build_state': result = prompts.buildState(input.body, input.limit ?? 12000); break;
        case 'build_state_json': result = { serialized: [...Buffer.from(JSON.stringify(prompts.buildState(JSON.parse(Buffer.from(input.bytes).toString('utf8')), input.limit ?? 12000)))] }; break;
        case 'build_ollama_state_json': result = { serialized: [...Buffer.from(JSON.stringify(ollama.buildOllamaState(JSON.parse(Buffer.from(input.bytes).toString('utf8')), input.limit ?? 3000)))] }; break;
        case 'context_size': {
          const body = JSON.parse(Buffer.from(input.bytes).toString('utf8'));
          result = routing.contextSizeBytes(body, input.model ?? body.model); break;
        }
        case 'router': {
          let now = 0;
          const config = configuration.readConfig(input.env ?? {});
          const router = new routing.Router({ ...config, ...(input.turn_entries === undefined ? {} : { turnEntries: input.turn_entries }) }, { now: () => now });
          result = [];
          for (const step of input.steps) {
            if (step.op === 'advance') now += step.ms;
            else if (step.op === 'complete') result.push(router.complete(step.request_id, step.evidence));
            else if (step.op === 'route') {
              const body = step.bytes ? JSON.parse(Buffer.from(step.bytes).toString('utf8')) : step.body;
              router.classify = async () => step.decision ?? { tier: 'haiku', source: 'jev', reason: 'classified' };
              const count_models = [];
              const countTokens = Object.hasOwn(step, 'count') ? async (_, model) => { count_models.push(model); return step.count; } : undefined;
              const decision = await router.route(body, { ...step.options, countTokens });
              delete decision.latency_ms; delete decision.evaluation_latency_ms;
              result.push({ decision, count_models });
            } else throw new Error('Invalid router fixture operation');
          }
          break;
        }
        case 'prompt_excerpt': result = prompts.promptExcerpt(input.body, Object.hasOwn(input, 'max_chars') ? input.max_chars : 500); break;
        case 'prompt_excerpt_json': result = prompts.promptExcerpt(JSON.parse(Buffer.from(input.bytes).toString('utf8')), Object.hasOwn(input, 'max_chars') ? input.max_chars : 500); break;
        case 'goal_feedback_indexes': result = [...prompts.goalFeedbackIndexes(input)]; break;
        case 'render_statusline': result = statusline.renderStatusLine(input.input, input.snapshot, { now: 100000, color: false, ...input.options }); break;
        case 'status_state': {
          let now = input.now ?? 100000;
          let saved;
          const originalNow = Date.now;
          Date.now = () => now;
          const fileSystem = { mkdtemp: async () => '/synthetic/status', chmod: async () => {}, rename: async () => {}, rm: async () => {},
            open: async () => ({ chmod: async () => {}, close: async () => {}, writeFile: async value => { saved = value; } }) };
          const store = status.createStatusState({ fileSystem, ...(Object.hasOwn(input, 'baseline_model') ? { baselineModel: input.baseline_model } : {}) });
          try {
            await store.ready;
            result = [];
            for (const step of input.steps) {
              if (step.op === 'now') now = step.value;
              else if (step.op === 'event') store.update(timestamped(step.event));
              else if (step.op === 'snapshot') { await store.flush(); const value = JSON.parse(saved); value.pid = input.pid ?? 123; result.push(value); }
              else throw new Error('unknown status step');
            }
          } finally { await store.close(); Date.now = originalNow; }
          break;
        }
        case 'js_json': {
          let value;
          try { value = JSON.parse(Buffer.from(input.bytes).toString('utf8')); }
          catch { result = { valid: false }; break; }
          result = { valid: true, serialized: [...Buffer.from(JSON.stringify(value))] };
          if (typeof value === 'string') result.string_units = Array.from({ length: value.length }, (_, index) => value.charCodeAt(index));
          if (typeof value === 'number') {
            const bytes = Buffer.alloc(8); bytes.writeDoubleBE(value); result.number_bits = [...bytes];
          }
          break;
        }
        case 'model_catalog': result = catalog.modelCapabilities(input.model) ?? null; break;
        case 'redact': result = redaction.redactSensitive(input); break;
        case 'normalize_usage': result = telemetry.normalizeUsageTelemetry(input) ?? null; break;
        case 'normalize_pricing': result = telemetry.normalizePricingContext(input) ?? null; break;
        case 'normalize_telemetry_json': result = telemetry.normalizeTelemetryEvent(timestamped(JSON.parse(Buffer.from(input.bytes).toString('utf8')))) ?? null; break;
        case 'normalize_session_json': result = telemetry.normalizeSessionRecord(timestamped(JSON.parse(Buffer.from(input.bytes).toString('utf8'))), { includePrompts: input.include_prompts ?? true }) ?? null; break;
        case 'normalize_telemetry': result = telemetry.normalizeTelemetryEvent(timestamped(input)) ?? null; break;
        case 'normalize_session': result = telemetry.normalizeSessionRecord(timestamped(input.entry), { includePrompts: input.include_prompts ?? true }) ?? null; break;
        case 'estimate_savings': result = savings.estimateOutcomeSavings(input); break;
        case 'savings_tracker': {
          const tracker = savings.createSavingsTracker(Object.hasOwn(input, 'baseline_model') ? { baselineModel: input.baseline_model } : {});
          if (input.steps) {
            result = [];
            for (const step of input.steps) {
              if (step.op === 'update') tracker.update(step.event);
              else if (step.op === 'clear') tracker.clear();
              else if (step.op === 'snapshot') result.push(tracker.snapshot());
              else throw new Error('unknown savings step');
            }
          } else {
            for (const event of input.events ?? []) tracker.update(event);
            result = tracker.snapshot();
          }
          break;
        }
        case 'prepare_request': result = preparation.prepareRequest(input.body, input.target); break;
        case 'target_compatibility':
          result = compatibility.targetCompatibility(input.body, input.target, { autoMode: input.auto_mode ?? false }); break;
        case 'can_route_auto': result = compatibility.canRouteAutoRequest(input.body, input.target); break;
        case 'safeguards': result = compatibility.hasRoutableSafeguards(input); break;
        case 'validate_request': result = validation.validateRequestShape(input); break;
        case 'validate_request_json': result = validation.validateRequestShape(JSON.parse(Buffer.from(input.bytes).toString('utf8'))); break;
        case 'read_config':
          process.chdir(resolve(root, input.cwd ?? '.'));
          result = configuration.readConfig(input.env ?? {}, { validateAll: input.validate_all ?? false });
          if (input.require_keys) configuration.requireKeys(result);
          break;
        case 'conflicting_providers': result = authentication.conflictingProviders(input); break;
        case 'subscription_request': result = authentication.isSubscriptionRequest(input); break;
        case 'client_profile': result = authentication.clientProfileForLaunch(input.profile, input.args); break;
        case 'build_claude_env': {
          const parent = structuredClone(input.parent);
          result = { child: authentication.buildClaudeEnv(configuration.readConfig(input.config), 'http://127.0.0.1:1234', parent), parent };
          break;
        }
        case 'apply_policy': result = policy.applyPolicy(input.env, input.policy, { allowlists: input.allowlists ?? true }); break;
        case 'turn_state': {
          let now = input.now ?? 0;
          const state = new turns.TurnState({ limit: input.limit ?? 1000, idleTtlMs: input.idle_ttl_ms ?? 1800000, now: () => now });
          result = input.steps.map(step => {
            let value;
            switch (step.action) {
              case 'now': now = step.value; break;
              case 'select': value = state.select(step.keys, step.pin, { scope: step.scope ?? '', requestId: step.request_id, sequence: step.sequence ?? 0 }); break;
              case 'get': value = state.get(step.key); break;
              case 'ambiguous': value = state.ambiguous(step.key); break;
              case 'tool_owner': value = state.toolOwner(step.scope ?? '', step.ids); break;
              case 'complete': value = state.complete(step.request_id, step.evidence); break;
              case 'counts': value = { records: state.records.size, attempts: state.attempts.size, aliases: state.aliases.size }; break;
              default: throw new Error('unknown turn_state action');
            }
            return structuredClone(value ?? null);
          });
          break;
        }
        case 'response_observer': {
          result = { forwarded: [], models: [], errors: [], usages: [], executions: [], completions: [] };
          const chunks = [];
          const stream = observation.createResponseObserver({
            contentType: input.content_type ?? 'text/event-stream', maxBufferBytes: input.max_buffer_bytes ?? 65536,
            onModel: value => result.models.push(value), onError: value => result.errors.push(value),
            onUsage: value => result.usages.push(value), onExecution: value => result.executions.push(value),
            onComplete: value => result.completions.push(value),
          });
          stream.on('data', chunk => chunks.push(chunk));
          const complete = once(stream, input.action === 'destroy' ? 'close' : 'end');
          for (const chunk of input.chunks) stream.write(Buffer.from(chunk));
          if (input.action === 'destroy') stream.destroy(); else stream.end();
          await complete;
          result.forwarded = [...Buffer.concat(chunks)];
          if (input.utf16_strings) result = utf16Values(result);
          break;
        }
        default: throw new Error('Unknown fixture operation');
      }
      return { id, op, result };
    } catch (error) {
      // These pure adapters produce fixed validation messages; no payload is
      // interpolated into fixture diagnostics or benchmark metadata.
      return { id, op, error: error instanceof Error ? error.message : 'Reference operation failed' };
    } finally {
      process.chdir(original);
    }
  };
}

async function main(args) {
  let root = repository;
  let check = false;
  for (let index = 0; index < args.length; index++) {
    if (args[index] === '--check-baseline') check = true;
    else if (args[index] === '--root' && args[index + 1]) root = resolve(args[++index]);
    else throw new Error('Usage: node scripts/rust-reference.mjs [--root PATH] [--check-baseline]');
  }
  if (check) {
    process.stdout.write(`${JSON.stringify(await verifyBaseline(root))}\n`);
    return;
  }
  const execute = await createReference(root);
  let pending = Buffer.alloc(0), received = 0;
  const ids = new Set();
  const emit = async bytes => {
    if (!bytes.length) throw new Error('Empty fixture line');
    if (bytes.length > MAX_LINE_BYTES) throw new Error('Fixture line exceeds byte limit');
    let fixture;
    try { fixture = JSON.parse(bytes.toString('utf8')); } catch { throw new Error('Invalid fixture JSON'); }
    if (ids.has(fixture?.id)) throw new Error('Duplicate fixture identity');
    const result = await execute(fixture);
    ids.add(fixture.id);
    process.stdout.write(`${JSON.stringify(result)}\n`);
  };
  for await (const chunk of process.stdin) {
    received += chunk.length;
    if (received > MAX_INPUT_BYTES) throw new Error('Fixture input exceeds byte limit');
    pending = Buffer.concat([pending, chunk]);
    let end;
    while ((end = pending.indexOf(10)) !== -1) {
      await emit(pending.subarray(0, end));
      pending = pending.subarray(end + 1);
    }
    if (pending.length > MAX_LINE_BYTES) throw new Error('Fixture line exceeds byte limit');
  }
  if (pending.length) await emit(pending);
  if (!ids.size) throw new Error('No fixture cases supplied');
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main(process.argv.slice(2)).catch(error => {
    console.error(error instanceof Error ? error.message : 'Reference runner failed');
    process.exitCode = 1;
  });
}
