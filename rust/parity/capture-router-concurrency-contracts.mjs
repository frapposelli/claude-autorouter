// Execute every frozen concurrency assertion unchanged. Capture its observed
// value separately from native scheduling and JavaScript cancellation identity.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const ts = createRequire(import.meta.url)('typescript');
const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/router-concurrency-contracts-${Date.now()}`));
assert.ok(process.argv.length <= 4, 'Usage: capture-router-concurrency-contracts.mjs [frozen-reference] [new-output-directory]');
await verifyBaseline(reference);
const sourcePath = 'test/router-concurrency.test.mjs';
const source = readFileSync(join(reference, sourcePath), 'utf8');
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const manifest = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json'), 'utf8'));
assert.equal(hash(source), manifest.files.find(row => row.path === sourcePath)?.sha256);
const { Router, CLASSIFICATION_LIMITS } = await import(pathToFileURL(join(reference, 'src/router.mjs')).href);
const { readConfig } = await import(pathToFileURL(join(reference, 'src/config.mjs')).href);
const syntax = ts.createSourceFile(sourcePath, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
assert.equal(syntax.parseDiagnostics.length, 0);
const definitions = [], edits = [], tests = [];
let current;

function snapshot(value) {
  if (value === undefined) return { kind: 'undefined' };
  if (value instanceof Promise) return { kind: 'promise' };
  if (value instanceof RegExp) return { kind: 'regexp', source: value.source, flags: value.flags };
  if (typeof value === 'function') return { kind: 'predicate' };
  const visit = item => {
    if (item === null || ['string', 'boolean'].includes(typeof item)) return;
    if (typeof item === 'number') { assert.ok(Number.isFinite(item)); return; }
    assert.equal(typeof item, 'object');
    assert.ok(Array.isArray(item) || Object.getPrototypeOf(item) === Object.prototype);
    for (const descriptor of Object.values(Object.getOwnPropertyDescriptors(item))) {
      if (!descriptor.enumerable) continue;
      assert.ok(Object.hasOwn(descriptor, 'value'));
      visit(descriptor.value);
    }
  };
  visit(value);
  return { kind: 'json', value: JSON.parse(JSON.stringify(value)) };
}

for (const statement of syntax.statements) {
  if (ts.isImportDeclaration(statement)) {
    edits.push({ start: statement.getStart(syntax), end: statement.end, value: '' });
    continue;
  }
  if (!ts.isExpressionStatement(statement) || !ts.isCallExpression(statement.expression)
    || statement.expression.expression.getText(syntax) !== 'test') continue;
  const call = statement.expression;
  const number = definitions.length + 1;
  const id = `${sourcePath}#${number}`;
  const definition = { id, number, name: call.arguments[0].text,
    source_sha256: hash(call.getText(syntax)), assertions: [] };
  const walk = node => {
    if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)
      && node.expression.expression.getText(syntax) === 'assert') {
      const method = node.expression.name.text;
      assert.ok(['equal', 'deepEqual', 'ok', 'rejects'].includes(method), `Unexpected assertion ${method}`);
      const assertion = { id: `${id}/assert-${definition.assertions.length + 1}`, method,
        expression: node.getText(syntax), line: syntax.getLineAndCharacterOfPosition(node.getStart(syntax)).line + 1 };
      definition.assertions.push(assertion);
      const receiver = node.expression.expression;
      edits.push({ start: receiver.getStart(syntax), end: receiver.end,
        value: `captureAssertion(${JSON.stringify(assertion.id)})` });
    }
    ts.forEachChild(node, walk);
  };
  walk(call);
  definitions.push(definition);
}
assert.equal(definitions.length, 10);
let transformed = source;
for (const edit of edits.sort((a, b) => b.start - a.start)) {
  transformed = transformed.slice(0, edit.start) + edit.value + transformed.slice(edit.end);
}
assert.doesNotMatch(transformed, /^\s*import\b/m);
function captureAssertion(id) {
  const owner = current;
  const declaration = owner.assertions.find(row => row.id === id);
  assert.ok(declaration, 'Assertion escaped its original definition');
  return { [declaration.method]: (...args) => {
    const observed = args.map(snapshot);
    const iteration = owner.invocations.get(id) ?? 0;
    owner.invocations.set(id, iteration + 1);
    const result = Reflect.apply(assert[declaration.method], assert, args);
    const record = () => { owner.observations.push({ assertion_id: id, iteration,
      method: declaration.method, arguments: observed, passed: true }); };
    if (declaration.method === 'rejects') return result.then(record);
    record();
    return result;
  } };
}
function test(name, options, callback) {
  if (typeof options === 'function') { callback = options; options = {}; }
  const declaration = definitions[tests.length];
  assert.equal(name, declaration.name);
  assert.equal(typeof callback, 'function');
  tests.push({ ...declaration, callback, timeout: options.timeout, observations: [], invocations: new Map() });
}
Function('test', 'assert', 'captureAssertion', 'delay', 'Router', 'CLASSIFICATION_LIMITS', 'readConfig',
  `"use strict";\n${transformed}`)(test, assert, captureAssertion, delay, Router, CLASSIFICATION_LIMITS, readConfig);
assert.equal(tests.length, definitions.length);
for (const definition of tests) {
  current = definition;
  let timer;
  try {
    await Promise.race([definition.callback(), new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`Frozen definition ${definition.id} timed out`)), definition.timeout ?? 30_000);
    })]);
  } finally { clearTimeout(timer); }
  for (const assertion of definition.assertions) {
    assert.ok(definition.observations.some(row => row.assertion_id === assertion.id), `Unexecuted ${assertion.id}`);
  }
  definition.observations.sort((a, b) => {
    const index = id => definition.assertions.findIndex(row => row.id === id);
    return index(a.assertion_id) - index(b.assertion_id) || a.iteration - b.iteration;
  });
}
const cases = tests.map(({ callback: _, timeout: __, invocations: ___, ...definition }) => definition);
const bytes = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
const report = { schema_version: 1, kind: 'frozen_router_concurrency_assertion_capture', passed: true,
  baseline_commit: manifest.baseline_commit, source_path: sourcePath, source_sha256: hash(source),
  generator_sha256: hash(readFileSync(new URL(import.meta.url))), node_version: process.version,
  typescript_version: ts.version, definitions: definitions.length,
  static_assertions: definitions.reduce((total, row) => total + row.assertions.length, 0),
  executed_assertions: cases.reduce((total, row) => total + row.observations.length, 0),
  cases_sha256: hash(bytes), classification_limits: CLASSIFICATION_LIMITS,
  limits: 'Original callbacks, synthetic fetch implementations, assertion expressions, real clocks and test-specific timeout bounds remain unchanged. Only assert receivers are wrapped to retain successfully checked arguments; assertion order is preserved during execution and records sorted by static call site afterward. Promise/regexp/predicate arguments are explicit tagged JavaScript boundaries; capture does not claim native cancellation-reason or callback identity parity. Captured boolean predicates still require corresponding native behavioral assertions. This capture alone proves no native scheduling, routing, transport or resource behavior.' };
mkdirSync(output, { recursive: false, mode: 0o700 });
writeFileSync(join(output, 'cases.jsonl'), bytes, { flag: 'wx', mode: 0o600 });
writeFileSync(join(output, 'capture.json'), JSON.stringify(report, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify(report, null, 2));
