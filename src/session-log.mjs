import { constants } from 'node:fs';
import fs from 'node:fs/promises';
import { createHash, randomBytes } from 'node:crypto';
import { join, resolve } from 'node:path';

const MAX_PENDING_BYTES = 1024 * 1024;
const MAX_SESSIONS = 128;
const WARNING = 'AutoRouter session logging disabled.';
const SOURCES = new Set(['jev', 'ollama', 'cache', 'fallback', 'passthrough']);
const ERRORS = new Set(['timeout', 'http_error', 'invalid_response', 'network_error']);
const identifier = value => typeof value === 'string' && /^[A-Za-z0-9_.:-]{1,200}$/.test(value) ? value : undefined;
const model = value => typeof value === 'string' && /^[A-Za-z0-9_.:/-]{1,120}$/.test(value) ? value : undefined;
const code = value => typeof value === 'string' && /^[a-z][a-z0-9_]{0,79}$/.test(value) ? value : undefined;

function excerpt(value) {
  if (typeof value !== 'string') return { text: '', truncated: false };
  let text = '', count = 0;
  for (const character of value) {
    if (count++ === 500) return { text: text.toWellFormed(), truncated: true };
    text += character;
  }
  return { text: text.toWellFormed(), truncated: false };
}

function normalize(entry) {
  if (!entry || typeof entry !== 'object' || Array.isArray(entry) || entry.event !== 'decision') return;
  const requestId = identifier(entry.request_id);
  const selectedModel = model(entry.selected_model);
  const requestedModel = model(entry.requested_model);
  if (!requestId || !selectedModel || !requestedModel) return;
  const anonymous = entry.session_id === undefined || entry.session_id === null || entry.session_id === '';
  const sessionId = anonymous ? undefined : identifier(entry.session_id);
  // An invalid explicit identity must not mix records into an anonymous file.
  if (!anonymous && !sessionId) return;
  const requestClass = code(entry.request_class);
  const foreground = entry.request_class === undefined || entry.request_class === null || entry.request_class === '' || entry.request_class === 'main';
  const prompt = foreground ? excerpt(entry.prompt_excerpt) : { text: '', truncated: false };
  const timestamp = typeof entry.timestamp === 'string' && /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/.test(entry.timestamp)
    && Number.isFinite(Date.parse(entry.timestamp)) ? entry.timestamp : new Date().toISOString();
  const row = { schema_version: 1, event: 'decision', timestamp, request_id: requestId,
    ...(sessionId ? { session_id: sessionId } : {}),
    prompt_excerpt: prompt.text, prompt_truncated: prompt.truncated || (foreground && entry.prompt_truncated === true),
    requested_model: requestedModel, selected_model: selectedModel };
  for (const field of ['agent_id', 'prompt_id']) {
    const value = identifier(entry[field]);
    if (value) row[field] = value;
  }
  if (requestClass) row.request_class = requestClass;
  if (typeof entry.decision_latency_ms === 'number' && Number.isFinite(entry.decision_latency_ms)
    && entry.decision_latency_ms >= 0) row.decision_latency_ms = entry.decision_latency_ms;
  if (SOURCES.has(entry.source)) row.source = entry.source;
  const reason = code(entry.reason);
  if (reason) row.reason = reason;
  if (['jev', 'ollama'].includes(entry.evaluator)) row.evaluator = entry.evaluator;
  if (['haiku', 'sonnet', 'opus'].includes(entry.classified_tier)) row.classified_tier = entry.classified_tier;
  if (ERRORS.has(entry.classifier_error)) row.classifier_error = entry.classifier_error;
  return { sessionKey: sessionId ? `session:${sessionId}` : 'anonymous', row };
}

