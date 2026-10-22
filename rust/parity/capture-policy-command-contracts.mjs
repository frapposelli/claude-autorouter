// Development-only whole-callback capture of frozen policy.test.mjs #6 and #7.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import * as fs from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, 'artifacts/rust-rewrite/policy-command-capture'));
assert.ok(process.argv.length <= 4);
const hash = value => createHash('sha256').update(value).digest('hex');
const before = await verifyBaseline(reference);
const scriptHash = hash(fs.readFileSync(import.meta.filename));
const nodeHash = hash(fs.readFileSync(process.execPath));
const sourcePath = 'test/policy.test.mjs';
const source = fs.readFileSync(join(reference, sourcePath), 'utf8');
const ts = createRequire(join(root, 'package.json'))('typescript');
const syntax = ts.createSourceFile(sourcePath, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
assert.equal(syntax.parseDiagnostics.length, 0);
const definitions = [], edits = [], helpers = [], callbacks = [], cases = [], raw = [];
let current, activeCase, fixtureRoot;
function assertions(node) {
  const sites = [];
  function visit(child) {
    if (ts.isCallExpression(child) && ts.isPropertyAccessExpression(child.expression)
      && child.expression.expression.getText(syntax) === 'assert') sites.push(child);
    ts.forEachChild(child, visit);
  }
  visit(node);
  return sites;
}
for (const statement of syntax.statements) {
  if (ts.isImportDeclaration(statement)) {
    edits.push({ at: statement.getStart(syntax), end: statement.end, text: '' });
    continue;
  }
  if (!ts.isExpressionStatement(statement) || !ts.isCallExpression(statement.expression)
    || statement.expression.expression.getText(syntax) !== 'test') {
    helpers.push({ text: statement.getText(syntax), sha256: hash(statement.getText(syntax)) });
    continue;
  }
  const call = statement.expression, number = definitions.length + 1;
  const selected = number === 6 || number === 7;
  const sites = assertions(call.arguments.at(-1));
  const definition = { id: `${sourcePath}#${number}`, number, name: call.arguments[0].text,
    line: syntax.getLineAndCharacterOfPosition(statement.getStart(syntax)).line + 1,
    statement: statement.getText(syntax), statement_sha256: hash(statement.getText(syntax)), selected,
    assertions: sites.map((site, index) => ({ id: `${sourcePath}#${number}:assert-${index + 1}`,
      line: syntax.getLineAndCharacterOfPosition(site.getStart(syntax)).line + 1,
      expression: site.getText(syntax), sha256: hash(site.getText(syntax)), executions: 0 })) };
  if (selected) for (const [index, site] of sites.entries()) {
    // Insertions preserve nested assertion expressions and original Promise returns.
    edits.push({ at: site.getStart(syntax), end: site.getStart(syntax), text: `__count(${JSON.stringify(definition.assertions[index].id)}, ` });
    edits.push({ at: site.end, end: site.end, text: ')' });
  }
  else edits.push({ at: statement.getStart(syntax), end: statement.end, text: '' });
  definitions.push(definition);
}
assert.equal(definitions.length, 7);
assert.equal(helpers.length, 2);
const selected = definitions.filter(row => row.selected);
assert.equal(selected.flatMap(row => row.assertions).length, 12);
let transformed = source;
for (const edit of edits.sort((a, b) => b.at - a.at)) transformed = transformed.slice(0, edit.at) + edit.text + transformed.slice(edit.end);
const settings = await import(pathToFileURL(join(reference, 'src/user-config.mjs')).href);
const configuration = await import(pathToFileURL(join(reference, 'src/config-command.mjs')).href);
const onboarding = await import(pathToFileURL(join(reference, 'src/onboarding.mjs')).href);
function normalize(value) {
  if (typeof value === 'string') return fixtureRoot ? value.split(fixtureRoot).join('<fixture-root>') : value;
  if (Array.isArray(value)) return value.map(normalize);
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, normalize(item)]));
  return value;
}
function snapshot() {
  assert.equal(typeof fixtureRoot, 'string');
  const rows = [];
  function visit(path, relative, depth) {
    assert.ok(depth <= 2 && rows.length < 16);
    const info = fs.lstatSync(path);
    assert.ok(info.isFile() || info.isDirectory());
    const row = { path: relative, kind: info.isFile() ? 'file' : 'directory', mode: info.mode & 0o777 };
    if (info.isFile()) {
      assert.ok(info.size <= 65536);
      row.content = fs.readFileSync(path, 'utf8');
      assert.equal(Buffer.byteLength(row.content), info.size);
    }
    rows.push(row);
    if (info.isDirectory()) for (const name of fs.readdirSync(path).sort()) visit(join(path, name), relative === '.' ? name : `${relative}/${name}`, depth + 1);
  }
  visit(fixtureRoot, '.', 0);
  return rows;
}
function start(operation, input) {
  assert.ok(activeCase.operations.length < 16);
  const row = { operation, input, before: snapshot(), output: [], prompts: [], status: 'pending' };
  activeCase.operations.push(row);
  return row;
}
function finish(row, value, error) {
  row.after = snapshot();
  if (error) {
    assert.equal(error.name, 'Error');
    assert.equal(error.code, 'AUTOROUTER_CONFIG_ERROR');
    row.status = 'error'; row.error = { message: error.message, name: error.name, code: error.code };
  } else {
    assert.notEqual(value, undefined);
    row.status = 'ok'; row.result = value;
  }
}
function optionsInput(options) {
  assert.deepEqual(Object.keys(options.policy).sort(), ['path', 'trustedUid']);
  assert.equal(options.policy.trustedUid, process.getuid());
  return { env: structuredClone(options.env), policy: { path: options.policy.path, trusted_owner: 'fixture_uid' } };
}
function saveUserConfig(values, options) {
  const row = start('save', { values, env: options.env });
  try { const value = settings.saveUserConfig(values, options); finish(row, value); return value; }
  catch (error) { finish(row, undefined, error); throw error; }
}
function loadUserConfig(env, options) {
  const row = start('load', optionsInput({ ...options, env }));
  try { const value = settings.loadUserConfig(env, options); finish(row, value); return value; }
  catch (error) { finish(row, undefined, error); throw error; }
}
async function command(operation, invoke, args, options) {
  const row = start(operation, { args, ...optionsInput(options) });
  const forwarded = { ...options, write(line) { row.output.push(line); return options.write(line); } };
  if (options.prompt) forwarded.prompt = (...args) => { row.prompts.push(args); return options.prompt(...args); };
  try { const value = await invoke(args, forwarded); finish(row, value); return value; }
  catch (error) { finish(row, undefined, error); throw error; }
}
function test(name, callback) {
  assert.equal(name, selected[callbacks.length].name);
  callbacks.push(callback);
}
function counted(id, value) {
  const site = current.assertions.find(row => row.id === id);
  assert.ok(site); site.executions += 1;
  activeCase.assertion_calls.push(id);
  return value;
}
function mkdtempSync(prefix) {
  assert.equal(fixtureRoot, undefined);
  fixtureRoot = fs.mkdtempSync(prefix);
  return fixtureRoot;
}
Function('test', 'assert', 'chmodSync', 'mkdirSync', 'mkdtempSync', 'readFileSync', 'rmSync',
  'writeFileSync', 'tmpdir', 'join', 'saveUserConfig', 'loadUserConfig', 'configCommand', 'setup', '__count',
  `"use strict";\n${transformed}`)(test, assert, fs.chmodSync, fs.mkdirSync, mkdtempSync, fs.readFileSync, fs.rmSync,
  fs.writeFileSync, tmpdir, join, saveUserConfig, loadUserConfig,
  (...args) => command('config', configuration.configCommand, ...args),
  (...args) => command('setup', onboarding.setup, ...args), counted);
