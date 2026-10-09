// Development-only execution of unchanged frozen synthetic savings callbacks.
// No prices, usage, telemetry, clocks, or source assertion expressions change.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/savings-finite-${Date.now()}`));
assert.ok(process.argv.length <= 4, 'Usage: capture-savings-finite-contracts.mjs [frozen-reference] [new-output]');
await verifyBaseline(reference);
const ts = createRequire(join(root, 'package.json'))('typescript');
const hash = value => createHash('sha256').update(value).digest('hex');
const file = 'test/savings.test.mjs';
const source = readFileSync(join(reference, file), 'utf8');
const baseline = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json'), 'utf8'));
assert.equal(baseline.baseline_commit, 'ea930c247626ce2af5ccdad721b5121417bf4ad8');
assert.equal(hash(source), baseline.files.find(row => row.path === file)?.sha256);
const syntax = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
const selected = new Set([1, 2, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 19, 20, 22]);
const bounds = { trackers: 128, outcomes: 32, steps_per_tracker: 256, total_steps: 4096,
  value_bytes: 65536, corpus_bytes: 1048576, depth: 32, value_nodes: 4096 };
const definitions = [], edits = [], callbacks = [], cases = [], assertionCalls = [];
let current, totalSteps = 0, trackers = 0, outcomes = 0;

function assertionNodes(node) {
  const result = [];
  function visit(child) {
    if (ts.isCallExpression(child) && ts.isPropertyAccessExpression(child.expression)
      && child.expression.expression.getText(syntax) === 'assert') result.push(child);
    ts.forEachChild(child, visit);
  }
  visit(node);
  return result;
}
for (const statement of syntax.statements) {
  if (ts.isImportDeclaration(statement)) {
    edits.push({ start: statement.getStart(syntax), end: statement.end, text: '' });
    continue;
  }
  if (!ts.isExpressionStatement(statement) || !ts.isCallExpression(statement.expression)
    || statement.expression.expression.getText(syntax) !== 'test') continue;
  const number = definitions.length + 1, call = statement.expression;
  const definition = { id: `${file}#${number}`, number, name: call.arguments[0].text,
    definition_call_sha256: hash(call.getText(syntax)),
    definition_statement_sha256: hash(statement.getText(syntax)),
    selected: selected.has(number), execution: selected.has(number) ? 'complete_unchanged_callback' : 'not_executed',
    assertions: assertionNodes(call.arguments[1]).map((node, index) => ({
      id: `${file}#${number}:assert-${index + 1}`,
      line: syntax.getLineAndCharacterOfPosition(node.getStart(syntax)).line + 1,
      expression: node.getText(syntax), sha256: hash(node.getText(syntax)), expanded_executions: 0,
    })), case_ids: [] };
  if (definition.selected) {
    for (const [index, node] of assertionNodes(call.arguments[1]).entries()) {
      edits.push({ start: node.getStart(syntax), end: node.end,
        text: `__assertion(${JSON.stringify(definition.assertions[index].id)}, () => ${node.getText(syntax)})` });
    }
  }
  definitions.push(definition);
}
assert.equal(definitions.length, 22);
edits.sort((a, b) => b.start - a.start);
let transformed = source, previous = source.length;
for (const edit of edits) {
  assert.ok(edit.end <= previous, 'Capture transformations overlap');
  transformed = transformed.slice(0, edit.start) + edit.text + transformed.slice(edit.end);
  previous = edit.start;
}
assert.doesNotMatch(transformed, /^\s*import\b/m);
const savings = await import(pathToFileURL(join(reference, 'src/savings.mjs')).href);

// The selected callbacks do not observe own-property identity of undefined
// fields. Keep every omitted path as evidence; native JSON omits those fields.
// Nonfinite numbers, accessors, arrays with undefined, and exotic objects are
// rejected here; their source definitions are outside this finite batch.
function wire(value) {
  const undefinedPaths = [];
  let nodes = 0;
  function check(item, path, depth, inArray = false) {
    assert.ok(++nodes <= bounds.value_nodes && depth <= bounds.depth, 'Value exceeds structural bound');
    if (item === undefined) {
      assert.ok(path.length > 0 && !inArray, 'Unsupported undefined array/root value');
      undefinedPaths.push(path);
      return;
    }
    if (item === null || typeof item === 'string' || typeof item === 'boolean') return;
    if (typeof item === 'number') { assert.ok(Number.isFinite(item)); return; }
    assert.equal(typeof item, 'object');
    assert.ok(Array.isArray(item) || Object.getPrototypeOf(item) === Object.prototype
      || Object.getPrototypeOf(item) === null, 'Unsupported exotic object');
    for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(item))) {
      if (Array.isArray(item) && key === 'length') continue;
      assert.ok(Object.hasOwn(descriptor, 'value'), 'Accessors are outside this capture');
      check(descriptor.value, [...path, key], depth + 1, Array.isArray(item));
    }
  }
  check(value, [], 0);
  const encoded = JSON.stringify(value);
  assert.ok(Buffer.byteLength(encoded) <= bounds.value_bytes, 'Value exceeds byte bound');
  return { value: JSON.parse(encoded), undefined_paths: undefinedPaths };
}

