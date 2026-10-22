// Original finite setup callbacks; observe built-in Responses without reading,
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
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/local-finite-${Date.now()}`));
assert.ok(process.argv.length <= 4, 'Usage: capture-local-finite-contracts.mjs [reference] [fresh-output]');
await verifyBaseline(reference);
const hash = value => createHash('sha256').update(value).digest('hex');
const sourcePath = 'test/ollama-setup.test.mjs';
const source = readFileSync(join(reference, sourcePath), 'utf8');
const baseline = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json')));
assert.equal(hash(source), baseline.files.find(row => row.path === sourcePath).sha256);
const load = path => import(pathToFileURL(join(reference, path)).href);
const [setup, models, evaluator] = await Promise.all([
  load('src/ollama-setup.mjs'), load('src/ollama-models.mjs'), load('src/ollama-evaluator.mjs'),
]);
const selected = [1, 2, 3, 5, 8, 11];
const limits = { cases: 32, requests_per_call: 16, field_bytes: 65536, response_bytes: 1048576,
  aggregate_bytes: 8388608, progress_per_call: 16, snapshot_nodes: 4096, snapshot_depth: 32 };
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
    statement_sha256: hash(statement.getText(syntax)), call_sha256: hash(call.getText(syntax)), assertions: [] };
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

const tests = [], cases = [], fetchIds = new WeakMap();
let current, nextFetch = 0;
const trackedAssert = new Proxy(assert, { get(target, key) {
  const original = target[key];
  return typeof original === 'function' ? (...args) => { current.executed_assertions += 1; return original(...args); } : original;
} });
function test(name, ...args) {
  const definition = definitions[tests.length];
  assert.equal(name, definition.name);
  tests.push({ ...definition, callback: args.at(-1), case_ids: [], executed_assertions: 0 });
}
function capturedOperation(kind, original) {
  return async (config, options = {}) => {
    assert.equal(typeof options.fetchImpl, 'function', 'Only original synthetic fetch callbacks are allowed');
    assert.ok(!Object.hasOwn(options, 'signal'), 'Cancellation is outside this finite batch');
    assert.ok(cases.length < limits.cases);
    const settings = snapshot(config);
    const plainOptions = {}, optionTags = [];
    for (const [key, value] of Object.entries(options)) {
      if (['fetchImpl', 'write'].includes(key)) { assert.equal(typeof value, 'function'); optionTags.push({ path: `$.${key}`, kind: 'function' }); }
      else plainOptions[key] = value;
    }
    const capturedOptions = snapshot(plainOptions);
    if (!fetchIds.has(options.fetchImpl)) fetchIds.set(options.fetchImpl, ++nextFetch);
    const row = { id: `baseline-local-finite-${current.number}-${current.case_ids.length + 1}`,
      source_tests: [current.id], kind, fetch_id: fetchIds.get(options.fetchImpl),
      config: settings.value, config_tags: settings.tags, options: capturedOptions.value,
      option_tags: [...optionTags, ...capturedOptions.tags], requests: [], progress: [] };
    account(row);
    cases.push(row); current.case_ids.push(row.id);
    const fetchImpl = async (url, request) => {
      assert.ok(row.requests.length < limits.requests_per_call);
      assert.equal(new URL(url).origin, config.ollamaEndpoint);
      assert.equal(request.redirect, 'error');
      const headers = Object.fromEntries(new Headers(request.headers));
      assert.ok(!('authorization' in headers) && !('x-api-key' in headers));
      assert.ok(Buffer.byteLength(request.body ?? '') <= limits.field_bytes);
      const call = { url: String(url), method: request.method, headers,
        body: request.body === undefined ? null : JSON.parse(request.body) };
      account(call);
      row.requests.push(call);
      const response = await options.fetchImpl(url, request);
      assert.ok(responseData.has(response), 'Unobserved or unsupported reference Response');
      call.response = responseData.get(response);
      return response;
    };
    const write = line => {
      assert.ok(row.progress.length < limits.progress_per_call);
      assert.equal(typeof line, 'string'); assert.ok(Buffer.byteLength(line) <= limits.field_bytes);
      account(line);
      row.progress.push(line); options.write?.(line);
    };
    try {
      const result = await original(config, { ...options, fetchImpl, ...(kind === 'setup' ? { write } : {}) });
      const observed = snapshot(result); row.node_expected = { ok: true, result: observed.value };
      account(row.node_expected);
      row.result_tags = observed.tags; return result;
    } catch (error) {
      row.node_expected = { ok: false, error: { code: error.code, message: error.message, has_cause: error.cause !== undefined } };
      account(row.node_expected);
      row.error_name = error.name;
      assert.ok(!JSON.stringify(row.node_expected).includes('PRIVATE_'));
      throw error;
    }
  };
}
const values = { test, assert: trackedAssert, Response,
  inspectOllama: capturedOperation('inspect', setup.inspectOllama), setupOllama: capturedOperation('setup', setup.setupOllama),
  DEFAULT_OLLAMA_MODEL: models.DEFAULT_OLLAMA_MODEL,
  validateOllamaEndpoint: models.validateOllamaEndpoint, validateOllamaModel: models.validateOllamaModel,
  OLLAMA_QUESTIONS: evaluator.OLLAMA_QUESTIONS };
const temporary = mkdtempSync(join(tmpdir(), 'autorouter-local-finite-'));
const key = `autorouter-local-finite:${temporary}`;
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
assert.equal(cases.length, 24); assert.equal(nextFetch, 23);
assert.equal(cases.filter(row => row.source_tests[0].endsWith('#5')).length, 2);
assert.equal(cases.reduce((sum, row) => sum + row.requests.length, 0), 63);
assert.ok(cases.every(row => row.requests.every(call => call.response)), 'Capture failure cannot masquerade as a safe setup error');
const serialized = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
assert.ok(Buffer.byteLength(serialized) <= limits.aggregate_bytes);
const report = { schema_version: 1, baseline_commit: baseline.baseline_commit, baseline_test: sourcePath,
  baseline_sha256: hash(source), generator_sha256: hash(readFileSync(import.meta.filename)), cases_sha256: hash(serialized),
  typescript_version: ts.version, selected_definitions: selected, cases: cases.length, fetch_groups: nextFetch,
  requests: cases.reduce((sum, row) => sum + row.requests.length, 0),
  executed_assertions: tests.reduce((sum, row) => sum + row.executed_assertions, 0),
  tests: tests.map(({ callback, ...row }) => row), limits,
  boundaries: ['Only static imports are removed; selected original test callbacks, loops and assertion expressions execute unchanged in the same realm.',
    'Lexical Response observer forwards to native constructors; no clone/tee/body read, changed response identity or staged classification answer.',
    'Function option tags preserve injected fetch/write boundaries; native replay compares their request/progress effects, not JS function identity.',
    'Own-undefined JSON input fields are explicitly tagged; original JSON.stringify bytes omit them exactly. No timing/report field is excluded.',
    'Safe error tuple compares code/message/cause absence; Error name is retained separately, stack/prototype identity is not native API parity.',
    'No live I/O, provider/model/service invocation, cancellation or pull-stream claim in this batch. Unselected definitions are listed with selected:false and zero executed assertions.'] };
mkdirSync(output);
writeFileSync(join(output, 'cases.jsonl'), serialized);
writeFileSync(join(output, 'capture.json'), JSON.stringify(report, null, 2) + '\n');
console.log(JSON.stringify({ definitions: selected.length, cases: cases.length, fetch_groups: nextFetch,
  requests: report.requests, assertions: report.executed_assertions, cases_sha256: report.cases_sha256 }));
