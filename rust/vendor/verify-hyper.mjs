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
const changed = Object.keys(manifest.patched_files).filter(path => manifest.patched_files[path] !== manifest.upstream_files[path]).sort();
if (JSON.stringify(changed) !== JSON.stringify([...manifest.changed_files].sort())) throw Error('Hyper changed-path allowlist mismatch');
for (const name of ['Cargo.toml', 'Cargo.toml.orig']) {
  const source = await readFile(join(directory, 'hyper', name), 'utf8');
  const features = source.split('[features]\n')[1]?.split(/^\[/m)[0];
  if (!features) throw Error(`Missing feature section in ${name}`);
  const array = key => {
    const value = features.match(new RegExp(`^${key}\\s*=\\s*\\[([\\s\\S]*?)\\]`, 'm'))?.[1];
    if (value === undefined || value.replace(/"[^"]*"/g, '').replace(/[\s,]/g, '') !== '') throw Error(`Invalid ${key} feature array in ${name}`);
    return [...value.matchAll(/"([^"]*)"/g)].map(match => match[1]);
  };
  if (JSON.stringify(array('node-http1-body-handoff')) !== '["node-http1-compat"]') throw Error(`Handoff feature dependency changed in ${name}`);
  if (JSON.stringify(array('node-http1-compat')) !== '["http1","server"]') throw Error(`Compatibility feature dependency changed in ${name}`);
  if (array('default').length !== 0 || array('full').includes('node-http1-body-handoff')) throw Error(`Experimental handoff enabled by default/full in ${name}`);
}
const patch = join(directory,'hyper-node-http1-compat.patch');
if(hash(await readFile(patch))!==manifest.patch_sha256) throw Error('Hyper patch hash mismatch');
const scratch = await mkdtemp(join(tmpdir(),'autorouter-hyper-provenance-'));
try {
  await cp(join(directory,'hyper'),scratch,{recursive:true});
  const applied = spawnSync('git',['apply','--reverse','--unsafe-paths',resolve(patch)],{cwd:scratch,stdio:'pipe'});
  if(applied.status!==0) throw Error('Vendored Hyper patch does not reverse cleanly');
  verify(await inventory(scratch),manifest.upstream_files,'Reconstructed upstream Hyper');
  const reapplied = spawnSync('git',['apply','--unsafe-paths',resolve(patch)],{cwd:scratch,stdio:'pipe',timeout:30000,maxBuffer:1024*1024});
  if(reapplied.status!==0 || reapplied.error) throw Error('Vendored Hyper patch does not reapply cleanly');
  verify(await inventory(scratch),manifest.patched_files,'Reapplied Hyper');
} finally { await rm(scratch,{recursive:true,force:true}); }
console.log(`Verified Hyper ${manifest.version}: ${Object.keys(manifest.upstream_files).length} upstream files and ${manifest.changed_files.length} explicitly patched paths.`);
