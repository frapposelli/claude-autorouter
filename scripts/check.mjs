import { readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

const root = fileURLToPath(new URL('../', import.meta.url));
let count = 0;
for (const directory of ['bin', 'src', 'scripts', 'test']) {
  for (const filename of readdirSync(join(root, directory)).filter(name => name.endsWith('.mjs')).sort()) {
    const result = spawnSync(process.execPath, ['--check', join(root, directory, filename)], { stdio: 'inherit' });
    if (result.error || result.status !== 0) process.exit(result.status || 1);
    count++;
  }
}
console.log(`Syntax checked ${count} JavaScript files.`);
