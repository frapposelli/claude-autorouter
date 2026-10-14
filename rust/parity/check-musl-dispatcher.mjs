// Independent /proc executable observation of a genuine mixed npm installation.
// Existing package-smoke runs the seven suites; this proves which bytes the shell execs.
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { constants } from 'node:fs';
import { open, mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import http from 'node:http';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { promisify } from 'node:util';
const exec = promisify(execFile);
assert.equal(process.argv.length, 6, 'Pass exact mixed archive, expected musl SHA, target, fresh report');
const [archive, expected, target, output] = process.argv.slice(2);
assert.match(expected, /^[a-f0-9]{64}$/);
assert.ok(['x86_64-unknown-linux-musl', 'aarch64-unknown-linux-musl'].includes(target));
assert.equal(process.platform, 'linux');
const MAX_BINARY = 32 * 1024 * 1024, MAX_MANIFEST = 2 * 1024 * 1024;
async function consume(path, maximum, { digest = false, ownedProc = false } = {}) {
  const file = await open(path, constants.O_RDONLY | (ownedProc ? 0 : constants.O_NOFOLLOW));
  try {
    const before = await file.stat(); assert.ok(before.isFile() && before.size <= maximum);
    const hash = createHash('sha256'), chunks = [], buffer = Buffer.alloc(65536); let total = 0;
    while (true) {
      const { bytesRead } = await file.read(buffer, 0, Math.min(buffer.length, maximum - total + 1), null);
      if (!bytesRead) break;
      total += bytesRead; assert.ok(total <= maximum, 'Bounded fixture file exceeded ceiling');
      if (digest) hash.update(buffer.subarray(0, bytesRead)); else chunks.push(Buffer.from(buffer.subarray(0, bytesRead)));
    }
    const after = await file.stat();
    assert.ok(before.ino === after.ino && before.dev === after.dev && before.size === after.size && before.mtimeMs === after.mtimeMs);
    return digest ? hash.digest('hex') : Buffer.concat(chunks, total);
  } finally { await file.close(); }
}
const hashFile = (path, ownedProc = false) => consume(path, MAX_BINARY, { digest: true, ownedProc });
const scratch = await mkdtemp(join(tmpdir(), 'autorouter-musl-selection-'));
let child, ended, npmChild, npmEnded, timer, stopped, healthRequest;
let leaderClosed = false, npmClosed = false, terminalCause = null, terminalExitTask, reportPublicationStarted = false;
async function bounded(promise, ms) { let stop; try { return await Promise.race([promise, new Promise((_, reject) => { stop = setTimeout(() => reject(Error('Owned fixture deadline')), ms); })]); } finally { clearTimeout(stop); } }
async function cleanup() {
  if (stopped) return stopped;
  return stopped = (async () => {
    healthRequest?.destroy(Error('Owned fixture cancelled'));
    if (child) {
      if (child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
      try { await bounded(ended, 3000); }
      catch { if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL'); await bounded(ended, 2000); }
    }
    if (npmChild) {
      if (npmChild.pid && npmChild.exitCode === null && npmChild.signalCode === null) npmChild.kill('SIGTERM');
      try { await bounded(npmEnded, 3000); }
      catch { if (npmChild.pid && npmChild.exitCode === null && npmChild.signalCode === null) npmChild.kill('SIGKILL'); await bounded(npmEnded, 2000); }
    }
    clearTimeout(timer); await rm(scratch, { recursive: true, force: true });
  })();
}
async function failureWitness(reason, cleanupCompleted) {
  const failure = { schema_version: 1, kind: 'actual_mixed_npm_musl_dispatch', passed: false, target, expected_sha256: expected,
    failure: reason, cleanup_completed: cleanupCompleted, leader_exit_observed: leaderClosed, npm_spawned: Boolean(npmChild?.pid), npm_close_observed: npmClosed };
  try { await writeFile(output, JSON.stringify(failure, null, 2) + '\n', { flag: 'wx', mode: 0o600 }); }
  catch { process.stderr.write('Cannot retain create-only failure witness\n'); }
}
function failAndExit(code, reason) {
  // The first pre-publication terminal cause is recorded synchronously before
  // cleanup can yield. A completed fixture's report-publication admission is
  // the explicit boundary; signals after it do not retroactively cancel work.
  if (reportPublicationStarted) return Promise.resolve();
  if (terminalCause !== null) return terminalExitTask;
  terminalCause = reason;
  return terminalExitTask = (async () => {
    let cleanupCompleted = false;
    try { await cleanup(); cleanupCompleted = true; } catch {}
    await failureWitness(terminalCause, cleanupCompleted); process.exit(code);
  })();
}
const interrupt = () => { void failAndExit(130, 'cancelled'); };
const terminate = () => { void failAndExit(143, 'cancelled'); };
process.once('SIGINT', interrupt); process.once('SIGTERM', terminate);
try {
  const prefix = join(scratch, "install ' $ with spaces");
  const archiveSha = await hashFile(resolve(archive));
  // Admission and owned-child assignment contain no await: a prior terminal
  // cause cannot start a new child after cleanup has already completed.
  assert.equal(terminalCause, null, 'Installer admission after terminal cause');
  const installPromise = exec('npm', ['install', '--global', '--offline', '--ignore-scripts', '--no-audit', '--no-fund', '--prefix', prefix, resolve(archive)],
    { cwd: scratch, env: { PATH: process.env.PATH, HOME: scratch, npm_config_cache: join(scratch, 'cache'), npm_config_userconfig: join(scratch, 'absent-npmrc'), npm_config_globalconfig: join(scratch, 'absent-globalrc'), npm_config_update_notifier: 'false' }, timeout: 120000, maxBuffer: 1048576 });
  npmChild = installPromise.child; assert.ok(npmChild);
  npmEnded = new Promise(resolve => npmChild.once('close', () => { npmClosed = true; resolve(); }));
  // Cancellation joins this actual child; a rejected callback cannot become an
  // unhandled rejection while the signal handler is awaiting owned cleanup.
  void installPromise.catch(() => {});
  const install = await installPromise;
  assert.equal(typeof install.stdout, 'string'); assert.ok(npmClosed);
  assert.equal(terminalCause, null);
  const packageRoot = join(prefix, 'lib/node_modules/claude-autorouter');
  const manifest = JSON.parse((await consume(join(packageRoot, 'build-manifest.json'), MAX_MANIFEST)).toString('utf8'));
  const artifacts = manifest.artifacts;
  assert.equal(artifacts.length, 2);
  const musl = artifacts.find(row => row.target === target);
  const gnu = artifacts.find(row => row.target === target.replace('-musl', '-gnu'));
  assert.ok(musl && gnu); assert.equal(musl.sha256, expected); assert.notEqual(gnu.sha256, expected);
  assert.equal(await hashFile(join(packageRoot, musl.path)), expected);
  const cwd = join(scratch, 'unrelated'); await mkdir(cwd);
  assert.equal(terminalCause, null, 'Serve admission after terminal cause');
  child = spawn(join(prefix, 'bin/claude-autorouter'), ['serve'], { cwd, stdio: ['ignore', 'ignore', 'pipe'],
    env: { HOME: cwd, XDG_CONFIG_HOME: cwd, PATH: cwd, AUTOROUTER_CONFIG: join(cwd, 'absent.json'), AUTOROUTER_PORT: '0',
      AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: 'synthetic-evaluator', ANTHROPIC_API_KEY: 'synthetic-provider', AUTOROUTER_TOKEN: 'synthetic-dispatch-token',
      AUTOROUTER_UPSTREAM_URL: 'http://127.0.0.1:1', AUTOROUTER_JEV_URL: 'http://127.0.0.1:1/v1/systemone' } });
  let spawnFailed = false;
  ended = new Promise(resolve => { child.once('error', () => { spawnFailed = true; resolve(); }); child.once('close', () => { leaderClosed = true; resolve(); }); });
  const stderrChunks = []; let stderrBytes = 0, stderrOverflow = false;
  child.stderr.on('data', chunk => { if (chunk.length > 65536 - stderrBytes) { stderrOverflow = true; child.kill('SIGKILL'); } else { stderrBytes += chunk.length; stderrChunks.push(chunk); } });
  timer = setTimeout(() => { void failAndExit(1, 'deadline'); }, 20000);
  let port;
  for (let attempt = 0; attempt < 1000; attempt++) {
    port = Number(Buffer.concat(stderrChunks, stderrBytes).toString('utf8').match(/AutoRouter listening on http:\/\/127\.0\.0\.1:(\d+)/)?.[1]);
    if (port || spawnFailed || stderrOverflow || child.exitCode !== null || child.signalCode !== null) break;
    await delay(5);
  }
  assert.ok(port && !spawnFailed && !stderrOverflow); assert.equal(child.exitCode, null); assert.equal(child.signalCode, null);
  // /proc magic-link following is intentional only for our still-owned live leader.
  const executedSha = await hashFile(`/proc/${child.pid}/exe`, true); assert.equal(executedSha, expected);
  assert.equal(child.exitCode, null); assert.equal(child.signalCode, null);
  const health = await bounded(new Promise((resolve, reject) => {
    healthRequest = http.get({ host: '127.0.0.1', port, path: '/health' }, response => { response.once('error', reject); response.once('aborted', () => reject(Error('Health response aborted'))); response.resume(); response.once('end', () => resolve(response.statusCode)); });
    healthRequest.setTimeout(2000, () => healthRequest.destroy(Error('Health deadline'))); healthRequest.once('error', reject);
  }), 3000);
  assert.equal(health, 200); assert.ok(!stderrOverflow);
  assert.equal(await hashFile(resolve(archive)), archiveSha, 'Archive changed during installed fixture');
  await cleanup(); assert.ok(leaderClosed && npmClosed, 'Every admitted direct child close must be observed');
  assert.equal(terminalCause, null);
  const report = { schema_version: 1, kind: 'actual_mixed_npm_musl_dispatch', passed: true, platform: process.platform, arch: process.arch,
    archive_sha256: archiveSha, target, installed_sha256: expected, executed_sha256: executedSha,
    gnu_binary_sha256: gnu.sha256, health_status: health, leader_exit_observed: true, npm_close_observed: true, node_in_child_path: false,
    completion_boundary: 'All owned closes and identity checks complete; no terminal cause before synchronous report-publication admission',
    scope: 'Owned ordinary serve leader observed via /proc while alive; no provider/model requests, escaped descendants or whole matrix qualification.' };
  assert.equal(terminalCause, null);
  reportPublicationStarted = true;
  await writeFile(output, JSON.stringify(report, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
  console.log(JSON.stringify({ passed: true, target, executed_sha256: expected }));
} catch (error) {
  if (reportPublicationStarted) { process.exitCode = 1; throw error; }
  await failAndExit(1, 'fixture_failed');
} finally {
  process.off('SIGINT', interrupt); process.off('SIGTERM', terminate); await cleanup();
}
