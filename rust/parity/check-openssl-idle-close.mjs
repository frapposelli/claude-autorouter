// Independent synthetic idle-close schedules; original session driver is unchanged.
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { createHash, X509Certificate } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import http from 'node:http';
import net from 'node:net';
import { join } from 'node:path';
import tls from 'node:tls';
import { setTimeout as delay } from 'node:timers/promises';
import { promisify } from 'node:util';
import { transportHarness } from './transport-harness.mjs';
assert.equal(process.argv.length, 4, 'Usage: check-openssl-idle-close.mjs <frozen-reference> <runtime-test-executable>');
assert.equal(process.version, 'v22.14.0');
const h = await transportHarness('autorouter-idle-close-', { candidateKind: 'test-only OpenSSL idle-close observation' });
const { scratch, root } = h;
const results = [], ownedChildren = new Set();
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const run = promisify(execFile);
const empty = join(scratch, 'empty.cnf');
async function openssl(...args) {
  return run('openssl', args, { timeout: 10000, maxBuffer: 1024 * 1024, env: { PATH: process.env.PATH, OPENSSL_CONF: empty } });
}
async function until(check, label) {
  const deadline = Date.now() + 2000;
  while (!check()) { if (Date.now() >= deadline) throw Error(label); await delay(5); }
}
async function fixtures() {
  await writeFile(empty, '');
  const ca = join(scratch, 'root');
  await writeFile(`${ca}.cnf`, '[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=ext\n[dn]\nCN=Synthetic Session Root\n[ext]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n');
  await openssl('ecparam', '-genkey', '-name', 'prime256v1', '-noout', '-out', `${ca}.key`);
  await openssl('req', '-new', '-x509', '-sha256', '-key', `${ca}.key`, '-out', `${ca}.crt`, '-days', '1', '-config', `${ca}.cnf`);
  const leaf = join(scratch, 'leaf');
  await openssl('ecparam', '-genkey', '-name', 'prime256v1', '-noout', '-out', `${leaf}.key`);
  await writeFile(`${leaf}-request.cnf`, '[req]\nprompt=no\ndistinguished_name=dn\n[dn]\nCN=Synthetic Session Leaf\n');
  await openssl('req', '-new', '-key', `${leaf}.key`, '-config', `${leaf}-request.cnf`, '-out', `${leaf}.csr`);
  const key = await readFile(`${leaf}.key`), leaves = {};
  for (const [name, ip] of [['valid', '127.0.0.1'], ['wrong-host', '127.0.0.2']]) {
    await writeFile(`${leaf}-${name}.cnf`, `basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost,IP:${ip}\n`);
    await openssl('x509', '-req', '-sha256', '-in', `${leaf}.csr`, '-CA', `${ca}.crt`, '-CAkey', `${ca}.key`, '-CAcreateserial', '-out', `${leaf}-${name}.crt`, '-days', '1', '-extfile', `${leaf}-${name}.cnf`);
    await openssl('verify', '-CAfile', `${ca}.crt`, `${leaf}-${name}.crt`);
    leaves[name] = { key, cert: await readFile(`${leaf}-${name}.crt`) };
  }
  const rootBytes = await readFile(`${ca}.crt`);
  const rootCert = new X509Certificate(rootBytes), validCert = new X509Certificate(leaves.valid.cert);
  assert.notEqual(rootCert.subject, validCert.subject, 'Synthetic CA and leaf subjects must differ');
  assert.equal(validCert.issuer, rootCert.subject, 'Synthetic leaf must be issued by the fixture CA');
  assert.ok(validCert.verify(rootCert.publicKey), 'Synthetic leaf signature must verify');
  return { ca: `${ca}.crt`, rootBytes, leaves };
}
async function bounded(promise, ms, label) {
  let timer;
  try { return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(Error(label)), ms); })]); }
  finally { clearTimeout(timer); }
}
async function untilFixed(predicate, label) {
  const deadline = Date.now() + 2000;
  while (!await predicate()) { if (Date.now() >= deadline) throw Error(label); await delay(1); }
}
function channel(socket) {
  let input = '', waiting;
  const queue = [];
  socket.on('data', bytes => {
    input += bytes.toString('utf8'); assert.ok(input.length <= 65536, 'idle control response bound');
    while (input.includes('\n')) {
      const end = input.indexOf('\n'), value = JSON.parse(input.slice(0, end)); input = input.slice(end + 1);
      if (waiting) { const send = waiting; waiting = undefined; send.resolve(value); }
      else { assert.ok(queue.length < 16); queue.push(value); }
    }
  });
  const failed = () => { if (waiting) { waiting.reject(Error('idle control closed')); waiting = undefined; } };
  socket.on('error', failed); socket.on('end', failed);
  const next = () => bounded(queue.length ? Promise.resolve(queue.shift()) : new Promise((resolve, reject) => { assert.equal(waiting, undefined); waiting = { resolve, reject }; }), 2000, 'idle control reply deadline');
  return { next, command: async value => { socket.write(JSON.stringify(value) + '\n'); return next(); } };
}
function request(port, ordinal) {
  return new Promise((resolve, reject) => {
    const req = http.get({ host: '127.0.0.1', port, path: `/v1/models/session-${ordinal}`, headers: { 'x-api-key': h.token } }, response => {
      let bytes = Buffer.alloc(0);
      response.on('data', chunk => { if (bytes.length + chunk.length > 65536) { req.destroy(); reject(Error('idle response bound')); } else bytes = Buffer.concat([bytes, chunk]); });
      response.on('end', () => { clearTimeout(timer); resolve({ status: response.statusCode, complete: response.complete, body: bytes.toString('hex') }); });
      response.once('error', error => { clearTimeout(timer); reject(error); });
    });
    const timer = setTimeout(() => req.destroy(Error('idle request deadline')), 2000);
    req.once('error', error => { clearTimeout(timer); reject(error); });
  });
}
async function runCase(name, version, mode, certificates) {
  const live = new Map(), handshakes = [], requests = [], responses = [], driver = [];
  let connectionCount = 0, peerCount = 0, firstSocket, secondPending, child, controlSocket, final, diagnostic = '';
  const provider = h.watch(tls.createServer({ ...certificates.leaves.valid, minVersion: version, maxVersion: version }, socket => {
    const id = ++peerCount; handshakes.push({ connection: id, resumed: socket.isSessionReused() });
    let input = Buffer.alloc(0);
    socket.on('data', chunk => {
      input = Buffer.concat([input, chunk]); assert.ok(input.length <= 16384);
      const end = input.indexOf('\r\n\r\n'); if (end < 0) return;
      const target = input.toString('latin1', 0, end).split(' ')[1]; input = input.subarray(end + 4);
      assert.match(target, /^\/v1\/models\/session-[0-2]$/);
      const requestOrdinal = Number(target.at(-1));
      requests.push({ request: requestOrdinal, connection: id, resumed: socket.isSessionReused() });
      const keep = mode !== 'unbarriered' || requestOrdinal === 0;
      const body = 'synthetic session';
      const wire = `HTTP/1.1 200 OK\r\ncontent-length: ${body.length}\r\nconnection: ${keep ? 'keep-alive' : 'close'}\r\n\r\n${body}`;
      if (requestOrdinal === 0) firstSocket = socket;
      if (mode === 'unbarriered' && requestOrdinal === 0) socket.write(wire, () => socket.destroy());
      else if (!keep) socket.end(wire);
      else socket.write(wire);
    });
    socket.on('error', () => {});
  }));
  provider.on('connection', socket => { const id = ++connectionCount; assert.ok(id <= 8, 'idle peer connection bound'); live.set(id, socket); socket.once('close', () => live.delete(id)); });
  provider.on('tlsClientError', () => {});
  const controls = h.watch(net.createServer());
  let finalResult;
  try {
    await bounded((async () => {
    provider.listen(0, '127.0.0.1'); controls.listen(0, '127.0.0.1');
    await bounded(Promise.all([once(provider, 'listening'), once(controls, 'listening')]), 2000, 'idle listener deadline');
    const controlConnection = once(controls, 'connection');
    const directory = join(scratch, `${name}-${version}-${mode}-${Date.now()}`); await mkdir(directory);
    const upstream = `https://127.0.0.1:${provider.address().port}`;
    const environment = { HOME: directory, XDG_CONFIG_HOME: directory, TMPDIR: directory, PATH: directory,
      AUTOROUTER_UPSTREAM_URL: upstream, AUTOROUTER_JEV_URL: `http://127.0.0.1:${provider.address().port}/v1/systemone`, AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: 'synthetic-evaluator', ANTHROPIC_API_KEY: 'synthetic-provider', AUTOROUTER_TOKEN: h.token,
      AUTOROUTER_SYNTHETIC_IDLE_CONTROL: `127.0.0.1:${controls.address().port}`, NODE_EXTRA_CA_CERTS: certificates.ca, SSL_CERT_FILE: empty, SSL_CERT_DIR: scratch,
      NODE_OPTIONS: `--tls-min-${version === 'TLSv1.2' ? 'v1.2 --tls-max-v1.2' : 'v1.3 --tls-max-v1.3'}` };
    const args = name === 'node' ? [join(root, 'rust/parity/idle-close-reference.mjs'), h.identity.reference] : ['--exact', 'tls_roots::openssl_spike::idle_close_tests::controlled_gateway_child', '--nocapture'];
    child = spawn(name === 'node' ? process.execPath : join(scratch, 'candidate'), args, { cwd: directory, env: environment, stdio: ['ignore', 'ignore', 'pipe'] });
    ownedChildren.add(child);
    child.stderr.on('data', bytes => { if (diagnostic.length + bytes.length > 65536) child.kill('SIGKILL'); else diagnostic += bytes; });
    const exited = new Promise(resolve => { child.once('error', () => resolve()); child.once('close', resolve); });
    child.idleExit = exited;
    [controlSocket] = await bounded(controlConnection, 2000, 'idle child control connection');
    const control = channel(controlSocket), ready = await control.next(); assert.ok(Number.isInteger(ready.port));
    let snapshot;
      responses.push(await request(ready.port, 0));
      assert.equal(responses[0].status, 200); assert.equal(responses[0].body, Buffer.from('synthetic session').toString('hex'));
      if (mode === 'unbarriered') {
        await until(() => live.size === 0, 'original peer-side close barrier');
        responses.push(await request(ready.port, 1));
        await until(() => live.size === 0, 'original second close barrier');
      } else if (mode === 'observed-close') {
        driver.push('close-first-peer'); firstSocket.destroy();
        await untilFixed(async () => { snapshot = await control.command({ op: 'snapshot' }); return snapshot.rows.includes('Released(1)'); }, 'observed client transport closure');
        assert.ok(snapshot.rows.includes('ReadEof(1)') || snapshot.rows.some(row => /^(ReadError|SocketError)\(1,/.test(row)), 'actual read-terminal evidence before next request');
        driver.push('observed-client-transport-closed');
        responses.push(await request(ready.port, 1)); assert.equal(responses[1].status, 200);
      } else {
        assert.equal(mode, 'selected-before-close');
        await control.command({ op: 'hold', kind: 'read', connection: 1 });
        await control.command({ op: 'hold', kind: 'write', connection: 1 });
        secondPending = request(ready.port, 1); secondPending.catch(() => {});
        await untilFixed(async () => { snapshot = await control.command({ op: 'snapshot' }); return snapshot.rows.includes('WriteHeld(1)'); }, 'selected request write gate');
        assert.ok(snapshot.selected.some(([request, connection]) => request === 2 && connection === 1));
        assert.ok(!snapshot.rows.includes('Released(1)'));
        driver.push('observed-exact-second-assignment'); firstSocket.destroy();
        await untilFixed(() => live.size === 0, 'selected peer close barrier');
        driver.push('peer-close-before-release'); await control.command({ op: 'release' });
        responses.push(await secondPending); secondPending = undefined; assert.equal(responses[1].status, 502);
        assert.equal(connectionCount, 1, 'no request retry or fresh fallback after assignment');
      }
      responses.push(await request(ready.port, 2)); assert.equal(responses[2].status, 200);
      if (mode === 'unbarriered') await until(() => live.size === 0, 'original final close barrier');
      snapshot = await control.command({ op: 'snapshot' });
      final = await control.command({ op: 'stop' }); assert.equal(final.cleanup, true);
      await bounded(exited, 2000, 'idle child exit'); assert.equal(child.exitCode, 0); ownedChildren.delete(child);
      finalResult = { responses, connections: connectionCount, handshakes, requests, driver, before_cleanup: snapshot, cleanup: final, child_exit: child.exitCode, diagnostic: diagnostic.replaceAll(scratch, '<fixture>') };
    })(), 8000, 'idle scenario outer deadline');
  } catch (error) {
    error.observed = { name, version, mode, responses, connections: connectionCount, handshakes, requests, driver, child_exit: child?.exitCode, diagnostic: diagnostic.replaceAll(scratch, '<fixture>') };
    throw error;
  } finally {
    if (child && ownedChildren.has(child)) {
      child.kill('SIGTERM');
      await bounded(child.idleExit, 2000, 'idle failed-child cleanup').catch(async () => { child.kill('SIGKILL'); await bounded(child.idleExit, 2000, 'idle failed-child kill'); });
      ownedChildren.delete(child);
    }
    controlSocket?.destroy();
    for (const socket of live.values()) socket.destroy();
    await Promise.all([h.close(provider), h.close(controls)]);
    await untilFixed(() => live.size === 0, 'idle owned peer cleanup');
    if (secondPending) await secondPending.catch(() => {});
  }
  return finalResult;
}
let fatal, failed;
try {
  const certificates = await fixtures();
  for (const version of ['TLSv1.2', 'TLSv1.3']) for (const mode of ['observed-close', 'selected-before-close', 'unbarriered', 'unbarriered', 'unbarriered']) {
    const row = { id: `${version}-${mode}-${results.length}`, version, mode };
    row.node = await runCase('node', version, mode, certificates);
    row.rust = await runCase('rust', version, mode, certificates);
    const application = value => ({ responses: value.responses, connections: value.connections, handshakes: value.handshakes, requests: value.requests });
    row.matched = JSON.stringify(application(row.node)) === JSON.stringify(application(row.rust));
    row.cleanup = row.node.cleanup.cleanup && row.rust.cleanup.cleanup;
    results.push(row); console.log(JSON.stringify({ id: row.id, matched: row.matched, cleanup: row.cleanup }));
  }
} catch (error) { fatal = String(error.message).replaceAll(scratch, '<fixture>'); failed = error.observed; }
finally { for (const child of ownedChildren) child.kill('SIGKILL'); await h.cleanup(); }
const paths = ['rust/Cargo.lock', 'rust/parity/check-openssl-idle-close.mjs', 'rust/parity/idle-close-reference.mjs', 'rust/parity/check-openssl-sessions.mjs', 'rust/crates/autorouter-runtime/src/tls_roots/openssl_spike.rs', ...['idle_close', 'idle_close_tests', 'client'].map(name => `rust/crates/autorouter-runtime/src/tls_roots/openssl_spike/${name}.rs`)];
const report = { schema_version: 1, kind: 'test_only_idle_close_barrier_characterization', identity: h.identity, passed: !fatal && results.length === 10 && results.every(row => row.matched && row.cleanup), matched: results.filter(row => row.matched).length, count: results.length, production_eligible: false, source_sha256: Object.fromEntries(await Promise.all(paths.map(async path => [path, hash(await readFile(join(root, path)))]))), ...(fatal ? { fatal, failed } : {}), results,
  limits: ['Original13/18 and opt-in15/18 expectations and reports are unchanged.', 'Observed transport closure is not direct pool insertion/removal observation.', 'Node read pause and request _flush gate, and native AsyncRead/AsyncWrite gates are explicitly instrumented schedules.', 'Unbarriered repetitions preserve requests/provider close barriers but observation instrumentation may perturb scheduling.', 'No body read-ahead, TLS handshake change, retry change, production promotion or performance claim.'] };
const bytes = JSON.stringify(report, null, 2) + '\n', path = join(root, 'artifacts/rust-rewrite/evidence', `openssl-idle-close-${hash(bytes)}.json`);
await writeFile(path, bytes, { flag: 'wx' }).catch(error => { if (error.code !== 'EEXIST') throw error; });
console.log(JSON.stringify({ report: path, passed: report.passed, matched: report.matched, count: report.count, fatal }));
if (!report.passed) process.exitCode = 1;
