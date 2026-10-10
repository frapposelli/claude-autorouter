// Synthetic raw-agent pool policy reference capture; no production transport switch.
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { createHash, X509Certificate } from 'node:crypto';
import { once } from 'node:events';
import { chmod, copyFile, mkdir, readFile, writeFile } from 'node:fs/promises';
import http from 'node:http';
import net from 'node:net';
import { join } from 'node:path';
import tls from 'node:tls';
import { setTimeout as delay } from 'node:timers/promises';
import { promisify } from 'node:util';
import { transportHarness } from './transport-harness.mjs';
const referenceOnly = process.argv.includes('--reference-only');
if (referenceOnly) process.argv.splice(process.argv.indexOf('--reference-only'), 1);
const busyOnly = process.argv.includes('--busy-only');
if (busyOnly) process.argv.splice(process.argv.indexOf('--busy-only'), 1);
const nativeOnly = process.argv.includes('--native-only');
if (nativeOnly) process.argv.splice(process.argv.indexOf('--native-only'), 1);
const caseArg = process.argv.find(value => value.startsWith('--case='));
if (caseArg) process.argv.splice(process.argv.indexOf(caseArg), 1);
const protocolArg = process.argv.find(value => value.startsWith('--protocol='));
if (protocolArg) process.argv.splice(process.argv.indexOf(protocolArg), 1);
const versions = protocolArg ? [protocolArg.slice('--protocol='.length)] : ['HTTP','TLSv1.2','TLSv1.3'];
assert.ok(versions.every(version => ['HTTP','TLSv1.2','TLSv1.3'].includes(version)), 'Declared protocol');
assert.ok(!(referenceOnly && nativeOnly), 'Modes are mutually exclusive');
assert.equal(process.argv.length, 4, 'Usage: check-openssl-raw-pool.mjs <frozen-reference> <runtime-test-executable> --reference-only');
assert.equal(process.version, 'v22.14.0');
const hints = [
  ['none', undefined], ['zero', 'timeout=0'], ['one', 'timeout=1'], ['two', 'timeout=2'], ['six', 'timeout=6'],
  ['case', 'Timeout=2'], ['other-first', 'max=7, timeout=2'], ['joined-first', 'timeout=2, max=7'],
  ['huge', `timeout=${'9'.repeat(100)}`], ['fraction', 'timeout=1.5'], ['negative', 'timeout=-2'],
  ['duplicate-empty-first', ['', 'timeout=2']], ['duplicate-timeout-first', ['timeout=2', '']],
];
const cases = [
  ...hints.map(([id, hint]) => ({ id: `hint-${id}`, kind: 'hint', hint })),
  { id: 'default-idle-expiry', kind: 'expiry' }, { id: 'hint-idle-expiry', kind: 'expiry', hint: 'timeout=2' },
  { id: 'lifo-three', kind: 'lifo' }, { id: 'free-cap-257', kind: 'cap' },
  { id: 'free-cap-257-veto-last', kind: 'cap', lastHint: 'timeout=0' },
  { id: 'staggered-idle-expiry', kind: 'staggered' },
  { id: 'busy-timeout-then-free', kind: 'busy' },
  { id: 'hinted-busy-timeout-then-free', kind: 'busy', hint: 'timeout=2', warm: true },
].filter(spec => (!busyOnly || spec.kind === 'busy') && (!caseArg || spec.id === caseArg.slice('--case='.length)));
assert.ok(cases.length, 'Declared case selection');
const previousSignals = Object.fromEntries(['SIGINT', 'SIGTERM'].map(signal => [signal, process.listeners(signal)]));
const h = await transportHarness('autorouter-raw-pool-', { candidateKind: referenceOnly ? 'not executed; raw-pool reference-only characterization' : 'test-only raw-pool owned HTTP1 adapter' });
const { root, scratch } = h;
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const run = promisify(execFile), empty = join(scratch, 'empty.cnf');
const ownedChildren = new Set(), results = [];
let interrupted, abortScenario;
const signals = Object.fromEntries(['SIGINT', 'SIGTERM'].map(signal => {
  for (const listener of process.listeners(signal)) if (!previousSignals[signal].includes(listener)) process.off(signal, listener);
  const handler = () => { interrupted = signal; abortScenario?.(Error(`raw pool interrupted by ${signal}`)); };
  process.on(signal, handler); return [signal, handler];
}));
const sources = ['rust/parity/check-openssl-raw-pool.mjs', 'rust/parity/raw-pool-reference.mjs', 'rust/parity/transport-harness.mjs', 'scripts/rust-reference.mjs', 'rust/parity/baseline.json'];
const sourceHashes = {};
for (const path of sources) {
  const bytes = await readFile(join(root, path)); sourceHashes[path] = hash(bytes);
  const target = join(scratch, 'inputs', path); await mkdir(join(target, '..'), { recursive: true }); await writeFile(target, bytes);
  const evidence = join(root, 'artifacts/rust-rewrite/evidence', `${hash(bytes)}.${path.endsWith('.json') ? 'json' : 'mjs'}`);
  await writeFile(evidence, bytes, { flag: 'wx' }).catch(error => { if (error.code !== 'EEXIST') throw error; });
  assert.equal(hash(await readFile(evidence)),hash(bytes));
}
const builtinHashes = Object.fromEntries(['_http_agent', '_http_client', '_http_incoming', 'net', 'https'].map(name => [name,hash(process.binding('natives')[name])]));

