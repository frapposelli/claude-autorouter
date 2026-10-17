// Deterministic frozen-Node control for the native shared-drain regression.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

assert.ok(process.argv.length <= 3, 'Usage: check-status-drain.mjs [frozen-reference]');
const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
await verifyBaseline(reference);
const { createStatusState } = await import(pathToFileURL(join(reference, 'src/status-state.mjs')).href);
const deferred = () => {
  let resolve;
  return { promise: new Promise(done => { resolve = done; }), resolve };
};
const entered = [null, ...Array.from({ length: 3 }, deferred)];
const gates = [null, ...Array.from({ length: 3 }, deferred)];
let writes = 0, saved;
const state = createStatusState({ fileSystem: {
  mkdtemp: async () => '/synthetic/status', chmod: async () => {}, rm: async () => {}, rename: async () => {},
  open: async () => ({ chmod: async () => {}, close: async () => {}, writeFile: async text => {
    const index = ++writes;
    if (index <= 3) {
      entered[index].resolve();
      if (index >= 2) await gates[index].promise;
    }
    saved = JSON.parse(text);
  } }),
} });
// This is an outer failure deadline, never a timing/performance assertion.
const watchdog = setTimeout(() => { throw new Error('Synthetic shared-drain barrier deadline'); }, 5000);
try {
  await state.ready;
  state.update({ event: 'request_start', request_id: 'r', session_id: 's' });
  let returned = false;
  const flushed = state.flush().then(() => { returned = true; });
  await entered[2].promise;
  state.update({ event: 'route', request_id: 'r', session_id: 's', model: 'claude-sonnet-5-5' });
  gates[2].resolve();
  await entered[3].promise;
  await new Promise(setImmediate);
  const premature = returned;
  gates[3].resolve();
  await flushed;
  assert.equal(premature, false);
  assert.equal(writes, 3);
  assert.equal(saved.sessions.s.selected_model, 'claude-sonnet-5-5');
  console.log(JSON.stringify({
    schema_version: 1, kind: 'frozen_status_shared_drain_control', passed: true,
    baseline_commit: 'ea930c247626ce2af5ccdad721b5121417bf4ad8', node_version: process.version,
    source_sha256: createHash('sha256').update(readFileSync(join(reference, 'src/status-state.mjs'))).digest('hex'),
    generator_sha256: createHash('sha256').update(readFileSync(new URL(import.meta.url))).digest('hex'),
    flush_returned_while_latest_accepted_write_blocked: premature, writes,
    selected_model: saved.sessions.s.selected_model,
    limits: 'Synthetic in-memory filesystem with explicit write2/write3 barriers; no timing, provider or hardware performance evidence.',
  }));
} finally {
  gates[2].resolve();
  gates[3].resolve();
  await state.close();
  clearTimeout(watchdog);
}
