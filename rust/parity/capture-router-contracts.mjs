// Execute all frozen router assertions; observe policy and transport separately.
// Every fetch/count callback remains the original synthetic baseline callback.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/router-contracts-${Date.now()}`));
assert.ok(process.argv.length <= 4, 'Usage: capture-router-contracts.mjs [frozen-reference] [new-output-directory]');
await verifyBaseline(reference);
const sourcePath = 'test/router.test.mjs';
const source = readFileSync(join(reference, sourcePath), 'utf8');
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const manifest = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json'), 'utf8'));
assert.equal(hash(source), manifest.files.find(row => row.path === sourcePath)?.sha256);
const routing = await import(pathToFileURL(join(reference, 'src/router.mjs')).href);
const configuration = await import(pathToFileURL(join(reference, 'src/config.mjs')).href);
const { createTokenCounter } = await import(pathToFileURL(join(reference, 'src/token-counter.mjs')).href);
const tests = [], cases = [], instances = [];
let current;
let routeSequence = 0;
const TURN_EPOCH_MS = 1800000000000;

function json(value, path = '$', ancestors = new Set()) {
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return JSON.stringify(value);
  if (typeof value === 'number') {
    assert.ok(Number.isFinite(value), `${path}: non-finite JSON input`);
    return JSON.stringify(value);
  }
  assert.equal(typeof value, 'object', `${path}: unsupported JSON input`);
  assert.ok(!ancestors.has(value), `${path}: circular JSON input`);
  assert.ok(Array.isArray(value) || Object.getPrototypeOf(value) === Object.prototype, `${path}: non-plain JSON input`);
  ancestors.add(value);
  const entries = [];
  for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(value))) {
    if (!descriptor.enumerable) continue;
    assert.ok(Object.hasOwn(descriptor, 'value'), `${path}.${key}: accessor`);
    entries.push([key, json(descriptor.value, `${path}.${key}`, ancestors)]);
  }
  ancestors.delete(value);
  if (Array.isArray(value)) {
    assert.equal(entries.length, value.length, `${path}: sparse/nonstandard array`);
    return `[${entries.map(([, encoded]) => encoded).join(',')}]`;
  }
  return `{${entries.map(([key, encoded]) => `${JSON.stringify(key)}:${encoded}`).join(',')}}`;
}
function tagged(value) {
  if (value === undefined) return { kind: 'undefined' };
  if (Number.isNaN(value)) return { kind: 'nan' };
  if (value === Infinity) return { kind: 'positive_infinity' };
  if (value === -Infinity) return { kind: 'negative_infinity' };
  return { kind: 'json', value: JSON.parse(json(value)) };
}
function decision(value) {
  const result = structuredClone(value);
  delete result.latency_ms;
  delete result.evaluation_latency_ms;
  return result;
}
function configSnapshot(config) {
  const values = {}, undefined_fields = [];
  for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(config))) {
    assert.ok(Object.hasOwn(descriptor, 'value'), 'Configuration getters require a separate boundary');
    if (descriptor.value === undefined) undefined_fields.push(key);
    else values[key] = descriptor.value;
  }
  return { values: JSON.parse(json(values)), undefined_fields };
}
function record(kind, input, expected) {
  const id = `baseline-router-${current.number}-${current.case_ids.length + 1}`;
  cases.push({ id, kind, input, node_expected: expected, source_tests: [current.id] });
  current.case_ids.push(id);
}
class Router extends routing.Router {
  constructor(config, options = {}) {
    const trace = { id: `baseline-router-instance-${instances.length + 1}`, source_test: current.id,
      config: configSnapshot(config), steps: [], now: TURN_EPOCH_MS };
    const originalFetch = options.fetchImpl;
    assert.equal(typeof originalFetch, 'function', 'Every captured Router must have an explicit synthetic fetch');
    // Freeze only the injected turn-state clock at each route's logical time.
    // The evaluator deadline, performance clock and classifier TTL stay real.
    super(config, { ...options, now: () => trace.now, fetchImpl: async (url, request) => {
      const step = trace.active;
      assert.ok(step, 'Unexpected evaluator work outside a captured route');
      const observation = { url, method: request.method, redirect: request.redirect,
        headers: structuredClone(request.headers), body: request.body };
      step.events.push('evaluate_start');
      step.evaluator_calls.push(observation);
      try {
        const response = await originalFetch(url, request);
        observation.status = response.status;
        observation.response = await response.clone().text();
        step.events.push('evaluate_resolve');
        return response;
      } catch (error) {
        observation.error_name = error?.name ?? 'Error';
        step.events.push('evaluate_reject');
        throw error;
      }
    } });
    this.trace = trace;
    instances.push(trace);
    current.instance_ids.push(trace.id);
  }
  async classify(...args) {
    const step = this.trace.active;
    step.events.push('classify_start');
    step.classify_calls += 1;
    const result = await super.classify(...args);
    assert.equal(step.classify_calls, 1);
    step.classification = structuredClone(result);
    step.events.push('classify_resolve');
    return result;
  }
  async route(body, options = {}) {
    const trace = this.trace;
    assert.ok(!trace.active, 'Overlapping source routes require a distinct schedule contract');
    // Capture at call time: tests intentionally mutate earlier bodies/configs.
    const step = { body_json: json(body), config: configSnapshot(this.config),
      options: {}, count_present: Object.hasOwn(options, 'countTokens'),
      now: TURN_EPOCH_MS + routeSequence++, events: [], evaluator_calls: [], count_calls: [], classify_calls: 0 };
    for (const [key, value] of Object.entries(options)) {
      if (key !== 'countTokens') step.options[key] = JSON.parse(json(value));
    }
    trace.now = step.now;
    trace.active = step;
    const forwarded = { ...options };
    if (step.count_present) {
      assert.equal(typeof options.countTokens, 'function');
      forwarded.countTokens = async (received, model) => {
        step.events.push('count_start');
        const call = { model, received_sha256: hash(json(received)) };
        step.count_calls.push(call);
        // The original callback is invoked before the wrapper's first await.
        try {
          const outcome = options.countTokens(received, model);
          call.outcome = tagged(await outcome);
          step.events.push('count_resolve');
          return await outcome;
        } catch (error) {
          call.outcome = { kind: 'throw', name: error?.name ?? 'Error' };
          step.events.push('count_reject');
          throw error;
        }
      };
    }
    try {
      const result = await super.route(body, forwarded);
      step.node_expected = decision(result);
      step.body_after_sha256 = hash(json(body));
      step.body_before_sha256 = hash(step.body_json);
      assert.equal(step.body_after_sha256, step.body_before_sha256, 'Source Router mutated a baseline request');
      trace.steps.push(step);
      return result;
    } finally { delete trace.active; }
  }
}
function buildState(...args) {
  const result = routing.buildState(...args);
  record('build_state', { body_json: json(args[0]), limit: args[1] ?? 12000 }, JSON.stringify(result));
  return result;
}
function contextSizeBytes(...args) {
  const result = routing.contextSizeBytes(...args);
  record('context_size', { body_json: json(args[0]), model: args[1] ?? args[0].model }, result);
  return result;
}
function readConfig(...args) {
  try { return configuration.readConfig(...args); }
  catch (error) {
    record('read_config_error', { env: JSON.parse(json(args[0])) }, { message: error.message });
    throw error;
  }
}
function test(name, callback) {
  tests.push({ id: `${sourcePath}#${tests.length + 1}`, number: tests.length + 1, name, callback, case_ids: [], instance_ids: [] });
}
let body = source;
for (const line of ["import test from 'node:test';", "import assert from 'node:assert/strict';",
  "import { readConfig } from '../src/config.mjs';", "import { Router, buildState, contextSizeBytes } from '../src/router.mjs';"]) {
  assert.equal(body.split(line).length, 2, 'Frozen import topology changed');
  body = body.replace(line, '');
}
assert.doesNotMatch(body, /^\s*import\b/m);
Function('test', 'assert', 'readConfig', 'Router', 'buildState', 'contextSizeBytes', `"use strict";\n${body}`)(test, assert, readConfig, Router, buildState, contextSizeBytes);
assert.equal(tests.length, 51);
for (const definition of tests) {
  current = definition;
  await definition.callback();
}
for (const trace of instances) {
  assert.equal(trace.active, undefined);
  // Observe the frozen counter's actual preparation separately. The original
  // Router fixture injects a value callback, whereas native Router owns HTTP
  // counting. This reference does not change the original callback schedule.
  for (const step of trace.steps) for (const call of step.count_calls) {
    let calls = 0;
    const countTokens = createTokenCounter(step.config.values, { fetchImpl: async (url, request) => {
      calls += 1;
      call.prepared_request = { url, method: request.method,
        headers: Object.fromEntries(request.headers), body_json: request.body };
      return Response.json({ input_tokens: 0 });
    } });
    assert.equal(await countTokens(JSON.parse(step.body_json), call.model), 0);
    assert.equal(calls, 1, 'Every native counting adaptation must have a source-observed request');
  }
  delete trace.now;
  cases.push({ id: trace.id, kind: 'router', input: trace, source_tests: [trace.source_test] });
}
// Fixed-size, lossless storage dictionary: repeated synthetic catalogs must not
// add tens of megabytes to the source bundle. This changes no semantic input.
const chunks = [], chunkIds = new Map();
function packBody(input) {
  const original = input.body_json;
  const ids = [];
  for (let offset = 0; offset < original.length; offset += 4096) {
    const text = original.slice(offset, offset + 4096);
    if (!chunkIds.has(text)) { chunkIds.set(text, chunks.length); chunks.push(text); }
    ids.push(chunkIds.get(text));
  }
  assert.equal(ids.map(id => chunks[id]).join(''), original);
  input.body_chunks = ids;
  input.body_sha256 = hash(original);
  delete input.body_json;
}
for (const row of cases) {
  if (row.kind === 'router') for (const step of row.input.steps) {
    packBody(step);
    for (const call of step.count_calls) packBody(call.prepared_request);
  }
  else if (row.input.body_json !== undefined) packBody(row.input);
}
const dictionary = { kind: 'string_dictionary', chunk_code_units: 4096, chunks };
const bytes = [dictionary, ...cases].map(row => JSON.stringify(row)).join('\n') + '\n';
const boundaries = instances.flatMap(instance => instance.steps.flatMap((step, route) => step.count_calls.flatMap((call, count) =>
  call.outcome.kind === 'json' ? [] : [{ source_test: instance.source_test, instance: instance.id, route, count, outcome: call.outcome }])));