await copyFile(process.execPath, join(scratch, 'node-reference')); await chmod(join(scratch, 'node-reference'), 0o700);
assert.equal(hash(await readFile(join(scratch, 'node-reference'))), h.identity.node_executable_sha256);
async function openssl(...args) { return run('openssl', args, { timeout: 10000, maxBuffer: 1024 * 1024, env: { PATH: process.env.PATH, OPENSSL_CONF: empty } }); }
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
async function pollUntil(predicate, label, ms = 2000, signal) {
  const deadline = Date.now() + ms;
  for (;;) { if (signal?.aborted) throw Error('raw pool barrier cancelled'); if (await predicate()) return; if (Date.now() >= deadline) throw Error(label); await delay(10, undefined, { signal }); }
}
function channel(socket, onFailure) {
  let input = '', waiting;
  const queue = [];
  socket.on('data', bytes => {
    try {
    input += bytes.toString('utf8'); assert.ok(Buffer.byteLength(input) <= 262144, 'pool control response bound');
    while (input.includes('\n')) {
      const end = input.indexOf('\n'), value = JSON.parse(input.slice(0, end)); input = input.slice(end + 1);
      if (waiting) { const send = waiting; waiting = undefined; send.resolve(value); }
      else { assert.ok(queue.length < 8); queue.push(value); }
    }
    } catch (error) { onFailure(error); if (waiting) { waiting.reject(error); waiting = undefined; } }
  });
  const failed = () => { if (waiting) { waiting.reject(Error('pool control closed')); waiting = undefined; } };
  socket.on('error', failed); socket.on('end', failed); socket.on('close', failed);
  const next = () => bounded(queue.length ? Promise.resolve(queue.shift()) : new Promise((resolve, reject) => { assert.equal(waiting, undefined); waiting = { resolve, reject }; }), 2000, 'pool control reply deadline');
  return { next, command: async value => { socket.write(JSON.stringify(value) + '\n'); return next(); } };
}
function request(port, ordinal, pending) {
  let req;
  const result = new Promise((resolve, reject) => {
    req = http.get({ host: '127.0.0.1', port, path: `/v1/models/pool-${ordinal}`, headers: { 'x-api-key': h.token } }, response => {
      const digest = createHash('sha256'); let bytes = 0;
      response.on('data', chunk => { bytes += chunk.length; if (bytes > 65536) req.destroy(Error('pool response bound')); else digest.update(chunk); });
      response.once('end', () => { clearTimeout(timer); resolve({ status: response.statusCode, complete: response.complete, bytes, sha256: digest.digest('hex') }); });
      response.once('error', error => { clearTimeout(timer); reject(error); });
    });
    const timer = setTimeout(() => req.destroy(Error('pool request deadline')), 14000);
    req.once('error', error => { clearTimeout(timer); reject(error); });
  });
  pending.add(req); result.finally(() => pending.delete(req)).catch(() => {}); result.catch(() => {});
  return result;
}
function successful(value) { assert.equal(value.status, 200); assert.equal(value.complete, true); assert.equal(value.bytes, 1); assert.equal(value.sha256, hash(Buffer.from('x'))); }
async function runCase(spec, version, certificates, implementation) {
  const live = new Map(), requests = [], wireCloses = [], replies = new Map(), handshakes = [], pending = new Set(), responses = [], stages = [];
  const caseStop = new AbortController();
  const until = (predicate, label, ms) => pollUntil(predicate, label, ms, caseStop.signal);
  const alive = () => { if (caseStop.signal.aborted) throw Error('raw pool scenario already stopped'); };
  const caseRequest = (...args) => { alive(); return request(...args); };
  const caseDelay = ms => delay(ms, undefined, { signal: caseStop.signal });
  let work;
  let rejectObservation;
  const observationFailed = new Promise((_, reject) => { rejectObservation = reject; }); observationFailed.catch(() => {});
  const guard = fn => (...args) => { try { return fn(...args); } catch (error) { rejectObservation(error); } };
  abortScenario = rejectObservation;
  let accepted = 0, secure = 0, child, controlSocket, diagnostic = '', control, output, snapshot, ready, failed;
  const onStream = socket => {
    const id = version === 'HTTP' ? accepted : ++secure;
    socket.once('close', () => wireCloses.push(id));
    if (version !== 'HTTP') handshakes.push({ connection: id, resumed: socket.isSessionReused() });
    let input = Buffer.alloc(0);
    socket.on('error', () => {});
    socket.on('data', guard(chunk => {
      input = Buffer.concat([input, chunk]); assert.ok(input.length <= 16384, 'pool header bound');
      const end = input.indexOf('\r\n\r\n'); if (end < 0) return;
      const target = input.toString('latin1', 0, end).split(' ')[1]; input = input.subarray(end + 4);
      assert.match(target, /^\/v1\/models\/pool-[0-9]{1,3}$/); const ordinal = Number(target.split('-').at(-1));
      assert.ok(requests.length < 520); requests.push({ request: ordinal, connection: id });
      const hint = ordinal === 256 && spec.lastHint !== undefined ? spec.lastHint : spec.hint;
      const reply = () => socket.write(`HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: keep-alive\r\n${hint === undefined ? '' : (Array.isArray(hint) ? hint : [hint]).map(value => `Keep-Alive: ${value}\r\n`).join('')}\r\nx`);
      if (['lifo', 'cap', 'staggered'].includes(spec.kind)) { assert.ok(!replies.has(ordinal)); replies.set(ordinal, reply); }
      else reply();
    }));
  };
  const provider = h.watch(version === 'HTTP' ? net.createServer() : tls.createServer({ ...certificates.leaves.valid, minVersion: version, maxVersion: version }, guard(onStream)));
  provider.on('connection', guard(socket => { const id = ++accepted; assert.ok(id <= 260, 'pool connection bound'); live.set(id, socket); socket.once('close', () => live.delete(id)); if (version === 'HTTP') onStream(socket); }));
  provider.on('tlsClientError', () => {});
  const controls = h.watch(net.createServer());
  const release = ordinal => { const reply = replies.get(ordinal); assert.ok(reply, 'owned provider reply'); replies.delete(ordinal); reply(); };
  const capture = async label => { snapshot = await control.command({ op: 'snapshot' }); stages.push({ label, snapshot, wire_requests: requests.map(row => ({...row})), wire_closes: [...wireCloses] }); return snapshot; };
  const waitFree = async count => until(async () => { snapshot = await control.command({ op: 'snapshot' }); return snapshot.free.flat().length === count; }, `pool free count ${count}`);
  try {
    work = (async () => {
      provider.listen(0, '127.0.0.1'); controls.listen(0, '127.0.0.1');
      await bounded(Promise.all([once(provider, 'listening'), once(controls, 'listening')]), 2000, 'pool listener deadline');
      const connecting = once(controls, 'connection');
      const directory = join(scratch, `${implementation}-${version}-${spec.id}`); await mkdir(directory);
      const upstream = `${version === 'HTTP' ? 'http' : 'https'}://127.0.0.1:${provider.address().port}`;
      const env = { HOME: directory, XDG_CONFIG_HOME: directory, TMPDIR: directory, PATH: directory,
        AUTOROUTER_UPSTREAM_URL: upstream, AUTOROUTER_JEV_URL: `http://127.0.0.1:${provider.address().port}/v1/systemone`, AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: 'synthetic-evaluator', ANTHROPIC_API_KEY: 'synthetic-provider', AUTOROUTER_TOKEN: h.token,
        AUTOROUTER_SYNTHETIC_POOL_CONTROL: `127.0.0.1:${controls.address().port}`, NODE_EXTRA_CA_CERTS: certificates.ca, SSL_CERT_FILE: empty, SSL_CERT_DIR: scratch,
        AUTOROUTER_SYNTHETIC_POOL_TIMERS: spec.kind === 'busy' ? '1' : '0',
        ...(version === 'HTTP' ? {} : { NODE_OPTIONS: `--tls-min-${version === 'TLSv1.2' ? 'v1.2 --tls-max-v1.2' : 'v1.3 --tls-max-v1.3'}` }) };
      alive();
      child = spawn(join(scratch, implementation === 'node' ? 'node-reference' : 'candidate'), implementation === 'node' ? [join(scratch, 'inputs/rust/parity/raw-pool-reference.mjs'), h.identity.reference] : ['--exact', 'tls_roots::openssl_spike::raw_pool::child::controlled_gateway_child', '--nocapture'], { cwd: directory, env, stdio: ['ignore', 'ignore', 'pipe'] });
      ownedChildren.add(child); child.stderr.on('data', bytes => { if (Buffer.byteLength(diagnostic) + bytes.length > 65536) child.kill('SIGKILL'); else diagnostic += bytes; });
      child.finished = new Promise(resolve => { child.once('error', resolve); child.once('close', resolve); });
      [controlSocket] = await bounded(connecting, 2000, 'pool reference startup'); control = channel(controlSocket, rejectObservation); const command = control.command; control.command = value => { alive(); return command(value); }; ready = await control.next();
      if (implementation === 'node') assert.deepEqual(ready.builtins, builtinHashes, 'Pinned executable builtin source identity');
      if (spec.kind === 'lifo' || spec.kind === 'cap') {
        const n = spec.kind === 'cap' ? 257 : 3;
        const jobs = [];
        for (let i = 0; i < n; i++) { jobs.push(caseRequest(ready.port, i, pending)); await until(() => replies.has(i), 'serial admission with all responses held'); }
        await until(() => replies.size === n, 'all concurrent pool requests received');
        for (let i = 0; i < n; i++) {
          release(i); const response = await jobs[i]; successful(response); responses.push(response);
          await waitFree(Math.min(i + 1, 256));
        }
        if (spec.kind === 'cap') {
          const rejectedWire = requests.find(row => row.request === 256).connection;
          await until(async () => live.size === 256 && wireCloses.includes(rejectedWire) && (await control.command({ op: 'snapshot' })).live === 256, 'exact cap excess peer closes before snapshot');
          assert.deepEqual(wireCloses, [rejectedWire], 'only rejected physical connection closes');
        }
        await capture('all-complete-and-free');
        const selectionOrder = snapshot.free.flat(); assert.equal(selectionOrder.length, Math.min(n, 256));
        const next = caseRequest(ready.port, n, pending); await until(() => replies.has(n), 'next LIFO request'); release(n);
        const response = await next; successful(response); responses.push(response); await waitFree(Math.min(n, 256)); await capture('next-complete');
        const assignment = snapshot.rows.find(row => row.event === 'assigned' && row.request === n); assert.equal(assignment.connection, selectionOrder.at(-1), 'actual LIFO selection');
        assert.equal(requests.find(row => row.request === n).connection, requests.find(row => row.request === Math.min(n, 256) - 1).connection, 'independent provider wire confirms exact retained LIFO connection');
        assert.equal(accepted, n);
        if (spec.kind === 'cap') { await until(() => live.size === 256, 'cap excess connection actually closed'); assert.equal(snapshot.rows.filter(row => row.event === 'free' && !row.kept).length, 1); }
      } else if (spec.kind === 'staggered') {
        const first = caseRequest(ready.port, 0, pending); await until(() => replies.has(0), 'first staggered admission held');
        const second = caseRequest(ready.port, 1, pending);
        await until(() => replies.size === 2, 'staggered requests'); release(0); successful(await first); await waitFree(1); await capture('first-idle-second-active');
        await caseDelay(2000); release(1); successful(await second); await waitFree(2); await capture('second-idle');
        await until(async () => { snapshot = await control.command({ op: 'snapshot' }); return snapshot.rows.some(row => row.event === 'close'); }, 'first staggered expiry', 7000);
        stages.push({ label: 'first-closed', snapshot }); assert.equal(snapshot.free.flat().length, 1);
        await until(() => live.size === 0, 'second staggered expiry', 7000); await capture('both-closed');
      } else if (spec.kind === 'busy') {
        const heldRequest = spec.warm ? 1 : 0;
        if (spec.warm) { const warm = await caseRequest(ready.port, 0, pending); successful(warm); responses.push(warm); await waitFree(1); }
        await control.command({ op: 'hold', request: heldRequest }); const first = caseRequest(ready.port, heldRequest, pending);
        await until(async () => { snapshot = await control.command({ op: 'snapshot' }); return snapshot.held.includes(heldRequest); }, 'response held');
        await capture('held-complete-response'); assert.equal(snapshot.free.flat().length, 0);
        await until(async () => { snapshot = await control.command({ op: 'snapshot' }); return snapshot.rows.some(row => row.event === 'timeout'); }, 'busy socket timeout observed', spec.warm ? 2000 : 7000);
        stages.push({ label: 'busy-timeout', snapshot }); assert.equal(snapshot.live, 1); assert.equal(snapshot.free.flat().length, 0); assert.equal(snapshot.rows.find(row => row.event === 'timeout').free, false);
        await control.command({ op: 'release', request: heldRequest }); successful(await first); await waitFree(1); await capture('free-after-busy-timeout');
        await caseDelay(spec.warm ? 1500 : 5500); await capture('free-at-post-timeout-barrier');
        const response = await caseRequest(ready.port, heldRequest + 1, pending); successful(response); responses.push(response); await capture('request-after-barrier');
      } else {
        const response = await caseRequest(ready.port, 0, pending); successful(response); responses.push(response);
        await until(async () => { snapshot = await control.command({ op: 'snapshot' }); return snapshot.rows.some(row => row.event === 'free'); }, 'first free decision'); await capture('first-free-decision');
        if (spec.kind === 'expiry') { await until(() => live.size === 0, 'observed idle expiry', 7000); await capture('idle-expired'); }
        const next = await caseRequest(ready.port, 1, pending); successful(next); responses.push(next); await capture('second-complete');
      }
      const final = await control.command({ op: 'stop' }); assert.equal(final.cleanup, true);
      await bounded(child.finished, 2000, 'pool child exit'); assert.equal(child.exitCode, 0); ownedChildren.delete(child);
      output = { id: `${version}-${spec.id}`, implementation, spec, version, ready, connections: accepted, requests, handshakes, wire_closes: wireCloses, responses, stages, cleanup: final, child_exit: child.exitCode, diagnostic: diagnostic.replaceAll(scratch, '<fixture>') };
    })();
    await bounded(Promise.race([observationFailed, work]), spec.kind === 'cap' ? 20000 : 15000, 'raw pool scenario outer deadline');
  } catch (error) { failed = error; output = { id: `${version}-${spec.id}`, implementation, spec, version, failure: String(error.message), connections: accepted, requests, handshakes, wire_closes: wireCloses, responses, stages, last_snapshot: snapshot, diagnostic: diagnostic.replaceAll(scratch, '<fixture>') }; }
  finally {
    const cleanupFailures = [];
    caseStop.abort();
    for (const req of pending) req.destroy();
    if (child && ownedChildren.has(child)) {
      child.kill('SIGTERM');
      try { await bounded(child.finished, 2000, 'pool failed child cleanup'); }
      catch { child.kill('SIGKILL'); try { await bounded(child.finished, 2000, 'pool failed child kill'); } catch (error) { cleanupFailures.push(error.message); } }
      if (child.exitCode !== null || child.signalCode !== null) ownedChildren.delete(child);
    }
    controlSocket?.destroy(); for (const socket of live.values()) socket.destroy();
    for (const result of await Promise.allSettled([h.close(provider), h.close(controls), pollUntil(() => live.size === 0 && pending.size === 0, 'pool owned peer/request cleanup')])) if (result.status === 'rejected') cleanupFailures.push(result.reason.message);
    if (work) await bounded(work.catch(() => {}), 2500, 'pool scenario work quiescence').catch(error => cleanupFailures.push(error.message));
    output ??= { id: `${version}-${spec.id}`, implementation, spec, version, connections: accepted, requests, handshakes, wire_closes: wireCloses, responses, stages };
    output.owner_cleanup = { child_reaped: !child || child.exitCode !== null || child.signalCode !== null, peer_sockets: live.size, pending_requests: pending.size, failures: cleanupFailures };
    if (cleanupFailures.length) { failed ??= Error('pool cleanup failed'); output.cleanup_failure = cleanupFailures; }
    abortScenario = undefined;
  }
  if (failed) output.failed = true;
  return output;
}
let fatal;
try {
  const certificates = await fixtures();
  for (const implementation of (referenceOnly ? ['node'] : nativeOnly ? ['native'] : ['node', 'native'])) for (const version of versions) for (const spec of cases) {
    if (interrupted) throw Error(`raw pool interrupted by ${interrupted}`);
    const result = await runCase(spec, version, certificates, implementation); results.push(result);
    console.log(JSON.stringify({ id: result.id, implementation: result.implementation, failed: Boolean(result.failed), connections: result.connections, ...(result.failure ? { failure: result.failure } : {}) }));
    if (result.failed) throw Error(`Declared raw-pool case failed: ${result.id}`);
  }
} catch (error) { fatal = String(error.message).replaceAll(scratch, '<fixture>'); }
finally {
  for (const child of ownedChildren) child.kill('SIGKILL');
  const reaped = await Promise.allSettled([...ownedChildren].map(child => bounded(child.finished, 2000, 'pool final child reap')));
  if (reaped.some(result => result.status === 'rejected')) fatal ??= 'pool final child reap failed';
  try { await h.cleanup(); } catch (error) { fatal ??= `harness cleanup: ${error.message}`; }
  for (const [signal, handler] of Object.entries(signals)) process.off(signal, handler);
}
for (const path of sources) { try { if (hash(await readFile(join(root,path))) !== sourceHashes[path]) fatal ??= `source changed during execution: ${path}`; } catch (error) { fatal ??= `source verification failed: ${path}: ${error.code}`; } }
const semantic = row => {
  const names = new Map();
  for (const stage of row.stages) for (const event of stage.snapshot.rows) if (event.event === 'assigned') {
    names.set(event.connection, Math.min(names.get(event.connection) ?? Infinity, event.request));
  }
  const physical = new Map();
  for (const request of row.requests) physical.set(request.connection, Math.min(physical.get(request.connection) ?? Infinity, request.request));
  const wire = row.requests.map(request => ({ request:request.request, connection:physical.get(request.connection) })).sort((a,b) => a.request-b.request);
  const observed = (row.stages.at(-1)?.snapshot.rows ?? []).filter(event => event.event === 'assigned').map(event => ({ request:event.request,connection:names.get(event.connection) })).sort((a,b) => a.request-b.request);
  assert.deepEqual(observed,wire,'Every probe assignment must match independent provider connection ownership, preserving duplicates');
  const identity = id => { assert.ok(names.has(id), 'Observed connection must have a request identity'); return names.get(id); };
  return {
    connections: row.connections, responses: row.responses, wire_ownership: wire,
    tls_resumption: row.handshakes.map(handshake => ({ first_request: Math.min(...row.requests.filter(request => request.connection === handshake.connection).map(request => request.request)), resumed: handshake.resumed })).sort((a,b) => a.first_request-b.first_request),
    stages: row.stages.map(({ label, snapshot: s }) => ({ label,
      free: s.free.map(group => group.map(identity)), live: s.live,
      ownership: s.rows.filter(r => ['assigned','free','timeout','close'].includes(r.event)).map(r => ({
        event:r.event, connection:identity(r.connection),
        ...(r.event === 'assigned' ? {request:r.request,reused:r.reused} : {}),
        ...(r.event === 'free' ? {kept:r.kept,timeout:r.timeout} : {}),
        ...(r.event === 'timeout' ? {free:r.free} : {}),
      })),
    })),
    cleanup: row.cleanup?.cleanup === true && row.owner_cleanup.child_reaped && row.owner_cleanup.peer_sockets === 0 && row.owner_cleanup.pending_requests === 0 && row.owner_cleanup.failures.length === 0,
  };
};
const comparisons = referenceOnly || nativeOnly ? [] : results.filter(row => row.implementation === 'node').map(reference => {
  const candidate = results.find(row => row.implementation === 'native' && row.id === reference.id);
  try { const expected = semantic(reference), actual = candidate && semantic(candidate); return { id: reference.id, matched: JSON.stringify(expected) === JSON.stringify(actual), expected, actual }; }
  catch (error) { return { id: reference.id, matched: false, comparison_error: error.message }; }
});
const declaredCount = cases.length * versions.length * (referenceOnly || nativeOnly ? 1 : 2);
const report = { schema_version: 1, kind: referenceOnly ? 'raw_pool_frozen_reference_characterization' : nativeOnly ? 'raw_pool_native_characterization' : 'raw_pool_differential', reference_only: referenceOnly, native_only: nativeOnly, production_eligible: false,
  identity: { ...h.identity, node_execution: 'isolated immutable byte-identical executable snapshot' }, source_sha256: sourceHashes, source_execution: 'reference helper and verifier run from immutable pre-execution source snapshot; all source hashes checked unchanged after execution', builtin_sha256: builtinHashes,
  completed: !fatal && results.length === declaredCount, count: results.length, declared_count: declaredCount, comparisons, matched: comparisons.filter(row => row.matched).length, ...(fatal ? { fatal } : {}), results,
  schedule: { admission: 'LIFO/cap requests admitted sequentially with every response held until all are active; replies then released one at a time with actual free barriers', historical_cap: 'Original concurrent-admission/concurrent-release reference and native snapshots are retained separately; this is a schedule refinement for deterministic ownership comparisons' },
  comparison_exclusions: ['elapsed timestamps', 'JS-only close.had_error and timeout.destroyed', 'JS timer-set/refresh callback traces', 'active/queued diagnostic arrays and per-socket destroyed flags', 'complete observer, parser-error TLS-cache, downstream forwarding and unbarriered close equivalence'],
  limits: ['Source-only global agent observations and explicitly declared Incoming.resume hold; no pool policy mutation.', 'Elapsed timeout observations are fixture-local, not performance or universal timing claims.', 'This fixed pool-policy matrix does not alone qualify fetch weak-cache, two-origin, credential, cancellation, downstream forwarding or unbarriered idle-close behavior.'] };
const bytes = JSON.stringify(report, null, 2) + '\n', path = join(root, 'artifacts/rust-rewrite/evidence', `openssl-raw-pool-${referenceOnly ? 'reference-' : ''}${hash(bytes)}.json`);
await writeFile(path, bytes, { flag: 'wx' }).catch(error => { if (error.code !== 'EEXIST') throw error; });
assert.equal(hash(await readFile(path)), hash(bytes), 'Content-addressed report bytes');
console.log(JSON.stringify({ report: path, completed: report.completed, count: report.count, matched: report.matched, fatal }));
if (!report.completed || (!referenceOnly && !nativeOnly && report.matched !== cases.length * versions.length)) process.exitCode = 1;
if (interrupted) process.exitCode = interrupted === 'SIGINT' ? 130 : 143;
