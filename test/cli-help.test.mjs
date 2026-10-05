import test from 'node:test';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, mkdir, writeFile, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname, delimiter } from 'node:path';
import { fileURLToPath } from 'node:url';

const execute = promisify(execFile);
const cli = fileURLToPath(new URL('../bin/autorouter.mjs', import.meta.url));

test('command help works with no credentials and an invalid config path', async () => {
  for (const command of ['setup', 'doctor', 'config', 'serve']) {
    const { stdout, stderr } = await execute(process.execPath, [cli, command, '--help'], {
      env: { AUTOROUTER_CONFIG: '/missing/autorouter-config.json' }, timeout: 5000,
    });
    assert.match(stdout, new RegExp(`Usage: claude-autorouter ${command}`));
    assert.equal(stderr, '');
    assert.ok(stdout.length < 2000);
  }
});

test('Claude help and version bypass configuration, gateway, Ollama and temporary files', async t => {
  const root = await mkdtemp(join(tmpdir(), 'autorouter-help-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const scratch = join(root, 'scratch');
  await mkdir(scratch);
  await writeFile(join(root, 'claude'), `#!/usr/bin/env node
const assert = require('node:assert/strict');
assert.equal(process.env.ANTHROPIC_BASE_URL, undefined);
assert.equal(process.env.AUTOROUTER_STATUS_FILE, undefined);
assert.equal(process.env.TYPESAFE_API_KEY, undefined);
assert.equal(process.env.AUTOROUTER_TOKEN, undefined);
assert.equal(process.argv.length, 3);
console.log('CLAUDE_DIRECT:' + process.argv[2]);
`, { mode: 0o700 });
  for (const argument of ['--help', '-h', '--version', '-v']) {
    const result = await execute(process.execPath, [cli, 'claude', argument], {
      env: { PATH: root + delimiter + dirname(process.execPath), TMPDIR: scratch,
        AUTOROUTER_CONFIG: join(root, 'missing-config.json'), AUTOROUTER_EVALUATOR: 'ollama',
        AUTOROUTER_OLLAMA_URL: 'not a valid endpoint', TYPESAFE_API_KEY: 'private-placeholder', AUTOROUTER_TOKEN: 'private-placeholder' },
      timeout: 5000,
    });
    assert.equal(result.stdout.trim(), `CLAUDE_DIRECT:${argument}`);
    assert.equal(result.stderr, '');
  }
  assert.deepEqual(await readdir(scratch), []);
});
