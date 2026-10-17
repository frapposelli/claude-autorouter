// Original timed synthetic callback: real Node deadline and unchanged mock delay.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/ollama-routing-timeout-${Date.now()}`));
assert.ok(process.argv.length <= 4);
await verifyBaseline(reference);
const hash = value => createHash('sha256').update(value).digest('hex');
const file = 'test/ollama-routing.test.mjs';
const source = readFileSync(join(reference, file), 'utf8');
const baseline = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json')));
assert.equal(hash(source), baseline.files.find(row => row.path === file).sha256);
const ts = createRequire(import.meta.url)('typescript');
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
let helperHash;
for (const statement of syntax.statements) {
  if (ts.isImportDeclaration(statement)) {
    edits.push({ start: statement.getStart(syntax), end: statement.end, text: '' }); continue;
  }
  if (ts.isFunctionDeclaration(statement) && statement.name?.text === 'localFixture') {
    helperHash = hash(statement.getText(syntax));
    helperAssertions.push(...assertions(statement, 'localFixture')); continue;
  }
  if (!ts.isExpressionStatement(statement) || !ts.isCallExpression(statement.expression)
    || statement.expression.expression.getText(syntax) !== 'test') continue;
  const call = statement.expression, number = definitions.length + 1;
  const row = { id: `${file}#${number}`, number, name: call.arguments[0].text, selected: number === 3,
    statement: statement.getText(syntax), statement_sha256: hash(statement.getText(syntax)), call_sha256: hash(call.getText(syntax)),
    line: syntax.getLineAndCharacterOfPosition(statement.getStart(syntax)).line + 1, assertions: [] };
  if (row.selected) row.assertions = assertions(call, row.id);
  definitions.push(row);
}
assert.equal(definitions.length, 8);
assert.equal(definitions[2].assertions.length, 12);
assert.equal(helperAssertions.length, 9);
let transformed = source, previous = source.length;
for (const edit of edits.sort((a, b) => b.start - a.start)) {
  assert.ok(edit.end <= previous);
  transformed = transformed.slice(0, edit.start) + edit.text + transformed.slice(edit.end);
  previous = edit.start;
}
const load = path => import(pathToFileURL(join(reference, path)).href);
const [configuration, routing, integration] = await Promise.all([
  load('src/config.mjs'), load('src/router.mjs'), load('scripts/test-ollama-routing.mjs'),
]);
const limits = { routers: 1, routes: 4, requests_per_route: 2, requests: 8, field_bytes: 65536,
  response_bytes: 65536, snapshot_nodes: 4096, snapshot_depth: 32, case_bytes: 524288,
  corpus_bytes: 1048576, aggregate_bytes: 4194304, external_capture_seconds: 30 };