// Inference never waits on this writer. Each launch creates new files; only
// accepted, bounded JSON lines are retained until the serialized writer drains.
export async function createSessionLog(directory, { warn = () => {} } = {}) {
  let accepting = true, failed = false, warned = false, pendingBytes = 0;
  let root, directoryIdentity, pump, closePromise;
  const sessions = new Map(), queue = [];
  const launch = `${new Date().toISOString().replace(/[-:.]/g, '')}-${randomBytes(12).toString('hex')}`;
  const disable = () => {
    accepting = false;
    if (warned) return;
    warned = true;
    try { Promise.resolve(warn(WARNING)).catch(() => {}); } catch {}
  };
  async function checkDirectory() {
    const current = await fs.lstat(root);
    if (!current.isDirectory() || current.isSymbolicLink()
      || (directoryIdentity && (current.dev !== directoryIdentity.dev || current.ino !== directoryIdentity.ino))) {
      throw new Error('Invalid session log directory');
    }
    return current;
  }
  try {
    if (typeof directory !== 'string' || !directory.trim() || typeof constants.O_NOFOLLOW !== 'number') throw new Error('Invalid session log directory');
    root = resolve(directory);
    try { await checkDirectory(); }
    catch (error) { if (error.code !== 'ENOENT') throw error; }
    await fs.mkdir(root, { recursive: true, mode: 0o700 });
    directoryIdentity = await checkDirectory();
  } catch { failed = true; disable(); }

  async function handleFor(session) {
    if (session.handle) return session.handle;
    await checkDirectory();
    const handle = await fs.open(session.path, constants.O_WRONLY | constants.O_APPEND | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
    // Track immediately, including when a subsequent check fails, so shutdown
    // always closes the descriptor. Never reopen or follow an existing path.
    session.handle = handle;
    const stat = await handle.stat();
    if (!stat.isFile() || stat.nlink !== 1) throw new Error('Invalid session log file');
    await handle.chmod(0o600);
    await checkDirectory();
    return handle;
  }
  async function drain() {
    let index = 0;
    try {
      while (index < queue.length && !failed) {
        const item = queue[index];
        queue[index++] = undefined;
        const handle = await handleFor(item.session);
        await handle.writeFile(item.line);
        pendingBytes -= item.bytes;
        // A steady producer can keep the queue nonempty indefinitely. Bound
        // processed slots as well as the pending strings they once held.
        if (index >= 256) { queue.splice(0, index); index = 0; }
      }
    } catch {
      failed = true;
      disable();
    } finally {
      queue.length = 0;
      pendingBytes = 0;
    }
  }
  function schedule() {
    if (pump || failed || !queue.length) return;
    pump = Promise.resolve().then(drain).finally(() => {
      pump = undefined;
      // A record can arrive between drain resolving and this continuation.
      // Keep it scheduled even if shutdown has already stopped new records.
      schedule();
    });
  }
  function record(entry) {
    if (!accepting) return false;
    try {
      const normalized = normalize(entry);
      if (!normalized) return false;
      const line = `${JSON.stringify(normalized.row)}\n`;
      const bytes = Buffer.byteLength(line);
      let session = sessions.get(normalized.sessionKey);
      if (pendingBytes + bytes > MAX_PENDING_BYTES || (!session && sessions.size >= MAX_SESSIONS)) {
        disable();
        return false;
      }
      if (!session) {
        const sessionHash = createHash('sha256').update(normalized.sessionKey).digest('hex');
        session = { path: join(root, `autorouter-session-${launch}-${sessionHash}.jsonl`) };
        sessions.set(normalized.sessionKey, session);
      }
      queue.push({ session, line, bytes });
      pendingBytes += bytes;
      schedule();
      return true;
    } catch { return false; }
  }
  function close() {
    if (!closePromise) {
      accepting = false;
      closePromise = (async () => {
        while (pump) await pump;
        for (const session of sessions.values()) {
          if (!session.handle) continue;
          try { await session.handle.close(); } catch { disable(); }
          session.handle = undefined;
        }
      })().catch(() => { disable(); });
    }
    return closePromise;
  }
  return { record, close };
}
