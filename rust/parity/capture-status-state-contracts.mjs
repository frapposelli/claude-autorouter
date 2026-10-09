// Run every original frozen assertion. Capture complete state-machine inputs
// and saved snapshots; storage scheduling remains a separate native contract.
import nodeTest from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import * as fileSystem from 'node:fs/promises';
import { Readable, Writable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/status-state-contracts-${Date.now()}`));
assert.ok(process.argv.length <= 4, 'Usage: capture-status-state-contracts.mjs [frozen-reference] [new-output-directory]');
await verifyBaseline(reference);
const sourcePath = 'test/status-state.test.mjs';
const source = readFileSync(join(reference, sourcePath), 'utf8');
const manifest = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json'), 'utf8'));
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
assert.equal(hash(source), manifest.files.find(row => row.path === sourcePath)?.sha256);
const { createStatusState } = await import(pathToFileURL(join(reference, 'src/status-state.mjs')).href);
const { renderStatusLine } = await import(pathToFileURL(join(reference, 'src/statusline.mjs')).href);
const tests = [], cases = [], boundaries = [];
const pure = new Set([1, ...Array.from({ length: 13 }, (_, index) => index + 3), 19]);
const states = new WeakMap();
let current;

function encode(value, path = '$', ancestors = new Set()) {
  if (typeof value === 'number') {
    if (Number.isNaN(value)) throw new Error(`${path}: NaN`);
    return value === Infinity ? '1e400' : value === -Infinity ? '-1e400' : JSON.stringify(value);
  }
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return JSON.stringify(value);
  if (typeof value !== 'object') throw new Error(`${path}: ${typeof value}`);
  if (ancestors.has(value)) throw new Error(`${path}: circular object`);
  if (!Array.isArray(value) && Object.getPrototypeOf(value) !== Object.prototype) throw new Error(`${path}: non-plain object`);
  ancestors.add(value);
  const entries = [];
  for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(value))) {
    if (!descriptor.enumerable) continue;
    if (!Object.hasOwn(descriptor, 'value')) throw new Error(`${path}.${key}: accessor`);
    entries.push([key, encode(descriptor.value, `${path}.${key}`, ancestors)]);
  }
  ancestors.delete(value);
  if (Array.isArray(value)) {
    assert.equal(entries.length, value.length, 'Sparse/nonstandard arrays require a migration boundary');
    return `[${entries.map(([, item]) => item).join(',')}]`;
  }
  return `{${entries.map(([key, item]) => `${JSON.stringify(key)}:${item}`).join(',')}}`;
}
function boundary(reason, fields = {}) {
  const entry = { baseline_test: current.id, reason, ...fields };
  current.migration_boundaries.push(entry);
  boundaries.push(entry);
}
function create(options) {
  const state = createStatusState(options);
  if (!pure.has(current.number)) return state;
  const trace = { events: [], blocked: false, reads: 0 };
  if (options && Object.hasOwn(options, 'baselineModel')) trace.baseline_model = options.baselineModel;
  const wrapped = { get path() { return state.path; }, ready: state.ready, flush: state.flush, close: state.close,
    update(event) {
      const now = Date.now();
      let serialized;
      try { serialized = encode(event); }
      catch (error) {
        trace.blocked = true;
        boundary(error.message, { event_call: trace.events.length + 1 });
      }
      trace.events.push({ now, ...(serialized === undefined ? { skipped: true } : { json: serialized }) });
      const originalNow = Date.now;
      Date.now = () => now;
      try { state.update(event); }
      finally { Date.now = originalNow; }
      if (serialized !== undefined) assert.equal(encode(event), serialized, 'Frozen state update mutated the caller input');
    } };
  states.set(wrapped, trace);
  return wrapped;
}
function captureRead(state, saved) {
  const trace = states.get(state);
  if (!trace) return;
  trace.reads += 1;
  if (trace.blocked) {
    current.skipped_snapshots += 1;
    return;
  }
  const id = `baseline-status-state-${current.number}-${trace.reads}`;
  const input = { events: structuredClone(trace.events), pid: saved.pid, heartbeat_at: saved.heartbeat_at };
  if (Object.hasOwn(trace, 'baseline_model')) input.baseline_model = trace.baseline_model;
  cases.push({ id, input, node_expected: structuredClone(saved), source_tests: [current.id] });
  current.case_ids.push(id);
}
function render(...args) {
  const now = args[2]?.now ?? Date.now();
  const originalNow = Date.now;
  let result;
  Date.now = () => now;
  try { result = renderStatusLine(...args); }
  finally { Date.now = originalNow; }
  const serialized = args.map(value => encode(value));
  const id = `baseline-status-render-${current.number}-${current.render_case_ids.length + 1}`;
  cases.push({ id, input: { input: JSON.parse(serialized[0]), snapshot: JSON.parse(serialized[1]), options: { ...JSON.parse(serialized[2]), now, alive: true } }, op: 'render_statusline', node_expected: result, source_tests: [current.id] });
  current.render_case_ids.push(id);
  return result;
}
let body = source;
for (const line of [
  "import test from 'node:test';", "import assert from 'node:assert/strict';",
  "import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';",
  "import { dirname, join } from 'node:path';", "import { tmpdir } from 'node:os';",
  "import * as fileSystem from 'node:fs/promises';", "import { Readable, Writable } from 'node:stream';",
  "import { pipeline } from 'node:stream/promises';", "import { createStatusState } from '../src/status-state.mjs';",
  "import { renderStatusLine } from '../src/statusline.mjs';",
]) {
  assert.equal(body.split(line).length, 2, 'Frozen import topology changed');
  body = body.replace(line, '');
}
assert.doesNotMatch(body, /^\s*import\b/m);
const readHelper = "const read = state => JSON.parse(readFileSync(state.path, 'utf8'));";
assert.equal(body.split(readHelper).length, 2, 'Frozen helper topology changed');
body = body.replace(readHelper, "const read = state => { const saved = JSON.parse(readFileSync(state.path, 'utf8')); captureRead(state, saved); return saved; };");
function test(name, callback) {
  tests.push({ id: `${sourcePath}#${tests.length + 1}`, number: tests.length + 1, name, callback, case_ids: [], render_case_ids: [], migration_boundaries: [], skipped_snapshots: 0 });
}
const values = { test, assert, existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync, dirname, join, tmpdir, fileSystem, Readable, Writable, pipeline, createStatusState: create, renderStatusLine: render, captureRead };
Function(...Object.keys(values), `"use strict";\n${body}`)(...Object.values(values));
assert.equal(tests.length, 23, 'Frozen definition inventory changed');
let completed = 0;
let cleanupFailed = false;
for (const definition of tests) {
  current = definition;
  await nodeTest(definition.name, async t => {
    const context = { mock: t.mock, after(callback) {
      t.after(async (...args) => {
        try { await callback(...args); }
        catch (error) { cleanupFailed = true; throw error; }
      });
    } };
    await definition.callback(context);
    completed += 1;
  });
}
assert.equal(completed, tests.length, 'Every original callback must pass before publishing capture');
assert.equal(cleanupFailed, false, 'Every original cleanup hook must pass before publishing capture');
const bytes = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
const report = { schema_version: 1, kind: 'frozen_status_state_contract_capture', passed: true,
  baseline_commit: manifest.baseline_commit, source_path: sourcePath, source_sha256: hash(source),
  generator_sha256: hash(readFileSync(new URL(import.meta.url))), node_version: process.version,
  cases: cases.length, cases_sha256: hash(bytes), tests: tests.map(({ callback, ...entry }) => entry), migration_boundaries: boundaries,
  limits: 'All23 original definitions, parameter loops and assertions execute with actual private Node status files. Fifteen state-machine definitions capture complete event prefixes and saved snapshots; update clocks are captured exactly and saved PID/heartbeat are explicit replay inputs. Infinity is retained as overflowing JSON for the metadata normalizer. Unsupported JavaScript inputs make subsequent snapshots ineligible rather than silently omitting events. Storage-specific definitions require separate native scheduling/filesystem tests. Render calls preserve complete strings and captured live PID observation. Capture alone is not native parity evidence.' };
mkdirSync(output, { mode: 0o700 });
writeFileSync(join(output, 'cases.jsonl'), bytes, { flag: 'wx', mode: 0o600 });
writeFileSync(join(output, 'capture.json'), JSON.stringify(report, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify({ passed: true, definitions: tests.length, cases: cases.length, migration_boundaries: boundaries.length, output }));
