// Execute frozen pure assertions and capture their complete native-visible
// inputs/results. Getters, cycles and unsupported JavaScript values stay explicit.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/telemetry-statusline-contracts-${Date.now()}`));
assert.ok(process.argv.length <= 4, 'Usage: capture-telemetry-statusline-contracts.mjs [frozen-reference] [new-output-directory]');
await verifyBaseline(reference);
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const manifest = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json'), 'utf8'));
const telemetry = await import(pathToFileURL(join(reference, 'src/telemetry-event.mjs')).href);
const statusline = await import(pathToFileURL(join(reference, 'src/statusline.mjs')).href);
const tests = [], cases = [], boundaries = [], sources = [];
let current;

function encode(value, path = '$', ancestors = new Set(), infinity = false) {
  if (typeof value === 'number') {
    if (Number.isNaN(value) || (!infinity && !Number.isFinite(value))) throw new Error(`${path}: non-finite number`);
    if (value === Infinity) return '1e400';
    if (value === -Infinity) return '-1e400';
    return JSON.stringify(value);
  }
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return JSON.stringify(value);
  if (typeof value !== 'object') throw new Error(`${path}: ${typeof value}`);
  if (ancestors.has(value)) throw new Error(`${path}: circular object`);
  if (!Array.isArray(value) && Object.getPrototypeOf(value) !== Object.prototype) throw new Error(`${path}: non-plain object`);
  ancestors.add(value);
  const descriptors = Object.getOwnPropertyDescriptors(value);
  const entries = [];
  for (const [key, descriptor] of Object.entries(descriptors)) {
    if (!descriptor.enumerable) continue;
    if (!Object.hasOwn(descriptor, 'value')) throw new Error(`${path}.${key}: accessor`);
    entries.push([key, encode(descriptor.value, `${path}.${key}`, ancestors, infinity)]);
  }
  ancestors.delete(value);
  if (Array.isArray(value)) {
    assert.equal(entries.length, value.length, 'Sparse/nonstandard array needs an explicit migration boundary');
    return `[${entries.map(([, item]) => item).join(',')}]`;
  }
  return `{${entries.map(([key, item]) => `${JSON.stringify(key)}:${item}`).join(',')}}`;
}

function invoke(op, fn, args) {
  current.calls += 1;
  let serialized;
  let boundary;
  try { serialized = args.map((value, index) => encode(value, `$[${index}]`, new Set(), op !== 'render_statusline')); }
  catch (error) { boundary = error.message; }
  const result = fn(...args); // Every original call still executes unchanged.
  if (boundary) {
    const entry = { baseline_test: current.id, call: current.calls, op, reason: boundary };
    boundaries.push(entry);
    current.migration_boundaries.push(entry);
    return result;
  }
  const after = args.map((value, index) => encode(value, `$[${index}]`, new Set(), op !== 'render_statusline'));
  assert.deepEqual(after, serialized, 'Frozen pure input mutation needs an explicit native assertion');
  const input = op === 'render_statusline'
    ? { input: JSON.parse(serialized[0]), snapshot: JSON.parse(serialized[1]), options: JSON.parse(serialized[2]), liveness: 'The captured snapshot PID belongs to this live capture process. The pure native renderer receives that observation as alive=true.' }
    : { json: serialized[0], include_prompts: args[1]?.includePrompts ?? true, now: result?.timestamp ?? '2026-10-09T12:00:00.000Z' };
  if (op === 'render_statusline') input.options.alive = true;
  const id = `baseline-${op}-${current.id.split('#')[1]}-${current.calls}`;
  cases.push({ id, op, input, node_expected: result === undefined ? { kind: 'undefined' } : { kind: 'value', value: structuredClone(result) }, source_tests: [current.id] });
  current.case_ids.push(id);
  return result;
}

