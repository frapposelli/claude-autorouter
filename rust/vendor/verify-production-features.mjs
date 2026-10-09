// Audit an actual ordinary shipping Cargo build. This does not infer production
// features from the workspace's dev-feature-unified dependency graph.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile, lstat, realpath } from 'node:fs/promises';
import { resolve, dirname, basename, sep } from 'node:path';
import { pathToFileURL } from 'node:url';

const root = resolve(import.meta.dirname, '..');
const manifest = resolve(root, 'vendor/hyper-util/Cargo.toml');
const cliManifest = resolve(root, 'crates/claude-autorouter/Cargo.toml');
const limit = 64 * 1024 * 1024;
const hash = value => createHash('sha256').update(value).digest('hex');
const fail = message => { throw Error(message); };

export function inspectBuildReport(bytes) {
  if (bytes.length > limit) fail('Cargo report exceeds the byte limit');
  const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  const lines = text.split('\n');
  if (lines.length > 100000) fail('Cargo report exceeds the record limit');
  const libraries = [];
  const binaries = [];
  let finished = false;
  for (const line of lines) {
    if (!line.trim()) continue;
    if (Buffer.byteLength(line) > 2 * 1024 * 1024) fail('Cargo record exceeds the byte limit');
    if (finished) fail('Cargo records follow build-finished');
    const row = JSON.parse(line);
    if (!row || typeof row !== 'object' || Array.isArray(row)) fail('Invalid Cargo record');
    if (row.reason === 'build-finished') {
      if (row.success !== true) fail('Cargo build did not succeed');
      finished = true;
    }
    if (row.reason === 'compiler-message' && row.message?.level === 'error') fail('Cargo emitted an error');
    if (row.reason !== 'compiler-artifact') continue;
    if (row.target?.name === 'hyper_util' && row.target.kind?.includes('lib')) {
      const packagePrefix = `path+${pathToFileURL(dirname(manifest)).href}#`;
      if (row.manifest_path !== manifest || ![`${packagePrefix}0.1.21`, `${packagePrefix}hyper-util@0.1.21`].includes(row.package_id)) fail('Hyper-util is not the exact vendored package');
      if (!Array.isArray(row.features) || row.features.some(feature => typeof feature !== 'string')) fail('Invalid Hyper-util feature list');
      if (row.features.includes('node-http1-request-lease')) fail('Shipping build enables experimental request leases');
      if (row.features.includes('node-http1-raw-pool')) fail('Shipping build enables the experimental raw HTTP/1 pool');
      if (row.profile?.test !== false || row.profile?.opt_level !== '3' || row.profile?.debug_assertions !== false) fail('Hyper-util artifact is not an ordinary release build');
      libraries.push(row);
    }
    if (row.target?.name === 'claude-autorouter' && row.target.kind?.includes('bin')) {
      if (row.manifest_path !== cliManifest || row.profile?.test !== false || row.profile?.opt_level !== '3' || row.profile?.debug_assertions !== false) fail('CLI artifact is not an ordinary release build');
      if (typeof row.executable !== 'string' || !row.filenames?.includes(row.executable) || !row.executable.split(sep).includes('release') || !['claude-autorouter', 'claude-autorouter.exe'].includes(basename(row.executable))) fail('Invalid release executable artifact');
      binaries.push(row);
    }
  }
  if (!finished || libraries.length !== 1 || binaries.length !== 1) fail('Cargo report lacks a unique successful shipping build');
  return { library: libraries[0], binary: binaries[0] };
}

function selfTest() {
  const library = { reason: 'compiler-artifact', package_id: `path+${pathToFileURL(dirname(manifest)).href}#0.1.21`, manifest_path: manifest, target: { name: 'hyper_util', kind: ['lib'] }, features: ['client', 'client-legacy', 'http1', 'tokio'], profile: { test: false, opt_level: '3', debug_assertions: false } };
  const executable = resolve(root, 'target/release/claude-autorouter');
  const binary = { reason: 'compiler-artifact', manifest_path: cliManifest, target: { name: 'claude-autorouter', kind: ['bin'] }, profile: { test: false, opt_level: '3', debug_assertions: false }, executable, filenames: [executable] };
  const done = { reason: 'build-finished', success: true };
  const encode = rows => Buffer.from(rows.map(row => JSON.stringify(row)).join('\n'));
  assert.equal(inspectBuildReport(encode([library, binary, done])).binary.executable, executable);
  // Check either feature independently: raw-pool's dependency must not be the
  // only reason a forged or future altered graph is rejected.
  for (const feature of ['node-http1-request-lease', 'node-http1-raw-pool']) {
    const report = [{ ...library, features: [...library.features, feature] }, binary, done];
    assert.throws(() => inspectBuildReport(encode(report)), /Shipping build enables/);
  }
  const invalid = [[], [binary, done], [library, done], [library, binary], [library, binary, { ...done, success: false }], [library, library, binary, done], [library, binary, done, done], [{ ...library, features: [...library.features, 'node-http1-request-lease'] }, binary, done], [{ ...library, manifest_path: '/different/Cargo.toml' }, binary, done], [{ ...library, package_id: library.package_id.replace('0.1.21', '0.1.20') }, binary, done], [{ ...library, features: null }, binary, done], [library, { ...binary, profile: { ...binary.profile, test: true } }, done], [library, { ...binary, profile: { ...binary.profile, opt_level: '0' } }, done], [library, { ...binary, executable: '/tmp/debug/claude-autorouter' }, done], [library, binary, { reason: 'compiler-message', message: { level: 'error' } }, done]];
  for (const rows of invalid) assert.throws(() => inspectBuildReport(encode(rows)));
  for (const bytes of [Buffer.from('{bad'), Buffer.from('null'), Buffer.from([255]), Buffer.alloc(limit + 1), Buffer.from(' '.repeat(2 * 1024 * 1024) + '{}'), Buffer.from('\n'.repeat(100001))]) assert.throws(() => inspectBuildReport(bytes));
  console.log(`Production feature auditor self-test: 1 accepted and ${invalid.length + 8} rejected controls (including each experimental feature independently).`);
}

async function boundedFile(path, bound) {
  const stat = await lstat(path);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size > bound) fail('Audit input must be a bounded regular file');
  const bytes = await readFile(path);
  if (bytes.length > bound) fail('Audit input grew beyond its bound');
  return bytes;
}

if (process.argv[1] && resolve(process.argv[1]) === import.meta.filename) {
  if (process.argv.length === 3 && process.argv[2] === '--self-test') selfTest();
  else if (process.argv.length === 4 && process.argv[2] === '--build-report') {
    const bytes = await boundedFile(resolve(process.argv[3]), limit);
    const inspected = inspectBuildReport(bytes);
    const executable = inspected.binary.executable;
    if (await realpath(executable) !== resolve(executable)) fail('Release executable must not be a symlink');
    const binary = await boundedFile(executable, 256 * 1024 * 1024);
    console.log(JSON.stringify({ passed: true, report_sha256: hash(bytes), binary: executable, binary_sha256: hash(binary), package_id: inspected.library.package_id, manifest_path: manifest, features: inspected.library.features, lease_feature: false, raw_pool_feature: false }, null, 2));
  } else fail('Usage: node rust/vendor/verify-production-features.mjs --self-test | --build-report PATH');
}
