// Original finite diagnostic callbacks; observe built-in Responses without reading,
// cloning or teeing their bodies. No network, model or evaluator is invoked.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const ts = createRequire(import.meta.url)('typescript');
const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/local-diagnostic-contracts-${Date.now()}`));
assert.ok(process.argv.length <= 4, 'Usage: capture-local-diagnostic-contracts.mjs [reference] [fresh-output]');
await verifyBaseline(reference);
const hash = value => createHash('sha256').update(value).digest('hex');
const sourcePath = 'test/local-diagnostic.test.mjs';
const source = readFileSync(join(reference, sourcePath), 'utf8');
const baseline = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json')));
assert.equal(hash(source), baseline.files.find(row => row.path === sourcePath).sha256);
const load = path => import(pathToFileURL(join(reference, path)).href);
const [diagnostic, configuration] = await Promise.all([
  load('src/local-diagnostic.mjs'), load('src/config.mjs'),
]);
const selected = [1, 2, 3, 4, 5, 9, 10];
const limits = { cases: 16, requests_per_call: 64, field_bytes: 65536, response_bytes: 1048576,
  aggregate_bytes: 8388608, progress_per_call: 64, snapshot_nodes: 4096, snapshot_depth: 32 };
let retainedBytes = 0;
function account(value) {
  const bytes = Buffer.byteLength(JSON.stringify(value));
  assert.ok(bytes <= limits.aggregate_bytes - retainedBytes, 'Aggregate capture bound');
  retainedBytes += bytes;
}
const syntax = ts.createSourceFile(sourcePath, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
assert.equal(syntax.parseDiagnostics.length, 0);
const definitions = [], edits = [];
for (const statement of syntax.statements) {
  if (ts.isImportDeclaration(statement)) {
    edits.push({ start: statement.getStart(syntax), end: statement.end });
    continue;
  }
  if (!ts.isExpressionStatement(statement) || !ts.isCallExpression(statement.expression)
    || statement.expression.expression.getText(syntax) !== 'test') continue;
  const call = statement.expression;
  const number = definitions.length + 1;
  const row = { id: `${sourcePath}#${number}`, number, name: call.arguments[0].text,
    selected: selected.includes(number), line: syntax.getLineAndCharacterOfPosition(statement.getStart(syntax)).line + 1,
    statement: statement.getText(syntax), statement_sha256: hash(statement.getText(syntax)), call_sha256: hash(call.getText(syntax)), assertions: [] };
  const walk = node => {
    if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)
      && node.expression.expression.getText(syntax) === 'assert') {
      row.assertions.push({ line: syntax.getLineAndCharacterOfPosition(node.getStart(syntax)).line + 1,
        expression: node.getText(syntax), sha256: hash(node.getText(syntax)) });
    }
    ts.forEachChild(node, walk);
  };
  walk(call);
  definitions.push(row);
}
assert.equal(definitions.length, 14);
// None of the seven selected definitions asserts elapsed durations. Timed
// definitions7/8 remain outside this capture; formatter output is never masked.
assert.ok(definitions.filter(row => row.selected).every(row =>
  row.assertions.every(item => !item.expression.includes('latency_ms'))));
const fixtureNode = syntax.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === 'fixture');
assert.ok(fixtureNode);
const fixtureAssertions = [];
function inspectHelper(node) {
  if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)
      && node.expression.expression.getText(syntax) === 'assert') {
    fixtureAssertions.push({ line: syntax.getLineAndCharacterOfPosition(node.getStart(syntax)).line + 1,
      expression: node.getText(syntax), sha256: hash(node.getText(syntax)) });
  }
  ts.forEachChild(node, inspectHelper);
}
inspectHelper(fixtureNode);
let body = source;
for (const edit of edits.sort((a, b) => b.start - a.start)) body = body.slice(0, edit.start) + body.slice(edit.end);
assert.doesNotMatch(body, /^\s*import\b/m);

