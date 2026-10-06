import * as fileSystem from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

// A launcher that is killed hard (SIGKILL, power loss) cannot delete its
// private status directory, which also holds the rewritten Claude settings.
// The next launch removes directories whose owning process is gone. Only
// directories this user created, with owner-only permissions, are considered.
const PREFIX = 'autorouter-status-';
const MAX_ENTRIES = 200;
const SNAPSHOT_LIMIT = 1024 * 1024;
// Without a readable snapshot, wait before treating a directory as abandoned so
// a launch that is still starting is never removed.
const UNREADABLE_GRACE_MS = 10 * 60 * 1000;
// A reused process ID can look alive. The heartbeat is written every few
// seconds, so a snapshot this old belongs to an earlier process even if a
// sleeping machine suspended the real one for a while.
const HEARTBEAT_STALE_MS = 24 * 60 * 60 * 1000;

const defaultAlive = pid => {
  try { process.kill(pid, 0); return true; } catch (error) { return error.code === 'EPERM'; }
};

async function readSnapshot(io, path) {
  let handle;
  try {
    handle = await io.open(path, 'r');
    const stat = await handle.stat();
    if (!stat.isFile() || stat.size > SNAPSHOT_LIMIT) return undefined;
    return JSON.parse(await handle.readFile('utf8'));
  } catch { return undefined; }
  finally { try { await handle?.close(); } catch {} }
}

/**
 * Best effort and never throws. Returns the number of directories removed.
 * @param {{directory?:string,io?:typeof fileSystem,alive?:(pid:number)=>boolean,now?:number,uid?:number}} [options]
 */
export async function removeStaleStatusDirectories({
  directory = tmpdir(), io = fileSystem, alive = defaultAlive, now = Date.now(), uid = process.getuid?.(),
} = {}) {
  let removed = 0;
  try {
    if (uid === undefined) return 0;
    const names = (await io.readdir(directory)).filter(name => name.startsWith(PREFIX)).slice(0, MAX_ENTRIES);
    for (const name of names) {
      try {
        const path = join(directory, name);
        const stat = await io.lstat(path);
        if (!stat.isDirectory() || stat.isSymbolicLink() || stat.uid !== uid || (stat.mode & 0o077) !== 0) continue;
        const snapshot = await readSnapshot(io, join(path, 'state.json'));
        const pid = snapshot?.pid;
        let stale;
        if (Number.isSafeInteger(pid) && pid > 0) {
          const heartbeat = Number.isFinite(snapshot.heartbeat_at) ? snapshot.heartbeat_at : 0;
          stale = !alive(pid) || now - heartbeat > HEARTBEAT_STALE_MS;
        } else stale = now - stat.mtimeMs > UNREADABLE_GRACE_MS;
        if (!stale) continue;
        await io.rm(path, { recursive: true, force: true });
        removed++;
      } catch {}
    }
  } catch {}
  return removed;
}
