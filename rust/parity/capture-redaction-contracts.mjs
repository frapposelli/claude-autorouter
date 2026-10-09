// Development-only capture of frozen synthetic privacy assertions. No network.
// Two timing callbacks execute only their exact idempotence prefix: elapsed
// assertions and adversarial suffixes remain explicit, unexecuted obligations.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/redaction-contracts-${Date.now()}`));
assert.ok(process.argv.length <= 4, 'Usage: capture-redaction-contracts.mjs [frozen-reference] [new-output-directory]');
await verifyBaseline(reference);
const ts = createRequire(join(root, 'package.json'))('typescript');
const hash = value => createHash('sha256').update(value).digest('hex');
const file = 'test/redaction.test.mjs';
const source = readFileSync(join(reference, file), 'utf8');
const manifest = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json'), 'utf8'));
assert.equal(hash(source), manifest.files.find(row => row.path === file)?.sha256);
const syntax = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
const selected = new Set([1, 2, 3, 4, 5, 6, 7, 9, 11, 12, 13, 14, 16, 17]);
const expectedCalls = [12, 7, 11, 5, 2, 2, 2, 3, 7, 15, 12, 11, 2, 2];
const expectedAssertions = [24, 7, 11, 5, 1, 14, 2, 5, 7, 29, 12, 11, 1, 12];
const definitions = [], edits = [], cases = [], assertionCalls = [];
const now = '2026-10-09T12:00:00.000Z';
let current;

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
  const call = statement.expression, number = definitions.length + 1;
  const callback = call.arguments[1], assertions = assertionNodes(callback);
  let cutoff;
  if (number === 5 || number === 16) {
    const adversarial = callback.body.statements.find(node => ts.isVariableStatement(node)
      && node.declarationList.declarations.some(declaration => declaration.name.getText(syntax) === 'adversarial'));
    assert.ok(adversarial, 'Frozen adversarial boundary changed');
    cutoff = adversarial.getStart(syntax);
    edits.push({ start: cutoff, end: callback.body.end - 1, text: '' });
  }
  const definition = {
    id: `${file}#${number}`, number, name: call.arguments[0].text,
    definition_call_sha256: hash(call.getText(syntax)),
    definition_statement_sha256: hash(statement.getText(syntax)),
    selected: selected.has(number), execution: cutoff ? 'exact_idempotence_prefix_only' : 'complete_callback',
    assertions: assertions.map((node, index) => ({
      id: `${file}#${number}:assert-${index + 1}`,
      line: syntax.getLineAndCharacterOfPosition(node.getStart(syntax)).line + 1,
      expression: node.getText(syntax), sha256: hash(node.getText(syntax)),
      executed: selected.has(number) && (cutoff === undefined || node.getStart(syntax) < cutoff),
      expanded_executions: 0,
    })),
    case_ids: [], calls: 0,
  };
  if (cutoff !== undefined) definition.skipped_suffix = {
    sha256: hash(source.slice(cutoff, callback.body.end - 1)),
    bytes: Buffer.byteLength(source.slice(cutoff, callback.body.end - 1)),
    timing_assertions: number === 5 ? 8 : 20,
    reason: 'Original per-input elapsed <2000ms assertions are not executed or qualified by this deterministic capture.',
  };
  for (let index = 0; index < assertions.length; index += 1) {
    const node = assertions[index];
    if (!definition.assertions[index].executed) continue;
    edits.push({ start: node.getStart(syntax), end: node.end,
      text: `__assertion(${JSON.stringify(definition.assertions[index].id)}, () => ${node.getText(syntax)})` });
  }
  definitions.push(definition);
}
assert.equal(definitions.length, 17);
// Import resolution only: retain the original await/destructuring behavior.
for (const module of ['prompt-state', 'telemetry-event']) {
  const needle = `import('../src/${module}.mjs')`;
  assert.equal(source.split(needle).length, 2, 'Frozen dynamic import topology changed');
  const start = source.indexOf(needle);
  edits.push({ start, end: start + needle.length, text: `__loadCapturedModule(${JSON.stringify(module)})` });
}
edits.sort((a, b) => b.start - a.start);
let transformed = source, previous = source.length;
for (const edit of edits) {
  assert.ok(edit.end <= previous, 'Capture transformations must not overlap');
  transformed = transformed.slice(0, edit.start) + edit.text + transformed.slice(edit.end);
  previous = edit.start;
}
assert.doesNotMatch(transformed, /^\s*import\b/m);
const redaction = await import(pathToFileURL(join(reference, 'src/redaction.mjs')).href);
const prompts = await import(pathToFileURL(join(reference, 'src/prompt-state.mjs')).href);
const ollama = await import(pathToFileURL(join(reference, 'src/ollama-evaluator.mjs')).href);
const telemetry = await import(pathToFileURL(join(reference, 'src/telemetry-event.mjs')).href);

