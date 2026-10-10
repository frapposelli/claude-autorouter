// Development-only complete frozen timed evaluator callbacks; no live fetch.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/ollama-evaluator-timed-${Date.now()}`));
assert.ok(process.argv.length <= 4, 'Usage: capture-ollama-evaluator-timed-contracts.mjs [reference] [fresh-output]');
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const before = await verifyBaseline(reference);
assert.equal(before.files_verified, 111);
const generator = readFileSync(import.meta.filename), nodeHash = hash(readFileSync(process.execPath));
const manifest = readFileSync(join(root, 'rust/parity/baseline.json'));
const verifier = readFileSync(join(root, 'scripts/rust-reference.mjs'));
const sourceFile = 'test/ollama-evaluator.test.mjs';
const source = readFileSync(join(reference, sourceFile), 'utf8');
assert.equal(hash(source), JSON.parse(manifest).files.find(row => row.path === sourceFile).sha256);
const ts = createRequire(import.meta.url)('typescript');
const syntax = ts.createSourceFile(sourceFile, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
assert.equal(syntax.parseDiagnostics.length, 0);
const selected = [7, 8, 9, 10, 15], definitions = [], helpers = [], edits = [], callbacks = [];
const executed = [], operations = [], routers = [], requests = [], streams = [], events = [], signalRows = [], errors = [];
const signalIds = new WeakMap(), errorIds = new WeakMap(), responseData = new WeakMap(), streamData = new WeakMap();
const signalObjects = new Map(), operationSignals = new Map();
let current, activeOperation, fault;
const limits = { operations: 32, requests: 48, streams: 24, events: 512, assertions: 128,
  snapshot_nodes: 8192, field_bytes: 131072, aggregate_bytes: 8388608 };
function observe(action) { try { return action(); } catch (error) { fault ??= error; throw error; } }
function snapshot(value) {
  let count = 0;
  const ancestors = new Set();
  function visit(item, depth) {
    assert.ok(++count <= limits.snapshot_nodes && depth < 32);
    if (item === undefined) return { $js_type: 'undefined' };
    if (typeof item === 'number') {
      assert.ok(Number.isFinite(item) && !Object.is(item, -0)); return item;
    }
    if (typeof item === 'string') assert.ok(Buffer.byteLength(item) <= limits.field_bytes);
    if (item === null || ['boolean', 'string'].includes(typeof item)) return item;
    assert.equal(typeof item, 'object');
    assert.ok(Array.isArray(item) || Object.getPrototypeOf(item) === Object.prototype);
    assert.ok(!ancestors.has(item)); ancestors.add(item);
    const result = Array.isArray(item) ? [] : {};
    for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(item))) {
      if (!descriptor.enumerable) continue;
      assert.ok(Object.hasOwn(descriptor, 'value'), 'Capture never invokes getters');
      assert.notEqual(key, '$js_type');
      result[key] = visit(descriptor.value, depth + 1);
    }
    if (Array.isArray(item)) assert.equal(Object.keys(result).length, item.length);
    ancestors.delete(item); return result;
  }
  return visit(value, 0);
}
function errorInfo(error) {
  assert.ok(error instanceof Error || error instanceof DOMException);
  if (!errorIds.has(error)) {
    const row = { id: `error-${errors.length + 1}`, name: error.name, message: error.message,
      code: snapshot(error.code) };
    errorIds.set(error, row.id); errors.push(row);
  }
  return errorIds.get(error);
}
function signalInfo(signal) {
  if (signal === undefined) return null;
  assert.ok(signal instanceof AbortSignal);
  if (!signalIds.has(signal)) {
    const row = { id: `signal-${signalRows.length + 1}`, aborted_at_first_observation: signal.aborted,
      initial_reason: signal.aborted ? errorInfo(signal.reason) : null };
    signalRows.push(row); signalIds.set(signal, row.id); signalObjects.set(row.id, signal);
  }
  return signalIds.get(signal);
}
const origin = process.hrtime.bigint();
function event(kind, detail = {}, context = current?.id) {
  assert.ok(events.length < limits.events);
  const row = { index: events.length, source_test: context, elapsed_ns: String(process.hrtime.bigint() - origin), kind, ...detail };
  events.push(row); return row.index;
}
for (const statement of syntax.statements) {
  if (ts.isImportDeclaration(statement)) {
    edits.push({ start: statement.getStart(syntax), end: statement.end, text: '' }); continue;
  }
  if (!ts.isExpressionStatement(statement) || !ts.isCallExpression(statement.expression)
    || statement.expression.expression.getText(syntax) !== 'test') {
    helpers.push({ statement: statement.getText(syntax), sha256: hash(statement.getText(syntax)),
      line: syntax.getLineAndCharacterOfPosition(statement.getStart(syntax)).line + 1 }); continue;
  }
  const call = statement.expression, number = definitions.length + 1;
  const row = { id: `${sourceFile}#${number}`, number, name: call.arguments[0].text,
    selected: selected.includes(number), statement: statement.getText(syntax),
    statement_sha256: hash(statement.getText(syntax)), call_sha256: hash(call.getText(syntax)),
    source_line: syntax.getLineAndCharacterOfPosition(statement.getStart(syntax)).line + 1, assertions: [] };
  const walk = node => {
    if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)
      && node.expression.expression.getText(syntax) === 'assert') {
      const site = { id: `${row.id}:assert-${row.assertions.length + 1}`,
        line: syntax.getLineAndCharacterOfPosition(node.getStart(syntax)).line + 1,
        expression: node.getText(syntax), sha256: hash(node.getText(syntax)), expanded_executions: 0 };
      row.assertions.push(site);
      // Insertions preserve nested assertions (#10's unreachable fail guard),
      // original expressions and Promise identities without overlapping edits.
      if (row.selected) {
        edits.push({ start: node.getStart(syntax), end: node.getStart(syntax), text: `(__assertion(${JSON.stringify(site.id)}), ` });
        edits.push({ start: node.end, end: node.end, text: ')' });
      }
    }
    ts.forEachChild(node, walk);
  };
  walk(call.arguments.at(-1));
  if (!row.selected) edits.push({ start: statement.getStart(syntax), end: statement.end, text: '' });
  definitions.push(row);
}
assert.equal(definitions.length, 15);
assert.equal(definitions.filter(row => row.selected).flatMap(row => row.assertions).length, 20);
let transformed = source;
for (const edit of edits.sort((a, b) => b.start - a.start)) transformed = transformed.slice(0, edit.start) + edit.text + transformed.slice(edit.end);
const transformedSyntax = ts.createSourceFile('capture.mjs', transformed, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
assert.equal(transformedSyntax.parseDiagnostics.length, 0);
mkdirSync(output);
writeFileSync(join(output, 'capture-source.mjs'), generator, { flag: 'wx', mode: 0o600 });
writeFileSync(join(output, 'transformed-callbacks.mjs'), transformed, { flag: 'wx', mode: 0o600 });
const originalFetch = globalThis.fetch;
const NativeResponse = globalThis.Response, NativeStream = globalThis.ReadableStream, NativeController = globalThis.AbortController;
function recordedResponse(response, body) {
  return observe(() => {
    responseData.set(response, { status: response.status, headers: [...response.headers], body }); return response;
  });
}
const Response = new Proxy(NativeResponse, {
  construct(target, args) {
    const input = args[0];
    const body = observe(() => {
      if (streamData.has(input)) return { stream: streamData.get(input).id };
      assert.ok(input == null || typeof input === 'string'); return { text: input ?? '' };
    });
    return recordedResponse(Reflect.construct(target, args, target), body);
  },
  get(target, key) {
    if (key !== 'json') return Reflect.get(target, key);
    return (value, init) => recordedResponse(target.json(value, init),
      observe(() => ({ json: snapshot(value), text: JSON.stringify(value) })));
  },
});
const ReadableStream = new Proxy(NativeStream, {
  construct(target, args) {
    const source = args[0];
    const row = observe(() => {
      assert.ok(streams.length < limits.streams);
      assert.ok(Object.keys(source).every(key => ['start', 'pull', 'cancel'].includes(key)));
      const row = { id: `stream-${streams.length + 1}`, source_test: current.id, operation: activeOperation.id,
        source_functions: Object.fromEntries(Object.entries(source).map(([key, fn]) => [key, fn.toString()])), events: [] };
      streams.push(row); return row;
    });
    const initialized = new WeakSet();
    function controllerObserver(controller) {
      if (initialized.has(controller)) return controller;
      initialized.add(controller);
      for (const name of ['enqueue', 'close', 'error']) {
        const original = controller[name];
        Object.defineProperty(controller, name, { configurable: true, value: function(...values) {
          observe(() => {
            const detail = { stream: row.id };
            if (name === 'enqueue') {
              assert.equal(values.length, 1); assert.ok(values[0] instanceof Uint8Array);
              assert.ok(values[0].byteLength <= limits.field_bytes);
              detail.bytes = values[0].byteLength; detail.base64 = Buffer.from(values[0]).toString('base64');
            } else if (name === 'error') detail.error = errorInfo(values[0]);
            row.events.push(event(`stream_${name}`, detail, row.source_test));
          });
          return Reflect.apply(original, controller, values);
        } });
      }
      return controller;
    }
    const wrapped = Object.fromEntries(Object.entries(source).map(([name, original]) => [name, function(...values) {
      observe(() => {
        const detail = { stream: row.id };
        if (name === 'cancel') detail.reason = values[0] === undefined ? null : errorInfo(values[0]);
        row.events.push(event(`stream_${name}`, detail, row.source_test));
      });
      if (name !== 'cancel') controllerObserver(values[0]);
      return Reflect.apply(original, source, values);
    }]));
    const stream = Reflect.construct(target, [wrapped, ...args.slice(1)], target);
    streamData.set(stream, row); return stream;
  },
});
const AbortController = new Proxy(NativeController, { construct(target, args) {
  const controller = Reflect.construct(target, args, target), abort = controller.abort;
  Object.defineProperty(controller, 'abort', { value: function(...values) {
    observe(() => event('caller_abort', { signal: signalInfo(controller.signal), reason: errorInfo(values[0]) }));
    return Reflect.apply(abort, controller, values);
  } });
  return controller;
} });
function recordAssertion(id) {
  observe(() => {
    assert.ok(executed.length < limits.assertions);
    const site = current.assertions.find(row => row.id === id); assert.ok(site);
    site.expanded_executions++; executed.push(id); event('assertion', { id });
  });
}
function begin(kind, input, config, signal, router) {
  return observe(() => {
    assert.ok(operations.length < limits.operations && activeOperation === undefined);
    const row = { id: `operation-${operations.length + 1}`, source_test: current.id, kind,
      router: router ?? null, input: snapshot(input), config: snapshot(config), signal: signalInfo(signal), request_ids: [] };
    row.signal_aborted_before = signal?.aborted ?? null;
    operationSignals.set(row.id, signal);
    operations.push(row); activeOperation = row; row.started = event('operation_start', { operation: row.id }); return row;
  });
}
async function finish(row, action) {
  try {
    const value = await action();
    observe(() => { row.output = snapshot(value); row.finished = event('operation_return', { operation: row.id }); });
    return value;
  } catch (error) {
    observe(() => { row.error = errorInfo(error);
      const signal = operationSignals.get(row.id);
      row.error_is_caller_reason = signal?.aborted ? error === signal.reason : null;
      row.finished = event('operation_reject', { operation: row.id, error: row.error }); });
    throw error;
  } finally { activeOperation = undefined; }
}
function fetchFacade(original) {
  return async function(...args) {
    const row = observe(() => {
      assert.equal(args.length, 2); assert.ok(requests.length < limits.requests);
      const [url, options] = args; assert.equal(typeof url, 'string');
      const fields = { ...options }; delete fields.signal;
      const row = { id: `request-${requests.length + 1}`, source_test: current.id, operation: activeOperation.id,
        url, options: snapshot(fields), parsed_body: JSON.parse(options.body), signal: signalInfo(options.signal),
        signal_aborted_before: options.signal.aborted };
      requests.push(row); activeOperation.request_ids.push(row.id); row.started = event('request_start', { request: row.id }); return row;
    });
    try {
      const response = await Reflect.apply(original, this, args);
      observe(() => {
        assert.ok(responseData.has(response), 'Unobserved response');
        row.response = responseData.get(response); row.signal_aborted_at_response = args[1].signal.aborted; row.finished = event('request_return', { request: row.id });
      });
      return response;
    } catch (error) {
      observe(() => { row.error = errorInfo(error); row.signal_aborted_at_rejection = args[1].signal.aborted;
        row.error_is_request_signal_reason = error === args[1].signal.reason;
        row.finished = event('request_reject', { request: row.id, error: row.error }); });
      throw error;
    }
  };
}
try {
  globalThis.fetch = () => { throw new Error('Frozen callback capture forbids live fetch'); };
  const load = path => import(pathToFileURL(join(reference, path)).href);
  const [routing, evaluator, configuration, prompt, models] = await Promise.all([
    load('src/router.mjs'), load('src/ollama-evaluator.mjs'), load('src/config.mjs'), load('src/prompt-state.mjs'), load('src/ollama-models.mjs'),
  ]);
  class Router {
    constructor(config, options) {
      const row = observe(() => {
        assert.ok(routers.length < 8); assert.deepEqual(Object.keys(options), ['fetchImpl']);
        const row = { id: `router-${routers.length + 1}`, source_test: current.id, config: snapshot(config),
          mock_fetch_source: options.fetchImpl.toString(), mock_fetch_sha256: hash(options.fetchImpl.toString()) };
        routers.push(row); return row;
      });
      this.row = row; this.config = config; this.inner = new routing.Router(config, { ...options, fetchImpl: fetchFacade(options.fetchImpl) });
    }
    classify(...args) { const row = begin('classify', args[0], this.config, args[1], this.row.id); return finish(row, () => this.inner.classify(...args)); }
    route(...args) { const row = begin('route', args[0], this.config, args[1], this.row.id); return finish(row, () => this.inner.route(...args)); }
  }
  function evaluateOllama(state, config, options) {
    const row = begin('evaluate', state, config, options.signal);
    row.mock_fetch_source = options.fetchImpl.toString(); row.mock_fetch_sha256 = hash(row.mock_fetch_source);
    return finish(row, () => evaluator.evaluateOllama(state, config, { ...options, fetchImpl: fetchFacade(options.fetchImpl) }));
  }
  function test(name, ...args) {
    const row = definitions.filter(row => row.selected)[callbacks.length]; assert.equal(name, row.name);
    callbacks.push(args.at(-1));
  }
  const values = { test, assert, delay, readConfig: configuration.readConfig, Router, buildState: prompt.buildState,
    buildOllamaState: evaluator.buildOllamaState, evaluateOllama, OLLAMA_QUESTIONS: evaluator.OLLAMA_QUESTIONS,
    DEFAULT_OLLAMA_MODEL: models.DEFAULT_OLLAMA_MODEL, Response, ReadableStream, AbortController, __assertion: recordAssertion };
  Function(...Object.keys(values), `"use strict";\n${transformed}`)(...Object.values(values));
  for (const [index, row] of definitions.filter(row => row.selected).entries()) {
    current = row; event('callback_start'); await callbacks[index]();
    if (fault) throw fault;
    event('callback_end');
  }
  assert.equal(executed.length, 36);
  assert.deepEqual(definitions.filter(row => row.selected).map(row => row.assertions.reduce((n, site) => n + site.expanded_executions, 0)), [15, 4, 10, 1, 6]);
  assert.equal(operations.length, 16); assert.equal(routers.length, 6); assert.equal(requests.length, 26);
  for (const row of operations) assert.ok(Object.hasOwn(row, 'output') !== Object.hasOwn(row, 'error'));
  const after = await verifyBaseline(reference); assert.deepEqual(after, before);
  assert.equal(hash(readFileSync(import.meta.filename)), hash(generator)); assert.equal(hash(readFileSync(process.execPath)), nodeHash);
  assert.equal(hash(readFileSync(join(root, 'rust/parity/baseline.json'))), hash(manifest));
  assert.equal(hash(readFileSync(join(root, 'scripts/rust-reference.mjs'))), hash(verifier));
  for (const row of signalRows) {
    const signal = signalObjects.get(row.id);
    row.aborted_after_callbacks = signal.aborted;
    row.final_reason = signal.aborted ? errorInfo(signal.reason) : null;
  }
  const raw = { routers, operations, requests, streams, events, signals: signalRows, errors };
  const cases = operations.map(row => ({ ...row, requests: row.request_ids.map(id => requests.find(request => request.id === id)) }));
  const excludedTimings = [];
  const stable = structuredClone(cases);
  for (const row of stable.filter(row => row.kind === 'route')) {
    for (const field of ['evaluation_latency_ms', 'latency_ms']) {
      const value = row.output[field];
      assert.ok(typeof value === 'number' && Number.isFinite(value) && value >= 0);
      excludedTimings.push({ operation: row.id, path: `$.output.${field}`, observed_ms: value });
      row.output[field] = 0;
    }
  }
  assert.equal(excludedTimings.length, 6);
  const corpus = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
  assert.ok(Buffer.byteLength(JSON.stringify(raw)) + Buffer.byteLength(corpus) <= limits.aggregate_bytes);
  const report = { schema_version: 1, kind: 'complete_frozen_timed_evaluator_callbacks', baseline: before,
    baseline_source: { path: sourceFile, sha256: hash(source) }, generator_sha256: hash(generator),
    node: { version: process.version, executable_sha256: nodeHash }, verifier_sha256: hash(verifier), manifest_sha256: hash(manifest),
    typescript_version: ts.version, selected_definitions: selected, static_assertions: 20, expanded_assertions: executed.length,
    operation_count: operations.length, request_count: requests.length, router_count: routers.length,
    definitions, helpers, executed_assertion_ids: executed, limits, raw,
    cases_sha256: hash(corpus), stable_non_timing_cases_sha256: hash(JSON.stringify(stable)), excluded_timings: excludedTimings,
    boundaries: [
      'All five original callbacks and all original helpers execute completely. Only import wiring, unselected-definition removal and assertion-entry counters are transformed. Nested fail guard is retained with zero executions.',
      'Original callback-local timers, delays, pending streams, injected fetch functions and assertions remain. Captured callbacks use real Node timers; no model or network call occurs.',
      'Response and stream facades return actual built-in objects without reading, teeing or cloning. Stream callbacks retain the original source receiver; controller method observers forward through the identical controller, recording exact enqueued bytes and cancellation ordering.',
      'Signal and Error ids represent exact object identity within this execution. Original arbitrary Error equality checks remain in Node. Rust typed cancellation is a documented library adaptation, not Error identity equivalence.',
      'Complete raw decisions and monotonic event observations are retained. The secondary stable operation digest replaces exactly six validated nonnegative route latency fields with zero; raw corpus is unmodified. No wall-clock parity claim is made. Native paused-clock schedules must use real production timers and real body/request drop.',
      'The Router facade records only original external route/classify invocations and delegates to one unchanged frozen Router instance per original constructor. Internal classify calls are not replaced.',
    ] };
  writeFileSync(join(output, 'cases.jsonl'), corpus, { flag: 'wx' });
  writeFileSync(join(output, 'capture.json'), JSON.stringify(report, null, 2) + '\n', { flag: 'wx' });
  console.log(JSON.stringify({ selected, assertions: executed.length, operations: operations.length, requests: requests.length, streams: streams.length, cases_sha256: hash(corpus) }));
} catch (error) {
  writeFileSync(join(output, 'failure.json'), JSON.stringify({ message: error.message, stack: error.stack,
    recorder_fault: fault?.message, current: current?.id, definitions, executed, routers, operations, requests, streams, events }, null, 2) + '\n', { flag: 'wx' });
  throw error;
} finally { globalThis.fetch = originalFetch; }
