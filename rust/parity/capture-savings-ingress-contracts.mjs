// Complete unchanged original #8/#17/#18 callbacks, including non-JSON values.
// Tagged observations preserve nonfinite numbers/accessors; none become null.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

assert.equal(process.argv.length, 4, 'Pass frozen reference and new output directory');
const root = resolve(import.meta.dirname, '../..'), reference = resolve(process.argv[2]), output = resolve(process.argv[3]);
assert.equal((await verifyBaseline(reference)).files_verified, 111);
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const baseline = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json')));
const file = 'test/savings.test.mjs', source = readFileSync(join(reference, file), 'utf8');
assert.equal(hash(source), baseline.files.find(row => row.path === file)?.sha256);
const ts = createRequire(join(root, 'package.json'))('typescript');
const syntax = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
assert.equal(syntax.parseDiagnostics.length, 0);
const selected = new Set([8, 17, 18]), definitions = [], edits = [], callbacks = [], cases = [], assertionOrder = [];
let current, totalSteps = 0, trackers = 0;
function assertionNodes(node) {
  const result = [];
  function visit(child) {
    if (ts.isCallExpression(child) && ts.isPropertyAccessExpression(child.expression) && child.expression.expression.getText(syntax) === 'assert') result.push(child);
    ts.forEachChild(child, visit);
  }
  visit(node); return result;
}
for (const statement of syntax.statements) {
  if (ts.isImportDeclaration(statement)) { edits.push({ start: statement.getStart(syntax), end: statement.end, text: '' }); continue; }
  if (!ts.isExpressionStatement(statement) || !ts.isCallExpression(statement.expression) || statement.expression.expression.getText(syntax) !== 'test') continue;
  const number = definitions.length + 1, call = statement.expression;
  const definition = { id: `${file}#${number}`, number, name: call.arguments[0].text, statement: statement.getText(syntax), statement_sha256: hash(statement.getText(syntax)), selected: selected.has(number), assertions: assertionNodes(call.arguments[1]).map((node, index) => ({ id: `${file}#${number}:assert-${index + 1}`, expression: node.getText(syntax), sha256: hash(node.getText(syntax)), line: syntax.getLineAndCharacterOfPosition(node.getStart(syntax)).line + 1, executions: 0 })), cases: [] };
  if (definition.selected) for (const [index, node] of assertionNodes(call.arguments[1]).entries()) edits.push({ start: node.getStart(syntax), end: node.end, text: `__assertion(${JSON.stringify(definition.assertions[index].id)}, () => ${node.getText(syntax)})` });
  definitions.push(definition);
}
assert.equal(definitions.length, 22);
let transformed = source, previous = source.length;
for (const edit of edits.sort((a, b) => b.start - a.start)) { assert.ok(edit.end <= previous); transformed = transformed.slice(0, edit.start) + edit.text + transformed.slice(edit.end); previous = edit.start; }
const savings = await import(pathToFileURL(join(reference, 'src/savings.mjs')).href);
function tagged(value, depth = 0) {
  assert.ok(depth < 32);
  if (value === undefined) return { $js_type: 'undefined' };
  if (typeof value === 'number' && !Number.isFinite(value)) return { $js_type: 'nonfinite', value: Number.isNaN(value) ? 'NaN' : value > 0 ? '+Infinity' : '-Infinity' };
  if (value === null || ['number', 'string', 'boolean'].includes(typeof value)) return value;
  assert.equal(typeof value, 'object');
  assert.ok(Array.isArray(value) || Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null);
  if (Array.isArray(value)) { assert.ok(value.length < 256); return value.map(v => tagged(v, depth + 1)); }
  const properties = Object.getOwnPropertyDescriptors(value); assert.ok(Object.keys(properties).length < 64);
  return Object.fromEntries(Object.entries(properties).filter(([, d]) => d.enumerable).map(([key, descriptor]) => [key,
    Object.hasOwn(descriptor, 'value') ? tagged(descriptor.value, depth + 1) : { $js_type: 'accessor', enumerable: descriptor.enumerable, configurable: descriptor.configurable, getter_source: descriptor.get?.toString() ?? null, setter_source: descriptor.set?.toString() ?? null },
  ]));
}
function addCase(kind, value) {
  assert.ok(current?.selected);
  const row = { id: `savings-ingress-${current.number}-${current.cases.length + 1}`, source_test: current.id, kind, ...value };
  cases.push(row); current.cases.push(row.id); return row;
}
function createSavingsTracker(...args) {
  assert.ok(++trackers <= 24); assert.equal(args.length, 0);
  const tracker = savings.createSavingsTracker();
  const row = addCase('tracker', { initial_snapshot: tagged(tracker.snapshot()), steps: [] });
  function step(value) { assert.ok(++totalSteps <= 256 && row.steps.length < 32); row.steps.push(value); }
  return {
    update(value) {
      const before = tagged(value), result = tracker.update(value);
      assert.equal(result, undefined); assert.deepEqual(tagged(value), before, 'original input unchanged without invoking accessors');
      step({ operation: 'update', tagged_event: before, original_return: tagged(result), expected_snapshot: tagged(tracker.snapshot()) }); return result;
    },
    snapshot() { const result = tracker.snapshot(); step({ operation: 'snapshot', expected_snapshot: tagged(result) }); return result; },
    clear() { const result = tracker.clear(); assert.equal(result, undefined); step({ operation: 'clear', original_return: tagged(result), expected_snapshot: tagged(tracker.snapshot()) }); return result; },
  };
}
function test(name, callback) { assert.equal(name, definitions[callbacks.length].name); callbacks.push(callback); }
function recordAssertion(id, execute) {
  const site = current.assertions.find(row => row.id === id); assert.ok(site);
  const result = execute(); site.executions++; assertionOrder.push(id); return result;
}
Function('test', 'assert', 'createSavingsTracker', 'estimateOutcomeSavings', 'PRICING_VERSION', 'PRICING_DATE', 'PRICING_SOURCE', 'PRICING_FACTS', '__assertion', `"use strict";\n${transformed}`)(test, assert, createSavingsTracker, savings.estimateOutcomeSavings, savings.PRICING_VERSION, savings.PRICING_DATE, savings.PRICING_SOURCE, savings.PRICING_FACTS, recordAssertion);
for (const definition of definitions) {
  if (!definition.selected) continue; current = definition;
  let before;
  if (definition.number === 18) before = tagged(savings.PRICING_FACTS);
  await callbacks[definition.number - 1]();
  if (definition.number === 18) {
    const after = tagged(savings.PRICING_FACTS); assert.deepEqual(after, before);
    const frozen = { root: Object.isFrozen(savings.PRICING_FACTS), models: Object.isFrozen(savings.PRICING_FACTS.models), rates: Object.fromEntries(Object.entries(savings.PRICING_FACTS.models).map(([id, rates]) => [id, Object.isFrozen(rates)])) };
    assert.ok(frozen.root && frozen.models && Object.values(frozen.rates).every(Boolean));
    addCase('pricing', { pricing_before: before, pricing_after: after, frozen, constants: { version: savings.PRICING_VERSION, date: savings.PRICING_DATE, source: savings.PRICING_SOURCE } });
  }
  assert.ok(definition.assertions.every(row => row.executions > 0));
}
const selectedDefinitions = definitions.filter(row => row.selected), corpus = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
assert.ok(Buffer.byteLength(corpus) < 256 * 1024);
assert.equal(trackers, 19); assert.equal(cases.length, 20); assert.equal(assertionOrder.length, 46);
assert.equal((await verifyBaseline(reference)).files_verified, 111);
const capture = { schema_version: 1, kind: 'exact_original_savings_ingress_and_facts', baseline_commit: baseline.baseline_commit, status: 'passed', generator_sha256: hash(readFileSync(import.meta.filename)), baseline_file_sha256: hash(source), cases_sha256: hash(corpus), cases: cases.length, trackers, steps: totalSteps, static_assertions: selectedDefinitions.reduce((n, d) => n + d.assertions.length, 0), expanded_assertions: assertionOrder.length, definitions: selectedDefinitions, node: { version: process.version, executable_sha256: hash(readFileSync(process.execPath)) }, semantics: ['Original callbacks, source helpers, all18 usage values and all12 original assertion expressions execute unchanged.', 'Own undefined, nonfinite Number values and accessor descriptors are tagged, never converted to null or invoked by capture.', 'The source getter actually runs inside original update and is caught by the source tracker; no native getter-equivalence claim is made.', 'Actual frozen pricing facts before/after both throwing mutation attempts and all nested frozen objects are recorded; no current-provider pricing claim.'], baseline_files_verified: 111 };
mkdirSync(output); writeFileSync(join(output, 'savings-ingress-contracts.jsonl'), corpus); writeFileSync(join(output, 'savings-ingress-contracts.capture.json'), JSON.stringify(capture, null, 2) + '\n'); writeFileSync(join(output, 'assertion-executions.json'), JSON.stringify(assertionOrder, null, 2) + '\n');
console.log(JSON.stringify({ status: 'passed', cases: cases.length, trackers, steps: totalSteps, original_assertions: assertionOrder.length, corpus_bytes: Buffer.byteLength(corpus), corpus_sha256: hash(corpus) }));
