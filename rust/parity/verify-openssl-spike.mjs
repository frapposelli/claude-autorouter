// Offline source/license verification for the dev-only transport experiment.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFile, readdir, lstat } from 'node:fs/promises';
import { basename, dirname, join, resolve } from 'node:path';
const root = resolve(import.meta.dirname, '../..');
const manifest = JSON.parse(await readFile(join(import.meta.dirname, 'openssl-spike-provenance.json'), 'utf8'));
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const metadata = spawnSync('cargo', ['metadata', '--offline', '--locked', '--format-version=1'], { cwd: join(root, 'rust'), encoding: 'utf8', timeout: 30000, maxBuffer: 16 * 1024 * 1024 });
assert.equal(metadata.status, 0, 'Locked offline Cargo metadata must succeed');
const graph = JSON.parse(metadata.stdout);
const dependency = graph.packages.find(pkg => pkg.name === manifest.name && pkg.version === manifest.version);
assert.ok(dependency, 'Pinned stream dependency missing');
assert.equal(dependency.source, manifest.registry_source);
assert.equal(dependency.license, manifest.license);
assert.deepEqual(graph.resolve.nodes.find(node => node.id === dependency.id).features, manifest.features);
const runtime = graph.packages.find(pkg => pkg.name === 'autorouter-runtime');
const declared = runtime.dependencies.find(dep => dep.name === manifest.name);
assert.equal(declared.kind, 'dev');
assert.equal(declared.req, `=${manifest.version}`);
assert.equal(declared.uses_default_features, manifest.default_features);
const directory = dirname(dependency.manifest_path);
const archive = resolve(directory, '../../..', 'cache', basename(dirname(directory)), `${manifest.name}-${manifest.version}.crate`);
assert.equal(hash(await readFile(archive)), manifest.crate_sha256, 'Published crate hash mismatch');
const lock = await readFile(join(root, 'rust/Cargo.lock'), 'utf8');
const block = lock.split('[[package]]').find(block => block.includes(`name = "${manifest.name}"\n`) && block.includes(`version = "${manifest.version}"\n`));
assert.ok(block?.includes(`checksum = "${manifest.crate_sha256}"`), 'Cargo lock checksum differs from reviewed archive');
const vcs = JSON.parse(await readFile(join(directory, '.cargo_vcs_info.json'), 'utf8'));
assert.equal(vcs.git.sha1, manifest.source_commit);
async function inventory(relative = '') {
  const result = {};
  for (const name of await readdir(join(directory, relative))) {
    if (!relative && ['.cargo-ok', '.cargo-checksum.json'].includes(name)) continue;
    const path = join(relative, name), stat = await lstat(join(directory, path));
    assert.equal(stat.isSymbolicLink(), false, 'Source symlinks are not expected');
    if (stat.isDirectory()) Object.assign(result, await inventory(path));
    else result[path] = hash(await readFile(join(directory, path)));
  }
  return result;
}
assert.deepEqual(await inventory(), manifest.upstream_files, 'Extracted source/license inventory differs from published archive');
for (const path of manifest.license_files) assert.ok(manifest.upstream_files[path], 'Full source license missing');
console.log(`Verified dev-only ${manifest.name} ${manifest.version}: archive, lock, features, ${Object.keys(manifest.upstream_files).length} source files and both complete licenses.`);
