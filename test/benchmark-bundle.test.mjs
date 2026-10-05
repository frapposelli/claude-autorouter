import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createBenchmarkBundle } from '../scripts/benchmark-bundle.mjs';
import { run } from '../scripts/release-pack.mjs';

test('transfer bundle contains verified synthetic benchmark sources and works without installing dependencies', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'autorouter-benchmark-test-'));
  try {
    const archive = join(directory, 'benchmark.tar.gz');
    const created = await createBenchmarkBundle(archive);
    const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
    assert.equal(created.sha256, sha256(await readFile(archive)));
    assert.equal(await readFile(`${archive}.sha256`, 'utf8'), `${created.sha256}  benchmark.tar.gz\n`);
    const listing = (await run('tar', ['-tzf', archive])).stdout.split('\n').filter(Boolean);
    assert.ok(listing.every(path => path.startsWith('autorouter-benchmark/') && !path.includes('..')));
    assert.ok(listing.every(path => !/(?:^|\/)(?:\.env|\.git|node_modules|artifacts|config\.json|autorouter-session)/.test(path)));
    await run('tar', ['-xzf', archive, '-C', directory]);
    const root = join(directory, 'autorouter-benchmark');
    const manifest = JSON.parse(await readFile(join(root, 'source-manifest.json'), 'utf8'));
    assert.equal(manifest.schema_version, 1);
    assert.equal(created.files, Object.keys(manifest.files).length + 1);
    for (const [path, hash] of Object.entries(manifest.files)) assert.equal(sha256(await readFile(join(root, path))), hash, path);
    assert.equal(manifest.fixture_sha256, manifest.files['test/fixtures/ollama-routing.json']);
    const { stdout } = await run(process.execPath, ['scripts/evaluate-ollama.mjs', '--help'], { cwd: root });
    assert.match(stdout, /Does not download models or contact Claude\/Jev/);
    assert.match(stdout, /--timeout-ms 0 disables the warm timer/);
  } finally { await rm(directory, { recursive: true, force: true }); }
});
