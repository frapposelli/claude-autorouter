#!/usr/bin/env node
import { createHash } from 'node:crypto';
import { lstat, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { run } from './release-pack.mjs';

// Explicit source allowlist: never copy configuration, logs, credentials,
// node_modules, git metadata or private benchmark results to another host.
export async function createBenchmarkBundle(destination) {
  const root = fileURLToPath(new URL('../', import.meta.url));
  const temporary = await mkdtemp(join(tmpdir(), 'autorouter-benchmark-bundle-'));
  const parent = join(temporary, 'autorouter-benchmark');
  try {
    const paths = ['package.json', 'scripts/evaluate-ollama.mjs', 'docs/hardware-benchmark.md', 'docs/hardware-results-16gb.md',
      'docs/hardware-results-16gb.json', 'test/fixtures/ollama-routing.json',
      ...(await readdir(join(root, 'src'))).filter(name => /^[A-Za-z0-9_-]+\.mjs$/.test(name)).map(name => `src/${name}`)];
    const sources = {};
    for (const path of paths) {
      const input = join(root, path), target = join(parent, path);
      const info = await lstat(input);
      if (!info.isFile() || info.isSymbolicLink() || info.size > 1024 * 1024) throw new Error('Benchmark source must be a bounded regular file.');
      const bytes = await readFile(input);
      sources[path] = createHash('sha256').update(bytes).digest('hex');
      await mkdir(dirname(target), { recursive: true });
      await writeFile(target, bytes);
    }
    await writeFile(join(parent, 'source-manifest.json'), `${JSON.stringify({ schema_version: 1,
      fixture_sha256: sources['test/fixtures/ollama-routing.json'], files: sources }, null, 2)}\n`);
    await mkdir(dirname(destination), { recursive: true });
    await run('tar', ['-czf', resolve(destination), '-C', temporary, 'autorouter-benchmark']);
    const checksum = createHash('sha256').update(await readFile(destination)).digest('hex');
    await writeFile(`${destination}.sha256`, `${checksum}  ${basename(destination)}\n`);
    return { bundle: resolve(destination), sha256: checksum, files: paths.length + 1 };
  } finally { await rm(temporary, { recursive: true, force: true }); }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.argv.length > 3) throw new Error('Usage: node scripts/benchmark-bundle.mjs [destination.tar.gz]');
  const report = await createBenchmarkBundle(resolve(process.argv[2] ?? 'artifacts/autorouter-hardware-benchmark.tar.gz'));
  console.log(JSON.stringify(report, null, 2));
}
