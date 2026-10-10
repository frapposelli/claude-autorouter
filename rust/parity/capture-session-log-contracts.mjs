// Development-only capture of complete, unchanged frozen session-log callbacks.
// The writer, filesystem mocks, assertion expressions and close Promises are real.
import assert from 'node:assert/strict';
import { mock } from 'node:test';
import { createHash } from 'node:crypto';
import fs from 'node:fs/promises';
import { constants, readFileSync, rmSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/session-log-contracts-${Date.now()}`));
assert.ok(process.argv.length <= 4, 'Usage: capture-session-log-contracts.mjs [reference] [new-output]');
const before = await verifyBaseline(reference);
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const nodeHash = hash(readFileSync(process.execPath));
const scriptHash = hash(readFileSync(import.meta.filename));
const sourceFile = 'test/session-log.test.mjs';
const source = readFileSync(join(reference, sourceFile), 'utf8');
const baseline = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json'), 'utf8'));
assert.equal(baseline.baseline_commit, 'ea930c247626ce2af5ccdad721b5121417bf4ad8');
assert.equal(hash(source), baseline.files.find(row => row.path === sourceFile)?.sha256);
const ts = createRequire(join(root, 'package.json'))('typescript');
const syntax = ts.createSourceFile(sourceFile, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
assert.equal(syntax.parseDiagnostics.length, 0);
const selectedNumbers = [1, 2, 3, 4, 5, 6, 7, 8, 11, 12, 13, 14, 15, 16, 17];
const definitions = [], edits = [], callbacks = [], assertionCalls = [], observations = [];
let current, observation;
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
  const selected = selectedNumbers.includes(number), sites = assertionNodes(call.arguments.at(-1));
  const definition = { id: `${sourceFile}#${number}`, number, name: call.arguments[0].text,
    source_line: syntax.getLineAndCharacterOfPosition(statement.getStart(syntax)).line + 1,
    statement: statement.getText(syntax), statement_sha256: hash(statement.getText(syntax)),
    selected, assertions: sites.map((node, index) => ({ id: `${sourceFile}#${number}:assert-${index + 1}`,
      line: syntax.getLineAndCharacterOfPosition(node.getStart(syntax)).line + 1,
      expression: node.getText(syntax), sha256: hash(node.getText(syntax)), expanded_executions: 0 })) };
  if (selected) for (const [index, node] of sites.entries()) {
    // Insertions preserve nested assert.doesNotThrow(() => assert.equal(...)).
    edits.push({ start: node.getStart(syntax), end: node.getStart(syntax),
      text: `__assertion(${JSON.stringify(definition.assertions[index].id)}, ` });
    edits.push({ start: node.end, end: node.end, text: ')' });
  }
  else edits.push({ start: statement.getStart(syntax), end: statement.end, text: '' });
  definitions.push(definition);
}
assert.equal(definitions.length, 17);
assert.equal(definitions.filter(row => row.selected).reduce((n, row) => n + row.assertions.length, 0), 88);
let transformed = source;
for (const edit of edits.sort((a, b) => b.start - a.start || b.end - a.end)) {
  transformed = transformed.slice(0, edit.start) + edit.text + transformed.slice(edit.end);
}
assert.doesNotMatch(transformed, /^\s*import\b/m);
const original = await import(pathToFileURL(join(reference, 'src/session-log.mjs')).href);
const originalOpen = fs.open, originalLstat = fs.lstat, originalReaddir = fs.readdir;
const roots = new Set();
const deadline = setTimeout(() => {
  for (const path of roots) rmSync(path, { recursive: true, force: true });
  process.stderr.write('Session-log capture whole-run deadline exceeded\n');
  process.exit(1);
}, 60_000);

