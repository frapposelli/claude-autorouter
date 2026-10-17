// Development-only capture of the frozen pure comparator. No measurements.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/benchmark-contracts-${Date.now()}`));
assert.ok(process.argv.length <= 4, 'Usage: capture-benchmark-router-contracts.mjs [frozen-reference] [new-output-directory]');
await verifyBaseline(reference);
const ts = createRequire(join(root, 'package.json'))('typescript');
const hash = value => createHash('sha256').update(value).digest('hex');
const file = 'test/benchmark-router.test.mjs';
const source = readFileSync(join(reference, file), 'utf8');
const manifest = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json'), 'utf8'));
assert.equal(hash(source), manifest.files.find(row => row.path === file)?.sha256);
const syntax = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
const definitions = [], edits = [], callbacks = [], cases = [];
let current;
for (const statement of syntax.statements) {
  if (ts.isImportDeclaration(statement)) {
    edits.push({ start: statement.getStart(syntax), end: statement.end, text: '' });
    continue;
  }
  if (!ts.isExpressionStatement(statement) || !ts.isCallExpression(statement.expression)
    || statement.expression.expression.getText(syntax) !== 'test') continue;
  const call = statement.expression, number = definitions.length + 1;
  const assertions = [];
  function visit(node) {
    if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)
      && node.expression.expression.getText(syntax) === 'assert') assertions.push(node);
    ts.forEachChild(node, visit);
  }
  visit(call.arguments[1]);
  const definition = {
    id: `${file}#${number}`, number, name: call.arguments[0].text,
    definition_call_sha256: hash(call.getText(syntax)),
    definition_statement_sha256: hash(statement.getText(syntax)),
    assertions: assertions.map((node, index) => ({
      id: `${file}#${number}:assert-${index + 1}`,
      line: syntax.getLineAndCharacterOfPosition(node.getStart(syntax)).line + 1,
      expression: node.getText(syntax), sha256: hash(node.getText(syntax)), expanded_executions: 0,
    })), case_ids: [], calls: 0,
  };
  assertions.forEach((node, index) => edits.push({ start: node.getStart(syntax), end: node.end,
    text: `__assertion(${JSON.stringify(definition.assertions[index].id)}, () => ${node.getText(syntax)})` }));
  definitions.push(definition);
}
assert.equal(definitions.length, 2);
edits.sort((a, b) => b.start - a.start);
let transformed = source, previous = source.length;
for (const edit of edits) {
  assert.ok(edit.end <= previous, 'Overlapping capture transformations');
  transformed = transformed.slice(0, edit.start) + edit.text + transformed.slice(edit.end);
  previous = edit.start;
}
// Importing this module does not invoke its main guard or runRouterBenchmark.
const { compareRouterBenchmarks } = await import(pathToFileURL(join(reference, 'scripts/benchmark-router.mjs')).href);
function nonfinite(value, path = '') {
  if (typeof value === 'number' && !Number.isFinite(value)) return [{ path, value: String(value) }];
  if (!value || typeof value !== 'object') return [];
  return Object.entries(value).flatMap(([key, child]) => nonfinite(child,
    `${path}/${key.replaceAll('~', '~0').replaceAll('/', '~1')}`));
}
function compare(baseline, candidate) {
  assert.ok(current, 'Unexpected call outside a frozen callback');
  const input = JSON.stringify({ baseline, candidate });
  const result = compareRouterBenchmarks(baseline, candidate);
  assert.equal(JSON.stringify({ baseline, candidate }), input, 'Frozen comparator mutated its inputs');
  const id = `benchmark-router-contract-${current.number}-${++current.calls}`;
  cases.push({ id, source_test: current.id, input: JSON.parse(input),
    expected: JSON.parse(JSON.stringify(result)), nonfinite: nonfinite(result) });
  current.case_ids.push(id);
  return result;
}
Function('test', 'assert', 'compareRouterBenchmarks', '__assertion', `"use strict";\n${transformed}`)(
  (name, callback) => {
    assert.equal(name, definitions[callbacks.length].name);
    callbacks.push(callback);
  }, assert, compare,
  (id, execute) => {
    const row = current.assertions.find(entry => entry.id === id);
    assert.ok(row, 'Unexpected assertion');
    const result = execute();
    row.expanded_executions += 1;
    return result;
  });
for (const definition of definitions) {
  current = definition;
  await callbacks[definition.number - 1]();
  assert.equal(definition.calls, definition.number === 1 ? 2 : 6);
  assert.equal(definition.assertions.reduce((n, row) => n + row.expanded_executions, 0),
    definition.number === 1 ? 5 : 6);
}
assert.equal(cases.length, 8);
assert.equal(cases.filter(row => row.nonfinite.length).length, 1);
assert.deepEqual(cases[7].nonfinite, [
  { path: '/latency/0/candidate_max_p95_ms', value: '-Infinity' },
  { path: '/resources/0/candidate_sampled_peak_heap_bytes', value: '-Infinity' },
  { path: '/resources/0/candidate_event_loop_p95_ms', value: '-Infinity' },
]);
const corpus = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
const report = {
  schema_version: 1, kind: 'frozen_benchmark_router_comparator_capture',
  baseline_commit: manifest.baseline_commit, baseline_file: file, baseline_file_sha256: hash(source),
  sources: ['scripts/benchmark-router.mjs'].map(path => ({ path, sha256: hash(readFileSync(join(reference, path))) })),
  capture_source_sha256: hash(readFileSync(import.meta.filename)), definitions,
  complete_callbacks: 2, static_assertions: 6, expanded_assertions: 11, calls: 8,
  corpus_sha256: hash(corpus),
  transformations: edits.map(edit => ({ start: edit.start, end: edit.end,
    original_sha256: hash(source.slice(edit.start, edit.end)), replacement_sha256: hash(edit.text) })),
  limits: ['Only compareRouterBenchmarks is called; no workload, socket, evaluator or timing measurement.',
    'Complete JSON-serialized outputs are retained; raw -Infinity maxima remain tagged separately and become null on the JSON wire.',
    'Native admission accepts bounded finite historical JSON reports, not arbitrary JavaScript objects or coercible values.'],
};
mkdirSync(output, { recursive: false, mode: 0o700 });
writeFileSync(join(output, 'cases.jsonl'), corpus, { flag: 'wx', mode: 0o600 });
writeFileSync(join(output, 'capture.json'), `${JSON.stringify(report, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify({ output, calls: cases.length, assertions: 11, corpus_sha256: hash(corpus) }));
