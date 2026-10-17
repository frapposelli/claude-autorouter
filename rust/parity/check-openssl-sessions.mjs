// Synthetic raw HTTPS session/lifecycle comparison; no shipping transport switch.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash, randomBytes, X509Certificate } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import http from 'node:http';
import { join } from 'node:path';
import tls from 'node:tls';
import { setTimeout as delay } from 'node:timers/promises';
import { promisify } from 'node:util';
import { transportHarness } from './transport-harness.mjs';

const referenceOnly = process.argv.at(-1) === '--reference-only';
if (referenceOnly) process.argv.pop();
assert.equal(process.argv.length, 4, 'Usage: check-openssl-sessions.mjs <frozen-reference> <runtime-lib-test-executable> [--reference-only]');
const h = await transportHarness('autorouter-openssl-session-', {
  candidateArguments: ['--exact', 'tls_roots::openssl_spike::gateway_child', '--nocapture'],
  candidateEnvironment: { AUTOROUTER_SYNTHETIC_OPENSSL_SPIKE: 'stage-b2' },
  candidateKind: 'runtime lib test-only OpenSSL B2 raw sessions',
});
const { scratch, root, watch, gateway } = h;
const results = [];
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const run = promisify(execFile);
const empty = join(scratch, 'empty.cnf');
async function openssl(...args) {
  return run('openssl', args, { timeout: 10000, maxBuffer: 1024 * 1024, env: { PATH: process.env.PATH, OPENSSL_CONF: empty } });
}
async function until(check, label) {
  for (let n = 0; n < 600; n++) { if (check()) return; await delay(5); }
  throw Error(label);
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
function request(port, path, cancelBody = false) {
  return new Promise(resolve => {
    let done = false;
    const finish = value => { if (!done) { done = true; clearTimeout(timer); resolve(value); } };
    const req = http.get({ host: '127.0.0.1', port, path, headers: { 'x-api-key': h.token } }, res => {
      let bytes = Buffer.alloc(0);
      res.on('data', chunk => {
        bytes = Buffer.concat([bytes, chunk]);
        if (bytes.length > 65536 || cancelBody) res.destroy();
      });
      res.on('end', () => finish({ status: res.statusCode, body: bytes.toString('hex'), complete: true }));
      res.on('error', () => finish({ status: res.statusCode, body: bytes.toString('hex'), complete: false }));
      res.on('close', () => { if (!res.complete) finish({ status: res.statusCode, body: bytes.toString('hex'), complete: false }); });
    });
    const timer = setTimeout(() => { req.destroy(); finish({ request_error: true, deadline: true }); }, 4000);
    req.on('error', () => finish({ request_error: true }));
  });
}
const cases = ['clean-close', 'keepalive', 'rotate-ticket-key', 'rejected-host-then-valid', 'invalid-head', 'short-body', 'abrupt-after-complete', 'abrupt-after-complete-barrier', 'cancel-body'];
async function runCase(name, row, certificates) {
  let connections = 0, step = 0;
  const live = new Set(), handshakes = [], requests = [];
  const options = { ...certificates.leaves[row.mode === 'rejected-host-then-valid' ? 'wrong-host' : 'valid'], minVersion: row.version, maxVersion: row.version };
  const provider = watch(tls.createServer(options, socket => {
    handshakes.push({ resumed: socket.isSessionReused(), protocol: socket.getProtocol() });
    let input = Buffer.alloc(0);
    socket.on('error', () => {});
    socket.on('data', chunk => {
      input = Buffer.concat([input, chunk]);
      if (input.length > 16384) { socket.destroy(); return; }
      const end = input.indexOf('\r\n\r\n');
      if (end < 0) return;
      const path = input.toString('latin1', 0, end).split(' ')[1];
      input = input.subarray(end + 4);
      requests.push({ path, resumed: socket.isSessionReused() });
      if (row.mode === 'invalid-head' && step === 0) {
        socket.end('HTTP/1.1 200 OK\r\ncontent-length: 1\r\ncontent-length: 1\r\n\r\nx');
      } else if (row.mode === 'short-body' && step === 0) {
        socket.end('HTTP/1.1 200 OK\r\ncontent-length: 20\r\nconnection: close\r\n\r\nshort');
      } else if (row.mode === 'cancel-body' && step === 0) {
        socket.write('HTTP/1.1 200 OK\r\ncontent-length: 10000\r\n\r\npartial');
      } else {
        const body = 'synthetic session';
        const keep = row.mode === 'keepalive' || (row.mode.startsWith('abrupt-after-complete') && step === 0);
        const wire = `HTTP/1.1 200 OK\r\ncontent-length: ${body.length}\r\nconnection: ${keep ? 'keep-alive' : 'close'}\r\n\r\n${body}`;
        if (row.mode === 'keepalive') socket.write(wire);
        else if (row.mode.startsWith('abrupt-after-complete') && step === 0) socket.write(wire, () => socket.destroy());
        else socket.end(wire);
      }
    });
  }));
  provider.on('connection', socket => { connections++; live.add(socket); socket.on('close', () => live.delete(socket)); });
  provider.on('tlsClientError', () => {});
  provider.listen(0, '127.0.0.1'); await once(provider, 'listening');
  if (row.mode === 'clean-close') {
    await new Promise((resolve, reject) => {
      const probe = tls.connect({ host: '127.0.0.1', port: provider.address().port, ca: certificates.rootBytes, minVersion: row.version, maxVersion: row.version }, () => { probe.end(); resolve(); });
      probe.setTimeout(3000, () => probe.destroy(Error('Synthetic certificate preflight deadline')));
      probe.once('error', error => reject(Error(`Synthetic certificate preflight failed: ${error.code}`)));
    });
    await until(() => live.size === 0, 'Synthetic certificate preflight close deadline');
    connections = 0; handshakes.length = 0;
  }
  const child = await gateway(name, `https://127.0.0.1:${provider.address().port}`, { NODE_EXTRA_CA_CERTS: certificates.ca, NODE_OPTIONS: `--tls-min-${row.version === 'TLSv1.2' ? 'v1.2 --tls-max-v1.2' : 'v1.3 --tls-max-v1.3'}`, SSL_CERT_FILE: empty, SSL_CERT_DIR: scratch });
  const responses = [];
  try {
    for (step = 0; step < 3; step++) {
      responses.push(await request(child.port, `/v1/models/session-${step}`, row.mode === 'cancel-body' && step === 0));
      if (row.mode !== 'keepalive') await until(() => live.size === 0, 'Synthetic peer close deadline');
      if (step === 0 && row.mode === 'abrupt-after-complete-barrier') {
        assert.equal((await request(child.port, '/health')).status, 200);
        assert.equal((await request(child.port, '/health')).status, 200);
      }
      if (step === 0 && row.mode === 'rotate-ticket-key') provider.setTicketKeys(randomBytes(48));
      if (step === 0 && row.mode === 'rejected-host-then-valid') {
        const keys = provider.getTicketKeys();
        provider.setSecureContext({ ...options, ...certificates.leaves.valid });
        provider.setTicketKeys(keys);
      }
    }
  } finally { await child.stop(); for (const socket of live) socket.destroy(); await h.close(provider); }
  const metrics = name === 'rust' ? JSON.parse(child.stderr().match(/OpenSSL session fixture: (\{[^\n]+\})/)?.[1] ?? 'null') : undefined;
  return { observed: { responses, connections, handshakes, requests }, diagnostics: child.stderr().replaceAll(scratch, '<fixture>'), ...(metrics ? { metrics } : {}) };
}
function applicationContract(observed, mode) {
  const [first, second, last] = observed.responses;
  const successful = value => value?.status === 200 && value.complete === true && value.body === Buffer.from('synthetic session').toString('hex');
  if (!successful(last)) return false;
  if (mode === 'abrupt-after-complete') return successful(first) && (successful(second) || second?.status === 502);
  if (!successful(second)) return false;
  if (['invalid-head', 'rejected-host-then-valid'].includes(mode)) return first?.status === 502 && first.complete === true;
  if (mode === 'short-body') return first?.status === 200 && first.complete === false && first.body === Buffer.from('short').toString('hex');
  if (mode === 'cancel-body') return first?.status === 200 && first.complete === false && first.body === Buffer.from('partial').toString('hex');
  return successful(first);
}
let fatal;
try {
  assert.equal(process.version, 'v22.14.0'); assert.equal(process.versions.openssl, '3.0.15+quic');
  const certificates = await fixtures();
  for (const version of ['TLSv1.2', 'TLSv1.3']) for (const mode of cases) {
    const row = { id: `${version}-${mode}`, version, mode };
    row.node = await runCase('node', row, certificates);
    row.node_contract = applicationContract(row.node.observed, mode);
    if (!referenceOnly) {
      row.rust = await runCase('rust', row, certificates);
      row.rust_contract = applicationContract(row.rust.observed, mode);
      row.matched = row.node_contract && row.rust_contract && JSON.stringify(row.node.observed) === JSON.stringify(row.rust.observed);
      row.cleanup = row.rust.metrics?.cleaned === true && row.rust.metrics?.cache_released === true && row.rust.metrics?.raw_active === 0;
    }
    results.push(row);
    console.log(JSON.stringify({ id: row.id, matched: row.matched, node_reused: row.node.observed.handshakes.map(v => v.resumed), rust_reused: row.rust?.observed.handshakes.map(v => v.resumed) }));
  }
} catch (error) { fatal = error.message.replaceAll(scratch, '<fixture>'); }
finally { await h.cleanup(); }
const paths = ['rust/Cargo.lock', 'rust/Cargo.toml', 'rust/crates/autorouter-runtime/Cargo.toml', 'rust/vendor/openssl-provenance.json', 'rust/crates/autorouter-runtime/src/tls_roots.rs', 'rust/crates/autorouter-runtime/src/tls_policy.rs', 'rust/parity/check-openssl-sessions.mjs', 'rust/parity/transport-harness.mjs', 'rust/crates/autorouter-runtime/src/tls_roots/openssl_spike.rs', ...['client', 'policy', 'session', 'options'].map(name => `rust/crates/autorouter-runtime/src/tls_roots/openssl_spike/${name}.rs`)];
const report = { schema_version: 1, kind: referenceOnly ? 'raw_https_session_reference_characterization' : 'OpenSSL_B2_raw_https_session_parity', identity: h.identity, source_sha256: Object.fromEntries(await Promise.all(paths.map(async path => [path, hash(await readFile(join(root, path)))]))), passed: !referenceOnly && !fatal && results.every(row => row.matched && row.cleanup), reference_only: referenceOnly, matched: results.filter(row => row.matched).length, count: results.length, production_eligible: false, unqualified: ['fetch weak cache', 'B3 configuration', 'full pool lifecycle', 'backend-wide equivalence', 'global trust store across independent clients', 'dynamic process environment mutation', 'cache byte bound'], ...(fatal ? { fatal } : {}), results };
const bytes = JSON.stringify(report, null, 2) + '\n';
const archive = join(root, 'artifacts/rust-rewrite/evidence'); await mkdir(archive, { recursive: true });
const path = join(archive, `openssl-b2-sessions-${hash(bytes)}.json`);
await writeFile(path, bytes, { flag: 'wx' }).catch(error => { if (error.code !== 'EEXIST') throw error; });
console.log(JSON.stringify({ report: path, passed: report.passed, matched: report.matched, count: report.count, fatal }));
if (fatal || (!referenceOnly && !report.passed)) process.exitCode = 1;
