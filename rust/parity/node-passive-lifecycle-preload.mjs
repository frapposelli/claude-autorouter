// Explicit synthetic opt-in only; loaded before the unchanged frozen CLI.
import assert from 'node:assert/strict';
import { writeSync } from 'node:fs';
import https from 'node:https';
import { createPassiveNodeLifecycle } from './node-passive-lifecycle.mjs';

assert.equal(process.version, 'v22.14.0');
assert.equal(process.env.AUTOROUTER_SYNTHETIC_NODE_LIFECYCLE, 'passive-session-v1');
assert.equal(process.argv[2], 'serve');
const upstream = new URL(process.env.AUTOROUTER_UPSTREAM_URL);
assert.equal(upstream.protocol, 'https:');
assert.equal(upstream.hostname, '127.0.0.1');
assert.equal(upstream.username, ''); assert.equal(upstream.password, '');
const descriptor = Object.getOwnPropertyDescriptor(https, 'request');
assert.ok(descriptor && typeof descriptor.value === 'function' && descriptor.configurable);
const original = https.request;
const lifecycle = createPassiveNodeLifecycle({ agent: https.globalAgent,
  requestNumbering: 'zero_based_session_path_ordinal' });
const intercepted = function(url, options) {
  const target = new URL(url);
  assert.equal(target.origin, upstream.origin);
  const match = /^\/v1\/models\/session-([0-2])$/.exec(target.pathname);
  assert.ok(match, 'passive session fixture endpoint');
  assert.equal(target.search, ''); assert.equal(options.method, 'GET');
  return lifecycle.invoke(original, this, arguments, Number(match[1]));
};
https.request = intercepted;
// Do not install a signal handler, timer, async write, or forced shutdown. The
// frozen CLI retains its signal handling and original shutdown behavior. Exit
// is only an observation boundary; it does not prove every physical socket shut.
process.once('exit', () => {
  let transportRestored = false;
  try {
    if (https.request === intercepted) { Object.defineProperty(https, 'request', descriptor); transportRestored = true; }
  } catch { /* The helper still restores its instance wrappers below. */ }
  const diagnostics = lifecycle.finish();
  try {
    if (!transportRestored) { diagnostics.complete = false; diagnostics.failure = 'transport_restoration_invariant'; }
    const line = Buffer.from(`Node passive lifecycle: ${JSON.stringify(diagnostics)}\n`);
    assert.ok(line.length <= 32768, 'passive lifecycle serialization bound');
    let offset = 0, writes = 0;
    while (offset < line.length) {
      assert.ok(++writes <= 64, 'passive lifecycle write bound');
      const written = writeSync(2, line, offset, line.length - offset);
      assert.ok(written > 0); offset += written;
    }
    if (!diagnostics.complete) process.exitCode ||= 1;
  } catch { process.exitCode ||= 1; }
});
