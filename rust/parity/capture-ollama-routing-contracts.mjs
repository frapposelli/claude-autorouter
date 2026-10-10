// Execute three unchanged frozen Claude-shaped Ollama routing callbacks with finite synthetic fetches.
// Keep each instance and retry/cache schedule; no real services or timed/stream cases.
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
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/ollama-routing-${Date.now()}`));
assert.ok(process.argv.length <= 4, 'Usage: capture-ollama-routing-contracts.mjs [reference] [fresh-output]');
await verifyBaseline(reference);
const hash = value => createHash('sha256').update(value).digest('hex');
const sourcePath = 'test/ollama-routing.test.mjs';
const source = readFileSync(join(reference, sourcePath), 'utf8');
const baseline = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json')));
assert.equal(hash(source), baseline.files.find(row => row.path === sourcePath).sha256);
const load = path => import(pathToFileURL(join(reference, path)).href);
const [ollama, prompts, configuration, models, routing, integration] = await Promise.all([
  load('src/ollama-evaluator.mjs'), load('src/prompt-state.mjs'), load('src/config.mjs'), load('src/ollama-models.mjs'), load('src/router.mjs'), load('scripts/test-ollama-routing.mjs'),
]);
const selected = [1, 2, 4];
const limits = { cases: 26, steps_per_router: 3, requests_per_step: 2, requests: 58,
  field_bytes: 65536, response_bytes: 65536, response_snapshot_bytes: 131072,
  aggregate_bytes: 16777216, corpus_bytes: 8388608, case_bytes: 262144,
  snapshot_nodes: 4096, snapshot_depth: 32 };
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
assert.equal(definitions.length, 8);
assert.equal(definitions.filter(row => row.selected).reduce((sum, row) => sum + row.assertions.length, 0), 12);
const helper = syntax.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === 'localFixture');
assert.ok(helper);
const helperAssertions = [];
function helperWalk(node) {
  if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)
    && node.expression.expression.getText(syntax) === 'assert') helperAssertions.push({
    line: syntax.getLineAndCharacterOfPosition(node.getStart(syntax)).line + 1,
    expression: node.getText(syntax), sha256: hash(node.getText(syntax)) });
  ts.forEachChild(node, helperWalk);
}
helperWalk(helper); assert.equal(helperAssertions.length, 9);
let body = source;
for (const edit of edits.sort((a, b) => b.start - a.start)) body = body.slice(0, edit.start) + body.slice(edit.end);
assert.doesNotMatch(body, /^\s*import\b/m);

function snapshot(value, response = false) {
  const tags = [], ancestors = new Set();
  let nodes = 0, budget = 0;
  const visit = (item, path, depth) => {
    assert.ok(++nodes <= limits.snapshot_nodes && depth <= limits.snapshot_depth, 'Snapshot bound');
    budget += 16 + (typeof item === 'string' ? Buffer.byteLength(item) : 0);
    assert.ok(budget <= (response ? limits.response_snapshot_bytes : limits.response_bytes), 'Snapshot byte bound');
    if (item === undefined || (typeof item === 'number' && !Number.isFinite(item))) {
      tags.push({ path, kind: item === undefined ? 'undefined' : Number.isNaN(item) ? 'nan' : item > 0 ? 'positive_infinity' : 'negative_infinity' });
      return item === undefined ? undefined : null;
    }
    if (typeof item === 'string') assert.ok(Buffer.byteLength(item) <= (response ? limits.response_bytes : limits.field_bytes), 'String bound');
    if (item === null || ['string', 'number', 'boolean'].includes(typeof item)) return item;
    assert.equal(typeof item, 'object', 'Only plain captured data is supported');
    assert.ok(Array.isArray(item) || Object.getPrototypeOf(item) === Object.prototype);
    assert.ok(!ancestors.has(item), 'Cyclic capture'); ancestors.add(item);
    const result = Array.isArray(item) ? [] : {};
    for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(item))) {
      if (!descriptor.enumerable) continue;
      budget += Buffer.byteLength(key); assert.ok(budget <= (response ? limits.response_snapshot_bytes : limits.response_bytes), 'Snapshot key bound');
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
      const captured = snapshot(value, true);
      const bytes = JSON.stringify(value);
      assert.ok(Buffer.byteLength(bytes) <= limits.response_bytes);
      return observeResponse(target.json(value, init), bytes, captured.tags);
    };
  },
});

const tests = [], cases = [];
let current, captureFault, requestCount = 0;
let activeFetch = false;
const trackedAssert = new Proxy(assert, { get(target, key) {
  const original = target[key];
  return typeof original === 'function' ? (...args) => { current.executed_assertions += 1; current[activeFetch ? 'helper_assertions' : 'direct_assertions'] += 1; return original(...args); } : original;
} });
function test(name, ...args) {
  const definition = definitions[tests.length];
  assert.equal(name, definition.name);
  tests.push({ ...definition, callback: args.at(-1), case_ids: [], executed_assertions: 0, helper_assertions: 0, direct_assertions: 0 });
}
function capture(action) {
  try { return action(); } catch (error) { captureFault ??= error; throw error; }
}
function addCase(kind, config) {
  return capture(() => {
    assert.ok(cases.length < limits.cases);
    const settings = snapshot(config);
    const row = { id: `baseline-ollama-routing-${current.number}-${current.case_ids.length + 1}`,
      source_test: current.id, kind, config: settings.value, config_tags: settings.tags, steps: [] };
    account(settings); cases.push(row); current.case_ids.push(row.id); return row;
  });
}
function outcome(value, route = false) {
  const observed = snapshot(value);
  if (route) {
    for (const field of ['latency_ms', 'evaluation_latency_ms']) {
      assert.ok(Number.isFinite(observed.value[field]) && observed.value[field] >= 0);
      delete observed.value[field];
    }
  }
  account(observed); return observed;
}
function fetchObserver(config, active, original) {
  assert.equal(typeof original, 'function', 'An explicit original synthetic fetch is mandatory');
  return async (url, request = {}) => {
    const step = active();
    const call = capture(() => {
      assert.ok(step && step.requests.length < limits.requests_per_step);
      assert.ok(++requestCount <= limits.requests);
      assert.equal(new URL(url).origin, config.ollamaEndpoint);
      assert.equal(request.redirect, 'error');
      assert.ok(request.signal instanceof AbortSignal && !request.signal.aborted);
      assert.equal(typeof request.body, 'string');
      assert.ok(Buffer.byteLength(request.body) <= limits.field_bytes);
      const headers = Object.fromEntries(new Headers(request.headers));
      assert.ok(!('authorization' in headers) && !('x-api-key' in headers));
      const call = { url: String(url), method: request.method ?? 'GET', headers,
        body: JSON.parse(request.body), body_text: request.body,
        node_options: { redirect: request.redirect, signal_kind: 'AbortSignal', signal_aborted_before: request.signal.aborted,
          omitted_fields: ['method', 'headers', 'body'].filter(key => !Object.hasOwn(request, key)) } };
      account(call); step.requests.push(call); return call;
    });
    let response;
    assert.equal(activeFetch, false);
    activeFetch = true;
    try { response = await original(url, request); }
    catch (error) { captureFault ??= error; throw error; }
    finally { activeFetch = false; }
    capture(() => {
      assert.ok(responseData.has(response), 'Unobserved reference Response');
      call.response = responseData.get(response);
      call.node_options.signal_aborted_after = request.signal.aborted;
      assert.equal(request.signal.aborted, false);
    });
    return response;
  };
}
class Router extends routing.Router {
  constructor(config, options = {}) {
    const row = addCase('router', config), trace = { active: undefined };
    assert.deepEqual(Object.keys(options), ['fetchImpl']);
    super(config, { ...options, fetchImpl: fetchObserver(config, () => trace.active, options.fetchImpl) });
    this.captureRow = row; this.captureTrace = trace;
  }
  async captured(kind, body, options, run) {
    const row = this.captureRow, trace = this.captureTrace;
    const step = capture(() => {
      assert.ok(!trace.active && row.steps.length < limits.steps_per_router);
      const observed = snapshot(body);
      const opts = snapshot(options);
      const step = { operation: kind, body_json: JSON.stringify(body), body_tags: observed.tags, options: opts.value, option_tags: opts.tags, requests: [] };
      assert.ok(Buffer.byteLength(step.body_json) <= limits.field_bytes);
      account(step); row.steps.push(step); trace.active = step; return step;
    });
    try {
      const result = await run();
      capture(() => {
        step.result = outcome(result, kind === 'route');
        assert.equal(JSON.stringify(body), step.body_json, 'The original Router must not mutate this request');
      });
      if (captureFault) throw captureFault;
      return result;
    } finally { trace.active = undefined; }
  }
  async route(body, options) {
    assert.ok(options && typeof options === 'object');
    return this.captured('route', body, options, () => super.route(body, options));
  }
}
const values = { test, assert: trackedAssert, Response, Router, readConfig: configuration.readConfig,
  buildRoutingRequest: integration.buildRoutingRequest, readRoutingFixtures: integration.readRoutingFixtures,
  runRoutingTests: () => assert.fail('Live harness is outside this finite routing capture') };
const temporary = mkdtempSync(join(tmpdir(), 'autorouter-ollama-routing-contracts-'));
const key = `autorouter-ollama-routing-contracts:${temporary}`;
try {
  globalThis[Symbol.for(key)] = values;
  writeFileSync(join(temporary, 'capture.mjs'), `const { ${Object.keys(values).join(', ')} } = globalThis[Symbol.for(${JSON.stringify(key)})];\n${body}`);
  await import(pathToFileURL(join(temporary, 'capture.mjs')).href);
  assert.equal(tests.length, 8);
  for (const definition of tests.filter(row => row.selected)) {
    current = definition;
    await definition.callback();
    if (captureFault) throw captureFault;
  }
} finally { delete globalThis[Symbol.for(key)]; rmSync(temporary, { recursive: true, force: true }); }
const steps = cases.flatMap(row => row.steps);
assert.equal(cases.length, 26);
assert.equal(steps.length, 29);
assert.equal(requestCount, 58);
assert.equal(steps.flatMap(step => step.requests).length, requestCount);
const directAssertions = tests.reduce((sum, row) => sum + row.direct_assertions, 0);
const helperExecutions = tests.reduce((sum, row) => sum + row.helper_assertions, 0);
assert.equal(directAssertions, 138); assert.equal(helperExecutions, 406);
const lines = cases.map(row => JSON.stringify(row));
assert.ok(lines.every(line => Buffer.byteLength(line) <= limits.case_bytes));
const serialized = lines.join('\n') + '\n';
assert.ok(Buffer.byteLength(serialized) <= limits.corpus_bytes);
const report = { schema_version: 1, kind: 'frozen_finite_ollama_routing_contract_capture',
  baseline_commit: baseline.baseline_commit, baseline_test: sourcePath, baseline_sha256: hash(source),
  source_inputs: ['scripts/test-ollama-routing.mjs', 'test/fixtures/ollama-integration.json', 'src/router.mjs', 'src/ollama-evaluator.mjs']
    .map(path => ({ path, sha256: hash(readFileSync(join(reference, path))) })),
  generator_sha256: hash(readFileSync(import.meta.filename)), node_version: process.version,
  typescript_version: ts.version, cases_sha256: hash(serialized), selected_definitions: selected,
  cases: cases.length, router_instances: cases.length, steps: steps.length, requests: requestCount,
  static_assertions: 12, helper_static_assertions: helperAssertions.length,
  direct_executed_assertions: directAssertions, helper_executed_assertions: helperExecutions,
  executed_assertions: directAssertions + helperExecutions,
  helper: { name: 'localFixture', sha256: hash(helper.getText(syntax)), assertions: helperAssertions },
  tests: tests.map(({ callback, ...row }) => row), limits,
  excluded_fields: ['Only unasserted route latency_ms/evaluation_latency_ms after finite/nonnegative validation'],
  boundaries: [
    'Three complete unchanged original callbacks, localFixture, model loops and assertions execute after static import replacement; original Router performs each route.',
    'Original buildRoutingRequest/readRoutingFixtures are imported from the verified opt-in script without invoking its main or runRoutingTests; no fixture labels are changed.',
    'Each source Router instance, route options scope/promptId/requestClass, complete body and complete outcome are retained; no completion evidence is fabricated.',
    'Response observer returns identical finite Response instances without clone/tee/read; full raw and parsed request bodies and headers independently recorded.',
    'No timed definition3, live harness definitions5–8, cancellation/body streaming, real model/service/provider/download, OS or numerical performance qualification.',
  ] };
mkdirSync(output, { mode: 0o700 });
writeFileSync(join(output, 'cases.jsonl'), serialized, { mode: 0o600 });
writeFileSync(join(output, 'capture.json'), JSON.stringify(report, null, 2) + '\n', { mode: 0o600 });
console.log(JSON.stringify({ definitions: selected.length, cases: cases.length, steps: steps.length,
  requests: requestCount, direct_assertions: directAssertions, helper_assertions: helperExecutions,
  cases_sha256: report.cases_sha256, corpus_bytes: Buffer.byteLength(serialized) }));
