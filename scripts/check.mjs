import { readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { checkContracts } from './check-contracts.mjs';
import { checkStyles } from './check-style.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));
let count = 0;
const paths = [];
for (const directory of ['bin', 'src', 'scripts', 'test']) {
  for (const filename of readdirSync(join(root, directory)).filter(name => name.endsWith('.mjs')).sort()) {
    const result = spawnSync(process.execPath, ['--check', join(root, directory, filename)], { stdio: 'inherit' });
    if (result.error || result.status !== 0) process.exit(result.status || 1);
    count++;
    paths.push(join(root, directory, filename));
  }
}
console.log(`Syntax checked ${count} JavaScript files.`);
paths.push(join(root, 'test/static-contracts.mts'));
const typecheck = spawnSync(process.execPath, [join(root, 'node_modules/typescript/bin/tsc'), '-p', join(root, 'tsconfig.json')], { stdio: 'inherit' });
if (typecheck.error || typecheck.status !== 0) process.exit(typecheck.status || 1);
const errors = [...await checkStyles(paths), ...await checkContracts(paths.filter(path => /[\\/](?:bin|src)[\\/]/.test(path)))];
for (const error of errors) console.error(error);
if (errors.length) process.exitCode = 1;
else console.log('Type contracts, event producers and style policy checked.');