const originalFetch = globalThis.fetch;
let fetchCalls = 0;
globalThis.fetch = () => { fetchCalls += 1; throw Error('Selected policy callbacks must reject before network'); };
try {
  for (const definition of selected) {
    current = definition; fixtureRoot = undefined;
    activeCase = { source_test: definition.id, operations: [], assertion_calls: [] };
    const cleanup = [];
    try {
      await callbacks[selected.indexOf(definition)]({ after(action) { cleanup.push(action); } });
      activeCase.final_files = snapshot();
      raw.push(structuredClone(activeCase));
      cases.push(normalize(activeCase));
    } finally {
      for (const action of cleanup.reverse()) await action();
      assert.equal(fs.existsSync(fixtureRoot), false);
    }
  }
} finally { globalThis.fetch = originalFetch; }
assert.equal(fetchCalls, 0);
assert.deepEqual(cases.map(row => row.operations.length), [4, 5]);
assert.deepEqual(selected.map(row => row.assertions.map(site => site.executions)), [[1, 1, 1, 1, 1, 1, 1], [1, 1, 1, 1, 0]]);
const after = await verifyBaseline(reference);
assert.deepEqual(before, after);
assert.equal(hash(fs.readFileSync(process.execPath)), nodeHash);
assert.equal(hash(fs.readFileSync(import.meta.filename)), scriptHash);
const corpus = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
assert.ok(Buffer.byteLength(corpus) < 1024 * 1024);
const report = { schema_version: 1, kind: 'frozen_policy_command_whole_callback_capture',
  baseline: before, source: { path: sourcePath, sha256: hash(source) }, capture_source_sha256: scriptHash,
  node: { version: process.version, sha256: nodeHash }, helpers, definitions: selected,
  static_assertions: 12, expanded_assertions: 11, untaken_prompt_guard: `${sourcePath}#7:assert-5`,
  operations: 9, command_calls: 4, setup_calls: 1, load_calls: 2, save_calls: 2, fetch_calls: fetchCalls,
  corpus: { bytes: Buffer.byteLength(corpus), sha256: hash(corpus), rows: cases.length },
  transformations: 'Complete original callbacks and fixture/uid helpers remain. Only imports, public-call recording facades and insertion-only assertion result counters change. Original assertion arguments and Promise return values remain; awaited callback completion proves awaited assertions settled.',
  adaptations: ['Private fixture-root occurrences become <fixture-root>; trustedUid is validated against the actual fixture owner and recorded as fixture_uid. Raw reports retain actual paths separately.',
    'Full config JSON output is retained as text; native comparison parses this JSON to ignore object insertion order and insignificant formatting. Actual config-file bytes, modes, absence, operation order, load fields, result values and error messages are compared.',
    'JavaScript Error name/code are retained as reference-only metadata. Native errors use Result<String>; original predicates test message text. Prompt guard remains unexecuted in the original.',
    'A global fetch rejection guard is installed only for selected execution and restored in finally; zero calls proves the complete selected callbacks performed no network access.'],
  bounds: { operations_per_definition: 16, filesystem_entries: 16, filesystem_depth: 2, file_bytes: 65536, corpus_bytes: 1048576 },
  cleanup: { fixture_directories_removed: 2 }, exclusions: ['No system policy writes or user configuration', 'No live provider, Keychain or setup-success claim'] };
const metadata = JSON.stringify(report, null, 2) + '\n';
assert.ok(Buffer.byteLength(metadata) < 1024 * 1024);
fs.mkdirSync(output, { mode: 0o700 });
for (const [name, data] of [['policy-command-contracts.jsonl', corpus], ['policy-command-contracts.capture.json', metadata], ['raw.json', JSON.stringify(raw, null, 2) + '\n']]) fs.writeFileSync(join(output, name), data, { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify({ output, corpus_sha256: hash(corpus), capture_sha256: hash(metadata), cases: cases.length, operations: 9, static_assertions: 12, expanded_assertions: 11 }));
