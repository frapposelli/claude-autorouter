// Finite frozen opt-in tool contracts; all HTTP responses come from original mocks.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';
const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/ollama-routing-tools-${Date.now()}`));
assert.ok(process.argv.length <= 4);
await verifyBaseline(reference);
const hash = value => createHash('sha256').update(value).digest('hex');
const file = 'test/ollama-routing.test.mjs';
const source = readFileSync(join(reference, file), 'utf8');
const manifest = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json')));
assert.equal(hash(source), manifest.files.find(row => row.path === file).sha256);
const fixtureBytes = readFileSync(join(reference, 'test/fixtures/ollama-integration.json'), 'utf8');
const fixtureCases = JSON.parse(fixtureBytes);
const ts = createRequire(join(root, 'package.json'))('typescript');
const syntax = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
assert.equal(syntax.parseDiagnostics.length, 0);
const definitions = [], edits = [], helperAssertions = [];
function assertions(node, owner) {
  const found = [];
  function walk(child) {
    if (ts.isCallExpression(child) && ts.isPropertyAccessExpression(child.expression)
      && child.expression.expression.getText(syntax) === 'assert') found.push(child);
    ts.forEachChild(child, walk);
  }
  walk(node);
  return found.map((child, index) => {
    const row = { id: `${owner}:assert-${index + 1}`, line: syntax.getLineAndCharacterOfPosition(child.getStart(syntax)).line + 1,
      expression: child.getText(syntax), sha256: hash(child.getText(syntax)) };
    edits.push({ start: child.getStart(syntax), end: child.end,
      text: `__assertion(${JSON.stringify(row.id)}, () => ${child.getText(syntax)})` });
    return row;
  });
}
for (const statement of syntax.statements) {
  if (ts.isImportDeclaration(statement)) { edits.push({ start: statement.getStart(syntax), end: statement.end, text: '' }); continue; }
  if (ts.isFunctionDeclaration(statement) && statement.name.text === 'localFixture') {
    helperAssertions.push(...assertions(statement, 'localFixture')); continue;
  }
  if (!ts.isExpressionStatement(statement) || !ts.isCallExpression(statement.expression)
    || statement.expression.expression.getText(syntax) !== 'test') continue;
  const call = statement.expression, number = definitions.length + 1;
  const row = { id: `${file}#${number}`, number, name: call.arguments[0].text,
    selected: number >= 5, statement_sha256: hash(statement.getText(syntax)), call_sha256: hash(call.getText(syntax)),
    line: syntax.getLineAndCharacterOfPosition(statement.getStart(syntax)).line + 1, assertions: [] };
  if (row.selected) row.assertions = assertions(call, row.id);
  definitions.push(row);
}
assert.equal(definitions.length, 8);
assert.equal(definitions.reduce((n, row) => n + row.assertions.length, 0), 25);
assert.equal(helperAssertions.length, 9);
let transformed = source, previous = source.length;
for (const edit of edits.sort((a, b) => b.start - a.start)) {
  assert.ok(edit.end <= previous);
  transformed = transformed.slice(0, edit.start) + edit.text + transformed.slice(edit.end);
  previous = edit.start;
}
const load = path => import(pathToFileURL(join(reference, path)).href);
const [configuration, routing, tool] = await Promise.all([load('src/config.mjs'), load('src/router.mjs'), load('scripts/test-ollama-routing.mjs')]);
const limits = { cases: 6, requests_per_case: 20, requests: 64, field_bytes: 65536, report_bytes: 131072,
  corpus_bytes: 2097152, aggregate_bytes: 4194304, nodes: 4096, depth: 32 };
