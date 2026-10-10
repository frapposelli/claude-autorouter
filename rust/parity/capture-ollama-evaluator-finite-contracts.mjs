// Run five unchanged frozen finite Ollama evaluator callbacks with synthetic fetches.
// No timer/stream/cancellation/classifier-cache cases or actual service calls.
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
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/ollama-evaluator-finite-${Date.now()}`));
assert.ok(process.argv.length <= 4, 'Usage: capture-ollama-evaluator-finite-contracts.mjs [reference] [fresh-output]');
await verifyBaseline(reference);
const hash = value => createHash('sha256').update(value).digest('hex');
const sourcePath = 'test/ollama-evaluator.test.mjs';
const source = readFileSync(join(reference, sourcePath), 'utf8');
const baseline = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json')));
assert.equal(hash(source), baseline.files.find(row => row.path === sourcePath).sha256);
const load = path => import(pathToFileURL(join(reference, path)).href);
const [ollama, prompts, configuration, models] = await Promise.all([
  load('src/ollama-evaluator.mjs'), load('src/prompt-state.mjs'), load('src/config.mjs'), load('src/ollama-models.mjs'),
]);
const selected = [1, 2, 3, 6, 11];
const limits = { cases: 16, requests_per_call: 2, state_calls: 24, field_bytes: 65536,
  response_bytes: 1048576, aggregate_bytes: 4194304, snapshot_nodes: 4096, snapshot_depth: 32 };
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
    edits.push({ start: statement.getStart(syntax), end: statement.end }); continue;
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
  walk(call); definitions.push(row);
}
assert.equal(definitions.length, 15);
assert.equal(definitions.filter(row => row.selected).reduce((sum, row) => sum + row.assertions.length, 0), 23);
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

const tests = [], cases = [], stateObjects = new WeakMap();
let current, captureFault, stateCount = 0;
const trackedAssert = new Proxy(assert, { get(target, key) {
  const original = target[key];
  return typeof original === 'function' ? (...args) => { current.executed_assertions += 1; return original(...args); } : original;
} });
function test(name, ...args) {
  const definition = definitions[tests.length];
  assert.equal(name, definition.name);
  tests.push({ ...definition, callback: args.at(-1), case_ids: [], state_calls: [], executed_assertions: 0 });
}
function capture(action) {
  try { return action(); } catch (error) { captureFault ??= error; throw error; }
}
function stateCall(name, original, args) {
  return capture(() => {
    assert.ok(++stateCount <= limits.state_calls);
    assert.equal(args.length, 1, 'Selected source calls use only the default limit');
    const input = snapshot(args[0]);
    const result = original(...args);
    const observed = snapshot(result);
    const row = { operation: name, input_json: JSON.stringify(args[0]), input_tags: input.tags,
      limit_argument_present: false, limit: name === 'buildState' ? 12000 : 3000,
      output_json: JSON.stringify(result), output_tags: observed.tags };
    account(row); current.state_calls.push(row); stateObjects.set(result, row);
    return result;
  });
}
const buildState = (...args) => stateCall('buildState', prompts.buildState, args);
const buildOllamaState = (...args) => stateCall('buildOllamaState', ollama.buildOllamaState, args);
function addCase(value) {
  assert.ok(cases.length < limits.cases);
  const row = { id: `baseline-ollama-evaluator-finite-${current.number}-${current.case_ids.length + 1}`,
    source_test: current.id, ...value };
  cases.push(row); current.case_ids.push(row.id); return row;
}
async function evaluateOllama(state, config, options = {}) {
  assert.deepEqual(Object.keys(options), ['fetchImpl']);
  assert.equal(typeof options.fetchImpl, 'function');
  const stateRow = stateObjects.get(state); assert.ok(stateRow && !stateRow.used);
  stateRow.used = true;
  const settings = snapshot(config);
  const row = addCase({ kind: 'evaluation', state_calls: [stateRow], config: settings.value, config_tags: settings.tags,
    options: {}, option_tags: [{ path: '$.fetchImpl', kind: 'function' }], requests: [] });
  account(settings);
  const fetchImpl = async (url, request = {}) => {
    const call = capture(() => {
      assert.ok(row.requests.length < limits.requests_per_call);
      assert.equal(new URL(url).origin, config.ollamaEndpoint);
      assert.equal(request.redirect, 'error');
      assert.ok(request.signal instanceof AbortSignal && !request.signal.aborted);
      const headers = Object.fromEntries(new Headers(request.headers));
      assert.ok(!('authorization' in headers) && !('x-api-key' in headers));
      assert.equal(typeof request.body, 'string');
      assert.ok(Buffer.byteLength(request.body) <= limits.field_bytes);
      const call = { url: String(url), method: request.method ?? 'GET', headers,
        body: JSON.parse(request.body), body_text: request.body,
        node_options: { redirect: request.redirect, signal_kind: 'AbortSignal', signal_aborted_before: request.signal.aborted,
          omitted_fields: ['method', 'headers', 'body'].filter(key => !Object.hasOwn(request, key)) } };
      account(call); row.requests.push(call); return call;
    });
    let response;
    try { response = await options.fetchImpl(url, request); }
    catch (error) { captureFault ??= error; throw error; }
    capture(() => {
      assert.ok(responseData.has(response), 'Unobserved reference Response');
      call.response = responseData.get(response);
      call.node_options.signal_aborted_after = request.signal.aborted;
      assert.equal(request.signal.aborted, false);
    });
    return response;
  };
  try {
    const result = await ollama.evaluateOllama(state, config, { ...options, fetchImpl });
    row.result = { ok: true, ...snapshot(result) }; account(row.result);
    if (captureFault) throw captureFault;
    return result;
  } catch (error) {
    if (captureFault) throw captureFault;
    // Stack locations are not stable observable API fields or source assertions.
    // Preserve every other own field plus inherited Error.name and explicit
    // undefined tags for the optional status/code consumed by source predicates.
    const names = Object.getOwnPropertyNames(error).filter(key => key !== 'stack');
    assert.ok(names.every(key => ['message', 'code', 'classifierStatus'].includes(key)));
    const observed = snapshot({ name: error.name, message: error.message, code: error.code, classifierStatus: error.classifierStatus });
    row.result = { ok: false, ...observed, own_properties: names }; account(row.result);
    throw error; // Preserve the actual original error object for its predicate.
  }
}
const values = { test, assert: trackedAssert, Response, readConfig: configuration.readConfig,
  buildState, buildOllamaState, evaluateOllama, OLLAMA_QUESTIONS: ollama.OLLAMA_QUESTIONS,
  DEFAULT_OLLAMA_MODEL: models.DEFAULT_OLLAMA_MODEL };
const temporary = mkdtempSync(join(tmpdir(), 'autorouter-ollama-finite-contracts-'));
const key = `autorouter-ollama-finite-contracts:${temporary}`;
try {
  globalThis[Symbol.for(key)] = values;
  writeFileSync(join(temporary, 'capture.mjs'), `const { ${Object.keys(values).join(', ')} } = globalThis[Symbol.for(${JSON.stringify(key)})];\n${body}`);
  await import(pathToFileURL(join(temporary, 'capture.mjs')).href);
  assert.equal(tests.length, 15);
  for (const definition of tests.filter(row => row.selected)) {
    current = definition;
    await definition.callback();
    const pure = definition.state_calls.filter(row => !row.used);
    if (definition.number === 1) {
      assert.equal(pure.length, 12);
      for (let i = 0; i < 12; i += 4) {
        const calls = pure.slice(i, i + 4);
        assert.deepEqual(calls.map(row => row.operation), ['buildOllamaState', 'buildOllamaState', 'buildOllamaState', 'buildState']);
        addCase({ kind: 'state', state_calls: calls, requests: [] });
      }
    } else if ([2, 11].includes(definition.number)) {
      assert.equal(pure.length, 1); addCase({ kind: 'state', state_calls: pure, requests: [] });
    } else assert.equal(pure.length, 0);
  }
} finally { delete globalThis[Symbol.for(key)]; rmSync(temporary, { recursive: true, force: true }); }
if (captureFault) throw captureFault;
for (const row of cases) for (const state of row.state_calls) delete state.used;
assert.equal(cases.length, 15);
assert.equal(stateCount, 24);
assert.equal(cases.reduce((sum, row) => sum + row.requests.length, 0), 18);
assert.equal(tests.reduce((sum, row) => sum + row.executed_assertions, 0), 70);
assert.ok(cases.every(row => row.requests.every(call => call.response)));
const serialized = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
assert.ok(Buffer.byteLength(serialized) <= limits.aggregate_bytes);
const report = { schema_version: 1, baseline_commit: baseline.baseline_commit, baseline_test: sourcePath,
  baseline_sha256: hash(source), generator_sha256: hash(readFileSync(import.meta.filename)),
  node_version: process.version, typescript_version: ts.version, cases_sha256: hash(serialized),
  selected_definitions: selected, cases: cases.length, state_calls: stateCount, requests: 18,
  static_assertions: 23, executed_assertions: 70,
  tests: tests.map(({ callback, state_calls, ...row }) => row), limits,
  excluded_fields: ['Error.stack only; no state/request/result/status/code/message/timing fields are masked'],
  boundaries: [
    'Only static imports are removed. All five selected original callbacks, loops, data constructors, helper functions and assertions execute unchanged in the same realm.',
    'Response observer forwards finite built-in constructors and returns the identical Response; no clone/tee/read or stream ownership changes.',
    'All 24 state-function invocations preserve input/output JSON bytes, including twelve invocations across three background-equivalence loops. Ten evaluator operations consume their actual captured state objects.',
    'Complete raw and parsed HTTP request bodies, headers, status and finite response text are retained. Error predicates receive the same original error; all diagnostic fields and own-property presence except stack are recorded.',
    'Own undefined and injected function tags remain explicit. Native typed errors/Option omission are reviewed API adaptations, not JS Error/AbortSignal identity claims.',
    'No classifier cache/routing, deadline/cancellation, response stream, provider/model/download, platform or performance qualification is performed.',
  ] };
mkdirSync(output, { mode: 0o700 });
writeFileSync(join(output, 'cases.jsonl'), serialized, { mode: 0o600 });
writeFileSync(join(output, 'capture.json'), JSON.stringify(report, null, 2) + '\n', { mode: 0o600 });
console.log(JSON.stringify({ definitions: selected.length, cases: cases.length, state_calls: stateCount,
  requests: report.requests, assertions: report.executed_assertions, cases_sha256: report.cases_sha256 }));