function invoke(op, fn, args) {
  assert.ok(current, 'Calls outside selected callbacks must not be captured');
  const before = JSON.stringify(args);
  const result = fn(...args);
  assert.equal(JSON.stringify(args), before, 'Pure frozen operation mutated its input');
  assert.notEqual(result, undefined, 'Unexpected undefined result needs an explicit outcome representation');
  const id = `redaction-contract-${current.number}-${++current.calls}`;
  cases.push({ id, source_test: current.id, op, args: JSON.parse(before), now,
    expected: { kind: 'value', value: structuredClone(result) } });
  current.case_ids.push(id);
  return result;
}
const wrapped = {
  redactSensitive: (...args) => invoke('redact', redaction.redactSensitive, args),
  buildState: (...args) => invoke('build_state', prompts.buildState, args),
  buildOllamaState: (...args) => invoke('build_ollama_state', ollama.buildOllamaState, args),
  promptExcerpt: (...args) => invoke('prompt_excerpt', prompts.promptExcerpt, args),
  normalizeSessionRecord: (...args) => {
    const OriginalDate = globalThis.Date;
    globalThis.Date = class extends OriginalDate {
      constructor(...values) { super(...(values.length ? values : [now])); }
      static now() { return OriginalDate.parse(now); }
    };
    try { return invoke('normalize_session', telemetry.normalizeSessionRecord, args); }
    finally { globalThis.Date = OriginalDate; }
  },
};
const callbacks = [];
function test(name, callback) {
  assert.equal(name, definitions[callbacks.length].name);
  callbacks.push(callback);
}
function recordAssertion(id, execute) {
  const row = current.assertions.find(entry => entry.id === id);
  assert.ok(row?.executed, 'Unexpected assertion execution');
  const result = execute();
  row.expanded_executions += 1;
  assertionCalls.push(id);
  return result;
}
const syntheticCredentials = Function('test', 'assert', 'redactSensitive', 'buildState', 'buildOllamaState', 'readConfig', 'Router',
  '__loadCapturedModule', '__assertion', `"use strict";\n${transformed}\nreturn [...Object.values(fake), ...Object.values(more)];`)(test, assert,
  wrapped.redactSensitive, wrapped.buildState, wrapped.buildOllamaState,
  () => assert.fail('Evaluator callback is outside this capture'),
  class { constructor() { assert.fail('Evaluator callback is outside this capture'); } },
  async module => module === 'prompt-state' ? { promptExcerpt: wrapped.promptExcerpt }
    : { normalizeSessionRecord: wrapped.normalizeSessionRecord }, recordAssertion);
let index = 0;
for (const definition of definitions) {
  if (!definition.selected) continue;
  current = definition;
  await callbacks[definition.number - 1]();
  assert.equal(definition.calls, expectedCalls[index], `${definition.id}: call count`);
  assert.equal(definition.assertions.reduce((n, row) => n + row.expanded_executions, 0), expectedAssertions[index], `${definition.id}: assertion count`);
  index += 1;
}
assert.equal(cases.length, 93);
assert.equal(assertionCalls.length, 141);
assert.equal(syntheticCredentials.length, 26);
assert.equal(new Set(syntheticCredentials).size, 26);
// The frozen tests construct these public fake credentials at runtime so the
// source contains no literal credential shape. Keep that property in captured
// JSON too: escape only the first code unit of each exact known synthetic value.
// JSON readers reconstruct the unchanged fixture, including embedded values.
function serializeFixture(row) {
  let encoded = JSON.stringify(row);
  for (const value of syntheticCredentials) {
    assert.equal(typeof value, 'string');
    assert.ok(value.length > 0 && value.length <= 1024 && /^[A-Za-z0-9-]/.test(value));
    const literal = JSON.stringify(value).slice(1, -1);
    const escaped = `\\u${value.charCodeAt(0).toString(16).padStart(4, '0')}${literal.slice(1)}`;
    encoded = encoded.split(literal).join(escaped);
  }
  assert.deepEqual(JSON.parse(encoded), row, 'Wire encoding changed a decoded fixture');
  for (const value of syntheticCredentials) {
    assert.ok(!encoded.includes(JSON.stringify(value).slice(1, -1)), 'Literal synthetic credential shape remains');
  }
  return encoded;
}
const corpus = cases.map(serializeFixture).join('\n') + '\n';
assert.ok(Buffer.byteLength(corpus) <= 512 * 1024, 'Captured corpus byte bound');
const report = {
  schema_version: 1, kind: 'frozen_redaction_functional_contract_capture',
  baseline_commit: manifest.baseline_commit, baseline_file: file, baseline_file_sha256: hash(source),
  sources: ['src/redaction.mjs', 'src/prompt-state.mjs', 'src/ollama-evaluator.mjs', 'src/telemetry-event.mjs']
    .map(path => ({ path, sha256: hash(readFileSync(join(reference, path))) })),
  capture_source_sha256: hash(readFileSync(import.meta.filename)),
  selected_definitions: 14, complete_callbacks: 12, idempotence_prefixes: 2,
  static_assertions: 37, executed_static_assertions: 35, expanded_functional_assertions: 141,
  unexecuted_timing_assertions: 28, calls: cases.length, corpus_sha256: hash(corpus),
  serialization: { kind: 'json_unicode_escape_known_synthetic_credentials', credential_values: 26,
    source: 'Frozen test fake and more dictionaries; only first UTF-16 code unit of exact value occurrences is escaped.',
    decoded_values_unchanged: true },
  clock: { now, scope: 'Only normalizeSessionRecord fallback Date; no elapsed-time clock is fabricated.' },
  transformations: edits.map(edit => ({ start: edit.start, end: edit.end,
    original_sha256: hash(source.slice(edit.start, edit.end)), replacement_sha256: hash(edit.text) })),
  definitions,
  limits: ['No Router/evaluator callback8; already-covered10/15 not executed.',
    'No elapsed-time, linear-complexity, provider, storage-file or gateway-byte qualification.',
    'All payloads derive from public synthetic baseline tests; whole returned values are retained for native comparison.'],
};
mkdirSync(output, { recursive: false, mode: 0o700 });
writeFileSync(join(output, 'cases.jsonl'), corpus, { flag: 'wx', mode: 0o600 });
writeFileSync(join(output, 'capture.json'), `${JSON.stringify(report, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify({ output, calls: cases.length, assertions: assertionCalls.length, corpus_sha256: hash(corpus) }));
