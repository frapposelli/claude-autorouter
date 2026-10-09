// Compare supported trust selectors with private CAs; separately retain all
// declared unqualified TLS policy differences. Neither report implies complete
// TLS compatibility outside the pinned Node reference and explicit fixtures.
import { execFile } from 'node:child_process';
import http from 'node:http';
import https from 'node:https';
import { once } from 'node:events';
import { mkdir, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { createHash } from 'node:crypto';
import { transportHarness } from './transport-harness.mjs';
const { root, scratch, token, identity, gateway, watch, close, cleanup } = await transportHarness('autorouter-tls-options-');
const run = promisify(execFile);
const openssl = (...args) => run('openssl', args, { timeout: 10000, maxBuffer: 1024 * 1024 });
async function request(port) {
  return new Promise(resolve => {
    const req = http.get({ host: '127.0.0.1', port, path: '/v1/models/synthetic', headers: { 'x-api-key': token } }, res => {
      res.resume(); res.on('end', () => resolve({ status: res.statusCode })); res.on('error', () => resolve({ response_error: true }));
    });
    req.setTimeout(3000, () => req.destroy()); req.on('error', () => resolve({ request_error: true }));
  });
}
async function certificateFixtures() {
  const ca = join(scratch, 'root'), intermediate = join(scratch, 'intermediate');
  await writeFile(`${ca}.cnf`, '[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=ext\n[dn]\nCN=Synthetic TLS Options Root\n[ext]\nbasicConstraints=critical,CA:TRUE,pathlen:1\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\n');
  await writeFile(`${intermediate}.cnf`, 'basicConstraints=critical,CA:TRUE,pathlen:0\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid:always\n');
  const leafExtensions = join(scratch, 'leaf.cnf');
  await writeFile(leafExtensions, 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n');
  for (const path of [ca, intermediate]) await openssl('ecparam', '-genkey', '-name', 'prime256v1', '-noout', '-out', `${path}.key`);
  await openssl('req', '-new', '-x509', '-sha256', '-key', `${ca}.key`, '-out', `${ca}.crt`, '-days', '1', '-config', `${ca}.cnf`);
  await openssl('req', '-new', '-key', `${intermediate}.key`, '-subj', '/CN=Synthetic Intermediate', '-out', `${intermediate}.csr`);
  await openssl('x509', '-req', '-sha256', '-in', `${intermediate}.csr`, '-CA', `${ca}.crt`, '-CAkey', `${ca}.key`, '-CAcreateserial', '-out', `${intermediate}.crt`, '-days', '1', '-extfile', `${intermediate}.cnf`);
  const leaves = {};
  for (const [name, issuer] of [['direct', ca], ['intermediate', intermediate]]) {
    const path = join(scratch, `${name}-leaf`);
    await openssl('ecparam', '-genkey', '-name', 'prime256v1', '-noout', '-out', `${path}.key`);
    await openssl('req', '-new', '-key', `${path}.key`, '-subj', '/CN=localhost', '-out', `${path}.csr`);
    await openssl('x509', '-req', '-sha256', '-in', `${path}.csr`, '-CA', `${issuer}.crt`, '-CAkey', `${issuer}.key`, '-CAcreateserial', '-out', `${path}.crt`, '-days', '1', '-extfile', leafExtensions);
    leaves[name] = { key: await readFile(`${path}.key`), cert: await readFile(`${path}.crt`) };
  }
  const caBytes = await readFile(`${ca}.crt`), intermediateBytes = await readFile(`${intermediate}.crt`);
  leaves.chain = { ...leaves.intermediate, cert: Buffer.concat([leaves.intermediate.cert, intermediateBytes]) };
  for (const [name, eku, address] of [['wrong-host', 'serverAuth', '127.0.0.2'], ['wrong-purpose', 'clientAuth', '127.0.0.1']]) {
    const path = join(scratch, name);
    await writeFile(`${path}.cnf`, `basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=${eku}\nsubjectAltName=IP:${address}\n`);
    await openssl('x509', '-req', '-sha256', '-in', join(scratch, 'direct-leaf.csr'), '-CA', `${ca}.crt`, '-CAkey', `${ca}.key`, '-CAcreateserial', '-out', `${path}.crt`, '-days', '1', '-extfile', `${path}.cnf`);
    leaves[name] = { key: leaves.direct.key, cert: await readFile(`${path}.crt`) };
  }
  // Preserve DNS-field presence even when IA5String bytes are malformed.
  for (const [name, san] of [
    ['cn-only', null],
    ['dns-malformed', '30:03:82:01:ff'],
    ['dns-empty', '30:02:82:00'],
    ['dns-malformed-valid', '30:0e:82:01:ff:82:09:6c:6f:63:61:6c:68:6f:73:74'],
    ['dns-valid', '30:0b:82:09:6c:6f:63:61:6c:68:6f:73:74'],
    ['dns-mismatch-cn-match', '30:07:82:05:6f:74:68:65:72'],
  ]) {
    const path = join(scratch, name);
    await writeFile(`${path}.cnf`, 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\n' + (san ? `subjectAltName=DER:${san}\n` : ''));
    await openssl('x509', '-req', '-sha256', '-in', join(scratch, 'direct-leaf.csr'), '-CA', `${ca}.crt`, '-CAkey', `${ca}.key`, '-CAcreateserial', '-out', `${path}.crt`, '-days', '1', '-extfile', `${path}.cnf`);
    leaves[name] = { key: leaves.direct.key, cert: await readFile(`${path}.crt`) };
  }
  const weakCa = join(scratch, 'rsa1024-root');
  await openssl('genrsa', '-out', `${weakCa}.key`, '1024');
  await openssl('req', '-new', '-x509', '-sha256', '-key', `${weakCa}.key`, '-out', `${weakCa}.crt`, '-days', '1', '-config', `${ca}.cnf`);
  await openssl('x509', '-req', '-sha256', '-in', join(scratch, 'direct-leaf.csr'), '-CA', `${weakCa}.crt`, '-CAkey', `${weakCa}.key`, '-CAcreateserial', '-out', join(scratch, 'rsa1024-root-leaf.crt'), '-days', '1', '-extfile', leafExtensions);
  leaves['rsa1024-root'] = { key: leaves.direct.key, cert: await readFile(join(scratch, 'rsa1024-root-leaf.crt')), ciphers: 'DEFAULT:@SECLEVEL=0' };
  const weakLeaf = join(scratch, 'rsa1024-leaf');
  await openssl('genrsa', '-out', `${weakLeaf}.key`, '1024');
  await openssl('req', '-new', '-key', `${weakLeaf}.key`, '-subj', '/CN=localhost', '-out', `${weakLeaf}.csr`);
  await openssl('x509', '-req', '-sha256', '-in', `${weakLeaf}.csr`, '-CA', `${ca}.crt`, '-CAkey', `${ca}.key`, '-CAcreateserial', '-out', `${weakLeaf}.crt`, '-days', '1', '-extfile', leafExtensions);
  leaves['rsa1024-leaf'] = { key: await readFile(`${weakLeaf}.key`), cert: await readFile(`${weakLeaf}.crt`), ciphers: 'DEFAULT:@SECLEVEL=0', minVersion: 'TLSv1.3' };
  leaves['rsa1024-leaf-tls12'] = { ...leaves['rsa1024-leaf'], minVersion: 'TLSv1.2', maxVersion: 'TLSv1.2' };
  const p521 = join(scratch, 'p521-leaf');
  await openssl('ecparam', '-genkey', '-name', 'secp521r1', '-noout', '-out', `${p521}.key`);
  await openssl('req', '-new', '-key', `${p521}.key`, '-subj', '/CN=localhost', '-out', `${p521}.csr`);
  await openssl('x509', '-req', '-sha256', '-in', `${p521}.csr`, '-CA', `${ca}.crt`, '-CAkey', `${ca}.key`, '-CAcreateserial', '-out', `${p521}.crt`, '-days', '1', '-extfile', leafExtensions);
  leaves['p521-leaf'] = { key: await readFile(`${p521}.key`), cert: await readFile(`${p521}.crt`), minVersion: 'TLSv1.3' };
  leaves['p521-leaf-tls12'] = { ...leaves['p521-leaf'], minVersion: 'TLSv1.2', maxVersion: 'TLSv1.2' };
  const p384 = join(scratch, 'p384-leaf');
  await openssl('ecparam', '-genkey', '-name', 'secp384r1', '-noout', '-out', `${p384}.key`);
  await openssl('req', '-new', '-key', `${p384}.key`, '-subj', '/CN=localhost', '-out', `${p384}.csr`);
  await openssl('x509', '-req', '-sha256', '-in', `${p384}.csr`, '-CA', `${ca}.crt`, '-CAkey', `${ca}.key`, '-CAcreateserial', '-out', `${p384}.crt`, '-days', '1', '-extfile', leafExtensions);
  const ecKeys = { p256: leaves.direct, p384: { key: await readFile(`${p384}.key`), cert: await readFile(`${p384}.crt`) }, p521: leaves['p521-leaf'] };
  // TLS 1.2's signature scheme fixes the hash; TLS 1.3 fixes curve and hash.
  // Restrict the server list to exercise each combination, not merely a
  // handshake that happened to select one preferred supported algorithm.
  for (const [curve, certificates] of Object.entries(ecKeys)) {
    for (const [digest, sigalgs] of [['sha256', 'ecdsa_secp256r1_sha256'], ['sha384', 'ecdsa_secp384r1_sha384'], ['sha512', 'ecdsa_secp521r1_sha512']]) {
      for (const version of ['TLSv1.2', 'TLSv1.3']) {
        leaves[`${curve}-${digest}-${version}`] = { ...certificates, minVersion: version, maxVersion: version, sigalgs };
      }
    }
  }
  for (const version of ['TLSv1.2', 'TLSv1.3']) {
    leaves[`only-${version}`] = { ...leaves.direct, minVersion: version, maxVersion: version };
  }
  await openssl('x509', '-req', '-sha1', '-in', join(scratch, 'direct-leaf.csr'), '-CA', `${ca}.crt`, '-CAkey', `${ca}.key`, '-CAcreateserial', '-out', join(scratch, 'sha1-leaf.crt'), '-days', '1', '-extfile', leafExtensions);
  leaves['sha1-leaf'] = { key: leaves.direct.key, cert: await readFile(join(scratch, 'sha1-leaf.crt')), ciphers: 'DEFAULT:@SECLEVEL=0' };
  const constraintRoots = {};
  for (const [name, permitted] of [['allowed', '127.0.0.1'], ['excluded', '127.0.0.2']]) {
    const path = join(scratch, `constraint-${name}`);
    await writeFile(`${path}.cnf`, (await readFile(`${ca}.cnf`, 'utf8')) + `nameConstraints=critical,permitted;IP:${permitted}/255.255.255.255\n`);
    await openssl('req', '-new', '-x509', '-sha256', '-key', `${ca}.key`, '-out', `${path}.crt`, '-days', '1', '-config', `${path}.cnf`);
    await openssl('x509', '-req', '-sha256', '-in', join(scratch, 'direct-leaf.csr'), '-CA', `${path}.crt`, '-CAkey', `${ca}.key`, '-CAcreateserial', '-out', `${path}-leaf.crt`, '-days', '1', '-extfile', leafExtensions);
    constraintRoots[name] = `${path}.crt`;
    leaves[`constraint-${name}`] = { key: leaves.direct.key, cert: await readFile(`${path}-leaf.crt`) };
  }
  const unknownCritical = join(scratch, 'unknown-critical');
  await writeFile(`${unknownCritical}.cnf`, (await readFile(leafExtensions, 'utf8')) + '1.2.3.4=critical,DER:05:00\n');
  await openssl('x509', '-req', '-sha256', '-in', join(scratch, 'direct-leaf.csr'), '-CA', `${ca}.crt`, '-CAkey', `${ca}.key`, '-CAcreateserial', '-out', `${unknownCritical}.crt`, '-days', '1', '-extfile', `${unknownCritical}.cnf`);
  leaves['unknown-critical'] = { key: leaves.direct.key, cert: await readFile(`${unknownCritical}.crt`) };
  const hash = (await openssl('x509', '-in', `${ca}.crt`, '-subject_hash', '-noout')).stdout.trim();
  const dirs = {};
  for (const name of ['empty', 'hashed', 'plain', 'wrong', 'gap', 'broken', 'late']) { dirs[name] = join(scratch, `${name}-certs`); await mkdir(dirs[name]); }
  await symlink(`${ca}.crt`, join(dirs.hashed, `${hash}.0`));
  await writeFile(join(dirs.plain, 'root.pem'), caBytes);
  await writeFile(join(dirs.wrong, `${hash === '00000000' ? '11111111' : '00000000'}.0`), caBytes);
  await writeFile(join(dirs.gap, `${hash}.1`), caBytes);
  await writeFile(join(dirs.broken, `${hash}.0`), 'synthetic invalid certificate');
  await writeFile(join(dirs.broken, `${hash}.1`), caBytes);
  const empty = join(scratch, 'empty.pem'); await writeFile(empty, '');
  const both = join(scratch, 'root-and-intermediate.pem'); await writeFile(both, Buffer.concat([caBytes, intermediateBytes]));
  const malformed = join(scratch, 'malformed.pem'); await writeFile(malformed, '-----BEGIN CERTIFICATE-----\ninvalid\n-----END CERTIFICATE-----\n');
  const legacy = join(scratch, 'legacy-root.pem'); await writeFile(legacy, caBytes.toString().replaceAll('CERTIFICATE', 'X509 CERTIFICATE'));
  const validThenMalformed = join(scratch, 'valid-then-malformed.pem'); await writeFile(validThenMalformed, Buffer.concat([caBytes, await readFile(malformed)]));
  const malformedThenValid = join(scratch, 'malformed-then-valid.pem'); await writeFile(malformedThenValid, Buffer.concat([await readFile(malformed), caBytes]));
  const trusted = join(scratch, 'trusted-root.pem');
  await openssl('x509', '-in', `${ca}.crt`, '-addreject', 'serverAuth', '-trustout', '-out', trusted);
  const accepted = join(scratch, 'accepted-intermediate.pem');
  await openssl('x509', '-in', `${intermediate}.crt`, '-addtrust', 'serverAuth', '-trustout', '-out', accepted);
  // The leaf is current, but its privately generated root expired years ago.
  // This isolates root validity from ordinary end-entity validity checking.
  const expired = join(scratch, 'expired-root');
  const database = join(scratch, 'cert-index'), serial = join(scratch, 'cert-serial'), newCerts = join(scratch, 'issued');
  await writeFile(database, ''); await writeFile(serial, '01\n'); await mkdir(newCerts);
  await writeFile(`${expired}.cnf`, `[ca]\ndefault_ca=synthetic\n[synthetic]\ndatabase=${database}\nserial=${serial}\nnew_certs_dir=${newCerts}\ncertificate=${ca}.crt\nprivate_key=${ca}.key\ndefault_md=sha256\npolicy=policy\nx509_extensions=ext\n[policy]\ncommonName=supplied\n[ext]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\n`);
  await openssl('req', '-new', '-key', `${ca}.key`, '-subj', '/CN=Synthetic Expired Root', '-out', `${expired}.csr`);
  await openssl('ca', '-batch', '-selfsign', '-config', `${expired}.cnf`, '-in', `${expired}.csr`, '-out', `${expired}.crt`, '-startdate', '20000101000000Z', '-enddate', '20010101000000Z');
  await openssl('x509', '-req', '-sha256', '-in', join(scratch, 'direct-leaf.csr'), '-CA', `${expired}.crt`, '-CAkey', `${ca}.key`, '-CAcreateserial', '-out', join(scratch, 'expired-root-leaf.crt'), '-days', '1', '-extfile', leafExtensions);
  leaves.expired = { key: leaves.direct.key, cert: await readFile(join(scratch, 'expired-root-leaf.crt')) };
  return { leaves, ca: `${ca}.crt`, intermediate: `${intermediate}.crt`, dirs, hash, weakCa: `${weakCa}.crt`, constraintRoots, empty, both, malformed, legacy, validThenMalformed, malformedThenValid, trusted, accepted, expired: `${expired}.crt` };
}
try {
  if (process.version !== 'v22.14.0' || process.versions.openssl !== '3.0.15+quic') throw Error('TLS qualification requires the frozen Node v22.14.0 / OpenSSL 3.0.15+quic reference');
  const fixtures = await certificateFixtures();
  const providers = {}, handshakes = {};
  for (const [name, certificates] of Object.entries(fixtures.leaves)) {
    handshakes[name] = { connections: 0, completed: [] };
    const server = watch(https.createServer(certificates, (_req, res) => res.end('synthetic TLS option fixture')));
    server.on('connection', () => { handshakes[name].connections++; });
    server.on('secureConnection', socket => { handshakes[name].completed.push({ protocol: socket.getProtocol(), resumed: socket.isSessionReused() }); });
    server.on('tlsClientError', () => {}); server.listen(0, '127.0.0.1'); await once(server, 'listening'); providers[name] = server;
  }
  const base = { SSL_CERT_FILE: fixtures.empty, SSL_CERT_DIR: fixtures.dirs.empty };
  const cases = [
    { id: 'bundled-ignores-openssl-file', env: { SSL_CERT_FILE: fixtures.ca } },
    { id: 'bundled-extra-root', env: { NODE_EXTRA_CA_CERTS: fixtures.ca } },
    ...['allowed', 'excluded'].map(name => ({ id: `root-name-constraint-${name}`, leaf: `constraint-${name}`, env: { NODE_EXTRA_CA_CERTS: fixtures.constraintRoots[name] } })),
    { id: 'unknown-critical-extension', leaf: 'unknown-critical', env: { NODE_EXTRA_CA_CERTS: fixtures.ca } },

    { id: 'rsa1024-root-security-level', leaf: 'rsa1024-root', env: { NODE_EXTRA_CA_CERTS: fixtures.weakCa } },
    { id: 'rsa1024-leaf-security-level', leaf: 'rsa1024-leaf', env: { NODE_EXTRA_CA_CERTS: fixtures.ca } },
    { id: 'rsa1024-leaf-tls12', leaf: 'rsa1024-leaf-tls12', env: { NODE_EXTRA_CA_CERTS: fixtures.ca } },
    { id: 'sha1-leaf-security-level', leaf: 'sha1-leaf', env: { NODE_EXTRA_CA_CERTS: fixtures.ca } },

    { id: 'extra-legacy-label', env: { NODE_EXTRA_CA_CERTS: fixtures.legacy } },
    { id: 'extra-valid-then-malformed', env: { NODE_EXTRA_CA_CERTS: fixtures.validThenMalformed } },
    { id: 'extra-malformed-then-valid', env: { NODE_EXTRA_CA_CERTS: fixtures.malformedThenValid } },
    ...['cn-only', 'dns-malformed', 'dns-empty', 'dns-malformed-valid', 'dns-valid', 'dns-mismatch-cn-match'].map(leaf => ({ id: leaf, leaf, host: 'localhost', env: { NODE_EXTRA_CA_CERTS: fixtures.ca } })),
    { id: 'dns-ip-san-cn-fallback', leaf: 'wrong-host', host: 'localhost', env: { NODE_EXTRA_CA_CERTS: fixtures.ca } },

    { id: 'openssl-empty-store', env: { NODE_OPTIONS: '--use-openssl-ca' } },
    { id: 'openssl-file', env: { NODE_OPTIONS: '--use-openssl-ca', SSL_CERT_FILE: fixtures.ca } },
    { id: 'openssl-extra-root', env: { NODE_OPTIONS: '--use-openssl-ca', NODE_EXTRA_CA_CERTS: fixtures.ca } },
    { id: 'openssl-missing-file', env: { NODE_OPTIONS: '--use-openssl-ca', SSL_CERT_FILE: join(scratch, 'missing.pem') } },
    { id: 'openssl-malformed-file', env: { NODE_OPTIONS: '--use-openssl-ca', SSL_CERT_FILE: fixtures.malformed } },
    { id: 'openssl-empty-string-paths', env: { NODE_OPTIONS: '--use-openssl-ca', SSL_CERT_FILE: '', SSL_CERT_DIR: '' } },
    { id: 'openssl-file-plus-extra', env: { NODE_OPTIONS: '--use-openssl-ca', SSL_CERT_FILE: fixtures.ca, NODE_EXTRA_CA_CERTS: fixtures.intermediate }, leaf: 'intermediate' },
    ...Object.entries(fixtures.dirs).filter(([name]) => name !== 'late').map(([name, path]) => ({ id: `openssl-directory-${name}`, env: { NODE_OPTIONS: '--use-openssl-ca', SSL_CERT_DIR: path } })),
    { id: 'openssl-directory-list', env: { NODE_OPTIONS: '--use-openssl-ca', SSL_CERT_DIR: `${fixtures.dirs.empty}:${fixtures.dirs.hashed}` } },
    { id: 'openssl-directory-leading-empty', env: { NODE_OPTIONS: '--use-openssl-ca', SSL_CERT_DIR: `:${fixtures.dirs.hashed}` } },
    { id: 'openssl-directory-trailing-empty', env: { NODE_OPTIONS: '--use-openssl-ca', SSL_CERT_DIR: `${fixtures.dirs.hashed}:` } },
    { id: 'openssl-directory-late-addition', env: { NODE_OPTIONS: '--use-openssl-ca', SSL_CERT_DIR: fixtures.dirs.late }, mutate: join(fixtures.dirs.late, `${fixtures.hash}.0`), missing: true },
    { id: 'openssl-file-late-addition', env: { NODE_OPTIONS: '--use-openssl-ca', SSL_CERT_FILE: join(scratch, 'late-file.pem') }, mutate: join(scratch, 'late-file.pem') },
    { id: 'extra-file-late-addition', env: { NODE_EXTRA_CA_CERTS: join(scratch, 'late-extra.pem') }, mutate: join(scratch, 'late-extra.pem') },
    { id: 'openssl-no-flag', env: { NODE_OPTIONS: '--no-use-openssl-ca', SSL_CERT_FILE: fixtures.ca } },
    { id: 'openssl-then-negated', env: { NODE_OPTIONS: '--use-openssl-ca --no-use-openssl-ca', SSL_CERT_FILE: fixtures.ca } },
    { id: 'bundled-negated', env: { NODE_OPTIONS: '--no-use-bundled-ca', SSL_CERT_FILE: fixtures.ca } },
    { id: 'bundled-then-negated', env: { NODE_OPTIONS: '--use-bundled-ca --no-use-bundled-ca', SSL_CERT_FILE: fixtures.ca } },
    { id: 'openssl-and-no-bundled', env: { NODE_OPTIONS: '--use-openssl-ca --no-use-bundled-ca', SSL_CERT_FILE: fixtures.ca } },
    { id: 'both-selectors', env: { NODE_OPTIONS: '--use-bundled-ca --use-openssl-ca', SSL_CERT_FILE: fixtures.ca } },
    { id: 'openssl-false-value', env: { NODE_OPTIONS: '--use-openssl-ca=false', SSL_CERT_FILE: fixtures.ca } },
    { id: 'quoted-underscore-selector', env: { NODE_OPTIONS: '"--use_openssl_ca"', SSL_CERT_FILE: fixtures.ca } },
    { id: 'unsupported-system-selector', env: { NODE_OPTIONS: '--use-system-ca', SSL_CERT_FILE: fixtures.ca } },
    { id: 'system-env-baseline', env: { NODE_USE_SYSTEM_CA: '1', SSL_CERT_FILE: fixtures.ca } },
    { id: 'extra-intermediate-without-root', env: { NODE_EXTRA_CA_CERTS: fixtures.intermediate }, leaf: 'intermediate' },
    { id: 'extra-root-chain-presented', env: { NODE_EXTRA_CA_CERTS: fixtures.ca }, leaf: 'chain' },
    { id: 'extra-root-chain-absent', env: { NODE_EXTRA_CA_CERTS: fixtures.ca }, leaf: 'intermediate' },
    { id: 'extra-root-and-intermediate', env: { NODE_EXTRA_CA_CERTS: fixtures.both }, leaf: 'intermediate' },
    { id: 'extra-root-wrong-host', env: { NODE_EXTRA_CA_CERTS: fixtures.ca }, leaf: 'wrong-host' },
    { id: 'extra-root-wrong-purpose', env: { NODE_EXTRA_CA_CERTS: fixtures.ca }, leaf: 'wrong-purpose' },
    { id: 'extra-expired-root', env: { NODE_EXTRA_CA_CERTS: fixtures.expired }, leaf: 'expired' },
    { id: 'openssl-intermediate-without-root', env: { NODE_OPTIONS: '--use-openssl-ca', SSL_CERT_FILE: fixtures.intermediate }, leaf: 'intermediate' },
    { id: 'openssl-root-and-intermediate', env: { NODE_OPTIONS: '--use-openssl-ca', SSL_CERT_FILE: fixtures.both }, leaf: 'intermediate' },
    { id: 'openssl-explicit-server-reject', env: { NODE_OPTIONS: '--use-openssl-ca', SSL_CERT_FILE: fixtures.trusted } },
    { id: 'openssl-explicit-intermediate-trust', env: { NODE_OPTIONS: '--use-openssl-ca', SSL_CERT_FILE: fixtures.accepted }, leaf: 'intermediate' },
    { id: 'extra-trusted-label-ignored', env: { NODE_EXTRA_CA_CERTS: fixtures.trusted } },
  ];
  const p521Tls12 = [];
  for (const leaf of ['p521-leaf', 'p521-leaf-tls12', ...Object.keys(fixtures.leaves).filter(name => /^p(?:256|384|521)-sha\d+-TLSv1\.[23]$/.test(name))]) {
    const wrongTls13Pair = leaf.endsWith('TLSv1.3') && !['p256-sha256-TLSv1.3', 'p384-sha384-TLSv1.3', 'p521-sha512-TLSv1.3'].includes(leaf);
    const row = { id: leaf, leaf, env: { NODE_EXTRA_CA_CERTS: fixtures.ca }, inspect_handshake: true, expected_status: wrongTls13Pair ? 502 : 200 };
    // OpenSSL TLS1.2 servers check the certificate curve against the client's
    // supported groups. Ring has no P521 key exchange; never advertise a group
    // that cannot actually be negotiated merely to enable this certificate.
    if (leaf === 'p521-leaf-tls12' || /^p521-sha\d+-TLSv1\.2$/.test(leaf)) p521Tls12.push({ ...row, expected_native_status: 502 });
    else cases.push(row);
  }
  const versionOptions = [
    '', '--tls-min-v1.2', '--tls-min-v1.3', '--tls-max-v1.2', '--tls-max-v1.3',
    '"--tls_min_v1.3=false"', '--tls_max_v1.2=0',
    '--tls-min-v1.2 --tls-min-v1.3', '--tls-min-v1.3 --tls-min-v1.2',
    '--tls-max-v1.2 --tls-max-v1.3', '--tls-max-v1.3 --tls-max-v1.2',
    '--tls-min-v1.3 --no-tls-min-v1.3', '--tls-max-v1.2 --no-tls-max-v1.2',
    '--tls-min-v1.3 --tls-min-v1.2 --no-tls-min-v1.2',
    '--tls-max-v1.3 --tls-max-v1.2 --no-tls-max-v1.3',
    '--tls-min-v1.0 --no-tls-min-v1.0',
  ];
  for (const [index, option] of versionOptions.entries()) {
    for (const version of ['TLSv1.2', 'TLSv1.3']) {
      cases.push({ id: `version-options-${index}-${version}`, leaf: `only-${version}`, env: { NODE_OPTIONS: option, NODE_EXTRA_CA_CERTS: fixtures.ca }, inspect_handshake: true });
    }
  }
  for (const [index, option] of [
    '--tls-min-v1.3 --tls-max-v1.2', '--tls-max-v1.2 --tls-min-v1.3',
    '--tls_min_v1.3=false --tls_max_v1.2=0',
    '--tls-min-v1.2 --tls-min-v1.3 --tls-max-v1.2 --tls-max-v1.3',
    '--tls-min-v1.3 --tls-max-v1.2 --no-tls-max-v1.2',
    '--tls-min-v1.3 --tls-max-v1.2 --no-tls-min-v1.3',
    '--use-openssl-ca --use-bundled-ca --tls-min-v1.3 --tls-max-v1.2',
  ].entries()) {
    cases.push({ id: `version-conflict-${index}`, env: { NODE_OPTIONS: option, NODE_EXTRA_CA_CERTS: fixtures.ca }, inspect_handshake: true });
  }
  // These modes are declared unqualified before measurement. They remain in a
  // separate failing characterization report, not counted as passing parity.
  const pending = [
    ...p521Tls12,
    ...['--tls-min-v1.0', '--tls-min-v1.1', '--tls-cipher-list=DEFAULT'].map(option => ({ id: option, env: { NODE_OPTIONS: option, NODE_EXTRA_CA_CERTS: fixtures.ca } })),
    { id: 'OPENSSL_CONF', env: { OPENSSL_CONF: fixtures.empty, NODE_EXTRA_CA_CERTS: fixtures.ca } },
    { id: 'openssl-config', env: { NODE_OPTIONS: `--openssl-config=${fixtures.empty}`, NODE_EXTRA_CA_CERTS: fixtures.ca } },
    { id: 'openssl-shared-config', env: { NODE_OPTIONS: '--openssl-shared-config', OPENSSL_CONF: fixtures.empty, NODE_EXTRA_CA_CERTS: fixtures.ca } },
    { id: 'disabled-certificate-verification', env: { NODE_TLS_REJECT_UNAUTHORIZED: '0' } },
  ].map(row => ({ ...row, unqualified: true }));
  const results = [];
  for (const row of [...cases, ...pending]) {
    const result = { id: row.id, ...(row.unqualified ? { unqualified: true } : {}) };
    for (const name of ['node', 'rust']) {
      const providerState = handshakes[row.leaf ?? 'direct'];
      const before = { connections: providerState.connections, completed: providerState.completed.length };
      if (row.mutate) {
        if (row.missing) await rm(row.mutate, { force: true });
        else await writeFile(row.mutate, '');
      }
      const child = await gateway(name, `https://${row.host ?? '127.0.0.1'}:${providers[row.leaf ?? 'direct'].address().port}`, { ...base, ...row.env }, { allowStartupFailure: true });
      if (child.startup_failure) result[name] = { startup_failure: child.startup_failure };
      else {
        try {
          result[name] = await request(child.port);
          if (row.mutate) {
            const first = result[name];
            await writeFile(row.mutate, await readFile(fixtures.ca));
            result[name] = { before_mutation: first, after_mutation: await request(child.port) };
          }
        } finally { await child.stop(); }
      }
      if (row.inspect_handshake) {
        // Every candidate runs in a fresh process. Count TCP attempts as well as
        // successful handshakes: a failed connection must not trigger a retry
        // with a different version, scheme, or verifier.
        result[name].handshake = { connections: providerState.connections - before.connections, completed: providerState.completed.slice(before.completed) };
      }
    }
    result.matched = JSON.stringify(result.node) === JSON.stringify(result.rust);
    if (row.expected_status !== undefined) result.matched &&= result.node.status === row.expected_status;
    if (row.inspect_handshake) {
      result.handshake_obligations_met = true;
      for (const name of ['node', 'rust']) {
        const expectedConnections = result[name].startup_failure ? 0 : 1;
        result.handshake_obligations_met &&= result[name].handshake.connections === expectedConnections && result[name].handshake.completed.every(handshake => !handshake.resumed);
      }
      result.matched &&= result.handshake_obligations_met;
    }
    if (row.unqualified) result.expected_difference = result.handshake_obligations_met !== false && result.node.status === 200 && (
      row.expected_native_status ? result.rust.status === row.expected_native_status :
      result.rust.startup_failure?.exit_code === 1 && !result.rust.startup_failure.timed_out &&
      result.rust.startup_failure.stderr.startsWith('Unsupported Node TLS trust options;'));
    results.push(result);
  }
  for (const server of Object.values(providers)) { server.closeAllConnections(); await close(server); }
  const qualified = results.filter(row => !row.unqualified), unqualified = results.filter(row => row.unqualified);
  const unexpected = results.filter(row => !row.matched && !row.expected_difference);
  const common = { schema_version: 1, identity, fixture_openssl: (await openssl('version')).stdout.trim(), reference_openssl: process.versions.openssl, source_contract: 'Official Node22.14/OpenSSL3.0.15; isolated SSL_CERT_FILE/DIR; native OpenSSL3.6.3 is a separate pinned implementation' };
  const report = { ...common, kind: 'TLS_supported_options_parity', passed: qualified.every(row => row.matched), matched: qualified.filter(row => row.matched).length, count: qualified.length, corpus_sha256: createHash('sha256').update(JSON.stringify(cases).replaceAll(scratch, '<fixture>')).digest('hex'), complete_tls_parity: false, unqualified_report: 'tls-policy-characterization.json', results: qualified };
  const characterization = { ...common, kind: 'TLS_unqualified_policy_characterization', passed: unqualified.every(row => row.matched), matched: unqualified.filter(row => row.matched).length, count: unqualified.length, corpus_sha256: createHash('sha256').update(JSON.stringify(pending).replaceAll(scratch, '<fixture>')).digest('hex'), results: unqualified };
  await writeFile(join(root, 'artifacts/rust-rewrite/parity-tls-options.json'), JSON.stringify(report, null, 2) + '\n');
  const characterizationPath = join(root, 'artifacts/rust-rewrite/tls-policy-characterization.json');
  const previous = await readFile(characterizationPath).catch(error => { if (error.code === 'ENOENT') return null; throw error; });
  if (previous) {
    const archive = join(root, 'artifacts/rust-rewrite/evidence'); await mkdir(archive, { recursive: true });
    await writeFile(join(archive, `tls-characterization-${createHash('sha256').update(previous).digest('hex')}.json`), previous, { flag: 'wx' }).catch(error => { if (error.code !== 'EEXIST') throw error; });
  }
  await writeFile(characterizationPath, JSON.stringify(characterization, null, 2) + '\n');
  console.log(JSON.stringify({ passed: report.passed, matched: report.matched, count: report.count, unqualified: unqualified.length, unexpected_differences: unexpected }, null, 2));
  if (unexpected.length) process.exitCode = 1;
} finally { await cleanup(); }