function addCase(kind, extra) {
  assert.ok(current?.selected);
  const row = { id: `savings-finite-${current.number}-${current.case_ids.length + 1}`,
    source_test: current.id, kind, ...extra };
  cases.push(row);
  current.case_ids.push(row.id);
  return row;
}
function createSavingsTracker(...args) {
  assert.ok(++trackers <= bounds.trackers);
  assert.ok(args.length <= 1 && (args.length === 0 || args[0] !== undefined));
  const options = wire(args.length ? args[0] : {});
  const tracker = savings.createSavingsTracker(...args);
  const row = addCase('tracker', { options: options.value, options_undefined_paths: options.undefined_paths,
    default_options: args.length === 0, initial_snapshot: wire(tracker.snapshot()).value, steps: [] });
  function step(value) {
    assert.ok(++totalSteps <= bounds.total_steps && row.steps.length < bounds.steps_per_tracker);
    row.steps.push(value);
  }
  return {
    update(value) {
      const input = wire(value);
      const result = tracker.update(value);
      assert.equal(result, undefined);
      assert.deepEqual(wire(value), input, 'Source update changed its input');
      const snapshot = wire(tracker.snapshot());
      assert.equal(snapshot.undefined_paths.length, 0);
      step({ op: 'update', event: input.value, undefined_paths: input.undefined_paths,
        expected_snapshot: snapshot.value });
      return result;
    },
    snapshot() {
      const result = tracker.snapshot();
      const captured = wire(result);
      assert.equal(captured.undefined_paths.length, 0);
      step({ op: 'snapshot', expected_snapshot: captured.value });
      return result;
    },
    clear() {
      const result = tracker.clear();
      assert.equal(result, undefined);
      step({ op: 'clear', expected_snapshot: wire(tracker.snapshot()).value });
      return result;
    },
  };
}
function estimateOutcomeSavings(...args) {
  assert.equal(args.length, 1);
  assert.ok(++outcomes <= bounds.outcomes);
  const input = wire(args[0]);
  const result = savings.estimateOutcomeSavings(...args);
  assert.deepEqual(wire(args[0]), input, 'Source outcome changed its input');
  const expected = wire(result);
  assert.equal(expected.undefined_paths.length, 0);
  addCase('outcome', { input: input.value, undefined_paths: input.undefined_paths, expected: expected.value });
  return result;
}
function test(name, callback) {
  assert.equal(name, definitions[callbacks.length].name);
  callbacks.push(callback);
}
function recordAssertion(id, execute) {
  const row = current.assertions.find(entry => entry.id === id);
  assert.ok(row, 'Unexpected original assertion');
  const result = execute();
  row.expanded_executions += 1;
  assertionCalls.push(id);
  return result;
}
Function('test', 'assert', 'createSavingsTracker', 'estimateOutcomeSavings', 'PRICING_VERSION',
  'PRICING_DATE', 'PRICING_SOURCE', 'PRICING_FACTS', '__assertion', `"use strict";\n${transformed}`)(
  test, assert, createSavingsTracker, estimateOutcomeSavings, savings.PRICING_VERSION,
  savings.PRICING_DATE, savings.PRICING_SOURCE, savings.PRICING_FACTS, recordAssertion);
for (const definition of definitions) {
  if (!definition.selected) continue;
  current = definition;
  await callbacks[definition.number - 1]();
  assert.ok(definition.case_ids.length > 0);
  assert.ok(definition.assertions.every(row => row.expanded_executions > 0));
}
const jsonl = cases.map(row => `${JSON.stringify(row)}\n`).join('');
assert.ok(Buffer.byteLength(jsonl) <= bounds.corpus_bytes, 'Corpus exceeds declared byte bound');
const selectedDefinitions = definitions.filter(row => row.selected);
const capture = {
  schema_version: 1, kind: 'frozen_savings_finite_contracts', baseline_commit: baseline.baseline_commit,
  source: { path: file, sha256: hash(source) },
  selected_definitions: selectedDefinitions.length, cases: cases.length, trackers, outcomes, total_steps: totalSteps,
  static_assertions: selectedDefinitions.reduce((n, row) => n + row.assertions.length, 0),
  expanded_assertions: assertionCalls.length,
  corpus_sha256: hash(jsonl), corpus_bytes: Buffer.byteLength(jsonl), bounds,
  adaptations: [
    'JavaScript Number values compare to native JSON numbers by exact f64 value, accepting 6 and 6.0 but no tolerance, rounding, missing keys, or array reordering.',
    'Only own object fields whose original value is undefined are omitted for native JSON; every path is retained. No selected assertion checks own-property identity.',
    'Plain JavaScript object prototype isolation maps to independent native JSON maps; no JavaScript prototype or ESM embedding compatibility is claimed.',
    'Native snapshots are owned detached Values; explicit native mutation checks supplement the unchanged source callback mutations.',
    'Snapshot reads after each update additionally expose complete intermediate state. They do not replace or remove any original source snapshot/assertion.',
  ],
  exclusions: 'Definitions8,14–18,21 are not executed here. No fresh pricing, performance, capacity, hostile accessor, immutable JS object, live provider or user-data claim.',
  definitions: selectedDefinitions,
};
mkdirSync(output);
writeFileSync(join(output, 'savings-finite-contracts.jsonl'), jsonl, { flag: 'wx' });
writeFileSync(join(output, 'savings-finite-contracts.capture.json'), `${JSON.stringify(capture, null, 2)}\n`, { flag: 'wx' });
console.log(JSON.stringify({ output, cases: cases.length, trackers, outcomes, total_steps: totalSteps,
  static_assertions: capture.static_assertions, expanded_assertions: capture.expanded_assertions,
  corpus_bytes: capture.corpus_bytes, corpus_sha256: capture.corpus_sha256 }));