// Own data descriptors only: recording must never invoke an original getter.
function input(value, depth = 0) {
  assert.ok(depth < 16, 'Input depth bound');
  if (value === undefined) return { $js: 'undefined' };
  if (typeof value === 'number' && !Number.isFinite(value)) return { $js: String(value) };
  if (value === null || typeof value !== 'object') return value;
  if (Array.isArray(value)) return value.map(row => input(row, depth + 1));
  const result = {};
  for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(value))) {
    if (value instanceof Error && key === 'stack') continue;
    result[key] = Object.hasOwn(descriptor, 'value') ? input(descriptor.value, depth + 1)
      : { $js: 'accessor', get: typeof descriptor.get === 'function', set: typeof descriptor.set === 'function' };
  }
  if (value instanceof Error) return { $js: 'Error', own: result };
  return result;
}
function compact(rows) {
  if (!rows.length) return { base: null, rows: [] };
  const base = rows[0];
  const encoded = rows.map(row => {
    if (!row || Array.isArray(row) || typeof row !== 'object' || !base || Array.isArray(base)
      || typeof base !== 'object') return { value: row };
    const set = {}, remove = Object.keys(base).filter(key => !Object.hasOwn(row, key));
    for (const [key, value] of Object.entries(row)) {
      if (!Object.hasOwn(base, key) || JSON.stringify(value) !== JSON.stringify(base[key])) set[key] = value;
    }
    return { set, remove };
  });
  const expanded = encoded.map(row => {
    if (Object.hasOwn(row, 'value')) return row.value;
    const value = { ...base, ...row.set };
    for (const key of row.remove) delete value[key];
    return value;
  });
  assert.deepEqual(expanded, rows, 'Compact representation expands to every original row');
  return { base, rows: encoded };
}
function counted(id, value) {
  assert.ok(assertionCalls.length < 6000, 'Assertion execution bound');
  const site = current.assertions.find(site => site.id === id);
  assert.ok(site);
  site.expanded_executions++;
  assertionCalls.push(id);
  return value;
}
function test(name, ...args) {
  const definition = definitions.filter(row => row.selected)[callbacks.length];
  assert.equal(name, definition.name);
  callbacks.push(args.at(-1));
}
async function createSessionLog(directory, options) {
  roots.add(dirname(directory));
  const writer = { id: observation.writers.length, directory: basename(directory),
    include_prompts: options?.includePrompts ?? true, inputs: [], records: [], closes: [] };
  observation.writers.push(writer);
  observation.operations.push({ kind: 'create', writer: writer.id });
  const warning = options?.warn ?? (() => {});
  const actual = await original.createSessionLog(directory, { ...options, warn(message) {
    observation.warnings.push({ writer: writer.id, message });
    observation.operations.push({ kind: 'warning', writer: writer.id, message });
    return warning(message);
  } });
  const record = actual.record, close = actual.close, promises = new Map();
  actual.record = entry => {
    assert.ok(writer.records.length < 5002, 'Record attempt bound');
    const snapshot = input(entry);
    const operation = { kind: 'record', writer: writer.id, index: writer.inputs.length };
    observation.operations.push(operation);
    const result = record(entry);
    operation.result = result;
    writer.inputs.push(snapshot);
    writer.records.push(result);
    return result;
  };
  actual.close = () => {
    const promise = close();
    if (!promises.has(promise)) promises.set(promise, promises.size);
    writer.closes.push(promises.get(promise));
    observation.operations.push({ kind: 'close', writer: writer.id, promise: promises.get(promise) });
    assert.ok(writer.closes.length < 16, 'Close invocation bound');
    return promise; // Preserve the exact original shared Promise, including identity.
  };
  return actual;
}
async function regularFile(path) {
  const handle = await originalOpen(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const stat = await handle.stat();
    assert.ok(stat.isFile() && stat.nlink === 1 && stat.size <= 2 * 1024 * 1024, 'Raw regular file bound');
    const bytes = await handle.readFile();
    assert.ok(bytes.length <= 2 * 1024 * 1024);
    return { bytes, mode: stat.mode & 0o777 };
  } finally { await handle.close(); }
}
async function inspect(directory) {
  const entries = [];
  async function visit(path, role, depth) {
    assert.ok(depth <= 2);
    const stat = await originalLstat(path);
    if (stat.isSymbolicLink()) { entries.push({ role, kind: 'symlink' }); return; }
    if (stat.isDirectory()) {
      entries.push({ role, kind: 'directory', mode: stat.mode & 0o777 });
      const names = (await originalReaddir(path)).sort();
      assert.ok(names.length <= 128, 'Session/directory entry bound');
      for (const name of names) await visit(join(path, name), role ? `${role}/${name}` : name, depth + 1);
      return;
    }
    const { bytes, mode } = await regularFile(path), text = bytes.toString('utf8');
    if (!basename(path).startsWith('autorouter-session-')) {
      entries.push({ role, kind: 'fixture', mode, text }); return;
    }
    assert.match(basename(path), /^autorouter-session-[A-Za-z0-9-]+\.jsonl$/);
    assert.equal(mode, 0o600);
    const rows = text.trim() ? text.trimEnd().split('\n').map(line => JSON.parse(line)) : [];
    assert.ok(rows.length <= 800);
    for (const row of rows) {
      assert.equal(typeof row.timestamp, 'string');
      assert.match(row.timestamp, /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/);
      assert.ok(Number.isFinite(Date.parse(row.timestamp)));
      const generated = current.number === 6 || (current.number === 17 && row.request_id === 'bad-request');
      if (generated) {
        assert.ok(Date.parse(row.timestamp) >= observation.started_ms - 1000
          && Date.parse(row.timestamp) <= Date.now() + 1000, 'Generated timestamp belongs to this execution');
      } else {
        assert.equal(row.timestamp, current.number === 16 && row.event === 'outcome'
          ? '2026-10-01T12:00:01.000Z' : '2026-10-01T12:00:00.000Z');
      }
    }
    entries.push({ role, kind: 'session', mode, text, rows });
  }
  await visit(directory, '', 0);
  return entries;
}
Function('test', 'assert', 'fs', 'join', 'tmpdir', 'createSessionLog', '__assertion',
  `"use strict";\n${transformed}`)(test, assert, fs, join, tmpdir, createSessionLog, counted);