const report = { schema_version: 1, kind: 'frozen_router_contract_capture', passed: true,
  baseline_commit: manifest.baseline_commit, source_path: sourcePath, source_sha256: hash(source),
  generator_sha256: hash(readFileSync(new URL(import.meta.url))), node_version: process.version,
  definitions: tests.length, instances: instances.length, routes: instances.reduce((total, row) => total + row.steps.length, 0),
  cases: cases.length, dictionary_chunks: chunks.length, cases_sha256: hash(bytes), tests: tests.map(({ callback, ...row }) => row), boundaries,
  turn_clock: { epoch_ms: TURN_EPOCH_MS, advance_ms_per_route: 1, scope: 'Only the documented injected TurnState clock; evaluator deadlines, performance clock and classifier cache TTL remain real.' },
  limits: 'All51 original callbacks and assertions execute, using original synthetic evaluator/count callbacks. Full per-instance policy schedules retain observed classifier outcomes, payloads, call counts and callback event order. Core replay of those outcomes proves policy only, never native classifier/cache/transport behavior. Input after-hashes are observed after the actual source route and must be checked after native execution. Turn-state clock is injected and fixed at each deterministic route timestamp; evaluator deadline, performance clock and cache TTL remain real. Only latency_ms/evaluation_latency_ms are excluded from exact decision comparison. Non-JSON count outcomes are tagged, never dropped; native callback-type and start-order adaptations require separate review. No forwarding claim comes from request preservation.' };
mkdirSync(output, { mode: 0o700 });
writeFileSync(join(output, 'cases.jsonl'), bytes, { flag: 'wx', mode: 0o600 });
writeFileSync(join(output, 'capture.json'), JSON.stringify(report, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify({ passed: true, definitions: tests.length, instances: instances.length, routes: report.routes, cases: cases.length, boundaries: boundaries.length, output }));