function snapshot(value) {
  const tags = [], ancestors = new Set();
  let nodes = 0, budget = 0;
  const visit = (item, path, depth) => {
    assert.ok(++nodes <= limits.snapshot_nodes && depth <= limits.snapshot_depth, 'Snapshot bound');
    budget += 16 + (typeof item === 'string' ? Buffer.byteLength(item) : 0);
    assert.ok(budget <= limits.response_bytes, 'Snapshot byte bound');
    if (item === undefined || (typeof item === 'number' && !Number.isFinite(item))) {
      tags.push({ path, kind: item === undefined ? 'undefined' : Number.isNaN(item) ? 'nan' : item > 0 ? 'positive_infinity' : 'negative_infinity' });
      return item === undefined ? undefined : null;
    }
    if (typeof item === 'string') assert.ok(Buffer.byteLength(item) <= limits.field_bytes, 'String bound');
    if (item === null || ['string', 'number', 'boolean'].includes(typeof item)) return item;
    assert.equal(typeof item, 'object', 'Only plain captured data is supported');
    assert.ok(Array.isArray(item) || Object.getPrototypeOf(item) === Object.prototype);
    assert.ok(!ancestors.has(item), 'Cyclic capture'); ancestors.add(item);
    const result = Array.isArray(item) ? [] : {};
    for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(item))) {
      if (!descriptor.enumerable) continue;
      budget += Buffer.byteLength(key); assert.ok(budget <= limits.response_bytes, 'Snapshot key bound');
      assert.ok(Object.hasOwn(descriptor, 'value'), 'Getter capture is forbidden');
      const child = visit(descriptor.value, `${path}.${key}`, depth + 1);
      if (Array.isArray(item)) result.push(child === undefined ? null : child);
      else if (child !== undefined) result[key] = child;
    }
    if (Array.isArray(item)) assert.equal(result.length, item.length, 'Sparse array capture');
    ancestors.delete(item); return result;
  };
  return { value: visit(value, '$', 0), tags };
}
const nativeResponse = globalThis.Response;
const responseData = new WeakMap();
function observeResponse(response, bytes, jsonTags = []) {
  assert.ok(Buffer.byteLength(bytes) <= limits.response_bytes, 'Response bound');
  const recorded = { status: response.status, headers: Object.fromEntries(response.headers),
    body: { text: bytes }, json_input_tags: jsonTags };
  account(recorded); responseData.set(response, recorded);
  return response;
}
// Forward to the original built-in and return the same Response instance. No
// body read/cancel/clone changes the reference's stream or ownership behavior.
const Response = new Proxy(nativeResponse, {
  construct(target, args) {
    assert.ok(args[0] == null || typeof args[0] === 'string', 'Only finite string/null Response bodies in this batch');
    assert.ok(Buffer.byteLength(args[0] ?? '') <= limits.response_bytes);
    return observeResponse(Reflect.construct(target, args, target), args[0] ?? '');
  },
  get(target, key) {
    if (key !== 'json') return Reflect.get(target, key);
    return (value, init) => {
      const captured = snapshot(value);
      const bytes = JSON.stringify(value);
      assert.ok(Buffer.byteLength(bytes) <= limits.response_bytes);
      return observeResponse(target.json(value, init), bytes, captured.tags);
    };
  },
});

