// Source-only local-patch audit: no downloads or execution of vendored code.
import { createHash } from 'node:crypto';
import { cp, lstat, mkdtemp, readFile, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
const directory = import.meta.dirname;
const manifest = JSON.parse(await readFile(join(directory, 'hyper-util-provenance.json'), 'utf8'));
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
async function inventory(root, prefix = '') {
  const result = {};
  for (const name of (await readdir(join(root, prefix))).sort()) {
    const path = join(prefix, name);
    const stat = await lstat(join(root, path));
    if (stat.isSymbolicLink()) throw Error('Vendored tree contains a symlink');
    if (stat.isDirectory()) Object.assign(result, await inventory(root, path));
    else {
      if (!stat.isFile() || stat.size > 16 * 1024 * 1024) throw Error('Vendored tree contains an invalid member');
      result[path.split('\\').join('/')] = hash(await readFile(join(root, path)));
    }
  }
  return result;
}
function verify(actual, expected, label) {
  for (const key of new Set([...Object.keys(actual), ...Object.keys(expected)])) {
    if (actual[key] !== expected[key]) throw Error(`${label} mismatch: ${key}`);
  }
}
if (manifest.version !== '0.1.21' || Object.keys(manifest.upstream_files).length !== 62 || Object.keys(manifest.patched_files).length !== 63) throw Error('Unexpected Hyper-util provenance shape');
const changed = Object.keys(manifest.patched_files).filter(path => manifest.patched_files[path] !== manifest.upstream_files[path]).sort();
if (JSON.stringify(changed) !== JSON.stringify([...manifest.changed_files].sort())) throw Error('Changed-path allowlist mismatch');
if (JSON.stringify(changed) !== JSON.stringify(['Cargo.toml', 'Cargo.toml.orig', 'src/client/legacy/client.rs', 'src/client/legacy/connect/mod.rs', 'src/client/legacy/connect/request_lease.rs'])) throw Error('Hyper-util patch exceeds its reviewed scope');
verify(await inventory(join(directory, 'hyper-util')), manifest.patched_files, 'Patched Hyper-util');
for (const name of ['Cargo.toml', 'Cargo.toml.orig']) {
  const source = await readFile(join(directory, 'hyper-util', name), 'utf8');
  const features = source.split('[features]\n')[1]?.split(/^\[/m)[0];
  if (!features) throw Error(`Missing feature section in ${name}`);
  const array = key => {
    const value = features.match(new RegExp(`^${key}\\s*=\\s*\\[([\\s\\S]*?)\\]`, 'm'))?.[1];
    if (value === undefined || value.replace(/"[^"]*"/g, '').replace(/[\s,]/g, '') !== '') throw Error(`Invalid ${key} feature array in ${name}`);
    return [...value.matchAll(/"([^"]*)"/g)].map(match => match[1]);
  };
  if (JSON.stringify(array('node-http1-raw-pool')) !== '["node-http1-request-lease"]') throw Error(`Raw-pool feature must depend only on request-lease in ${name}`);
  if (JSON.stringify(array('node-http1-request-lease')) !== '["client-legacy","http1"]') throw Error(`Request-lease feature dependencies changed in ${name}`);
  if (array('default').length !== 0 || array('full').some(feature => ['node-http1-request-lease', 'node-http1-raw-pool'].includes(feature))) throw Error(`Experimental feature enabled by default/full in ${name}`);
}
const patch = resolve(directory, 'hyper-util-request-lease.patch');
if (hash(await readFile(patch)) !== manifest.patch_sha256) throw Error('Hyper-util patch hash mismatch');
const scratch = await mkdtemp(join(tmpdir(), 'autorouter-hyper-util-provenance-'));
function apply(reverse) {
  const result = spawnSync('git', ['apply', '--unidiff-zero', '--unsafe-paths', ...(reverse ? ['--reverse'] : []), patch], { cwd: scratch, stdio: 'pipe', timeout: 30000, maxBuffer: 1024 * 1024 });
  if (result.status !== 0 || result.error) throw Error(`Hyper-util patch ${reverse ? 'reverse' : 'forward'} verification failed`);
}
try {
  await cp(join(directory, 'hyper-util'), scratch, { recursive: true });
  apply(true);
  verify(await inventory(scratch), manifest.upstream_files, 'Reconstructed upstream Hyper-util');
  apply(false);
  verify(await inventory(scratch), manifest.patched_files, 'Reapplied Hyper-util patch');
} finally { await rm(scratch, { recursive: true, force: true }); }
console.log(`Verified Hyper-util ${manifest.version}: 62 exact upstream members, one added lease module, five reviewed patch paths; forward/reverse patch and default-off lease/raw-pool features verified.`);