function captureFile(path, imports, parameters, values, count, truncate) {
  const source = readFileSync(join(reference, path), 'utf8');
  assert.equal(hash(source), manifest.files.find(row => row.path === path)?.sha256);
  sources.push({ path, sha256: hash(source) });
  let body = source;
  if (truncate) {
    assert.equal(body.split(truncate).length, 2, 'Frozen pure/CLI boundary changed');
    body = body.slice(0, body.indexOf(truncate));
  }
  for (const line of imports) {
    assert.equal(body.split(line).length, 2, 'Frozen import topology changed');
    body = body.replace(line, '');
  }
  assert.doesNotMatch(body, /^\s*import\b/m);
  const definitions = [];
  function test(name, callback) { definitions.push({ id: `${path}#${definitions.length + 1}`, name, callback, calls: 0, case_ids: [], migration_boundaries: [] }); }
  Function('test', 'assert', ...parameters, `"use strict";\n${body}`)(test, assert, ...values);
  assert.equal(definitions.length, count);
  for (const definition of definitions) {
    current = definition;
    const returned = definition.callback();
    assert.equal(returned, undefined, 'Only pure synchronous definitions belong in this capture');
    tests.push(definition);
  }
}
captureFile('test/telemetry-event.test.mjs', [
  "import test from 'node:test';", "import assert from 'node:assert/strict';",
  "import { normalizeTelemetryEvent, normalizeSessionRecord, normalizeUsageTelemetry, normalizePricingContext,\n  TELEMETRY_SCHEMA_VERSION } from '../src/telemetry-event.mjs';",
], ['normalizeTelemetryEvent', 'normalizeSessionRecord', 'normalizeUsageTelemetry', 'normalizePricingContext', 'TELEMETRY_SCHEMA_VERSION'], [
  (...args) => invoke('normalize_telemetry', telemetry.normalizeTelemetryEvent, args),
  (...args) => invoke('normalize_session', telemetry.normalizeSessionRecord, args),
  (...args) => invoke('normalize_usage', telemetry.normalizeUsageTelemetry, args),
  (...args) => invoke('normalize_pricing', telemetry.normalizePricingContext, args),
  telemetry.TELEMETRY_SCHEMA_VERSION,
], 10);
captureFile('test/statusline.test.mjs', [
  "import test from 'node:test';", "import assert from 'node:assert/strict';", "import { spawn } from 'node:child_process';",
  "import { mkdtemp, writeFile, rm } from 'node:fs/promises';", "import { tmpdir } from 'node:os';", "import { join } from 'node:path';",
  "import { fileURLToPath } from 'node:url';", "import { renderStatusLine } from '../src/statusline.mjs';",
], ['renderStatusLine'], [(...args) => invoke('render_statusline', statusline.renderStatusLine, args)], 30, 'const cliPath = ');
const bytes = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
const report = { schema_version: 1, kind: 'frozen_telemetry_statusline_contract_capture', passed: true,
  baseline_commit: manifest.baseline_commit, sources, generator_sha256: hash(readFileSync(new URL(import.meta.url))),
  node_version: process.version, cases: cases.length, cases_sha256: hash(bytes),
  tests: tests.map(({ callback, ...entry }) => entry), migration_boundaries: boundaries,
  limits: 'Original40 pure definitions and assertions execute. Native expected values preserve undefined-vs-value result kind. Overflow JSON preserves Infinity for document-based telemetry normalization. Accessors/cycles/nonplain objects/undefined arguments and unsupported nonfinite status inputs are explicit skipped calls. Default PID liveness is captured as true; injected liveness callbacks remain skipped. Missing timestamp uses captured injected now. Executable statusline31 is excluded. Capture alone is not native parity evidence.' };
mkdirSync(output, { mode: 0o700 });
writeFileSync(join(output, 'cases.jsonl'), bytes, { flag: 'wx', mode: 0o600 });
writeFileSync(join(output, 'capture.json'), JSON.stringify(report, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify({ passed: true, definitions: tests.length, cases: cases.length, migration_boundaries: boundaries.length, output }));
