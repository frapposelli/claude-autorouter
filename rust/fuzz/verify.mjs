// Offline inventory check for the separate, opt-in fuzz workspace.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFile, readdir, lstat } from 'node:fs/promises';
import { basename, dirname, join, resolve } from 'node:path';

const root = import.meta.dirname;
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const manifest = JSON.parse(await readFile(join(root, 'provenance.json'), 'utf8'));
const seeds = JSON.parse(await readFile(join(root, 'seeds.json'), 'utf8'));
function metadata(directory, manifestPath) {
  const result = spawnSync('cargo', ['metadata', '--manifest-path', manifestPath, '--offline', '--locked', '--format-version=1'], { cwd: directory, encoding: 'utf8', timeout: 30000, maxBuffer: 16 * 1024 * 1024 });
  assert.equal(result.status, 0, 'Locked offline metadata must succeed');
  return JSON.parse(result.stdout);
}
const fuzz = metadata(dirname(root), join(root, 'Cargo.toml'));
const main = metadata(dirname(root), join(root, '../Cargo.toml'));
assert.ok(!main.packages.some(pkg => pkg.name === 'libfuzzer-sys'), 'Fuzzer must not enter product workspace');
const mainIds = new Set(main.packages.filter(pkg => pkg.source).map(pkg => `${pkg.name}@${pkg.version}`));
const added = fuzz.packages.filter(pkg => pkg.source && !mainIds.has(`${pkg.name}@${pkg.version}`));
assert.deepEqual(added.map(pkg => `${pkg.name}@${pkg.version}`).sort(), manifest.added_registry_dependencies.map(pkg => `${pkg.name}@${pkg.version}`).sort());
const lock = await readFile(join(root, 'Cargo.lock'), 'utf8');
assert.equal(hash(lock), manifest.fuzz_lock_sha256, 'Fuzz lock differs from the reviewed build input');
function registryLocks(text) {
  const result = new Map();
  for (const block of text.split('[[package]]').slice(1)) {
    const fields = Object.fromEntries([...block.matchAll(/^(name|version|source|checksum) = "([^"\n]+)"$/gm)].map(match => [match[1], match[2]]));
    if (!fields.source?.startsWith('registry+')) continue;
    assert.ok(fields.name && fields.version && fields.checksum, 'Incomplete registry lock entry');
    const key = `${fields.name}@${fields.version}`;
    assert.ok(!result.has(key), 'Ambiguous registry lock identity');
    result.set(key, fields);
  }
  return result;
}
const mainLocks = registryLocks(await readFile(join(root, '../Cargo.lock'), 'utf8'));
for (const [id, entry] of registryLocks(lock)) {
  if (mainLocks.has(id)) assert.deepEqual(entry, mainLocks.get(id), `Shared dependency source/checksum drift: ${id}`);
}
async function files(directory, relative = '') {
  const output = {};
  for (const name of await readdir(join(directory, relative))) {
    if (!relative && ['.cargo-ok', '.cargo-checksum.json'].includes(name)) continue;
    const path = join(relative, name), stat = await lstat(join(directory, path));
    assert.equal(stat.isSymbolicLink(), false);
    if (stat.isDirectory()) Object.assign(output, await files(directory, path));
    else output[path] = hash(await readFile(join(directory, path)));
  }
  return output;
}
for (const row of manifest.added_registry_dependencies) {
  const pkg = added.find(pkg => pkg.name === row.name && pkg.version === row.version);
  assert.equal(pkg.license, row.license);
  assert.equal(pkg.source, row.source);
  const directory = dirname(pkg.manifest_path);
  const archive = resolve(directory, '../../..', 'cache', basename(dirname(directory)), `${row.name}-${row.version}.crate`);
  assert.equal(hash(await readFile(archive)), row.crate_sha256);
  const block = lock.split('[[package]]').find(block => block.includes(`name = "${row.name}"\n`) && block.includes(`version = "${row.version}"\n`));
  assert.ok(block?.includes(`checksum = "${row.crate_sha256}"`));
  for (const license of row.license_files) assert.equal(hash(await readFile(join(directory, license.path))), license.sha256);
  if (row.upstream_files) assert.deepEqual(await files(directory), row.upstream_files);
}
assert.equal(hash(await readFile(join(root, manifest.external_license.local_path))), manifest.external_license.sha256);
const actualSeeds = await files(join(root, 'seeds'));
assert.deepEqual(Object.keys(actualSeeds).sort(), seeds.seeds.map(seed => seed.path.slice('seeds/'.length)).sort());
for (const seed of seeds.seeds) {
  const bytes = await readFile(join(root, seed.path));
  assert.equal(bytes.length, seed.bytes);
  assert.equal(hash(bytes), seed.sha256);
  assert.ok(bytes.length <= 65536);
}
console.log(`Verified ${added.length} fuzz-only dependencies and ${seeds.seeds.length} synthetic seeds; product workspace excludes libFuzzer.`);
