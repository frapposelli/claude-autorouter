// Local synthetic certificates and fake credentials only. No provider traffic.
import { execFile } from 'node:child_process';
import http from 'node:http';
import https from 'node:https';
import { once } from 'node:events';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { transportHarness } from './transport-harness.mjs';
const run = promisify(execFile);
const exec = (file, args) => run(file, args, { timeout: 10000, maxBuffer: 1024 * 1024 });
const { root, scratch, token, identity, gateway, watch, close, cleanup } = await transportHarness('autorouter-security-audit-');
async function request(port) {
  return new Promise(resolve => {
    const req = http.get({ host: '127.0.0.1', port, path: '/v1/models/synthetic', headers: { 'x-api-key': token } }, res => { res.resume(); res.on('end', () => resolve(res.statusCode)); });
    req.setTimeout(3000, () => req.destroy());
    req.on('error', () => resolve('request_error'));
  });
}
async function certificates() {
  const ca = join(scratch, 'ca'); const leaf = join(scratch, 'leaf');
  await writeFile(`${ca}.cnf`, '[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=ext\n[dn]\nCN=synthetic-local-root\n[ext]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n');
  await writeFile(`${leaf}.cnf`, 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n');
  for (const base of [ca, leaf]) await exec('openssl', ['ecparam', '-genkey', '-name', 'prime256v1', '-noout', '-out', `${base}.key`]);
  await exec('openssl', ['req', '-new', '-x509', '-sha256', '-key', `${ca}.key`, '-out', `${ca}.crt`, '-days', '1', '-config', `${ca}.cnf`]);
  await exec('openssl', ['req', '-new', '-key', `${leaf}.key`, '-subj', '/CN=localhost', '-out', `${leaf}.csr`]);
  await exec('openssl', ['x509', '-req', '-sha256', '-in', `${leaf}.csr`, '-CA', `${ca}.crt`, '-CAkey', `${ca}.key`, '-CAcreateserial', '-out', `${leaf}.crt`, '-days', '1', '-extfile', `${leaf}.cnf`]);
  return { ca: `${ca}.crt`, key: await readFile(`${leaf}.key`), cert: await readFile(`${leaf}.crt`) };
}
try {
  const certs = await certificates();
  const secure = watch(https.createServer({ key: certs.key, cert: certs.cert }, (_req, res) => res.end('synthetic TLS')));
  secure.on('tlsClientError', () => {});
  secure.listen(0, '127.0.0.1'); await once(secure, 'listening');
  const tls = [];
  const caBytes = await readFile(certs.ca);
  const broken = '-----BEGIN CERTIFICATE-----\ninvalid synthetic certificate\n-----END CERTIFICATE-----\n';
  await writeFile(join(scratch, 'valid-then-broken.pem'), Buffer.concat([caBytes, Buffer.from(broken)]));
  await writeFile(join(scratch, 'broken-then-valid.pem'), Buffer.concat([Buffer.from(broken), caBytes]));
  try {
    for (const [id, extra] of [['default-private-root', {}], ['node-extra-ca', { NODE_EXTRA_CA_CERTS: certs.ca }], ['ssl-cert-file', { SSL_CERT_FILE: certs.ca }], ['missing-extra-ca', { NODE_EXTRA_CA_CERTS: join(scratch, 'missing.pem') }], ['valid-then-broken-extra', { NODE_EXTRA_CA_CERTS: join(scratch, 'valid-then-broken.pem') }], ['broken-then-valid-extra', { NODE_EXTRA_CA_CERTS: join(scratch, 'broken-then-valid.pem') }]]) {
      const result = { id };
      for (const name of ['node', 'rust']) {
        const child = await gateway(name, `https://127.0.0.1:${secure.address().port}`, extra);
        try { result[name] = await request(child.port); } finally { await child.stop(); }
      }
      tls.push(result);
    }
  } finally { secure.closeAllConnections(); await close(secure); }
  let header = null;
  const provider = watch(http.createServer((req, res) => { header = Buffer.from(req.headers['x-api-key'] ?? '', 'latin1').toString('hex'); res.end('synthetic header'); }));
  provider.listen(0, '127.0.0.1'); await once(provider, 'listening');
  const headers = [];
  try {
    for (const [id, value] of [['latin1', 'synthetic-é'], ['non-byte-string', 'synthetic-Ā'], ['latin1-control', 'synthetic-\u0085']]) {
      const result = { id };
      for (const name of ['node', 'rust']) {
        header = null;
        const child = await gateway(name, `http://127.0.0.1:${provider.address().port}`, { ANTHROPIC_API_KEY: value });
        try { result[name] = { status: await request(child.port), header_hex: header }; } finally { await child.stop(); }
      }
      headers.push(result);
    }
  } finally { provider.closeAllConnections(); await close(provider); }
  const report = { schema_version: 1, kind: 'local_synthetic_TLS_and_header_encoding', source_node: process.version, identity, bundled_root_version:'v22.14.0', reference_runtime_matches_bundled_root_version:process.version==='v22.14.0', scope:'Local synthetic private CA and byte-string header semantics; different Node root-set versions require a separate root membership audit.', tls, headers };
  await writeFile(join(root, 'artifacts/rust-rewrite/parity-upstream-security.json'), JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify(report, null, 2));
  if (tls.some(row => row.node !== row.rust) || headers.some(row => JSON.stringify(row.node) !== JSON.stringify(row.rust))) process.exitCode = 1;
} finally { await cleanup(); }
