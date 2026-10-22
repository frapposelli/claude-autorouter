// Capture every dynamic guard call made by the unchanged frozen baseline tests.
// This executes only their synchronous synthetic fixtures, never product startup.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createReference } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/auto-routing-contracts-${Date.now()}`));
assert.ok(process.argv.length <= 4, 'Usage: capture-auto-routing-contracts.mjs [frozen-reference] [new-output-directory]');
const evaluate = await createReference(reference); // verifies every frozen file
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const sourcePath = 'test/auto-routing.test.mjs';
const source = readFileSync(join(reference, sourcePath), 'utf8');
const manifest = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json'), 'utf8'));
assert.equal(hash(source), manifest.files.find(row => row.path === sourcePath)?.sha256);
const guards = await import(pathToFileURL(join(reference, 'src/auto-routing.mjs')).href);
const imports = [
  "import test from 'node:test';",
  "import assert from 'node:assert/strict';",
  "import { canRouteAutoRequest, hasRoutableSafeguards, targetCompatibility } from '../src/auto-routing.mjs';",
];
let body = source;
for (const line of imports) {
  assert.equal(body.split(line).length, 2, 'Frozen import topology changed');
  body = body.replace(line, '');
}
assert.doesNotMatch(body, /^\s*import\b/m, 'Unexpected executable import');
const cases = [], tests = [], migrationBoundaries = [];
let current;
function nonJson(value, path = '$') {
  if (typeof value === 'number' && !Number.isFinite(value)) return [`${path}: non-finite number`];
  if (value === undefined || typeof value === 'function' || typeof value === 'symbol' || typeof value === 'bigint') return [`${path}: ${typeof value}`];
  if (!value || typeof value !== 'object') return [];
  return Object.entries(value).flatMap(([key, item]) => nonJson(item, `${path}.${key}`));
}
function invoke(kind, request, target, options) {
  const apply = value => kind === 'safeguards' ? guards.hasRoutableSafeguards(value)
    : kind === 'auto' ? guards.canRouteAutoRequest(value, target)
      : guards.targetCompatibility(value, target, options);
  const before = JSON.stringify(request);
  const result = apply(request);
  const after = JSON.stringify(request);
  current.calls += 1;
  const boundaries = nonJson(request);
  if (boundaries.length) {
    migrationBoundaries.push({ baseline_test: current.id, call: current.calls, guard: kind, boundaries });
    current.non_json_calls += 1;
    return result;
  }
  // Ensure serialization preserves the original baseline call's behavior before
  // using JSON as the native fixture boundary. No normalization hides a mismatch.
  assert.deepEqual(apply(JSON.parse(before)), result);
  const input = { guard: kind, bytes: [...Buffer.from(before)] };
  if (kind !== 'safeguards') input.target = target;
  if (kind === 'target') input.auto_mode = options?.autoMode ?? false;
  const id = `baseline-auto-${tests.length}-${current.calls}`;
  cases.push({ id, op: 'guard_contract_json', input, node_expected: { result, before, after }, source_tests: [current.id] });
  current.case_ids.push(id);
  return result;
}
function test(name, callback) {
  assert.equal(typeof name, 'string');
  assert.equal(typeof callback, 'function');
  current = { id: `${sourcePath}#${tests.length + 1}`, name, calls: 0, non_json_calls: 0, case_ids: [] };
  tests.push(current);
  const returned = callback();
  assert.equal(returned, undefined, 'Guard capture supports synchronous baseline definitions only');
}
// The verified fixture source runs in the same realm as assert/structuredClone,
// retaining native object-prototype and frozen-input assertion semantics.
Function('test', 'assert', 'canRouteAutoRequest', 'hasRoutableSafeguards', 'targetCompatibility', `"use strict";\n${body}`)(
  test, assert,
  (request, target) => invoke('auto', request, target),
  request => invoke('safeguards', request),
  (request, target, options) => invoke('target', request, target, options),
);
assert.equal(tests.length, 23, 'Frozen definition inventory changed');
assert.ok(tests.every(row => row.calls > 0));
for (const fixture of cases) {
  assert.deepEqual(await evaluate(fixture), { id: fixture.id, op: fixture.op, result: fixture.node_expected });
}
const bytes = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
const report = {
  schema_version: 1, kind: 'frozen_auto_routing_contract_capture', passed: true,
  baseline_commit: manifest.baseline_commit, source_path: sourcePath, source_sha256: hash(source),
  generator_sha256: hash(readFileSync(new URL(import.meta.url))), node_version: process.version,
  cases: cases.length, cases_sha256: hash(bytes), tests, migration_boundaries: migrationBoundaries,
  limits: 'Baseline assertions and dynamic loops executed unchanged. JSON-safe calls capture result and before/after serialization for authoritative native comparison. Non-JSON API calls remain explicit migration boundaries. Capture alone is not native parity evidence.',
};
mkdirSync(output, { mode: 0o700 });
writeFileSync(join(output, 'cases.jsonl'), bytes, { flag: 'wx', mode: 0o600 });
writeFileSync(join(output, 'capture.json'), JSON.stringify(report, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify({ passed: true, definitions: tests.length, cases: cases.length, migration_boundaries: migrationBoundaries.length, output }));