const tests = [], cases = [], reports = new WeakMap();
let current, captureFault;
const trackedAssert = new Proxy(assert, { get(target, key) {
  const original = target[key];
  return typeof original === 'function' ? (...args) => { current.executed_assertions += 1; return original(...args); } : original;
} });
function test(name, ...args) {
  const definition = definitions[tests.length];
  assert.equal(name, definition.name);
  tests.push({ ...definition, callback: args.at(-1), case_ids: [], executed_assertions: 0 });
}
// Recorder failures must survive the product's deliberate safe-error/progress
// handling. Otherwise an observer failure could look like a valid failed gate.
function capture(action) {
  try { return action(); } catch (error) { captureFault ??= error; throw error; }
}
async function runLocalDiagnostic(config, options = {}) {
  assert.equal(typeof options.fetchImpl, 'function');
  assert.ok(!Object.hasOwn(options, 'signal'), 'Cancellation is outside this finite batch');
  assert.ok(cases.length < limits.cases);
  const settings = snapshot(config);
  const optionTags = Object.entries(options).map(([key, value]) => {
    assert.ok(['fetchImpl', 'onProgress'].includes(key)); assert.equal(typeof value, 'function');
    return { path: `$.${key}`, kind: 'function' };
  });
  const row = { id: `baseline-local-diagnostic-${current.number}-${current.case_ids.length + 1}`,
    source_tests: [current.id], config: settings.value, config_tags: settings.tags,
    options: {}, option_tags: optionTags, requests: [], progress: [], source_formatter_calls: 0 };
  account(row); cases.push(row); current.case_ids.push(row.id);
  const fetchImpl = async (url, request = {}) => {
    let call;
    capture(() => {
      assert.ok(row.requests.length < limits.requests_per_call);
      assert.equal(new URL(url).origin, config.ollamaEndpoint);
      assert.equal(request.redirect, 'error');
      assert.ok(request.signal instanceof AbortSignal);
      assert.equal(request.signal.aborted, false);
      const headers = Object.fromEntries(new Headers(request.headers));
      assert.ok(!('authorization' in headers) && !('x-api-key' in headers));
      assert.ok(request.body === undefined || typeof request.body === 'string');
      assert.ok(Buffer.byteLength(request.body ?? '') <= limits.field_bytes);
      call = { url: String(url), method: request.method ?? 'GET', headers,
        body: request.body === undefined ? null : JSON.parse(request.body), body_text: request.body ?? '',
        node_options: { redirect: request.redirect, signal_kind: 'AbortSignal', signal_aborted_before: request.signal.aborted,
          omitted_fields: ['method', 'headers', 'body'].filter(key => !Object.hasOwn(request, key)) } };
      account(call); row.requests.push(call);
    });
    let response;
    try { response = await options.fetchImpl(url, request); }
    catch (error) { captureFault ??= error; throw error; }
    capture(() => {
      assert.ok(responseData.has(response), 'Unobserved or unsupported reference Response');
      call.response = responseData.get(response);
      call.node_options.signal_aborted_after = request.signal.aborted;
      assert.equal(request.signal.aborted, false);
    });
    return response;
  };
  const onProgress = event => capture(() => {
    assert.ok(row.progress.length < limits.progress_per_call);
    const observed = snapshot(event); account(observed); row.progress.push(observed);
    return options.onProgress?.(event);
  });
  const report = await diagnostic.runLocalDiagnostic(config, { ...options, fetchImpl, onProgress });
  if (captureFault) throw captureFault;
  const observed = snapshot(report); account(observed);
  assert.ok(!JSON.stringify(observed).includes('PRIVATE_'));
  row.report = observed;
  // Capture exact complete formatting for every original report. Three source
  // definitions additionally call the wrapper below in their original asserts.
  row.formatted = diagnostic.formatLocalDiagnostic(report); account(row.formatted);
  reports.set(report, row);
  return report;
}
function formatLocalDiagnostic(report) {
  const row = reports.get(report); assert.ok(row);
  row.source_formatter_calls += 1;
  const lines = diagnostic.formatLocalDiagnostic(report);
  assert.deepEqual(lines, row.formatted); return lines;
}

const values = { test, assert: trackedAssert, Response,
  readConfig: configuration.readConfig, runLocalDiagnostic, formatLocalDiagnostic,
  LOCAL_DIAGNOSTIC_CASES: diagnostic.LOCAL_DIAGNOSTIC_CASES };
const temporary = mkdtempSync(join(tmpdir(), 'autorouter-local-diagnostic-contracts-'));
const key = `autorouter-local-diagnostic-contracts:${temporary}`;
try {
  globalThis[Symbol.for(key)] = values;
  writeFileSync(join(temporary, 'capture.mjs'), `const { ${Object.keys(values).join(', ')} } = globalThis[Symbol.for(${JSON.stringify(key)})];\n${body}`);
  await import(pathToFileURL(join(temporary, 'capture.mjs')).href);
  assert.equal(tests.length, 14);
  for (const definition of tests.filter(row => row.selected)) {
    current = definition;
    await definition.callback();
  }
} finally { delete globalThis[Symbol.for(key)]; rmSync(temporary, { recursive: true, force: true }); }
if (captureFault) throw captureFault;
assert.equal(cases.length, 10);
assert.equal(cases.reduce((sum, row) => sum + row.requests.length, 0), 102);
assert.equal(cases.reduce((sum, row) => sum + row.progress.length, 0), 58);
assert.equal(cases.reduce((sum, row) => sum + row.source_formatter_calls, 0), 3);
assert.ok(cases.every(row => row.requests.every(call => call.response)));

