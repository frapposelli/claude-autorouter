import { constants } from 'node:fs';
import fs from 'node:fs/promises';
import { createHash, randomBytes } from 'node:crypto';
import { join, resolve } from 'node:path';
import { normalizeSessionRecord } from './telemetry-event.mjs';

const MAX_PENDING_BYTES = 1024 * 1024;
const MAX_SESSIONS = 128;
const WARNING = 'AutoRouter session logging disabled.';
// Inference never waits on this writer. Each launch creates new files; only
// accepted, bounded JSON lines are retained until the serialized writer drains.
export async function createSessionLog(directory, { includePrompts = true, warn = () => {} } = {}) {
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
      const row = normalizeSessionRecord(entry, { includePrompts });
      if (!row) return false;
      const sessionKey = row.session_id ? `session:${row.session_id}` : 'anonymous';
      const line = `${JSON.stringify(row)}\n`;
      const bytes = Buffer.byteLength(line);
      let session = sessions.get(sessionKey);
      if (pendingBytes + bytes > MAX_PENDING_BYTES || (!session && sessions.size >= MAX_SESSIONS)) {
        disable();
        return false;
      }
      if (!session) {
        const sessionHash = createHash('sha256').update(sessionKey).digest('hex');
        session = { path: join(root, `autorouter-session-${launch}-${sessionHash}.jsonl`) };
        sessions.set(sessionKey, session);
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
