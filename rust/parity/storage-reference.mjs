// Frozen-module storage oracle. Every scenario runs in a fresh owned child.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { performance } from 'node:perf_hooks';
import { setTimeout as sleep } from 'node:timers/promises';
import { once } from 'node:events';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const [reference, id, root, mode, profile] = process.argv.slice(2);
const protocolBytes = readFileSync(new URL('./storage-benchmark-v1.json', import.meta.url));
const protocol = JSON.parse(protocolBytes);
assert.equal(process.argv.length, 7);
assert.ok(protocol.validation_scenarios.includes(id) || ['measure_normal', 'measure_slow'].includes(id));
assert.ok(['validate', 'measure'].includes(mode));
assert.equal(mode === 'measure', id.startsWith('measure_'));
assert.ok(['paired', 'legacy-status-workload'].includes(profile));
await verifyBaseline(resolve(reference));
const { createStatusState } = await import(pathToFileURL(join(reference, 'src/status-state.mjs')).href);
const { createSessionLog } = await import(pathToFileURL(join(reference, 'src/session-log.mjs')).href);
const original = Object.fromEntries(['open', 'mkdir', 'lstat'].map(key => [key, fs[key]]));
const abort = new AbortController();
const cancel = () => abort.abort();
process.stdin.on('end', cancel); process.stdin.on('error', cancel); process.stdin.resume();
process.on('SIGINT', cancel); process.on('SIGTERM', cancel);
const watchdog = setTimeout(cancel, (mode === 'measure' ? 60 : 15) * 1000);
const now = Date.now;
if (mode === 'validate') Date.now = () => 1760000000000;
const stores = [], writers = [], controls = [];
const fixedTime = '2026-10-01T12:00:00.000Z';
function decision(index, session) {
  return { schema_version: 2, event: 'decision', timestamp: fixedTime, request_id: `request-${index}`, request_class: 'main',
    prompt_excerpt: 'Fix a typo', prompt_truncated: false, requested_model: 'claude-haiku-4-5-20251001', selected_model: 'claude-sonnet-5-5',
    decision_latency_ms: 12.5, source: 'jev', reason: 'low_confidence', evaluator: 'jev', classified_tier: 'haiku',
    body: 'PRIVATE_BODY', headers: { authorization: 'PRIVATE_AUTH' }, error: 'PRIVATE_ERROR', ...(session === undefined ? {} : { session_id: session }) };
}
function outcome(index, session) {
  return { schema_version: 2, event: 'outcome', timestamp: fixedTime, request_id: `request-${index}`, status: 'completed', http_status: 200,
    requested_model: 'claude-haiku-4-5-20251001', selected_model: 'claude-sonnet-5-5', confirmed_model: 'claude-sonnet-5-5',
    completion_confirmed: true, usage_complete: true, usage: { input_tokens: 100, output_tokens: 20, private: 'PRIVATE_USAGE' },
    baseline_model: 'claude-opus-5-5', pricing_version: '2026-09-29.1', total_latency_ms: 650, body: 'PRIVATE_BODY', prompt_excerpt: 'PRIVATE_PROMPT',
    ...(session === undefined ? {} : { session_id: session }) };
}
function burst(store, index) {
  for (const [event, fields] of [['request_start', {}], ['route', { model: 'claude-sonnet-5-5', source: 'jev', evaluation_latency_ms: 200 }],
    ['upstream_response', { status: 200 }], ['upstream_model', { model: 'claude-sonnet-5-5' }],
    ['upstream_usage', { usage: { input_tokens: 1000, output_tokens: 100 } }], ['request_complete', {}]]) {
    store.update({ event, request_id: `r-${index}`, session_id: `s-${index % 20}`, ...fields });
  }
}
const deferred = () => { let resolve; const promise = new Promise(done => { resolve = done; }); return { promise, resolve }; };
async function waitFor(predicate) {
  const started = performance.now();
  while (!predicate()) {
    if (abort.signal.aborted || performance.now() - started > 8000) throw Error('Storage barrier deadline');
    await sleep(1);
  }
}
function control(delay = 0) {
  const state = { held: false, gates: [], entered: 0, active: 0, maximum: 0, writes: 0, closes: 0, removals: 0, failNext: false, failInit: false, last: null,
    release() { this.held = false; for (const gate of this.gates.splice(0)) gate.resolve(); },
    async before() {
      this.active++; this.maximum = Math.max(this.maximum, this.active); this.entered++;
      try {
        if (this.held) { const gate = deferred(); this.gates.push(gate); const onAbort = () => gate.resolve(); abort.signal.addEventListener('abort', onAbort, { once: true });
          try { if (!abort.signal.aborted) await gate.promise; } finally { abort.signal.removeEventListener('abort', onAbort); } }
        if (abort.signal.aborted) throw Error('Storage owner cancelled');
        if (delay) await sleep(delay, undefined, { signal: abort.signal });
        if (this.failNext) { this.failNext = false; throw Error('Synthetic storage failure'); }
      } catch (error) { this.active--; throw error; }
    },
  };
  controls.push(state); return state;
}
function statusBackend(state) {
  const temporary = new Map();
  return { ...fs,
    async mkdtemp(...args) { if (state.failInit) throw Error('Synthetic create failure'); return fs.mkdtemp(...args); },
    async open(path, ...args) {
      const handle = await original.open(path, ...args); const write = handle.writeFile.bind(handle);
      handle.writeFile = async bytes => { await state.before(); try { await write(bytes); temporary.set(path, String(bytes)); state.writes++; } finally { state.active--; } };
      return handle;
    },
    async rename(from, to) { await fs.rename(from, to); state.last = temporary.get(from); temporary.delete(from); },
    async rm(path, options) { if (options?.recursive) { assert.equal(state.active, 0); state.removals++; } temporary.delete(path); return fs.rm(path, options); },
  };
}
function normalize(snapshot) { assert.equal(snapshot.pid, process.pid); assert.ok(Number.isSafeInteger(snapshot.heartbeat_at) && snapshot.heartbeat_at >= 0); assert.ok(snapshot.sessions && typeof snapshot.sessions === 'object' && !Array.isArray(snapshot.sessions));
  if (mode === 'validate') assert.equal(snapshot.heartbeat_at, 1760000000000);
  for (const session of Object.values(snapshot.sessions)) { assert.ok(session && typeof session === 'object' && !Array.isArray(session)); assert.ok(Number.isSafeInteger(session.updated_at) && session.updated_at >= 0); if (mode === 'validate') assert.equal(session.updated_at, 1760000000000); }
  snapshot.pid = 0; snapshot.heartbeat_at = 0; for (const session of Object.values(snapshot.sessions)) session.updated_at = 0; return snapshot; }
