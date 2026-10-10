// Development-only execution of complete, unchanged frozen history callbacks.
// Assertions are counted without replacing their expressions or return values.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import fs from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { basename, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/history-contracts-${Date.now()}`));
assert.ok(process.argv.length <= 4, 'Usage: capture-history-contracts.mjs [frozen-reference] [new-output]');
const before = await verifyBaseline(reference);
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const nodeHash = hash(readFileSync(process.execPath));
const scriptHash = hash(readFileSync(import.meta.filename));
const sourceFile = 'test/session-history.test.mjs';
const source = readFileSync(join(reference, sourceFile), 'utf8');
const baseline = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json'), 'utf8'));
assert.equal(baseline.baseline_commit, 'ea930c247626ce2af5ccdad721b5121417bf4ad8');
assert.equal(hash(source), baseline.files.find(row => row.path === sourceFile)?.sha256);
const ts = createRequire(join(root, 'package.json'))('typescript');
const syntax = ts.createSourceFile(sourceFile, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
assert.equal(syntax.parseDiagnostics.length, 0);
const definitions = [], edits = [], callbacks = [], assertionCalls = [], reads = [], commands = [];
let current;
function assertions(node) {
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
  const selected = number <= 10;
  const sites = assertions(call.arguments.at(-1));
  const definition = { id: `${sourceFile}#${number}`, number, name: call.arguments[0].text,
    source_line: syntax.getLineAndCharacterOfPosition(statement.getStart(syntax)).line + 1,
    statement: statement.getText(syntax), statement_sha256: hash(statement.getText(syntax)),
    call_sha256: hash(call.getText(syntax)), selected,
    execution: selected ? 'complete_unchanged_callback' : 'not_executed',
    assertions: sites.map((node, index) => ({ id: `${sourceFile}#${number}:assert-${index + 1}`,
      line: syntax.getLineAndCharacterOfPosition(node.getStart(syntax)).line + 1,
      expression: node.getText(syntax), sha256: hash(node.getText(syntax)), expanded_executions: 0 })) };
  if (selected) for (const [index, node] of sites.entries()) {
    edits.push({ start: node.getStart(syntax), end: node.end,
      text: `__assertion(${JSON.stringify(definition.assertions[index].id)}, ${node.getText(syntax)})` });
  }
  if (!selected) edits.push({ start: statement.getStart(syntax), end: statement.end, text: '' });
  definitions.push(definition);
}
assert.equal(definitions.length, 14);
let transformed = source, previous = source.length;
for (const edit of edits.sort((a, b) => b.start - a.start)) {
  assert.ok(edit.end <= previous, 'Capture edits overlap');
  transformed = transformed.slice(0, edit.start) + edit.text + transformed.slice(edit.end);
  previous = edit.start;
}
assert.doesNotMatch(transformed, /^\s*import\b/m);
const history = await import(pathToFileURL(join(reference, 'src/session-history.mjs')).href);
const settings = await import(pathToFileURL(join(reference, 'src/user-config.mjs')).href);
const savings = await import(pathToFileURL(join(reference, 'src/savings.mjs')).href);
function test(name, ...args) {
  assert.equal(name, definitions[callbacks.length].name);
  callbacks.push(args.at(-1));
}
function counted(id, value) {
  assert.ok(assertionCalls.length < 128);
  const site = current.assertions.find(site => site.id === id);
  assert.ok(site);
  site.expanded_executions += 1;
  assertionCalls.push(id);
  return value;
}
async function readSessionHistory(directory, options) {
  assert.ok(reads.length < 32);
  reads.push({ source_test: current.id, directory_role: basename(directory), options: options ?? {} });
  return history.readSessionHistory(directory, options);
}
async function sessionsCommand(args, options) {
  assert.ok(commands.length < 16);
  commands.push({ source_test: current.id, args });
  return history.sessionsCommand(args, options);
}
function execFileSync() { assert.fail('Selected callbacks must never spawn a subprocess'); }
Function('test', 'assert', 'fs', 'join', 'tmpdir', 'readSessionHistory', 'sessionsCommand',
  'saveUserConfig', 'PRICING_VERSION', 'PRICING_DATE', 'PRICING_SOURCE', 'execFileSync', '__assertion',
  `"use strict";\n${transformed}`)(test, assert, fs, join, tmpdir, readSessionHistory, sessionsCommand,
  settings.saveUserConfig, savings.PRICING_VERSION, savings.PRICING_DATE, savings.PRICING_SOURCE,
  execFileSync, counted);
for (const definition of definitions.filter(row => row.selected)) {
  current = definition;
  const cleanup = [];
  try {
    await callbacks[definition.number - 1]({ after: action => cleanup.push(action) });
    assert.ok(definition.assertions.every(row => row.expanded_executions > 0));
  } finally {
    for (const action of cleanup.reverse()) await action();
  }
}
const after = await verifyBaseline(reference);
assert.deepEqual(after, before);
assert.equal(hash(readFileSync(process.execPath)), nodeHash);
assert.equal(hash(readFileSync(import.meta.filename)), scriptHash);
assert.equal(hash(readFileSync(join(reference, sourceFile))), hash(source));
assert.equal(reads.length, 21);
assert.equal(commands.length, 7);
assert.equal(assertionCalls.length, 100);
const selected = definitions.filter(row => row.selected);
assert.equal(selected.reduce((sum, row) => sum + row.assertions.length, 0), 88);
const report = { schema_version: 1, kind: 'frozen_history_contract_execution',
  baseline: before, source: { path: sourceFile, sha256: hash(source) },
  capture_source_sha256: scriptHash, node: { version: process.version, sha256: nodeHash },
  selected_definitions: 10, static_assertions: 88, expanded_assertions: assertionCalls.length,
  direct_reader_calls: reads.length, command_calls: commands.length,
  transformations: 'Unselected definitions are removed; selected definitions have import wiring and source-assertion result counters only. Complete original selected helpers, inputs, callbacks, loops, filesystem operations, assertion expressions and cleanup remain unchanged. Public-call facades count invocations and delegate to exact frozen exports.',
  adaptations: ['Private temporary path identities are omitted from metadata; directory roles and original ID/limit arguments remain.',
    'Assertions returning promises retain the exact promise. Complete callback completion proves those awaited assertions resolved. No source assertion is omitted or replaced.'],
  exclusions: 'Only definitions1 through10. No provider, model, performance, total-memory, current pricing or complete project claim.',
  definitions: selected, assertion_calls: assertionCalls, reads, commands };
const encoded = `${JSON.stringify(report, null, 2)}\n`;
assert.ok(Buffer.byteLength(encoded) <= 256 * 1024, 'Capture metadata bound');
await fs.mkdir(output);
await fs.writeFile(join(output, 'history-contracts.capture.json'), encoded, { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify({ output, definitions: 10, static_assertions: 88,
  expanded_assertions: assertionCalls.length, reader_calls: reads.length, command_calls: commands.length,
  capture_sha256: hash(encoded), bytes: Buffer.byteLength(encoded) }));
