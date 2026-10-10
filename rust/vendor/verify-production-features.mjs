// Audit an actual ordinary shipping Cargo build. This does not infer production
// features from the workspace's dev-feature-unified dependency graph.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile, lstat, realpath } from 'node:fs/promises';
import { resolve, dirname, basename, sep } from 'node:path';
import { pathToFileURL } from 'node:url';

const root = resolve(import.meta.dirname, '..');
const manifest = resolve(root, 'vendor/hyper-util/Cargo.toml');
const hyperManifest = resolve(root, 'vendor/hyper/Cargo.toml');
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
  const hyperLibraries = [];
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
    if (row.target?.name === 'hyper' && row.target.kind?.includes('lib')) {
      const packagePrefix = `path+${pathToFileURL(dirname(hyperManifest)).href}#`;
      if (row.manifest_path !== hyperManifest || ![`${packagePrefix}1.12.0`, `${packagePrefix}hyper@1.12.0`].includes(row.package_id)) fail('Hyper is not the exact vendored package');
      if (!Array.isArray(row.features) || row.features.some(feature => typeof feature !== 'string')) fail('Invalid Hyper feature list');
      if (row.features.includes('node-http1-body-handoff')) fail('Shipping build enables the experimental body handoff');
      if (row.profile?.test !== false || row.profile?.opt_level !== '3' || row.profile?.debug_assertions !== false) fail('Hyper artifact is not an ordinary release build');
      hyperLibraries.push(row);
    }
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
  if (!finished || libraries.length !== 1 || hyperLibraries.length !== 1 || binaries.length !== 1) fail('Cargo report lacks a unique successful shipping build');
  return { library: libraries[0], hyper_library: hyperLibraries[0], binary: binaries[0] };
}

function selfTest() {
  const profile = { test: false, opt_level: '3', debug_assertions: false };
  const library = { reason: 'compiler-artifact', package_id: `path+${pathToFileURL(dirname(manifest)).href}#0.1.21`, manifest_path: manifest, target: { name: 'hyper_util', kind: ['lib'] }, features: ['client', 'client-legacy', 'http1', 'tokio'], profile };
  const hyper = { reason: 'compiler-artifact', package_id: `path+${pathToFileURL(dirname(hyperManifest)).href}#1.12.0`, manifest_path: hyperManifest, target: { name: 'hyper', kind: ['lib'] }, features: ['client', 'http1', 'server', 'node-http1-compat'], profile };
  const executable = resolve(root, 'target/release/claude-autorouter');
  const binary = { reason: 'compiler-artifact', manifest_path: cliManifest, target: { name: 'claude-autorouter', kind: ['bin'] }, profile, executable, filenames: [executable] };
  const done = { reason: 'build-finished', success: true };
  const encode = rows => Buffer.from(rows.map(row => JSON.stringify(row)).join('\n'));
  const valid = [library, hyper, binary, done];
  assert.equal(inspectBuildReport(encode(valid)).binary.executable, executable);
  assert.equal(inspectBuildReport(encode(valid)).hyper_library.manifest_path, hyperManifest);
  let rejected = 0;
  const rejects = (rows, pattern) => { assert.throws(() => inspectBuildReport(encode(rows)), pattern); rejected++; };
  // Each experiment must be rejected by itself, independently of dependencies.
  for (const feature of ['node-http1-request-lease', 'node-http1-raw-pool']) {
    rejects([{ ...library, features: [...library.features, feature] }, hyper, binary, done], /Shipping build enables/);
  }
  rejects([library, { ...hyper, features: [...hyper.features, 'node-http1-body-handoff'] }, binary, done], /Shipping build enables/);
  for (let index = 0; index < valid.length; index++) rejects(valid.filter((_, position) => position !== index));
  for (const artifact of [library, hyper, binary]) rejects([artifact, ...valid]);
  for (const artifact of [library, hyper]) {
    for (const replacement of [
      { ...artifact, manifest_path: '/different/Cargo.toml' },
      { ...artifact, package_id: artifact.package_id + '-wrong' },
      { ...artifact, features: null },
      { ...artifact, features: [123] },
      { ...artifact, profile: { ...profile, test: true } },
      { ...artifact, profile: { ...profile, opt_level: '0' } },
      { ...artifact, profile: { ...profile, debug_assertions: true } },
    ]) rejects(valid.map(row => row === artifact ? replacement : row));
  }
  for (const rows of [[], [library, hyper, binary, { ...done, success: false }], [...valid, done], [library, hyper, { ...binary, profile: { ...profile, test: true } }, done], [library, hyper, { ...binary, executable: '/tmp/debug/claude-autorouter' }, done], [library, hyper, binary, { reason: 'compiler-message', message: { level: 'error' } }, done]]) rejects(rows);
  for (const bytes of [Buffer.from('{bad'), Buffer.from('null'), Buffer.from([255]), Buffer.alloc(limit + 1), Buffer.from(' '.repeat(2 * 1024 * 1024) + '{}'), Buffer.from('\n'.repeat(100001))]) {
    assert.throws(() => inspectBuildReport(bytes)); rejected++;
  }
  console.log(`Production feature auditor self-test: 1 accepted and ${rejected} rejected controls (including each experimental feature independently).`);
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
    console.log(JSON.stringify({ passed: true, report_sha256: hash(bytes), binary: executable, binary_sha256: hash(binary), package_id: inspected.library.package_id, manifest_path: manifest, features: inspected.library.features, lease_feature: false, raw_pool_feature: false, hyper_package_id: inspected.hyper_library.package_id, hyper_manifest_path: hyperManifest, hyper_features: inspected.hyper_library.features, body_handoff_feature: false }, null, 2));
  } else fail('Usage: node rust/vendor/verify-production-features.mjs --self-test | --build-report PATH');
}
