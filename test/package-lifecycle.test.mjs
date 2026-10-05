import test from 'node:test';
import assert from 'node:assert/strict';
import { runLifecycleChild } from '../scripts/package-lifecycle.mjs';

test('package lifecycle runner observes readiness and forwards requested signals once', async () => {
  for (const name of ['SIGINT', 'SIGTERM']) {
    const result = await runLifecycleChild(process.execPath, ['-e', `
      process.once(${JSON.stringify(name)}, () => process.exit(23));
      console.log(JSON.stringify({marker:'lifecycle_ready', gateway:'synthetic'}));
      setInterval(() => {}, 1000);
    `], { env: {}, signalOnReady: name });
    assert.equal(result.code, 23);
    assert.equal(result.ready.gateway, 'synthetic');
    assert.equal(result.stderr, '');
  }
});

test('package lifecycle runner retains nonzero exits without concealing test diagnostics', async () => {
  const result = await runLifecycleChild(process.execPath, ['-e', "console.error('synthetic failure'); process.exitCode=17;"], { env: {} });
  assert.equal(result.code, 17);
  assert.equal(result.stderr.trim(), 'synthetic failure');
  assert.equal(result.ready, undefined);
});

test('package lifecycle runner bounds a stalled child and rejects missing executables', async () => {
  await assert.rejects(runLifecycleChild(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { env: {}, timeoutMs: 100 }), /deadline/);
  await assert.rejects(runLifecycleChild('/nonexistent/synthetic-autorouter-executable', [], { env: {} }), { code: 'ENOENT' });
});