// Raw timings and formatter strings stay in the corpus. The secondary digest
// documents repeatability of non-timing observations only, using exact paths.
const excludedTimings = [];
function stableProjection(row) {
  const copy = structuredClone(row);
  const replace = (object, path) => {
    assert.ok(Object.hasOwn(object, 'latency_ms'));
    const value = object.latency_ms;
    assert.ok(typeof value === 'number' && Number.isFinite(value) && value >= 0);
    excludedTimings.push({ case: row.id, path: `${path}.latency_ms`, observed_ms: value });
    object.latency_ms = 0;
  };
  const report = copy.report.value;
  if (report.startup !== null) replace(report.startup, '$.report.value.startup');
  report.rows.forEach((item, index) => replace(item, `$.report.value.rows[${index}]`));
  copy.progress.forEach((item, index) => {
    if (['startup_complete', 'case_complete'].includes(item.value.event)) replace(item.value, `$.progress[${index}].value`);
  });
  copy.formatted = diagnostic.formatLocalDiagnostic(report);
  return copy;
}
const stable = cases.map(stableProjection);
const serialized = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
assert.ok(Buffer.byteLength(serialized) <= limits.aggregate_bytes);
const report = { schema_version: 1, baseline_commit: baseline.baseline_commit, baseline_test: sourcePath,
  baseline_sha256: hash(source), generator_sha256: hash(readFileSync(import.meta.filename)),
  cases_sha256: hash(serialized), stable_non_timing_cases_sha256: hash(JSON.stringify(stable)),
  typescript_version: ts.version, selected_definitions: selected, cases: cases.length,
  requests: cases.reduce((sum, row) => sum + row.requests.length, 0),
  progress_events: cases.reduce((sum, row) => sum + row.progress.length, 0),
  source_formatter_calls: cases.reduce((sum, row) => sum + row.source_formatter_calls, 0),
  executed_assertions: tests.reduce((sum, row) => sum + row.executed_assertions, 0),
  tests: tests.map(({ callback, ...row }) => row),
  fixture_helper: { line: syntax.getLineAndCharacterOfPosition(fixtureNode.getStart(syntax)).line + 1,
    sha256: hash(fixtureNode.getText(syntax)), assertions: fixtureAssertions },
  limits, excluded_timings: excludedTimings,
  boundaries: ['Only static imports are removed. Selected original callbacks, fixture helper, loops and assertions execute unchanged in the same realm.',
    'Lexical Response observer forwards finite built-in constructors and returns identical responses; no clone, tee or body consumption.',
    'All reports, progress events, request parsed bodies and exact body text, responses, raw timings and complete formatted lines are retained.',
    'Only report.startup.latency_ms, report.rows[].latency_ms and matching startup_complete/case_complete event fields differ across execution clocks; selected source assertions never test them. Each must be finite and nonnegative.',
    'Stable digest sets exactly those named fields to zero and regenerates formatting from that projected report. Native formatter must instead match raw captured strings on raw captured reports, without exclusions.',
    'Own-undefined properties are tagged explicitly; native Option omission is reviewed, not converted to null. Injected functions, AbortSignal identity and fetch method omission are source API observations, not Rust object identity claims.',
    'No actual service/provider/model/download, response cancellation, elapsed-time threshold, other-platform or performance qualification is performed.'] };
mkdirSync(output);
writeFileSync(join(output, 'cases.jsonl'), serialized);
writeFileSync(join(output, 'capture.json'), JSON.stringify(report, null, 2) + '\n');
console.log(JSON.stringify({ definitions: selected.length, cases: cases.length, requests: report.requests,
  progress_events: report.progress_events, assertions: report.executed_assertions,
  cases_sha256: report.cases_sha256, stable_non_timing_cases_sha256: report.stable_non_timing_cases_sha256 }));
