// Source-only provenance verification. Never downloads or executes vendored code.
import { createHash } from 'node:crypto';
import { cp, mkdtemp, readFile, readdir, rm, lstat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
const directory = import.meta.dirname;
const manifest = JSON.parse(await readFile(join(directory, 'hyper-provenance.json'), 'utf8'));
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
async function inventory(directory, prefix='') {
  const result = {};
  for (const name of (await readdir(join(directory,prefix))).sort()) {
    const path = join(prefix,name);
    const stat = await lstat(join(directory,path));
    if (stat.isSymbolicLink()) throw Error('Vendored tree must not contain symlinks');
    if (stat.isDirectory()) Object.assign(result,await inventory(directory,path));
    else result[path] = hash(await readFile(join(directory,path)));
  }
  return Object.fromEntries(Object.entries(result).sort(([a],[b])=>a.localeCompare(b,'en')));
}
function verify(actual,expected,label) {
  const keys = new Set([...Object.keys(actual),...Object.keys(expected)]);
  for(const key of keys) if(actual[key]!==expected[key]) throw Error(`${label} mismatch: ${key}`);
}
verify(await inventory(join(directory,'hyper')),manifest.patched_files,'Patched Hyper');
const patch = join(directory,'hyper-node-http1-compat.patch');
if(hash(await readFile(patch))!==manifest.patch_sha256) throw Error('Hyper patch hash mismatch');
const scratch = await mkdtemp(join(tmpdir(),'autorouter-hyper-provenance-'));
try {
  await cp(join(directory,'hyper'),scratch,{recursive:true});
  const applied = spawnSync('git',['apply','--reverse','--unsafe-paths',resolve(patch)],{cwd:scratch,stdio:'pipe'});
  if(applied.status!==0) throw Error('Vendored Hyper patch does not reverse cleanly');
  verify(await inventory(scratch),manifest.upstream_files,'Reconstructed upstream Hyper');
} finally { await rm(scratch,{recursive:true,force:true}); }
console.log(`Verified Hyper ${manifest.version}: ${Object.keys(manifest.upstream_files).length} upstream files and ${manifest.changed_files.length} explicitly patched paths.`);
