// Execute complete frozen statusline callbacks with explicit native value/API boundaries.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/statusline-boundaries-${Date.now()}`));
assert.ok(process.argv.length <= 4);
await verifyBaseline(reference);
const hash = value => createHash('sha256').update(value).digest('hex');
const ts = createRequire(join(root, 'package.json'))('typescript');
const baseline = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json')));
assert.equal(baseline.baseline_commit, 'ea930c247626ce2af5ccdad721b5121417bf4ad8');
const file = 'test/statusline.test.mjs';
const source = readFileSync(join(reference, file), 'utf8');
assert.equal(hash(source), baseline.files.find(row => row.path === file).sha256);
const syntax = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
const selected = new Set([5, 7, 8, 9, 12, 16, 18, 19, 23, 27]);
const bounds = { calls: 256, argument_bytes: 65536, nodes: 4096, depth: 32, corpus_bytes: 1048576 };
const definitions = [], edits = [], callbacks = [], cases = [], raw = [];
let current;
for (const statement of syntax.statements) {
  if (ts.isImportDeclaration(statement)) {
    edits.push({ start: statement.getStart(syntax), end: statement.end, text: '' });
    continue;
  }
  if (!ts.isExpressionStatement(statement) || !ts.isCallExpression(statement.expression)
    || statement.expression.expression.getText(syntax) !== 'test') continue;
  const call = statement.expression, number = definitions.length + 1;
  const row = { id: `${file}#${number}`, number, name: call.arguments[0].text,
    statement_sha256: hash(statement.getText(syntax)), call_sha256: hash(call.getText(syntax)),
    selected: selected.has(number), assertions: [], case_ids: [] };
  function visit(node) {
    if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)
      && node.expression.expression.getText(syntax) === 'assert') {
      const item = { id: `${row.id}:assert-${row.assertions.length + 1}`,
        expression: node.getText(syntax), sha256: hash(node.getText(syntax)),
        line: syntax.getLineAndCharacterOfPosition(node.getStart(syntax)).line + 1, executions: 0 };
      row.assertions.push(item);
      if (row.selected) edits.push({ start: node.getStart(syntax), end: node.end,
        text: `__assertion(${JSON.stringify(item.id)}, () => ${node.getText(syntax)})` });
    }
    ts.forEachChild(node, visit);
  }
  visit(call.arguments[1]);
  definitions.push(row);
}
assert.equal(definitions.length, 31);
// The final definition is the executable test and is not evaluated by this pure adapter.
const cutoff = source.indexOf('const cliPath = ');
assert.ok(cutoff > 0);
let transformed = source.slice(0, cutoff), previous = cutoff;
for (const edit of edits.filter(row => row.end <= cutoff).sort((a, b) => b.start - a.start)) {
  assert.ok(edit.end <= previous);
  transformed = transformed.slice(0, edit.start) + edit.text + transformed.slice(edit.end);
  previous = edit.start;
}
assert.doesNotMatch(transformed, /^\s*import\b/m);
const { renderStatusLine } = await import(pathToFileURL(join(reference, 'src/statusline.mjs')).href);

