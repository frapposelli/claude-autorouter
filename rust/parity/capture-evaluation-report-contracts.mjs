// Execute the unchanged frozen callbacks; observe reports and synthetic I/O.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/evaluation-report-contracts-${Date.now()}`));
assert.ok(process.argv.length <= 4);
await verifyBaseline(reference);
const sourcePath = 'test/evaluation-report.test.mjs';
const source = readFileSync(join(reference, sourcePath), 'utf8');
const hash = value => createHash('sha256').update(value).digest('hex');
const manifest = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json'), 'utf8'));
assert.equal(hash(source), manifest.files.find(row => row.path === sourcePath).sha256);
const load = path => import(pathToFileURL(join(reference, path)).href);
const [reports, evaluation, ollama, routing, configuration] = await Promise.all([
  load('src/evaluation-report.mjs'), load('scripts/evaluate.mjs'), load('scripts/evaluate-ollama.mjs'),
  load('src/router.mjs'), load('src/config.mjs'),
]);
const tests = [], cases = [], configs = new WeakMap();
let current, activeRun;

function snapshot(value) {
  const tags = [], ancestors = new Set();
  function visit(item, path) {
    if (item === undefined || (typeof item === 'number' && !Number.isFinite(item))) {
      tags.push({ path, kind: item === undefined ? 'undefined' : Number.isNaN(item) ? 'nan' : item > 0 ? 'positive_infinity' : 'negative_infinity' });
      return item === undefined ? undefined : null;
    }
    if (item === null || ['string', 'number', 'boolean'].includes(typeof item)) return item;
    assert.equal(typeof item, 'object', `${path}: unsupported input`);
    assert.ok(!ancestors.has(item), `${path}: circular input`);
    assert.ok(Array.isArray(item) || Object.getPrototypeOf(item) === Object.prototype);
    ancestors.add(item);
    const result = Array.isArray(item) ? [] : {};
    for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(item))) {
      if (!descriptor.enumerable) continue;
      assert.ok(Object.hasOwn(descriptor, 'value'), `${path}.${key}: getter`);
      const field = visit(descriptor.value, `${path}.${key}`);
      if (Array.isArray(item)) result.push(field === undefined ? null : field);
      else if (field !== undefined) result[key] = field;
    }
    if (Array.isArray(item)) assert.equal(result.length, item.length, 'Sparse array');
    ancestors.delete(item);
    return result;
  }
  return { value: visit(value, '$'), tags };
}
function record(op, input) {
  const captured = snapshot(input);
  const row = { id: `baseline-evaluation-report-${current.number}-${current.case_ids.length + 1}`,
    op, input: captured.value, input_tags: captured.tags, source_tests: [current.id] };
  if (captured.tags.some(tag => tag.kind !== 'undefined')) row.native_boundary = 'Nonfinite JavaScript policy API value';
  cases.push(row); current.case_ids.push(row.id);
  return row;
}
function success(row, result) {
  const captured = snapshot(result);
  row.node_expected = { ok: true, result: captured.value };
  row.result_tags = captured.tags;
}
function failure(row, error) {
  row.node_expected = { ok: false, error: error.message };
  row.error_name = error.name;
}
function pure(op, fn, input) {
  return (...args) => {
    const row = record(op, input(args));
    row.arguments_count = args.length;
    try { const result = fn(...args); success(row, result); return result; }
    catch (error) { failure(row, error); throw error; }
  };
}
function withoutTime(result) {
  const copy = structuredClone(result);
  delete copy.routing_p50_ms; delete copy.routing_p95_ms;
  for (const row of copy.rows ?? []) delete row.ms;
  return copy;
}
class Router extends routing.Router {
  constructor(config, options = {}) {
    const trace = activeRun;
    assert.ok(trace, 'Router construction outside captured run');
    assert.equal(typeof options.fetchImpl, 'function', 'Only original synthetic callbacks may run');
    const original = options.fetchImpl;
    super(config, { ...options, fetchImpl: async (url, request) => {
      const call = { url, method: request.method, redirect: request.redirect, headers: structuredClone(request.headers), body: request.body };
      trace.requests.push(call);
      try {
        const response = await original(url, request);
        call.status = response.status; call.response = await response.clone().text();
        return response;
      } catch (error) { call.error_name = error.name; throw error; }
    } });
    trace.router_instances += 1;
  }
}
async function runEvaluation(options) {
  const env = configs.get(options.config);
  assert.ok(env, 'Untracked configuration');
  const row = record('run_evaluation', { env, cases: options.cases, policy: options.policy ?? { profile: options.config.clientProfile } });
  row.requests = []; row.router_instances = 0; row.factory_calls = 0;
  const before = JSON.stringify(options.cases);
  row.cases_before_sha256 = hash(before);
  const original = options.routerFactory;
  assert.equal(typeof original, 'function');
  activeRun = row;
  try {
    const result = await evaluation.runEvaluation({ ...options, routerFactory: () => { row.factory_calls += 1; return original(); } });
    success(row, withoutTime(result));
    return result;
  } catch (error) { failure(row, error); throw error; }
  finally {
    row.cases_after_sha256 = hash(JSON.stringify(options.cases));
    assert.equal(row.cases_after_sha256, row.cases_before_sha256, 'Source mutated input cases');
    activeRun = undefined;
  }
}
function test(name, callback) {
  tests.push({ id: `${sourcePath}#${tests.length + 1}`, number: tests.length + 1, name, callback, case_ids: [], executed_assertions: 0 });
}
const trackedAssert = new Proxy(assert, { get(target, key) {
  const original = target[key];
  return typeof original === 'function' ? (...args) => { current.executed_assertions += 1; return original(...args); } : original;
} });
const values = { test, assert: trackedAssert, readFile, Router, runEvaluation,
  readConfig(env) { const result = configuration.readConfig(env); configs.set(result, structuredClone(env)); return result; },
  createEvaluationPolicy: pure('evaluation_policy', reports.createEvaluationPolicy, args => args[0] ?? {}),
  evaluateRoutingReport: pure('routing_report', reports.evaluateRoutingReport, args => ({ rows: args[0], options: args[1] ?? {} })),
  evaluateLiveCase: pure('live_case', reports.evaluateLiveCase, args => args[0]),
  evaluateLiveReport: pure('live_report', reports.evaluateLiveReport, args => ({ cases: args[0], options: args[1] ?? {} })),
  parseEvaluationArgs: pure('parse_evaluation_args', evaluation.parseEvaluationArgs, args => args[0]),
  parseOllamaEvaluationArgs: pure('parse_ollama_evaluation_args', ollama.parseOllamaEvaluationArgs, args => args[0]),
};
let body = source;
for (const line of source.split('\n').filter(line => line.startsWith('import '))) {
  assert.equal(body.split(line).length, 2);
  body = body.replace(line, '');
}
assert.doesNotMatch(body, /^\s*import\b/m);
const temporary = mkdtempSync(join(tmpdir(), 'autorouter-evaluation-capture-'));
const key = `autorouter-evaluation-capture:${temporary}`;
try {
  // Keep callback source unchanged, including import.meta.url. Its sole relative
  // input is an exact hash-verified copy of the frozen public routing fixture.
  mkdirSync(join(temporary, 'fixtures'));
  copyFileSync(join(reference, 'test/fixtures/routing.json'), join(temporary, 'fixtures/routing.json'));
  assert.equal(hash(readFileSync(join(temporary, 'fixtures/routing.json'))), manifest.files.find(row => row.path === 'test/fixtures/routing.json').sha256);
  globalThis[Symbol.for(key)] = values;
  writeFileSync(join(temporary, 'capture.mjs'), `const { ${Object.keys(values).join(', ')} } = globalThis[Symbol.for(${JSON.stringify(key)})];\n${body}`);
  await import(pathToFileURL(join(temporary, 'capture.mjs')).href);
  assert.equal(tests.length, 16);
  for (const definition of tests) { current = definition; await definition.callback(); }
} finally { delete globalThis[Symbol.for(key)]; rmSync(temporary, { recursive: true, force: true }); }
assert.equal(cases.length, 104);
assert.equal(tests.reduce((sum, row) => sum + row.executed_assertions, 0), 147);
assert.equal(cases.filter(row => row.native_boundary).length, 2);
assert.equal(cases.reduce((sum, row) => sum + (row.requests?.length ?? 0), 0), 45);
const serialized = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
mkdirSync(output, { recursive: false });
writeFileSync(join(output, 'cases.jsonl'), serialized);
const captured = { schema_version: 1, baseline_commit: manifest.baseline_commit, baseline_test: sourcePath,
  baseline_sha256: hash(source), generator_sha256: hash(readFileSync(import.meta.filename)), cases_sha256: hash(serialized),
  definitions: tests.length, executed_assertions: 147, cases: cases.length, requests: 45,
  tests: tests.map(({ callback, ...row }) => row),
  limits: 'All original callbacks and assertions run unchanged with original synthetic evaluator callbacks. Pure/parser result comparisons use production JSON report serialization; own-undefined inputs/results remain separately tagged. Two nonfinite policy API calls and JS Object.freeze/factory identity assertions remain explicit migration boundaries. Native runtime replay must invoke actual Router and compare every request, report and after-input hash; captured classifications must never be supplied as native answers. Only rows[].ms/routing_p50_ms/routing_p95_ms are removed from runtime report comparison. No model, evaluator service, provider or hardware benchmark is invoked.' };
writeFileSync(join(output, 'capture.json'), `${JSON.stringify(captured, null, 2)}\n`);
console.log(JSON.stringify({ definitions: 16, assertions: 147, cases: 104, requests: 45, cases_sha256: captured.cases_sha256 }));
