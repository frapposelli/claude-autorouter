// Offline verification of the explicit native TLS root-set build dependency.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createHash, X509Certificate } from 'node:crypto';
const directory = new URL('./node-ca/', import.meta.url);
const metadata = JSON.parse(await readFile(new URL('provenance.json', directory), 'utf8'));
const pem = await readFile(new URL('node-v22.14.0.pem', directory));
const license = await readFile(new URL('LICENSE', directory));
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
assert.equal(hash(pem), metadata.pem_sha256, 'Bundled root file hash changed');
assert.equal(hash(license), metadata.license_sha256, 'Node license hash changed');
const certificates = pem.toString().match(/-----BEGIN CERTIFICATE-----[\s\S]*?-----END CERTIFICATE-----/g) ?? [];
assert.equal(certificates.length, metadata.certificate_count);
assert.deepEqual(certificates.map(pem => { const cert = new X509Certificate(pem); return { subject: cert.subject, sha256: cert.fingerprint256.replaceAll(':', '').toLowerCase() }; }), metadata.certificates);
assert.equal(metadata.official_source_der_fingerprints_match, true);
console.log(`Verified ${certificates.length} bundled roots from ${metadata.node_version}`);