let current, fault, retainedBytes = 0;
const cases = [], raw = [], executed = [], callbacks = [];
function checked(action) { try { return action(); } catch (error) { fault ??= error; throw error; } }
function account(value) {
  retainedBytes += Buffer.byteLength(JSON.stringify(value));
  assert.ok(retainedBytes <= limits.aggregate_bytes, 'Capture aggregate bound');
}
function snapshot(value) {
  const tags = []; let nodes = 0;
  function walk(item, path, depth) {
    assert.ok(++nodes <= limits.nodes && depth <= limits.depth);
    if (item === undefined) { tags.push({ path, kind: 'undefined' }); return; }
    if (typeof item === 'number') assert.ok(Number.isFinite(item), 'Unexpected nonfinite fixture');
    if (typeof item === 'string') assert.ok(Buffer.byteLength(item) <= limits.field_bytes);
    if (!item || typeof item !== 'object') return;
    assert.ok(Array.isArray(item) || Object.getPrototypeOf(item) === Object.prototype);
    for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(item))) {
      if (!descriptor.enumerable) continue;
      assert.ok(Object.hasOwn(descriptor, 'value'), 'Unexpected getter');
      walk(descriptor.value, `${path}.${key}`, depth + 1);
    }
  }
  walk(value, '$', 0);
  const text = JSON.stringify(value); assert.ok(Buffer.byteLength(text) <= limits.report_bytes);
  return { value: JSON.parse(text), tags };
}
const NativeResponse = globalThis.Response, responses = new WeakMap();
const Response = new Proxy(NativeResponse, { get(target, key) {
  if (key !== 'json') return Reflect.get(target, key);
  return (value, init) => checked(() => {
    const response = target.json(value, init), body = JSON.stringify(value);
    assert.ok(Buffer.byteLength(body) <= limits.field_bytes);
    responses.set(response, { status: response.status, headers: Object.fromEntries(response.headers), body });
    return response;
  });
} });
function reportValue(value) {
  const out = snapshot(value);
  assert.match(out.value.timestamp, /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/);
  out.value.timestamp = '<clock>';
  for (const object of [out.value, ...out.value.rows]) {
    const key = object === out.value ? 'warmup_ms' : 'latency_ms';
    assert.ok(Number.isFinite(object[key]) && object[key] >= 0);
    object[key] = 0;
  }
  return out;
}
function progress(value) {
  assert.equal(typeof value, 'string'); assert.ok(Buffer.byteLength(value) <= limits.field_bytes);
  return value.replace(/, \d+(?:\.\d+)?ms$/, ', <clock>ms');
}
async function runRoutingTests(config, options) {
  assert.ok(current && typeof options.fetchImpl === 'function' && typeof options.write === 'function');
  assert.ok(cases.length < limits.cases);
  const settings = snapshot(config), input = snapshot(options.cases);
  const fixture = JSON.stringify(options.cases) === JSON.stringify(fixtureCases) ? fixtureBytes : JSON.stringify(options.cases);
  const row = { id: `ollama-routing-tool-${current.number}-${current.case_ids.length + 1}`, source_test: current.id,
    config: settings.value, config_tags: settings.tags, cases: input.value,
    fixture_bytes: fixture, fixture_sha256: hash(fixture), fixture_hash_supplied: options.fixtureHash !== undefined,
    supplied_fixture_hash: options.fixtureHash ?? null, requests: [], writes: [] };
  const original = { id: row.id, writes: [] }; current.case_ids.push(row.id); cases.push(row); raw.push(original);
  try {
    const result = await tool.runRoutingTests(config, { ...options,
      write: value => { checked(() => { row.writes.push(progress(value)); original.writes.push(value); }); return options.write(value); },
      fetchImpl: async (url, init) => {
        const call = checked(() => {
          assert.ok(row.requests.length < limits.requests_per_case);
          assert.equal(init.redirect, 'error'); assert.ok(init.signal instanceof AbortSignal); assert.equal(init.signal.aborted, false);
          const body = init.body ?? null; assert.ok(body === null || typeof body === 'string' && Buffer.byteLength(body) <= limits.field_bytes);
          const request = { url: String(url), method: init.method ?? 'GET', headers: Object.fromEntries(new Headers(init.headers)), body,
            node_options: { redirect: init.redirect, signal: 'nonaborted_AbortSignal' } };
          row.requests.push(request); return request;
        });
        const response = await options.fetchImpl(url, init);
        checked(() => { assert.ok(responses.has(response), 'Only finite original Response.json mocks allowed'); call.response = responses.get(response); });
        return response;
      } });
    checked(() => { original.report = snapshot(result); row.outcome = { kind: 'value', ...reportValue(result) }; });
    return result;
  } catch (error) {
    checked(() => { assert.ok(error instanceof Error); row.outcome = { kind: 'error', name: error.name, message: error.message, code: error.code ?? null }; });
    throw error;
  } finally { checked(() => { account(row); account(original); }); }
}
function test(name, callback) {
  const row = definitions[callbacks.length]; assert.equal(name, row.name); callbacks.push(callback);
}
function recordAssertion(id, execute) {
  const result = execute();
  const record = () => executed.push({ definition: current.id, assertion: id });
  if (result && typeof result.then === 'function') return result.then(value => { record(); return value; });
  record(); return result;
}
await Function('test', 'assert', 'delay', 'readConfig', 'Router', 'buildRoutingRequest', 'readRoutingFixtures', 'runRoutingTests', 'Response', '__assertion',
  `"use strict";return (async () => {${transformed}\n})()`)(test, assert,
  () => assert.fail('Timed Router callback is outside this finite tool capture'), configuration.readConfig, routing.Router,
  tool.buildRoutingRequest, tool.readRoutingFixtures, runRoutingTests, Response, recordAssertion);
