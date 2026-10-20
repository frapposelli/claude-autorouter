// Opt-in B3 characterization. Owned UTF-8 configs, built-in SSL commands and
// synthetic loopback services only. No provider/engine/include configuration.
// Loading, handshake effects and diagnostic bytes are separate observations.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import http from 'node:http';
import https from 'node:https';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { transportHarness } from './transport-harness.mjs';

const execute = promisify(execFile);
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const reportPath = process.env.AUTOROUTER_SYNTHETIC_B3_REPORT;
assert.ok(reportPath, 'An explicit new report path is required');
assert.equal(process.version, 'v22.14.0');
assert.equal(process.versions.openssl, '3.0.15+quic');
const harness = await transportHarness('autorouter-openssl-b3-', {
  candidateArguments: ['--exact', 'tls_roots::openssl_spike::gateway_config_child', '--nocapture'],
  candidateEnvironment: { AUTOROUTER_SYNTHETIC_OPENSSL_SPIKE: 'stage-b3' },
  candidateKind: 'private runtime libtest B3 first-use configuration transport',
});
const { root, scratch, candidate, token, identity, gateway, watch, cleanup } = harness;
async function run(...args) {
  const promise = execute(...args);
  const owner = harness.ownChild(promise.child);
  try { return await promise; }
  finally { await owner.stop(); }
}
const configs = new Map();
async function config(name, text) {
  const path = join(scratch, name);
  await writeFile(path, text, { flag: 'wx', mode: 0o600 });
  configs.set(path, { name, bytes: Buffer.byteLength(text), sha256: hash(text) });
  return path;
}
function ssl(command, selected = 'nodejs_conf') {
  return `${selected}=startup\n[startup]\nssl_conf=contexts\n[contexts]\nsystem_default=policy\n[policy]\n${command}\n`;
}
async function request(port, inference) {
  const body = inference ? JSON.stringify({ model: 'claude-sonnet-4-6', max_tokens: 16, messages: [{ role: 'user', content: 'Synthetic: implement a bounded string formatter.' }] }) : undefined;
  return new Promise(resolve => {
    const req = http.request({ host: '127.0.0.1', port, method: inference ? 'POST' : 'GET', path: inference ? '/v1/messages' : '/v1/models/synthetic', headers: { 'x-api-key': token, ...(body ? { 'content-type': 'application/json', 'content-length': Buffer.byteLength(body) } : {}) } }, res => {
      let bytes = 0;
      res.on('data', chunk => { bytes += chunk.length; if (bytes > 65536) res.destroy(); });
      res.on('end', () => resolve({ status: res.statusCode }));
      res.on('error', () => resolve({ response_error: true }));
    });
    req.setTimeout(3000, () => req.destroy());
    req.on('error', () => resolve({ request_error: true }));
    req.end(body);
  });
}
function startupProjection(failure) {
  return {
    phase: failure.stderr.includes('OpenSSL configuration error:') ? 'configuration' : failure.exit_code === 9 ? 'options' : 'other',
    exit_code: failure.exit_code, signal: failure.signal, timed_out: failure.timed_out,
  };
}
const results = [], firstUse = [];
let declaration;
try {
  const empty = await config('empty.cnf', '');
  const missing = join(scratch, 'absent.cnf');
  const dangling = await config('dangling.cnf', 'nodejs_conf=absent\n');
  const sharedDangling = await config('shared-dangling.cnf', 'openssl_conf=absent\n');
  const noSelection = await config('no-selection.cnf', '[unused]\nvalue=ok\n');
  const syntax = await config('syntax.cnf', '[unclosed\n');
  const malformedUnselected = await config('malformed-unselected.cnf', '[unused\n');
  const unselected = await config('unselected-module.cnf', 'openssl_conf=startup\n[startup]\nssl_conf=absent\n');
  const dual = await config('dual.cnf', 'nodejs_conf=valid\nopenssl_conf=absent\n[valid]\n');
  const validShared = await config('valid-shared.cnf', 'nodejs_conf=absent\nopenssl_conf=valid\n[valid]\n');
  const spaces = await config('owned path with spaces.cnf', 'nodejs_conf=valid\n[valid]\n');
  const accept = await config('signature-accept.cnf', ssl('SignatureAlgorithms=ecdsa_secp256r1_sha256'));
  const reject = await config('signature-reject.cnf', ssl('SignatureAlgorithms=rsa_pss_rsae_sha256'));
  const group = await config('configured-group.cnf', ssl('Groups=P-384'));
  const level = await config('configured-level.cnf', ssl('CipherString=DEFAULT:@SECLEVEL=4'));
  const invalidCommand = await config('invalid-command.cnf', ssl('SyntheticUnknownCommand=value'));
  const options = path => `--openssl-config="${path}"`;
  const cases = [
    { id: 'explicit-empty-env', env: { OPENSSL_CONF: '' }, startup: true, status: 200 },
    { id: 'empty-env-file', env: { OPENSSL_CONF: empty }, startup: true, status: 200 },
    { id: 'empty-node-option', node: options(empty), startup: true, status: 200 },
    { id: 'empty-cli-option', cli: ['--openssl-config', empty], startup: true, status: 200 },
    { id: 'empty-shared', env: { OPENSSL_CONF: empty }, node: '--openssl-shared-config', startup: true, status: 200 },
    { id: 'missing-file-ignored', env: { OPENSSL_CONF: missing }, startup: true, status: 200 },
    { id: 'missing-selection-key', env: { OPENSSL_CONF: noSelection }, startup: true, status: 200 },
    { id: 'no-fallback-to-shared', env: { OPENSSL_CONF: unselected }, startup: true, status: 200 },
    { id: 'dangling-selected-key', env: { OPENSSL_CONF: dangling }, startup: false, phase: 'configuration' },
    { id: 'shared-selects-dangling', env: { OPENSSL_CONF: sharedDangling }, node: '--openssl-shared-config', startup: false, phase: 'configuration' },
    { id: 'dual-default-key', env: { OPENSSL_CONF: dual }, startup: true, status: 200 },
    { id: 'dual-shared-key', env: { OPENSSL_CONF: dual }, node: '--openssl-shared-config', startup: false, phase: 'configuration' },
    { id: 'shared-valid-key', env: { OPENSSL_CONF: validShared }, node: '--openssl-shared-config', startup: true, status: 200 },
    { id: 'whole-file-syntax', env: { OPENSSL_CONF: syntax }, startup: false, phase: 'configuration' },
    { id: 'unselected-syntax-still-parsed', env: { OPENSSL_CONF: malformedUnselected }, startup: false, phase: 'configuration' },
    { id: 'selected-missing-ssl-section', env: { OPENSSL_CONF: unselected }, node: '--openssl-shared-config', startup: false, phase: 'configuration' },
    { id: 'cli-over-env', env: { OPENSSL_CONF: syntax }, cli: ['--openssl-config', empty], startup: true, status: 200 },
    { id: 'node-option-over-env', env: { OPENSSL_CONF: syntax }, node: options(empty), startup: true, status: 200 },
    { id: 'cli-over-node-option', node: options(syntax), cli: ['--openssl-config', empty], startup: true, status: 200 },
    { id: 'last-cli-wins', cli: ['--openssl-config', syntax, '--openssl-config', empty], startup: true, status: 200 },
    { id: 'last-node-option-wins', node: `${options(syntax)} ${options(empty)}`, startup: true, status: 200 },
    { id: 'cli-empty-falls-back-to-env', env: { OPENSSL_CONF: empty }, cli: ['--openssl-config', ''], startup: true, status: 200 },
    { id: 'cli-empty-clears-node-option', node: options(syntax), cli: ['--openssl-config', ''], startup: true, status: 200 },
    { id: 'node-option-spaces', node: options(spaces), startup: true, status: 200 },
    { id: 'cli-spaces', cli: ['--openssl-config', spaces], startup: true, status: 200 },
    { id: 'shared-false-is-positive', env: { OPENSSL_CONF: dual }, node: '--openssl-shared-config=false', startup: false, phase: 'configuration' },
    { id: 'shared-negation-clears', env: { OPENSSL_CONF: dual }, node: '--openssl-shared-config --no-openssl-shared-config=true', startup: true, status: 200 },
    { id: 'config-empty-equals', node: '--openssl-config=', startup: false, phase: 'options' },
    { id: 'config-missing-operand', node: '--openssl-config', startup: false, phase: 'options' },
    { id: 'config-negation', node: '--no-openssl-config=owned', startup: false, phase: 'options' },
    { id: 'options-before-config-error', env: { OPENSSL_CONF: syntax }, node: '--openssl-config=', startup: false, phase: 'options' },
    ...['TLSv1.2', 'TLSv1.3'].flatMap(version => [
      { id: `signature-accept-${version}`, env: { OPENSSL_CONF: accept }, version, startup: true, status: 200 },
      { id: `signature-reject-${version}`, env: { OPENSSL_CONF: reject }, version, startup: true, status: 502 },
      { id: `group-survives-${version}`, env: { OPENSSL_CONF: group }, version, peerGroup: 'P-256', startup: true, status: 502, known_context_gap: true },
      { id: `security-level-survives-${version}`, env: { OPENSSL_CONF: level }, version, startup: true, status: 502, known_context_gap: true },
    ]),
    { id: 'invalid-command-deferred-context', env: { OPENSSL_CONF: invalidCommand }, startup: true, status: 200 },
  ];
  declaration = {
    version: 1, cases: JSON.parse(JSON.stringify(cases).replaceAll(scratch, '<fixture>')),
    first_use: ['no-config-first', 'cached-success', 'cached-failure', 'cached-failure-bytes'],
    config_files: [...configs.values()],
    thresholds: 'Each Node startup phase/status must meet its declaration; native must independently match it. No retry or merged pass. Four Groups/security canaries are expected to expose gaps but remain failures in the overall result.',
    diagnostics: 'Raw synthetic stderr retained privately; startup phase/exit projection is not diagnostic-byte equivalence. No arbitrary config, default absent-env path, permission or non-UTF8 qualification.',
  };
  // Retain the declared inventory before spawning either implementation.
  await writeFile(`${reportPath}.declaration.json`, JSON.stringify(declaration, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
  const commandEnv = { PATH: '/usr/bin:/bin:/usr/sbin:/sbin', OPENSSL_CONF: empty, HOME: scratch, TMPDIR: scratch };
  const openssl = (...args) => run('openssl', args, { env: commandEnv, timeout: 10000, maxBuffer: 1024 * 1024 });
  const certConfig = await config('certificate.cnf', '[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=ext\n[dn]\nCN=localhost\n[ext]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,digitalSignature,keyCertSign\nextendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1\n');
  const keyPath = join(scratch, 'peer.key'), certPath = join(scratch, 'peer.pem');
  await openssl('ecparam', '-genkey', '-name', 'prime256v1', '-noout', '-out', keyPath);
  await openssl('req', '-new', '-x509', '-sha256', '-key', keyPath, '-out', certPath, '-days', '1', '-config', certConfig);
  const key = await readFile(keyPath), cert = await readFile(certPath);
  const providers = new Map();
  for (const version of ['TLSv1.2', 'TLSv1.3']) for (const peerGroup of ['auto', 'P-256']) {
    const state = { attempts: 0, handshakes: [], requests: [], sockets: new Set() };
    const server = watch(https.createServer({ key, cert, minVersion: version, maxVersion: version, ecdhCurve: peerGroup }, (req, res) => {
      state.requests.push({ path: req.url, alpn: req.socket.alpnProtocol || null, resumed: req.socket.isSessionReused() });
      req.resume();
      if (req.url === '/v1/systemone') { res.setHeader('content-type', 'application/json'); res.end(JSON.stringify({ answers: { tier: { choice: 'sonnet', confidence: 1 } } })); }
      else res.end('synthetic configuration fixture');
    }));
    server.on('connection', socket => { state.attempts++; state.sockets.add(socket); socket.once('close', () => state.sockets.delete(socket)); });
    server.on('secureConnection', socket => state.handshakes.push({ protocol: socket.getProtocol(), resumed: socket.isSessionReused() }));
    server.on('tlsClientError', () => {});
    server.listen(0, '127.0.0.1'); await once(server, 'listening');
    providers.set(`${version}/${peerGroup}`, { server, state });
  }
  for (const row of cases) {
    const result = { id: row.id, expected: { startup: row.startup, status: row.status, phase: row.phase }, known_context_gap: !!row.known_context_gap };
    const { server, state } = providers.get(`${row.version ?? 'TLSv1.3'}/${row.peerGroup ?? 'auto'}`);
    for (const name of ['node', 'rust']) {
      const before = { attempts: state.attempts, handshakes: state.handshakes.length, requests: state.requests.length };
      const extra = { OPENSSL_CONF: '', SSL_CERT_FILE: empty, SSL_CERT_DIR: scratch, NODE_EXTRA_CA_CERTS: certPath, NODE_OPTIONS: row.node ?? '', ...(row.env ?? {}), AUTOROUTER_SYNTHETIC_OPENSSL_CLI: JSON.stringify(row.cli ?? []) };
      const child = await gateway(name, `https://127.0.0.1:${server.address().port}`, extra, { allowStartupFailure: true, nodeArguments: row.cli ?? [] });
      let observed;
      if (child.startup_failure) observed = { startup: false, failure: startupProjection(child.startup_failure), raw_stderr: child.startup_failure.stderr };
      else {
        observed = { startup: true };
        try {
          observed.raw = await request(child.port, false);
          observed.raw_attempts = state.attempts - before.attempts;
          const inferenceBefore = state.attempts;
          observed.inference = await request(child.port, true);
          observed.inference_attempts = state.attempts - inferenceBefore;
        }
        finally { await child.stop(); }
        observed.exit = child.exit();
        observed.raw_stderr = child.stderr().replaceAll(scratch, '<fixture>');
      }
      // Wait on actual peer close events, not a timing delay or forced cleanup.
      let timer;
      observed.cleaned = await Promise.race([
        Promise.all([...state.sockets].map(socket => new Promise(resolve => socket.once('close', resolve)))).then(() => state.sockets.size === 0),
        new Promise(resolve => { timer = setTimeout(() => resolve(false), 2000); }),
      ]).finally(() => clearTimeout(timer));
      observed.connections = state.attempts - before.attempts;
      observed.handshakes = state.handshakes.slice(before.handshakes);
      observed.requests = state.requests.slice(before.requests);
      observed.declared_outcome = observed.startup === row.startup && observed.cleaned && (row.startup
        ? observed.raw?.status === row.status && observed.inference?.status === row.status && observed.exit.code === 0 && observed.handshakes.every(h => !h.resumed)
        : observed.failure.phase === row.phase && !observed.failure.timed_out && observed.failure.signal === null && observed.connections === 0);
      if (row.startup && row.status === 200) observed.declared_outcome &&= JSON.stringify(observed.requests) === JSON.stringify([
        { path: '/v1/models/synthetic', alpn: null, resumed: false },
        { path: '/v1/systemone', alpn: 'http/1.1', resumed: false },
        { path: '/v1/messages', alpn: null, resumed: false },
      ]);
      if (row.startup && row.status === 502) observed.declared_outcome &&= observed.raw_attempts === 1 && observed.inference_attempts === 2 && observed.requests.length === 0;
      result[name] = observed;
    }
    result.startup_exit_equal = result.node.startup === result.rust.startup && (result.node.startup || JSON.stringify(result.node.failure) === JSON.stringify(result.rust.failure));
    result.diagnostic_bytes_equal = result.node.raw_stderr === result.rust.raw_stderr;
    result.semantic_matched = result.node.declared_outcome && result.rust.declared_outcome && result.startup_exit_equal;
    results.push(result);
  }
  for (const mode of ['no-config-first', 'cached-success', 'cached-failure', 'cached-failure-bytes']) {
    const file = await config(`first-use-${mode}.cnf`, mode === 'cached-failure-bytes' ? Buffer.from('nodejs_conf=\xff\n', 'latin1') : mode === 'cached-failure' ? 'nodejs_conf=absent\n' : '');
    const observed = await run(candidate, ['--exact', 'tls_roots::openssl_spike::config_init::first_use_child', '--nocapture'], { env: { HOME: scratch, TMPDIR: scratch, PATH: scratch, OPENSSL_CONF: '', AUTOROUTER_SYNTHETIC_OPENSSL_INIT: mode, AUTOROUTER_SYNTHETIC_OPENSSL_FILE: file }, timeout: 5000, maxBuffer: 65536 }).then(result => ({ exit: 0, stdout: result.stdout, stderr: result.stderr }), error => ({ exit: error.code, signal: error.signal, stdout: error.stdout, stderr: error.stderr }));
    firstUse.push({ mode, ...observed, passed: observed.exit === 0 && observed.stderr.includes('OpenSSL first-use fixture:') });
  }
} finally {
  await cleanup();
  const report = { schema_version: 1, kind: 'PRIVATE_B3_CONFIGURATION_CHARACTERIZATION', identity, declaration, declaration_sha256: declaration ? hash(JSON.stringify(declaration)) : null, passed: results.length === declaration?.cases.length && results.every(r => r.semantic_matched) && firstUse.length === 4 && firstUse.every(r => r.passed), semantic_matched: results.filter(r => r.semantic_matched).length, count: results.length, first_use: firstUse, results, production_eligible: false, complete_tls_parity: false, limits: ['Owned UTF-8 paired configs plus one native-only invalid-byte failure-cache control; no includes/providers/engines/FIPS/external services.', 'Startup phase/exit projection does not establish exact stderr or diagnostic privacy migration.', 'Default path with absent OPENSSL_CONF, permission failures, arbitrary Groups/security effects and backend-wide policy remain unqualified.', 'One finite run; all failures retained. No old223-row oracle or inventory changed.'] };
  await mkdir(join(root, 'artifacts/rust-rewrite/evidence'), { recursive: true });
  const bytes = JSON.stringify(report, null, 2) + '\n';
  await writeFile(reportPath, bytes, { flag: 'wx', mode: 0o600 });
  await writeFile(join(root, 'artifacts/rust-rewrite/evidence', `openssl-b3-${hash(bytes)}.json`), bytes, { flag: 'wx', mode: 0o600 });
  console.log(JSON.stringify({ passed: report.passed, semantic_matched: report.semantic_matched, count: report.count, first_use: firstUse.map(({ mode, passed }) => ({ mode, passed })), report: reportPath }));
  if (!report.passed) process.exitCode = 1;
}
