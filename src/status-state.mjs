import * as fileSystem from 'node:fs/promises';
import { randomBytes } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createSavingsTracker } from './savings.mjs';
import { normalizeTelemetryEvent } from './telemetry-event.mjs';

const tokenCount = value => Number.isSafeInteger(value) && value >= 0;
const TIMING_FIELDS = ['evaluation_latency_ms', 'routing_latency_ms', 'decision_latency_ms', 'latency_ms', 'first_response_ms', 'total_latency_ms'];

function contextUsage(usage, model) {
  if (!model || !usage || typeof usage !== 'object' || Array.isArray(usage) || usage.pricing_unsupported === true) return;
  const counts = {};
  for (const field of ['input_tokens', 'cache_creation_input_tokens', 'cache_read_input_tokens']) {
    const value = Object.hasOwn(usage, field) ? usage[field] : field === 'input_tokens' ? undefined : 0;
    if (!tokenCount(value)) return;
    counts[field] = value;
  }
  const total = Object.values(counts).reduce((sum, value) => sum + value, 0);
  if (!Number.isSafeInteger(total)) return;
  // Match Claude's context percentage: cached and uncached input, excluding
  // output. These are the latest response's counts, never session totals.
  return { model, input_tokens: total };
}

// All telemetry is best-effort. Never let filesystem or malformed-event
// failures interrupt routing, and never persist request bodies or raw errors.
export function createStatusState(options = {}) {
  let privateDirectory, snapshotPath, path = null, pending, heartbeat, closing = false, disabled = false;
  let initialized = false, dirty = false, writing, closingPromise;
  const sessions = new Map();
  const savings = createSavingsTracker({ baselineModel: options?.baselineModel });
  let io = fileSystem;

  async function writeSnapshot() {
    let temporary, handle;
    try {
      const snapshot = { version: 1, pid: process.pid, heartbeat_at: Date.now(), sessions: Object.fromEntries(sessions), savings: savings.snapshot() };
      // Capture only one bounded snapshot. Events arriving during I/O merely
      // mark the latest in-memory state dirty; they never queue more copies.
      const serialized = JSON.stringify(snapshot);
      temporary = join(privateDirectory, `.state-${randomBytes(8).toString('hex')}.tmp`);
      handle = await io.open(temporary, 'wx', 0o600);
      await handle.chmod(0o600);
      await handle.writeFile(serialized);
      await handle.close(); handle = undefined;
      if (disabled) return false;
      await io.rename(temporary, snapshotPath);
      temporary = undefined;
      return true;
    } catch {
      return false;
    } finally {
      if (handle) { try { await handle.close(); } catch {} }
      if (temporary) { try { await io.rm(temporary, { force: true }); } catch {} }
    }
  }

  function drain() {
    if (writing) return writing;
    if (!initialized || disabled || !dirty) return Promise.resolve();
    writing = (async () => {
      while (dirty && !disabled) {
        dirty = false;
        await writeSnapshot();
      }
    })().finally(() => { writing = undefined; });
    return writing;
  }

  function flush() {
    if (closing) return closingPromise ?? Promise.resolve();
    if (disabled) return Promise.resolve();
    dirty = true;
    if (pending) { clearImmediate(pending); pending = undefined; }
    // A caller may await durability, but update() never waits for this work.
    return initialization.then(drain).catch(() => {});
  }

  function schedule() {
    if (closing || disabled) return;
    dirty = true;
    if (pending || !initialized) return;
    try {
      pending = setImmediate(() => { pending = undefined; void drain(); });
      pending.unref?.();
    } catch {}
  }

  function update(event) {
    if (closing || disabled) return;
    try {
      event = normalizeTelemetryEvent(event);
      if (!event) return;
      // Account for all session calls, including parallel agents and auxiliary
      // requests, independently of which foreground request owns the model UI.
      savings.update(event);
      schedule();
      if ((event.agent_id != null && event.agent_id !== '')
        || (event.request_class != null && event.request_class !== '' && event.request_class !== 'main')) return;
      // The normalizer rejects invalid explicit identities; absent IDs remain
      // the anonymous session, and future non-main classes stay isolated.
      const sessionId = event.session_id ?? '';
      const requestId = event.request_id;
      const previous = sessions.get(sessionId);
      if (event.event === 'request_start') {
        const state = {
          request_id: requestId,
          phase: 'routing',
          updated_at: Date.now(),
          ...(previous?.last_model ? { last_model: previous.last_model } : {}),
          ...((previous?.context_usage ?? previous?.last_context_usage)
            ? { last_context_usage: previous.context_usage ?? previous.last_context_usage } : {}),
        };
        const requested = event.requested_model;
        const promptId = event.prompt_id;
        if (requested) state.requested_model = requested;
        if (promptId) state.prompt_id = promptId;
        sessions.delete(sessionId);
        sessions.set(sessionId, state);
        while (sessions.size > 100) sessions.delete(sessions.keys().next().value);
        schedule();
        return;
      }
      if (!previous || previous.request_id !== requestId) return;
      const state = previous;
      state.updated_at = Date.now();
      const status = event.status;
      const terminal = ['error', 'cancelled', 'ready'].includes(state.phase);
      for (const key of TIMING_FIELDS) if (event[key] !== undefined) state[key] = event[key];
      if (event.completion_confirmed !== undefined) state.completion_confirmed = event.completion_confirmed;
      switch (event.event) {
        case 'route': {
          const fields = { requested_model: event.requested_model, selected_model: event.selected_model ?? event.model };
          for (const key of ['source', 'evaluator', 'reason', 'compatibility_reason', 'continuity_state',
            'classified_tier', 'context_check', 'counted_input_tokens', 'classifier_error', 'classifier_status']) fields[key] = event[key];
          for (const [key, value] of Object.entries(fields)) if (value !== undefined) state[key] = value;
          if (!terminal) state.phase = 'connecting';
          break;
        }
        case 'upstream_response':
          if (status !== undefined) state.status = status;
          if (status >= 400) { delete state.context_usage; state.phase = 'error'; state.error_type = event.error_type ?? 'http_error'; }
          else if (!terminal) state.phase = 'streaming';
          break;
        case 'upstream_model': {
          const model = event.confirmed_model ?? event.model;
          if (model) { state.actual_model = model; state.last_model = model; }
          if (!terminal) state.phase = 'streaming';
          break;
        }
        case 'upstream_usage': {
          if (state.phase === 'error' || state.phase === 'cancelled') break;
          const usage = contextUsage(event.usage, state.actual_model);
          if (usage) state.context_usage = usage;
          break;
        }
        case 'upstream_error':
        case 'request_error':
          delete state.context_usage;
          state.phase = 'error';
          state.error_type = event.error_type ?? (event.event === 'upstream_error' ? 'unknown_error' : 'request_error');
          if (status !== undefined) state.status = status;
          break;
        case 'request_complete':
          if (!terminal) state.phase = 'ready';
          break;
        case 'request_cancelled':
          delete state.context_usage;
          if (state.phase !== 'error') state.phase = 'cancelled';
          break;
      }
      schedule();
    } catch {}
  }

  function close() {
    if (closingPromise) return closingPromise;
    closing = true;
    try { clearImmediate(pending); clearInterval(heartbeat); } catch {}
    closingPromise = (async () => {
      // An accepted write must finish before removing its directory. No
      // detached filesystem promise may recreate a file after shutdown.
      await initialization;
      await drain();
      if (privateDirectory) { try { await io.rm(privateDirectory, { recursive: true, force: true }); } catch {} }
      sessions.clear();
      savings.clear();
    })().catch(() => {});
    return closingPromise;
  }

  const initialization = (async () => {
    try {
      io = options?.fileSystem ?? fileSystem;
      const directory = options?.directory ?? tmpdir();
      privateDirectory = await io.mkdtemp(join(directory, 'autorouter-status-'));
      await io.chmod(privateDirectory, 0o700);
      if (disabled) return;
      snapshotPath = join(privateDirectory, 'state.json');
      dirty = false;
      if (!await writeSnapshot() || disabled) { disabled = true; return; }
      initialized = true;
      path = snapshotPath;
      if (!closing) {
        heartbeat = setInterval(schedule, 5000);
        heartbeat.unref();
        if (dirty) schedule();
      }
    } catch { disabled = true; }
    finally {
      if (disabled && privateDirectory) { try { await io.rm(privateDirectory, { recursive: true, force: true }); } catch {} }
    }
  })();
  // Optional UI storage must not hold up Claude startup indefinitely. The
  // underlying operation still belongs to close(), even after this deadline.
  let readinessTimer;
  const ready = Promise.race([initialization, new Promise(resolve => {
    readinessTimer = setTimeout(() => { disabled = true; path = null; resolve(); }, 1000);
  })]).finally(() => clearTimeout(readinessTimer));
  return { get path() { return path; }, ready, update, flush, close };
}
