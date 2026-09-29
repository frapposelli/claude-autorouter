import { closeSync, chmodSync, fchmodSync, mkdtempSync, openSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { randomBytes } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createSavingsTracker } from './savings.mjs';

const EVENTS = new Set(['request_start', 'route', 'upstream_response', 'upstream_model', 'upstream_usage', 'upstream_error', 'request_complete', 'request_error', 'request_cancelled']);
const CLASSIFIER_ERRORS = new Set(['timeout', 'http_error', 'invalid_response', 'network_error']);
const UPSTREAM_ERRORS = new Set(['invalid_request_error', 'authentication_error', 'billing_error', 'permission_error', 'not_found_error', 'request_too_large', 'rate_limit_error', 'api_error', 'overloaded_error', 'timeout_error', 'unknown_error', 'http_error', 'request_error']);
const identifier = value => typeof value === 'string' && /^[\w.:-]{1,200}$/.test(value) ? value : undefined;
const modelName = value => typeof value === 'string' && /^[\w.:/-]{1,120}$/.test(value) ? value : undefined;
const code = value => typeof value === 'string' && /^[a-z][a-z0-9_]{0,63}$/.test(value) ? value : undefined;
const httpStatus = value => Number.isInteger(value) && value >= 100 && value <= 599 ? value : undefined;
const latency = value => typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= 3600000 ? value : undefined;
const tokenCount = value => Number.isSafeInteger(value) && value >= 0;

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
  let privateDirectory, path = null, pending, heartbeat, closed = false;
  const sessions = new Map();
  const savings = createSavingsTracker({ baselineModel: options?.baselineModel });

  function flush() {
    if (closed) return;
    if (pending) { clearImmediate(pending); pending = undefined; }
    if (!path) return;
    let temporary, fd;
    try {
      const snapshot = { version: 1, pid: process.pid, heartbeat_at: Date.now(), sessions: Object.fromEntries(sessions), savings: savings.snapshot() };
      temporary = join(privateDirectory, `.state-${randomBytes(8).toString('hex')}.tmp`);
      fd = openSync(temporary, 'wx', 0o600);
      fchmodSync(fd, 0o600);
      writeFileSync(fd, JSON.stringify(snapshot));
      closeSync(fd); fd = undefined;
      renameSync(temporary, path);
    } catch {
      if (fd !== undefined) { try { closeSync(fd); } catch {} }
      if (temporary) { try { rmSync(temporary, { force: true }); } catch {} }
    }
  }

  function schedule() {
    if (closed || pending || !path) return;
    try { pending = setImmediate(flush); pending.unref?.(); } catch {}
  }

  function update(event) {
    if (closed) return;
    try {
      if (!event || !EVENTS.has(event.event)) return;
      // Account for all session calls, including parallel agents and auxiliary
      // requests, independently of which foreground request owns the model UI.
      savings.update(event);
      schedule();
      if ((event.agent_id != null && event.agent_id !== '')
        || (event.request_class != null && event.request_class !== '' && event.request_class !== 'main')) return;
      const sessionId = event.session_id == null || event.session_id === '' ? '' : identifier(event.session_id);
      const requestId = identifier(event.request_id);
      // Invalid IDs must never collapse into an unrelated/anonymous session.
      if (sessionId === undefined || !requestId) return;
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
        const requested = modelName(event.requested_model);
        const promptId = identifier(event.prompt_id);
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
      const status = httpStatus(event.status);
      const terminal = ['error', 'cancelled', 'ready'].includes(state.phase);
      switch (event.event) {
        case 'route': {
          const fields = { requested_model: modelName(event.requested_model), selected_model: modelName(event.model),
            source: ['jev', 'cache', 'fallback'].includes(event.source) ? event.source : undefined, reason: code(event.reason), latency_ms: latency(event.latency_ms),
            classified_tier: ['haiku', 'sonnet', 'opus'].includes(event.classified_tier) ? event.classified_tier : undefined,
            context_check: ['within_budget', 'over_budget', 'count_unavailable'].includes(event.context_check) ? event.context_check : undefined,
            counted_input_tokens: tokenCount(event.counted_input_tokens) ? event.counted_input_tokens : undefined,
            classifier_error: CLASSIFIER_ERRORS.has(event.classifier_error) ? event.classifier_error : undefined, classifier_status: httpStatus(event.classifier_status) };
          for (const [key, value] of Object.entries(fields)) if (value !== undefined) state[key] = value;
          if (!terminal) state.phase = 'connecting';
          break;
        }
        case 'upstream_response':
          if (status !== undefined) state.status = status;
          if (status >= 400) { delete state.context_usage; state.phase = 'error'; state.error_type = UPSTREAM_ERRORS.has(event.error_type) ? event.error_type : 'http_error'; }
          else if (!terminal) state.phase = 'streaming';
          break;
        case 'upstream_model': {
          const model = modelName(event.model);
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
          state.error_type = UPSTREAM_ERRORS.has(event.error_type) ? event.error_type : (event.event === 'upstream_error' ? 'unknown_error' : 'request_error');
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
    if (closed) return;
    closed = true;
    try { clearImmediate(pending); clearInterval(heartbeat); } catch {}
    sessions.clear();
    savings.clear();
    if (privateDirectory) { try { rmSync(privateDirectory, { recursive: true, force: true }); } catch {} }
  }

  try {
    const directory = options?.directory ?? tmpdir();
    privateDirectory = mkdtempSync(join(directory, 'autorouter-status-'));
    chmodSync(privateDirectory, 0o700);
    path = join(privateDirectory, 'state.json');
    flush();
    heartbeat = setInterval(schedule, 5000);
    heartbeat.unref();
  } catch {
    if (privateDirectory) { try { rmSync(privateDirectory, { recursive: true, force: true }); } catch {} }
    path = null;
  }
  return { path, update, flush, close };
}