function project(args) {
  const adaptations = [];
  let nodes = 0;
  function walk(value, path, inObject = false) {
    assert.ok(++nodes <= bounds.nodes && path.length <= bounds.depth);
    if (value === undefined) {
      adaptations.push({ path, kind: inObject ? 'omit_own_undefined' : 'undefined_argument_to_null' });
      return null;
    }
    if (typeof value === 'number' && !Number.isFinite(value)) {
      adaptations.push({ path, kind: 'invalid_nonfinite_to_null', original: String(value) });
      return null;
    }
    if (path.length === 2 && path[0] === 1 && path[1] === 'pid' && value === process.pid) {
      adaptations.push({ path, kind: 'capture_process_pid', native: 1 });
      return 1;
    }
    if (typeof value === 'function') {
      assert.deepEqual(path, [2, 'alive']);
      adaptations.push({ path, kind: 'liveness_callback', source: value.toString() });
      return null;
    }
    if (value === null || ['string', 'boolean', 'number'].includes(typeof value)) return value;
    assert.equal(typeof value, 'object');
    assert.ok(Array.isArray(value) || Object.getPrototypeOf(value) === Object.prototype);
    const result = Array.isArray(value) ? [] : Object.create(null);
    for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(value))) {
      if (Array.isArray(value) && key === 'length') continue;
      assert.ok(descriptor.enumerable && Object.hasOwn(descriptor, 'value'), 'Unsupported property');
      const next = walk(descriptor.value, [...path, key], !Array.isArray(value));
      if (descriptor.value !== undefined || Array.isArray(value)) result[key] = next;
    }
    return result;
  }
  const values = args.map((value, index) => walk(value, [index]));
  assert.ok(Buffer.byteLength(JSON.stringify(values)) <= bounds.argument_bytes);
  return { values, adaptations };
}
function invoke(...args) {
  assert.equal(args.length, 3);
  assert.ok(current?.selected && cases.length < bounds.calls);
  const before = project(args), liveness = [];
  const actualArgs = args.slice();
  if (typeof args[2].alive === 'function') {
    const alive = args[2].alive;
    actualArgs[2] = { ...args[2], alive(...values) {
      assert.deepEqual(values, [process.pid]);
      try {
        const value = alive.apply(this, values);
        liveness.push({ kind: 'returned', value });
        return value;
      } catch (error) {
        liveness.push({ kind: 'threw', name: error.name, message: error.message });
        throw error;
      }
    } };
  }
  const expected = renderStatusLine(...actualArgs);
  assert.equal(typeof expected, 'string');
  assert.deepEqual(project(args), before, 'Pure render must preserve original inputs');
  const projected = structuredClone(before.values);
  if (typeof args[2].alive === 'function') {
    assert.equal(liveness.length, 1);
    const observed = liveness[0];
    if (observed.kind === 'returned') assert.equal(observed.value, false);
    else assert.deepEqual(observed, { kind: 'threw', name: 'Error', message: 'gone' });
    projected[2].alive = observed.kind === 'returned' ? observed.value !== false : false;
  }
  // This second frozen call proves only these concrete native API projections
  // preserve complete output. It does not claim arbitrary JS value equivalence.
  assert.equal(renderStatusLine(...projected), expected, 'Native projection changed frozen output');
  const id = `statusline-boundary-${current.number}-${current.case_ids.length + 1}`;
  const row = { id, source_test: current.id, arguments: projected,
    adaptations: before.adaptations, liveness, expected };
  cases.push(row); current.case_ids.push(id);
  raw.push({ id, capture_pid: process.pid, expected });
  return expected;
}
function test(name, callback) {
  const definition = definitions[callbacks.length];
  assert.equal(name, definition.name);
  callbacks.push({ definition, callback });
}
function __assertion(id, run) {
  const item = current.assertions.find(row => row.id === id);
  assert.ok(item); item.executions++;
  return run();
}
Function('test', 'assert', 'renderStatusLine', '__assertion', `"use strict";\n${transformed}`)(test, assert, invoke, __assertion);
assert.equal(callbacks.length, 30);
for (const { definition, callback } of callbacks) {
  if (!definition.selected) continue;
  current = definition;
  assert.equal(callback(), undefined);
  assert.ok(current.case_ids.length > 0);
}
const bytes = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
assert.ok(Buffer.byteLength(bytes) <= bounds.corpus_bytes);
const report = { schema_version: 1, kind: 'frozen_statusline_boundary_contracts', passed: true,
  baseline_commit: baseline.baseline_commit, generator_sha256: hash(readFileSync(new URL(import.meta.url))),
  source: { path: file, sha256: hash(source) }, node_version: process.version, bounds,
  cases: cases.length, corpus_sha256: hash(bytes), definitions: definitions.filter(row => row.selected),
  counts: { direct_static_assertions: definitions.filter(row => row.selected).reduce((n, row) => n + row.assertions.length, 0),
    expanded_assertions: definitions.reduce((n, row) => n + row.assertions.reduce((sum, a) => sum + a.executions, 0), 0),
    projection_replays: cases.length, adaptations: cases.reduce((n, row) => n + row.adaptations.length, 0) },
  limits: ['Complete unchanged callbacks execute; each complete output also matches a second frozen call with its explicit native JSON projection.',
    'Own undefined fields omit; top-level undefined input/snapshot and nonfinite invalid-number examples map to null. Every path and original nonfinite value is recorded; no general JS/ESM value equivalence is claimed.',
    'Native pure renderer accepts observed liveness as a boolean; original false/throw callbacks are invoked through a recording wrapper with original PID. Snapshot capture PID becomes positive synthetic1; this is not OS process-liveness evidence.',
    'No CLI/filesystem or timing evidence; complete ANSI bytes, width behavior, session isolation, labels and privacy outputs are compared.'] };
mkdirSync(output, { mode: 0o700 });
for (const [name, value] of [['cases.jsonl', bytes], ['capture.json', JSON.stringify(report, null, 2) + '\n'], ['raw.json', JSON.stringify(raw, null, 2) + '\n']]) {
  writeFileSync(join(output, name), value, { flag: 'wx', mode: 0o600 });
}
console.log(JSON.stringify({ passed: true, cases: cases.length, counts: report.counts, output }));
