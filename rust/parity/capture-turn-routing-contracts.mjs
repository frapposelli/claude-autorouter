// Development-only capture of four complete frozen turn-routing callbacks.
// This delegates to the frozen Router and its original callback-local mocks.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import fs from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/turn-routing-contracts-${Date.now()}`));
assert.ok(process.argv.length <= 4, 'Usage: capture-turn-routing-contracts.mjs [frozen-reference] [new-output]');
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const descriptor = path => {
  const bytes = readFileSync(join(root, path));
  return { path, bytes: bytes.length, sha256: hash(bytes) };
};
const baseline = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json'), 'utf8'));
const before = await verifyBaseline(reference);
assert.equal(before.baseline_commit, 'ea930c247626ce2af5ccdad721b5121417bf4ad8');
assert.equal(before.files_verified, 111);
const nodeHash = hash(readFileSync(process.execPath));
const scriptHash = hash(readFileSync(import.meta.filename));
const verifier = descriptor('scripts/rust-reference.mjs');
const manifest = descriptor('rust/parity/baseline.json');
const sourceFile = 'test/turn-state.test.mjs';
const source = readFileSync(join(reference, sourceFile), 'utf8');
assert.equal(hash(source), baseline.files.find(row => row.path === sourceFile)?.sha256);
const ts = createRequire(join(root, 'package.json'))('typescript');
const syntax = ts.createSourceFile(sourceFile, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
assert.equal(syntax.parseDiagnostics.length, 0);
const selectedNumbers = [5, 6, 15, 16];
const definitions = [], helpers = [], edits = [], callbacks = [];
const assertionCalls = [], configs = [], routers = [], routes = [], completes = [], evaluatorCalls = [];
const events = [], rawDecisions = [];
const mockResponses = new WeakMap();
let current, currentRoute, recorderFault;

// A tagged undefined preserves own properties and argument presence, unlike
// JSON.stringify alone. No non-JSON source value is silently projected away.
function snapshot(value, depth = 0) {
  assert.ok(depth < 32, 'Capture value depth bound');
  if (value === undefined) return { $js_type: 'undefined' };
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return value;
  if (typeof value === 'number') {
    assert.ok(Number.isFinite(value) && !Object.is(value, -0), 'Finite ordinary source number');
    return value;
  }
  if (Array.isArray(value)) return value.map(item => snapshot(item, depth + 1));
  assert.equal(typeof value, 'object');
  assert.equal(Object.getPrototypeOf(value), Object.prototype, 'Plain source object');
  assert.ok(!Object.hasOwn(value, '$js_type'), 'Reserved capture tag');
  return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, snapshot(item, depth + 1)]));
}
function observe(action) {
  try { return action(); }
  catch (error) { recorderFault ??= error; throw error; }
}
function healthy() { if (recorderFault) throw recorderFault; }
function event(kind, details) {
  assert.ok(events.length < 512, 'Event count bound');
  events.push({ index: events.length, source_test: current.id, kind, ...details });
}
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
    || statement.expression.expression.getText(syntax) !== 'test') {
    helpers.push({ line: syntax.getLineAndCharacterOfPosition(statement.getStart(syntax)).line + 1,
      statement: statement.getText(syntax), sha256: hash(statement.getText(syntax)) });
    continue;
  }
  const call = statement.expression, number = definitions.length + 1;
  const selected = selectedNumbers.includes(number), sites = assertions(call.arguments.at(-1));
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
  else edits.push({ start: statement.getStart(syntax), end: statement.end, text: '' });
  definitions.push(definition);
}
assert.equal(definitions.length, 16);
assert.equal(helpers.length, 2);
const selected = definitions.filter(row => row.selected);
assert.equal(selected.reduce((sum, row) => sum + row.assertions.length, 0), 19);
let transformed = source, previous = source.length;
for (const edit of edits.sort((a, b) => b.start - a.start)) {
  assert.ok(edit.end <= previous, 'Capture edits overlap');
  transformed = transformed.slice(0, edit.start) + edit.text + transformed.slice(edit.end);
  previous = edit.start;
}
assert.doesNotMatch(transformed, /^\s*import\b/m);
await fs.mkdir(output);
await fs.writeFile(join(output, 'capture-source.mjs'), readFileSync(import.meta.filename), { flag: 'wx', mode: 0o600 });
const originalFetch = globalThis.fetch;
try {
  globalThis.fetch = () => { throw new Error('Capture forbids real network fetch'); };
  const routing = await import(pathToFileURL(join(reference, 'src/router.mjs')).href);
  const turns = await import(pathToFileURL(join(reference, 'src/turn-state.mjs')).href);
  const configuration = await import(pathToFileURL(join(reference, 'src/config.mjs')).href);
  const CapturedResponse = new Proxy(Response, { get(target, property) {
    if (property !== 'json') return Reflect.get(target, property);
    return (...args) => observe(() => {
      const inputs = snapshot(args);
      const response = Response.json(...args);
      mockResponses.set(response, { arguments: inputs, status: response.status,
        headers: [...response.headers], serialized_body: JSON.stringify(args[0]) });
      return response;
    });
  } });
  class Router extends routing.Router {
    constructor(config, options) {
      const row = observe(() => {
        assert.ok(routers.length < 4);
        assert.equal(typeof options.fetchImpl, 'function');
        assert.deepEqual(Object.keys(options).sort(), current.number === 5 ? ['fetchImpl', 'now'] : ['fetchImpl']);
        return { id: `router-${routers.length + 1}`, source_test: current.id, config: snapshot(config),
          option_keys: Object.keys(options), mock_fetch_source: options.fetchImpl.toString(),
          mock_fetch_sha256: hash(options.fetchImpl.toString()),
          clock: options.now ? { kind: 'original_callback_clock', source: options.now.toString(),
            sha256: hash(options.now.toString()), observations: [] } : { kind: 'unchanged_default_Date.now' } };
      });
      const originalMock = options.fetchImpl;
      const delegated = { ...options, fetchImpl: async function(...args) {
        const call = observe(() => {
          assert.ok(evaluatorCalls.length < 16);
          assert.equal(args.length, 2);
          const [url, request] = args;
          assert.ok(request.signal instanceof AbortSignal);
          const fields = { ...request }; delete fields.signal;
          const entry = { id: `evaluator-${evaluatorCalls.length + 1}`, source_test: current.id,
            router: row.id, route: currentRoute, url, request: snapshot(fields),
            parsed_body: JSON.parse(request.body),
            signal: { kind: 'actual_frozen_combined_AbortSignal', aborted_before: request.signal.aborted } };
          evaluatorCalls.push(entry); event('evaluator_call', { call: entry.id });
          return entry;
        });
        const response = await Reflect.apply(originalMock, this, args);
        observe(() => {
          assert.ok(mockResponses.has(response), 'Original callback Response.json result');
          call.response = mockResponses.get(response);
          call.signal.aborted_after = args[1].signal.aborted;
          event('evaluator_return', { call: call.id });
        });
        return response;
      } };
      if (options.now) delegated.now = function(...args) {
        const value = Reflect.apply(options.now, this, args);
        observe(() => {
          assert.ok(row.clock.observations.length < 128);
          row.clock.observations.push(snapshot(value));
          event('injected_clock', { router: row.id, value });
        });
        return value;
      };
      super(config, delegated);
      this.captureId = row.id;
      routers.push(row);
      event('router_construct', { router: row.id });
    }
    async route(...args) {
      const row = observe(() => {
        healthy(); assert.ok(routes.length < 16); assert.equal(args.length, 2);
        const entry = { id: `route-${routes.length + 1}`, source_test: current.id,
          router: this.captureId, arguments: snapshot(args) };
        routes.push(entry); currentRoute = entry.id; event('route_call', { call: entry.id });
        return entry;
      });
      const decision = await super.route(...args);
      observe(() => {
        healthy();
        assert.deepEqual(snapshot(args), row.arguments, 'Route inputs remain unchanged');
        const raw = snapshot(decision), stable = { ...raw };
        for (const field of ['latency_ms', 'evaluation_latency_ms']) {
          assert.ok(Object.hasOwn(raw, field) && Number.isFinite(raw[field]) && raw[field] >= 0);
          delete stable[field];
        }
        row.decision = stable;
        row.latency_projection = ['latency_ms', 'evaluation_latency_ms'];
        row.state_after = { records: this.turns.records.size, attempts: this.turns.attempts.size,
          pending_evaluations: this.pendingEvaluations.size, evaluation_subscribers: this.evaluationSubscribers };
        rawDecisions.push({ route: row.id, decision: raw });
        event('route_return', { call: row.id }); currentRoute = undefined;
      });
      return decision;
    }
    complete(...args) {
      return observe(() => {
        healthy(); assert.ok(completes.length < 16); assert.ok(args.length === 1 || args.length === 2);
        const row = { id: `complete-${completes.length + 1}`, source_test: current.id,
          router: this.captureId, arguments: snapshot(args), argument_count: args.length };
        completes.push(row); event('complete_call', { call: row.id });
        const result = super.complete(...args);
        row.result = snapshot(result);
        assert.deepEqual(snapshot(args), row.arguments, 'Completion inputs remain unchanged');
        row.state_after = { records: this.turns.records.size, attempts: this.turns.attempts.size };
        event('complete_return', { call: row.id });
        return result;
      });
    }
  }
  function readConfig(...args) {
    const result = configuration.readConfig(...args);
    observe(() => {
      assert.ok(configs.length < 4); assert.deepEqual(args, [{ AUTOROUTER_EVALUATOR: 'jev' }]);
      configs.push({ source_test: current.id, arguments: snapshot(args), result: snapshot(result) });
      event('read_config', { index: configs.length - 1 });
    });
    return result;
  }
  function test(name, callback) {
    assert.equal(name, selected[callbacks.length].name); assert.equal(typeof callback, 'function');
    callbacks.push(callback);
  }
  function counted(id, value) {
    return observe(() => {
      assert.ok(assertionCalls.length < 32);
      const site = current.assertions.find(site => site.id === id); assert.ok(site);
      site.expanded_executions += 1; assertionCalls.push(id); event('assertion_pass', { assertion: id });
      return value;
    });
  }
  Function('test', 'assert', 'TurnState', 'Router', 'readConfig', 'Response', '__assertion',
    `"use strict";\n${transformed}`)(test, assert, turns.TurnState, Router, readConfig, CapturedResponse, counted);
  assert.equal(callbacks.length, 4);
  for (const [index, definition] of selected.entries()) {
    current = definition;
    await callbacks[index]();
    healthy(); assert.ok(definition.assertions.every(row => row.expanded_executions === 1));
    definition.passed = true;
  }
  assert.deepEqual(await verifyBaseline(reference), before);
  assert.equal(hash(readFileSync(process.execPath)), nodeHash);
  assert.equal(hash(readFileSync(import.meta.filename)), scriptHash);
  assert.deepEqual(descriptor(verifier.path), verifier);
  assert.deepEqual(descriptor(manifest.path), manifest);
  assert.equal(configs.length, 4); assert.equal(routers.length, 4);
  assert.equal(routes.length, 13); assert.equal(completes.length, 11);
  assert.equal(evaluatorCalls.length, 9);
  assert.ok(evaluatorCalls.every(row => !row.signal.aborted_before && !row.signal.aborted_after));
  assert.ok(routes.every(row => row.state_after.pending_evaluations === 0
    && row.state_after.evaluation_subscribers === 0));
  assert.equal(assertionCalls.length, 19);
  const report = { schema_version: 1, kind: 'frozen_turn_routing_contract_execution', status: 'passed',
    baseline: before, baseline_manifest: manifest, baseline_files: baseline.files,
    source: { path: sourceFile, bytes: Buffer.byteLength(source), sha256: hash(source) },
    capture_source_sha256: scriptHash, verifier,
    node: { version: process.version, sha256: nodeHash },
    selected_definitions: selected.length, static_assertions: 19, expanded_assertions: assertionCalls.length,
    router_instances: routers.length, route_calls: routes.length, completion_calls: completes.length,
    evaluator_calls: evaluatorCalls.length, actual_network_calls: 0,
    transformations: 'Imports use verified frozen exports with transparent recording facades. Unselected definitions are removed; selected complete callbacks, helpers, loops, inputs, original assertion expressions, callback-local mock fetch and injected clock remain unchanged. Each source assertion result is counted after the original assertion succeeds.',
    adaptations: [
      'Only the two top-level measured latency fields are omitted from stable decisions after finite nonnegative validation. Every full decision, including both latency fields, is retained in the sibling observations artifact. No clock or timer is replaced.',
      'Undefined own properties use the reversible {$js_type: "undefined"} tag. Complete argument arrays preserve omitted completion evidence and all supplied values.',
      'The original injected clock is invoked exactly at its original call sites with the same receiver and arguments; observations record those returned values without additional clock reads. Default clocks remain unchanged.',
      'Response.json records its arguments and returns the original Response without cloning, consuming or replacing its body. Evaluator request signal identity is passed unchanged to the original mock; only type and before/after aborted state are recorded.',
      'Recorder failures are sticky across production fallback catches. Selected callbacks still receive original decision and response objects; no classifier or routing verdict is injected.',
    ],
    exclusions: 'Finite selected original callbacks only. No provider calls, runtime performance/timing parity, complete routing coverage or native equivalence claim.',
    definition_inventory: definitions.map(({ id, number, name, source_line, selected }) => ({ id, number, name, source_line, selected })),
    helpers, definitions: selected, assertion_calls: assertionCalls, configs, routers, routes,
    completes, evaluator_requests: evaluatorCalls, events };
  const encoded = `${JSON.stringify(report, null, 2)}\n`;
  const observations = `${JSON.stringify({ schema_version: 1, kind: 'frozen_turn_routing_raw_decisions',
    capture_sha256: hash(encoded), capture_source_sha256: scriptHash, decisions: rawDecisions }, null, 2)}\n`;
  assert.ok(Buffer.byteLength(encoded) <= 512 * 1024, 'Capture metadata bound');
  assert.ok(Buffer.byteLength(observations) <= 128 * 1024, 'Raw observation bound');
  await fs.writeFile(join(output, 'turn-routing-contracts.observations.json'), observations, { flag: 'wx', mode: 0o600 });
  await fs.writeFile(join(output, 'turn-routing-contracts.capture.json'), encoded, { flag: 'wx', mode: 0o600 });
  console.log(JSON.stringify({ output, definitions: 4, static_assertions: 19, expanded_assertions: 19,
    router_instances: routers.length, route_calls: routes.length, completion_calls: completes.length,
    evaluator_calls: evaluatorCalls.length, capture_sha256: hash(encoded), bytes: Buffer.byteLength(encoded),
    observations_sha256: hash(observations) }));
} catch (error) {
  await fs.writeFile(join(output, 'failure.json'), `${JSON.stringify({ status: 'failed',
    capture_source_sha256: scriptHash, source_test: current?.id,
    error: { name: error.name, message: error.message, stack: error.stack },
    recorder_fault: recorderFault?.message, assertion_calls: assertionCalls,
    routes, completes, evaluator_requests: evaluatorCalls, events, raw_decisions: rawDecisions }, null, 2)}\n`,
  { flag: 'wx', mode: 0o600 });
  throw error;
} finally { globalThis.fetch = originalFetch; }