let fault, retainedBytes = 0, current, requestCount = 0;
const cases = [], observations = [], callbacks = [], executed = [];
function checked(action) { try { return action(); } catch (error) { fault ??= error; throw error; } }
function account(value) {
  retainedBytes += Buffer.byteLength(JSON.stringify(value));
  assert.ok(retainedBytes <= limits.aggregate_bytes, 'Capture aggregate bound');
}
function snapshot(value) {
  const tags = [], ancestors = new Set(); let nodes = 0, budget = 0;
  function walk(item, path, depth) {
    assert.ok(++nodes <= limits.snapshot_nodes && depth <= limits.snapshot_depth);
    budget += 16 + (typeof item === 'string' ? Buffer.byteLength(item) : 0);
    assert.ok(budget <= limits.case_bytes);
    if (item === undefined) { tags.push({ path, kind: 'undefined' }); return; }
    if (typeof item === 'number') assert.ok(Number.isFinite(item), 'Nonfinite fixture');
    if (typeof item === 'string') assert.ok(Buffer.byteLength(item) <= limits.field_bytes);
    if (item === null || ['string', 'number', 'boolean'].includes(typeof item)) return item;
    assert.equal(typeof item, 'object');
    assert.ok(Array.isArray(item) || Object.getPrototypeOf(item) === Object.prototype);
    assert.ok(!ancestors.has(item)); ancestors.add(item);
    const out = Array.isArray(item) ? [] : {};
    for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(item))) {
      if (!descriptor.enumerable) continue;
      assert.ok(Object.hasOwn(descriptor, 'value'));
      const child = walk(descriptor.value, `${path}.${key}`, depth + 1);
      if (Array.isArray(out)) out.push(child === undefined ? null : child);
      else if (child !== undefined) out[key] = child;
    }
    if (Array.isArray(out)) assert.equal(out.length, item.length);
    ancestors.delete(item); return out;
  }
  return { value: walk(value, '$', 0), tags };
}
const NativeResponse = globalThis.Response, responses = new WeakMap();
const Response = new Proxy(NativeResponse, { get(target, key) {
  if (key !== 'json') return Reflect.get(target, key);
  return (value, init) => checked(() => {
    const observed = snapshot(value), body = JSON.stringify(value);
    assert.ok(Buffer.byteLength(body) <= limits.response_bytes);
    const response = target.json(value, init);
    responses.set(response, { status: response.status, headers: Object.fromEntries(response.headers), body, input_tags: observed.tags });
    return response;
  });
} });
function fetchObserver(config, active, original) {
  return async (url, init) => {
    const step = active();
    const call = checked(() => {
      assert.ok(step && step.requests.length < limits.requests_per_route);
      assert.ok(++requestCount <= limits.requests);
      assert.equal(new URL(url).origin, config.ollamaEndpoint);
      assert.equal(init.redirect, 'error');
      assert.ok(init.signal instanceof AbortSignal && !init.signal.aborted);
      assert.equal(typeof init.body, 'string');
      assert.ok(Buffer.byteLength(init.body) <= limits.field_bytes);
      const row = { url: String(url), method: init.method ?? 'GET', headers: Object.fromEntries(new Headers(init.headers)),
        body: init.body, parsed_body: JSON.parse(init.body),
        node_options: { redirect: init.redirect, signal_kind: 'AbortSignal', signal_aborted_before: init.signal.aborted } };
      step.requests.push(row); return row;
    });
    try {
      const response = await original(url, init);
      checked(() => {
        assert.ok(responses.has(response), 'Only original finite Response.json mocks');
        call.outcome = { kind: 'response', ...responses.get(response) };
        call.node_options.signal_aborted_after = init.signal.aborted;
        assert.equal(init.signal.aborted, false);
      });
      return response;
    } catch (error) {
      checked(() => {
        assert.equal(error.name, 'TimeoutError');
        assert.equal(init.signal.aborted, true);
        assert.equal(error, init.signal.reason, 'Original timeout reason identity');
        assert.ok(Buffer.byteLength(error.message) <= 1024);
        call.outcome = { kind: 'error', name: error.name, message: error.message };
        Object.assign(call.node_options, { signal_aborted_after: true, signal_reason_name: init.signal.reason.name, same_error_reason: true });
      });
      throw error;
    } finally { checked(() => account(call)); }
  };
}
class Router extends routing.Router {
  constructor(config, options) {
    assert.equal(cases.length, 0);
    assert.deepEqual(Object.keys(options), ['fetchImpl']);
    assert.equal(typeof options.fetchImpl, 'function');
    const settings = snapshot(config), trace = { active: undefined };
    const row = { id: 'baseline-ollama-routing-timeout-3-1', source_test: current.id,
      config: settings.value, config_tags: settings.tags, steps: [] };
    super(config, { ...options, fetchImpl: fetchObserver(config, () => trace.active, options.fetchImpl) });
    cases.push(row); account(settings); this.captureRow = row; this.captureTrace = trace;
  }
  async route(...args) {
    const row = this.captureRow, trace = this.captureTrace;
    const step = checked(() => {
      assert.equal(args.length, 1, 'Original options remain absent');
      assert.ok(!trace.active && row.steps.length < limits.routes);
      const body = snapshot(args[0]);
      const step = { operation: 'route', body_json: JSON.stringify(args[0]), body_tags: body.tags,
        options: { kind: 'absent', argument_count: args.length }, requests: [] };
      assert.ok(Buffer.byteLength(step.body_json) <= limits.field_bytes);
      row.steps.push(step); trace.active = step; return step;
    });
    try {
      const result = await super.route(...args);
      checked(() => {
        const raw = snapshot(result), observed = snapshot(result);
        for (const field of ['latency_ms', 'evaluation_latency_ms']) {
          assert.ok(Number.isFinite(observed.value[field]) && observed.value[field] >= 0);
          delete observed.value[field];
        }
        assert.equal(JSON.stringify(args[0]), step.body_json);
        step.result = observed;
        observations.push({ route: row.steps.length, result: raw }); account(raw); account(step);
      });
      if (fault) throw fault;
      return result;
    } finally { trace.active = undefined; }
  }
}
function test(name, callback) {
  assert.equal(name, definitions[callbacks.length].name); callbacks.push(callback);
}
function recordAssertion(id, execute) {
  return checked(() => { const value = execute(); assert.ok(!value || typeof value.then !== 'function'); executed.push(id); return value; });
}
mkdirSync(output, { recursive: false, mode: 0o700 });
function write(name, value) { writeFileSync(join(output, name), JSON.stringify(value, null, 2) + '\n', { flag: 'wx', mode: 0o600 }); }
try {
  await Function('test', 'assert', 'delay', 'readConfig', 'Router', 'buildRoutingRequest', 'readRoutingFixtures', 'runRoutingTests', 'Response', '__assertion',
    `"use strict";return (async () => {${transformed}\n})()`)(test, assert, delay, configuration.readConfig, Router,
    integration.buildRoutingRequest, integration.readRoutingFixtures, () => assert.fail('Unselected live tool'), Response, recordAssertion);
  assert.equal(callbacks.length, 8); current = definitions[2];
  await callbacks[2](); if (fault) throw fault;
  assert.equal(cases.length, 1); assert.equal(cases[0].steps.length, 4);
  assert.equal(requestCount, 8); assert.equal(cases[0].config.ollamaTimeoutMs, 5);
  const requests = cases[0].steps.flatMap(step => step.requests);
  assert.equal(requests.filter(row => row.outcome.kind === 'error').length, 1);
  assert.equal(cases[0].steps[1].requests[1].outcome.kind, 'error');
  const direct = executed.filter(id => id.startsWith(current.id + ':')).length;
  const helper = executed.filter(id => id.startsWith('localFixture:')).length;
  assert.equal(direct, 12); assert.equal(helper, 53);
  const corpus = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
  assert.ok(Buffer.byteLength(corpus) <= limits.case_bytes && Buffer.byteLength(corpus) <= limits.corpus_bytes);
  const capture = { schema_version: 1, kind: 'frozen_ollama_routing_timeout_contract', baseline_commit: baseline.baseline_commit,
    baseline_file: file, baseline_file_sha256: hash(source), generator_sha256: hash(readFileSync(import.meta.filename)),
    selected_definitions: [3], definitions: [current], helper: { name: 'localFixture', sha256: helperHash, assertions: helperAssertions },
    cases: 1, routers: 1, routes: 4, requests: requestCount, response_outcomes: 7, timeout_outcomes: 1,
    static_assertions: 12, helper_static_assertions: 9, direct_executed_assertions: direct,
    helper_executed_assertions: helper, executed_assertions: executed, cases_sha256: hash(corpus), limits,
    source_inputs: ['scripts/test-ollama-routing.mjs', 'test/fixtures/ollama-integration.json', 'src/router.mjs', 'src/ollama-evaluator.mjs']
      .map(path => ({ path, sha256: hash(readFileSync(join(reference, path))) })),
    boundaries: ['One unchanged original callback and localFixture, with successful original assertion wrappers; real node:timers/promises delay(20) and production AbortSignal.timeout(5).',
      'Timeout fetch rethrows the actual original signal reason. Only original source outputs become expected decisions; none are injected into native classification.',
      'Full config with constructor timeout override, absent route arguments, one Router and four sequential calls are retained.',
      'Only validated nonnegative route latency_ms/evaluation_latency_ms are projected; raw results are separately retained. No wall-clock or event-loop equivalence claim.',
      'No network service, provider, live evaluator, model download, user config write or hardware performance measurement.'] };
  writeFileSync(join(output, 'cases.jsonl'), corpus, { flag: 'wx', mode: 0o600 });
  write('capture.json', capture); write('observations.json', observations);
  console.log(JSON.stringify({ cases: 1, routes: 4, requests: requestCount, direct, helper, cases_sha256: hash(corpus) }));
} catch (error) {
  write('failure.json', { completed: false, error: { name: error.name, message: String(error.message).slice(0, 4096) },
    cases, observations, executed, generator_sha256: hash(readFileSync(import.meta.filename)) });
  throw error;
}