async function snapshot(store) { const bytes = await fs.readFile(store.path); assert.ok(bytes.length <= 1048576); return normalize(JSON.parse(bytes)); }
async function privateModes(path) { assert.equal((await fs.stat(path)).mode & 0o777, 0o600); assert.equal((await fs.stat(resolve(path, '..'))).mode & 0o777, 0o700); }
async function statusCase() {
  const state = control(); state.failInit = id === 'status_create_failure'; state.failNext = id === 'status_initial_failure'; state.held = id === 'status_readiness_timeout';
  const store = createStatusState({ directory: root, fileSystem: statusBackend(state) }); stores.push(store); await store.ready;
  if (['status_create_failure', 'status_initial_failure', 'status_readiness_timeout'].includes(id)) {
    assert.equal(store.path, null); burst(store, 0); let closed = false; const closing = store.close().then(() => { closed = true; }); await new Promise(setImmediate);
    if (id === 'status_readiness_timeout') assert.equal(closed, false);
    state.release(); await closing; assert.deepEqual(await fs.readdir(root), []);
    return { ready: false, late_update_ignored: true, owned_initial_write: id === 'status_readiness_timeout' };
  }
  await privateModes(store.path); const checks = { ready: true, private: true };
  if (id === 'status_normal') { for (let i = 0; i < 60; i++) { burst(store, i); await store.flush(); } checks.snapshot = await snapshot(store); }
  else if (['status_held', 'status_concurrent_close'].includes(id)) {
    state.held = true; const before = state.entered; burst(store, 0); let returned = false; const first = store.flush().then(() => { returned = true; }); await waitFor(() => state.entered >= before + 1);
    for (let i = 1; i < 2000; i++) burst(store, i);
    let secondReturned = false; const second = store.flush().then(() => { secondReturned = true; });
    for (let i = 0; i < 4; i++) { await sleep(1); assert.equal(state.active, 1); }
    assert.equal(returned, false); assert.equal(secondReturned, false);
    if (id === 'status_concurrent_close') {
      let firstClosed = false, secondClosed = false; const closing = store.close().then(() => { firstClosed = true; }); const closing2 = store.close().then(() => { secondClosed = true; });
      await new Promise(setImmediate); assert.equal(firstClosed, false); assert.equal(secondClosed, false); burst(store, 9999); state.release(); await Promise.all([closing, closing2, first, second]);
      checks.late_update_ignored = true; checks.close_waited = true; checks.snapshot = normalize(JSON.parse(state.last)); assert.ok(!JSON.stringify(checks.snapshot).includes('r-9999'));
    } else { state.release(); await Promise.all([first, second]); checks.snapshot = await snapshot(store); }
    assert.equal(state.maximum, 1); Object.assign(checks, { bursts: 2000, progress_while_held: 4, single_writer: true });
  } else {
    burst(store, 0); await store.flush(); const before = await snapshot(store); const path = store.path;
    if (id === 'status_write_recovery') state.failNext = true;
    else if (id === 'status_rename_recovery') { await fs.unlink(path); await fs.mkdir(path); }
    else throw Error('Unknown status scenario');
    burst(store, 1); await store.flush();
    if (id === 'status_write_recovery') assert.deepEqual(await snapshot(store), before);
    else { assert.equal((await fs.readdir(resolve(path, '..'))).length, 1); await fs.rmdir(path); }
    burst(store, 2); await store.flush(); checks.snapshot = await snapshot(store); checks.recovered = true;
  }
  await store.close(); assert.equal(state.active, 0); assert.equal(state.removals, 1); assert.deepEqual(await fs.readdir(root), []); checks.removed = true; return checks;
}
async function logFiles() {
  const files = {}; let total = 0, count = 0; let names;
  try { names = await fs.readdir(join(root, 'logs')); } catch (error) { if (error.code === 'ENOENT') return files; throw error; }
  for (const name of names.sort()) {
    const path = join(root, 'logs', name); await privateModes(path); const bytes = await fs.readFile(path); total += bytes.length; assert.ok(total <= 2097152);
    const rows = bytes.toString().split('\n').filter(Boolean).map(line => { count++; assert.ok(count <= 3000); const row = JSON.parse(line); assert.equal(row.timestamp, fixedTime); return { ...row, timestamp: '<timestamp>' }; });
    const key = rows[0]?.session_id ?? '<anonymous>'; assert.ok(!Object.hasOwn(files, key)); files[key] = rows;
  }
  return files;
}
async function logCase() {
  const state = control(); state.held = ['log_queue_bound', 'log_session_bound', 'log_close'].includes(id); state.failInit = id === 'log_init_warning_failure'; state.failNext = id === 'log_append_failure';
  let writer, warnings = 0, continuous = false; const warningMessages = [];
  fs.lstat = async (...args) => { if (state.failInit) throw Error('Synthetic initialize failure'); return original.lstat(...args); };
  fs.open = async (...args) => {
    // Append-level injection occurs before backend file creation in Rust.
    // Fail the equivalent first open in Node, retaining the same zero-file result.
    if (id === 'log_append_failure' && state.failNext) { state.entered++; state.failNext = false; throw Error('Synthetic append boundary failure'); }
    const handle = await original.open(...args); const write = handle.writeFile.bind(handle), close = handle.close.bind(handle);
    handle.writeFile = async line => { await state.before(); try { if (continuous && state.writes < 799) assert.equal(writer.record(decision(state.writes + 1, 'session-a')), true); await write(line); state.writes++; } finally { state.active--; } };
    handle.close = async () => { state.closes++; return close(); }; return handle;
  };
  writer = await createSessionLog(join(root, 'logs'), { includePrompts: id !== 'log_metadata', warn(message) { warnings++; warningMessages.push(message); if (id === 'log_init_warning_failure') throw Error('Synthetic warning callback failure'); } }); writers.push(writer);
  const checks = {}; let accepted = 0;
  if (['log_normal', 'log_metadata'].includes(id)) {
    for (let i = 0; i < 4; i++) { const session = i ? `session-${i % 2}` : undefined; assert.equal(writer.record(decision(i, session)), true); assert.equal(writer.record(outcome(i, session)), true); accepted += 2; }
  } else if (id === 'log_queue_bound') {
    for (let i = 0; i < 3000; i++) { if (writer.record({ ...decision(i, 'session-a'), prompt_excerpt: '\0'.repeat(499) + '😀' })) accepted++; else break; }
    assert.ok(accepted >= 100 && accepted <= 1000); assert.equal(writer.record(decision(9999)), false); state.release();
  } else if (id === 'log_session_bound') {
    for (let i = 0; i < 129; i++) { const ok = writer.record(decision(i, `session-${i}`)); assert.equal(ok, i < 128); accepted += Number(ok); } state.release();
  } else if (id === 'log_continuous') { continuous = true; assert.equal(writer.record(decision(0, 'session-a')), true); await waitFor(() => state.writes === 800); accepted = 800; }
  else if (id === 'log_append_failure') { assert.equal(writer.record(decision(0, 'session-a')), true); await waitFor(() => warnings === 1); assert.equal(writer.record(decision(1)), false); accepted = 1; }
  else if (id === 'log_close') {
    for (let i = 0; i < 3; i++) assert.equal(writer.record(decision(i, 'session-a')), true); accepted = 3; await waitFor(() => state.entered >= 1);
    let closed = false; const first = writer.close(); assert.equal(writer.close(), first); const observed = first.then(() => { closed = true; });
    assert.equal(writer.record(decision(9999)), false); await new Promise(setImmediate); assert.equal(closed, false); state.release(); await observed; checks.close_waited = true;
  } else if (id === 'log_init_warning_failure') assert.equal(writer.record(decision(0)), false);
  else throw Error('Unknown log scenario');
  await writer.close(); await writer.close(); assert.equal(state.active, 0); assert.equal(writer.record(decision(9999)), false);
  const files = await logFiles(); const rows = Object.values(files).flat(); assert.equal(rows.length, id === 'log_append_failure' ? 0 : accepted);
  if (id === 'log_session_bound') assert.equal(Object.keys(files).length, 128);
  if (['log_continuous', 'log_close', 'log_queue_bound'].includes(id)) assert.deepEqual(files['session-a'].map(row => row.request_id), Array.from({ length: accepted }, (_, i) => `request-${i}`));
  const encoded = JSON.stringify(files); for (const marker of ['PRIVATE_BODY', 'PRIVATE_AUTH', 'PRIVATE_ERROR', 'PRIVATE_USAGE']) assert.ok(!encoded.includes(marker));
  if (id === 'log_metadata') for (const marker of ['prompt_excerpt', 'prompt_truncated', 'PRIVATE_PROMPT']) assert.ok(!encoded.includes(marker));
  assert.equal(state.closes, Object.keys(files).length);
  // Rust SessionSink::close is one aggregate backend call; Node closes each
  // opened file within one shared close Promise. Both adapters prove this path.
  return { ...checks, accepted, files, warnings, warning_messages: warningMessages, close_calls: 1, late_rejected: true };
}
async function measurement() {
  const delay = id === 'measure_slow' ? 20 : 0; const state = control(delay); const started = performance.now();
  const store = createStatusState({ directory: root, fileSystem: statusBackend(state) }); stores.push(store); await store.ready; assert.ok(store.path);
  const ready = performance.now() - started; const warmup = profile === 'paired' ? 200 : 0, samples = profile === 'paired' ? 2000 : 60;
  for (let i = 0; i < warmup; i++) { burst(store, i); await store.flush(); }
  const updates = [], admissions = [], completion = [], progress = [], pending = []; let previous = performance.now();
  const timer = setInterval(() => { const now = performance.now(); if (progress.length >= 10000) { abort.abort(); return; } progress.push(Math.max(0, now - previous - 5)); previous = now; }, 5);
  try {
    for (let i = 0; i < samples; i++) {
      await sleep(5, undefined, { signal: abort.signal }); const update = performance.now(); burst(store, i); updates.push(performance.now() - update);
      const start = performance.now(); const flushed = store.flush(); admissions.push(performance.now() - start); pending.push(flushed.then(() => completion.push(performance.now() - start)));
    }
    await Promise.all(pending); await sleep(10);
  } finally { clearInterval(timer); }
  const saved = await snapshot(store); const start = performance.now(); await store.close(); const close = performance.now() - start;
  assert.equal(updates.length, samples); return { profile, warmup_bursts: warmup, measured_bursts: samples, injected_write_delay_ms: delay,
    raw: { update_ms: updates, flush_admission_ms: admissions, flush_completion_ms: completion, timer_lateness_ms: progress, ready_ms: [ready], close_ms: [close] },
    snapshot: saved, snapshot_writes: state.writes, acceptance_qualified: false, allocations: null, open_file_descriptors: null, cpu: null, peak_rss: null };
}
let semantics = null, error = null, joined = false, empty = false;
try { assert.deepEqual(await fs.readdir(root), []); semantics = mode === 'measure' ? await measurement() : id.startsWith('status_') ? await statusCase() : await logCase(); }
catch { error = 'Storage scenario failed'; }
finally {
  for (const state of controls) state.release();
  try { await Promise.all(stores.map(store => store.close())); await Promise.all(writers.map(writer => writer.close())); joined = controls.every(state => state.active === 0); }
  catch { error = 'Storage cleanup failed'; }
  for (const [key, value] of Object.entries(original)) fs[key] = value;
  await fs.rm(join(root, 'logs'), { recursive: true, force: true }); empty = (await fs.readdir(root)).length === 0;
  clearTimeout(watchdog); Date.now = now; process.stdin.destroy(); process.off('SIGINT', cancel); process.off('SIGTERM', cancel);
}
const report = { schema_version: 1, kind: 'storage_child', implementation: 'node', scenario: id, mode,
  protocol_sha256: createHash('sha256').update(protocolBytes).digest('hex'), passed: error === null && joined && empty && !abort.signal.aborted,
  semantics, error, cleanup: { active_io: controls.reduce((n, state) => n + state.active, 0), joined, scratch_empty: empty }, numerical_values_retained: mode === 'measure' };
function* encode(value) {
  if (Array.isArray(value)) { yield '['; for (let i = 0; i < value.length; i++) { if (i) yield ','; yield* encode(value[i]); } yield ']'; }
  else if (value !== null && typeof value === 'object') { yield '{'; let first = true; for (const [key, item] of Object.entries(value)) { if (!first) yield ','; first = false; yield JSON.stringify(key) + ':'; yield* encode(item); } yield '}'; }
  else yield JSON.stringify(value);
}
let emitted = 0;
for (const chunk of encode(report)) { emitted += Buffer.byteLength(chunk); if (emitted > 8388608) throw Error('Storage report bound'); if (!process.stdout.write(chunk)) await once(process.stdout, 'drain'); }
process.stdout.write('\n'); if (!report.passed) process.exitCode = 1;
