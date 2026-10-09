// Shared source-only harness for isolated transport differentials.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { lookup } from 'node:dns/promises';
import { isIP } from 'node:net';
import { chmod, copyFile, mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

const root = resolve(import.meta.dirname, '../..');
async function hashFile(path) { const hash = createHash('sha256'); for await (const chunk of createReadStream(path)) hash.update(chunk); return hash.digest('hex'); }
async function bounded(promise, milliseconds, label) {
  let timer;
  try { return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(Error(label)), milliseconds); })]); }
  finally { clearTimeout(timer); }
}
export async function transportHarness(prefix, { candidateArguments = ['serve'], candidateEnvironment = {}, candidateKind = 'shipping CLI' } = {}) {
  const args = process.argv.slice(2);
  assert.ok(args.length <= 2, 'Usage: transport audit [frozen-reference-directory] [candidate-executable]');
  const reference = resolve(args[0] ?? join(root, 'artifacts/rust-rewrite/reference'));
  const originalCandidate = resolve(args[1] ?? join(root, 'rust/target/debug/claude-autorouter'));
  const checked = spawnSync(process.execPath, [join(root, 'scripts/rust-reference.mjs'), '--root', reference, '--check-baseline'], { encoding: 'utf8', timeout: 15000, maxBuffer: 1024 * 1024 });
  assert.equal(checked.status, 0, 'Frozen source verification failed');
  const scratch = await mkdtemp(join(tmpdir(), prefix));
  const candidate = join(scratch, 'candidate');
  let identity;
  try {
    await copyFile(originalCandidate, candidate); await chmod(candidate, 0o700);
    identity = { frozen_source: JSON.parse(checked.stdout), reference, node_version: process.version, node_executable: process.execPath, node_executable_sha256: await hashFile(process.execPath), candidate_executable: originalCandidate, candidate_sha256: await hashFile(candidate), candidate_execution: 'isolated immutable byte-identical snapshot', candidate_kind: candidateKind };
  }
  catch (error) { await rm(scratch, { recursive: true, force: true }); throw error; }
  const token = 'synthetic-transport-audit-token';
  const children = new Set(), servers = new Set(), sockets = new Set();
  function watch(server) {
    servers.add(server);
    server.on('connection', socket => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)); socket.on('error', () => {}); });
    return server;
  }
  async function gateway(name, upstream, extra = {}, { allowStartupFailure = false } = {}) {
    // Every service is synthetic. Never inherit an external evaluator default,
    // even for GET-only cases or a future accidental inference fixture.
    const loopback = address => address === '::1' || (isIP(address) === 4 && address.startsWith('127.'));
    async function localEndpoint(value) {
      const url = new URL(value), host = url.hostname.replace(/^\[|\]$/g, '');
      assert.ok(['http:', 'https:'].includes(url.protocol) && !url.username && !url.password, 'Synthetic endpoint must be plain HTTP(S) without embedded credentials');
      assert.ok(loopback(host) || host === 'localhost', 'Synthetic endpoint must use a numeric loopback literal or localhost');
      if (host === 'localhost') {
        const addresses = await bounded(lookup(host, { all: true }), 2000, 'Localhost resolution deadline');
        assert.ok(addresses.length && addresses.every(({ address }) => loopback(address)), 'Localhost must resolve only to numeric loopback addresses');
      }
      return url;
    }
    const provider = await localEndpoint(upstream);
    const evaluator = new URL('/v1/systemone', provider);
    if (evaluator.hostname === 'localhost') evaluator.hostname = '127.0.0.1';
    const evaluatorUrl = extra.AUTOROUTER_JEV_URL ?? evaluator.href;
    const checkedEvaluator = await localEndpoint(evaluatorUrl);
    assert.ok(loopback(checkedEvaluator.hostname.replace(/^\[|\]$/g, '')), 'Synthetic evaluator must use an explicit numeric loopback URL');
    assert.equal(extra.AUTOROUTER_EVALUATOR ?? 'jev', 'jev', 'Transport harness only enables its local synthetic evaluator');
    const directory = join(scratch, `${name}-${Math.random().toString(16).slice(2)}`);
    await mkdir(directory, { mode: 0o700 });
    const child = spawn(name === 'node' ? process.execPath : candidate, name === 'node' ? [join(reference, 'bin/autorouter.mjs'), 'serve'] : candidateArguments, { cwd: directory, env: { HOME: directory, XDG_CONFIG_HOME: directory, TMPDIR: directory, PATH: directory, AUTOROUTER_PORT: '0', AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: 'synthetic-evaluator', ANTHROPIC_API_KEY: 'synthetic-provider', AUTOROUTER_TOKEN: token, ...(name === 'node' ? {} : candidateEnvironment), ...extra, AUTOROUTER_UPSTREAM_URL: provider.href, AUTOROUTER_JEV_URL: checkedEvaluator.href }, stdio: ['ignore', 'ignore', 'pipe'] });
    let stderr = '', spawnError = false;
    child.stderr.on('data', bytes => { if (stderr.length + bytes.length > 1024 * 1024) child.kill('SIGKILL'); else stderr += bytes; });
    const ended = new Promise(resolve => { child.once('error', () => { spawnError = true; resolve(); }); child.once('close', resolve); });
    const watchdog = setTimeout(() => child.kill('SIGKILL'), 30000);
    let stopping;
    const stop = () => stopping ??= (async () => {
      try { child.kill('SIGTERM'); await bounded(ended, 3000, 'Synthetic process shutdown deadline'); }
      catch { child.kill('SIGKILL'); await bounded(ended, 2000, 'Synthetic process kill deadline'); }
      finally { clearTimeout(watchdog); children.delete(stop); }
    })();
    children.add(stop);
    try {
      let port;
      for (let attempt = 0; attempt < 1000; attempt++) {
        port = Number(stderr.match(/AutoRouter listening on http:\/\/127\.0\.0\.1:(\d+)/)?.[1]);
        if (port || spawnError || child.exitCode !== null) break;
        await delay(5);
      }
      if (!port && allowStartupFailure) {
        const startup_failure = { exit_code: child.exitCode, signal: child.signalCode, timed_out: !spawnError && child.exitCode === null && child.signalCode === null, stderr: stderr.replaceAll(process.execPath, 'runtime').replaceAll(candidate, 'runtime').replaceAll(scratch, '<fixture>').trim() };
        await stop();
        return { startup_failure };
      }
      assert.ok(port, 'Synthetic gateway failed startup');
      return { port, stop, exit: () => ({ code: child.exitCode, signal: child.signalCode }), stderr: () => stderr };
    } catch (error) { await stop(); throw error; }
  }
  const close = server => bounded(new Promise(resolve => server.close(resolve)), 2000, 'Synthetic server cleanup deadline');
  let cleaning;
  function cleanup() { return cleaning ??= (async () => {
    process.off('SIGINT', interrupt); process.off('SIGTERM', terminate);
    await Promise.allSettled([...children].map(stop => stop()));
    for (const socket of sockets) socket.destroy();
    await Promise.allSettled([...servers].map(close));
    await rm(scratch, { recursive: true, force: true });
  })(); }
  const interrupt = () => { void cleanup().finally(() => process.exit(130)); };
  const terminate = () => { void cleanup().finally(() => process.exit(143)); };
  process.once('SIGINT', interrupt); process.once('SIGTERM', terminate);
  return { root, scratch, token, identity, gateway, watch, close, cleanup };
}