try {
  for (const [index, definition] of definitions.filter(row => row.selected).entries()) {
    current = definition;
    observation = { source_test: definition.id, started_ms: Date.now(), writers: [], warnings: [],
      operations: [], files: [], handles: [] };
    observations.push(observation);
    const cleanup = [], handles = [];
    mock.method(fs, 'open', async (...args) => {
      const handle = await originalOpen(...args);
      handles.push(handle);
      return handle;
    });
    try {
      await callbacks[index]({ after: action => cleanup.push(action), mock });
      assert.ok(definition.assertions.every(row => row.expanded_executions > 0));
      assert.equal(roots.size, 1);
      observation.files = await inspect([...roots][0]);
      observation.handles = handles.map(handle => ({ closed: handle.fd === -1 }));
      assert.ok(observation.handles.every(handle => handle.closed));
    } finally {
      for (const action of cleanup.reverse()) await action();
      mock.restoreAll();
      for (const path of roots) {
        await assert.rejects(originalLstat(path), { code: 'ENOENT' });
      }
      roots.clear();
    }
  }
} finally {
  clearTimeout(deadline);
  mock.restoreAll();
  for (const path of roots) await fs.rm(path, { recursive: true, force: true });
}
const cases = observations.map(observed => {
  const files = observed.files.map(file => {
    if (file.kind !== 'session') return file;
    const rows = file.rows.map(row => ({ ...row, timestamp: row.timestamp === '2026-10-01T12:00:00.000Z'
      || row.timestamp === '2026-10-01T12:00:01.000Z' ? row.timestamp : '<generated-iso-timestamp>' }));
    const sessionHash = /-([a-f0-9]{64})\.jsonl$/.exec(file.role)?.[1];
    assert.ok(sessionHash);
    return { kind: file.kind, directory: dirname(file.role), session_hash: sessionHash, mode: file.mode,
      bytes: Buffer.byteLength(file.text), rows: compact(rows) };
  }).sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b), 'en'));
  return { source_test: observed.source_test, writers: observed.writers.map(writer => ({ ...writer,
    inputs: compact(writer.inputs) })), warnings: observed.warnings, operations: observed.operations,
    files, handles: observed.handles };
});
const after = await verifyBaseline(reference);
assert.deepEqual(after, before);
assert.equal(hash(readFileSync(process.execPath)), nodeHash);
assert.equal(hash(readFileSync(import.meta.filename)), scriptHash);
assert.equal(hash(readFileSync(join(reference, sourceFile))), hash(source));
const canonical = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
const report = { schema_version: 1, kind: 'frozen_session_log_contract_execution', baseline: before,
  source: { path: sourceFile, sha256: hash(source) }, capture_source_sha256: scriptHash,
  node: { version: process.version, sha256: nodeHash }, selected_definitions: cases.length,
  static_assertions: 88, expanded_assertions: assertionCalls.length,
  writer_count: observations.reduce((sum, row) => sum + row.writers.length, 0),
  record_count: observations.reduce((sum, row) => sum + row.writers.reduce((n, writer) => n + writer.records.length, 0), 0),
  raw_file_count: observations.reduce((sum, row) => sum + row.files.filter(file => file.kind === 'session').length, 0),
  raw_row_count: observations.reduce((sum, row) => sum + row.files.reduce((n, file) => n + (file.rows?.length ?? 0), 0), 0),
  canonical: { bytes: Buffer.byteLength(canonical), sha256: hash(canonical) },
  bounds: { whole_run_ms: 60_000, record_attempts_per_writer: 5002, assertions: 6000,
    session_files: 128, rows_per_file: 800, raw_file_bytes: 2 * 1024 * 1024, corpus_bytes: 1024 * 1024 },
  transformations: 'Only imports, unselected definition removal and nested-safe assertion-result counter insertions. Selected original helpers/callbacks/fixtures/mocks/expressions are unchanged. The exact real writer object and close Promises are retained; factory/record/close/warning observations delegate to frozen production behavior.',
  projections: ['Random session launch filenames are validated by original assertions and capture format/mode checks before replacing their launch portion with the exact session digest; actual files/rows remain in raw evidence.',
    'Every raw timestamp is validated as a finite ISO timestamp. Original fixed timestamps are checked exactly for their fixture/event before projection; only definitions6 and17 generated timestamp fields receive a marker, after verifying the actual execution time interval.',
    'Own property descriptors encode undefined, nonfinite numbers and accessors without invoking getters; no JS-only API capability is claimed for Rust.',
    'Compact base/set/remove tables are expanded and compared with all original rows/inputs before publication. Exact finite numbers and absent fields are preserved.'],
  definitions: definitions.filter(row => row.selected), assertion_calls: assertionCalls };
const encoded = JSON.stringify(report, null, 2) + '\n';
const raw = JSON.stringify(observations) + '\n';
assert.ok(Buffer.byteLength(canonical) <= 1024 * 1024);
assert.ok(Buffer.byteLength(encoded) <= 512 * 1024);
assert.ok(Buffer.byteLength(raw) <= 12 * 1024 * 1024);
await fs.mkdir(output, { mode: 0o700 });
for (const [name, bytes] of [['session-log-contracts.jsonl', canonical], ['session-log-contracts.capture.json', encoded],
  ['raw-observations.json', raw]]) await fs.writeFile(join(output, name), bytes, { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify({ output, selected_definitions: cases.length, static_assertions: 88,
  expanded_assertions: assertionCalls.length, records: report.record_count, rows: report.raw_row_count,
  corpus_sha256: hash(canonical), capture_sha256: hash(encoded), raw_sha256: hash(raw) }));