for (const definition of definitions.filter(row => row.selected)) {
  current = definition; definition.case_ids = [];
  await callbacks[definition.number - 1]();
  if (fault) throw fault;
  definition.expanded_assertions = executed.filter(row => row.definition === definition.id).length;
}
assert.equal(cases.length, 6);
assert.equal(executed.length, 377);
const requests = cases.reduce((n, row) => n + row.requests.length, 0);
assert.equal(requests, 58);
const corpus = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
assert.ok(Buffer.byteLength(corpus) <= limits.corpus_bytes);
const capture = { schema_version: 1, kind: 'frozen_ollama_routing_tool_contracts', baseline_commit: manifest.baseline_commit,
  baseline_file: file, baseline_file_sha256: hash(source), capture_source_sha256: hash(readFileSync(import.meta.filename)),
  selected_definitions: [5, 6, 7, 8], static_assertions: 25, helper_static_assertions: 9, expanded_assertions: executed.length,
  cases: cases.length, requests, corpus_sha256: hash(corpus), definitions: definitions.filter(row => row.selected), helper_assertions: helperAssertions,
  assertion_executions: executed, limits, fixture_source_sha256: hash(fixtureBytes),
  adaptations: ['Only timestamp,warmup_ms,row latency_ms and final progress duration are projected after validation; raw observations remain separately retained. No timing qualification.',
    'Native accepts fixture bytes rather than cases plus optional fixtureHash. Preserve supplied/omitted hash tags; independently verify native digest of exact input bytes.',
    'Fetch redirect:error/AbortSignal are Node API properties; native owned transport does not follow redirects. All actual request headers,URL,method and body remain compared.',
    'Native typed error identity must preserve source code/message. Generic JS Error stack/prototype is outside Rust API.',
    'No live evaluator, provider, model download, configuration write or numerical benchmark is executed.'] };
mkdirSync(output, { recursive: false, mode: 0o700 });
for (const [name, bytes] of [['cases.jsonl', corpus], ['capture.json', JSON.stringify(capture, null, 2) + '\n'], ['observations.json', JSON.stringify(raw, null, 2) + '\n']]) writeFileSync(join(output, name), bytes, { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify({ cases: cases.length, requests, expanded_assertions: executed.length, corpus_sha256: hash(corpus) }));
