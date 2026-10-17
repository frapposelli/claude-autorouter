// Development-only execution of two complete frozen Router callbacks.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3]);
assert.equal(process.argv.length, 4, 'Pass frozen reference and new output directory');
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const baseline = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json'), 'utf8'));
const before = await verifyBaseline(reference);
assert.equal(before.files_verified, 111);
const scriptHash = hash(readFileSync(import.meta.filename));
const nodeHash = hash(readFileSync(process.execPath));
const ts = createRequire(join(root, 'package.json'))('typescript');
const routing = await import(pathToFileURL(join(reference, 'src/router.mjs')).href);
const configuration = await import(pathToFileURL(join(reference, 'src/config.mjs')).href);
const configInputs = new WeakMap(), responses = new WeakMap();
const cases = [], definitions = [], sources = [], rawDecisions = [], assertionCalls = [];
const instances = [];
let current, fault;
const originalFetch = globalThis.fetch;
function observe(action) {
  try { return action(); } catch (error) { fault ??= error; throw error; }
}
function healthy() { if (fault) throw fault; }
function snapshot(value, depth = 0) {
  assert.ok(depth < 32, 'Capture depth bound');
  if (value === undefined) return { $js_type: 'undefined' };
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return value;
  if (typeof value === 'number') {
    assert.ok(Number.isFinite(value) && !Object.is(value, -0)); return value;
  }
  assert.equal(typeof value, 'object');
  if (Array.isArray(value)) return value.map(item => snapshot(item, depth + 1));
  assert.equal(Object.getPrototypeOf(value), Object.prototype);
  const entries = Object.entries(Object.getOwnPropertyDescriptors(value));
  assert.ok(entries.length < 256);
  return Object.fromEntries(entries.filter(([, d]) => d.enumerable).map(([key, d]) => {
    assert.ok(Object.hasOwn(d, 'value')); return [key, snapshot(d.value, depth + 1)];
  }));
}
class CapturedResponse extends Response {
  static json(...args) {
    const result = Response.json(...args);
    observe(() => { responses.set(result, snapshot(args)); });
    return result;
  }
}
function readConfig(...args) {
  const result = configuration.readConfig(...args);
  observe(() => { configInputs.set(result, snapshot(args)); });
  return result;
}
class Router extends routing.Router {
  constructor(config, options) {
    const row = observe(() => {
      assert.ok(cases.length < 16); assert.equal(typeof options.fetchImpl, 'function');
      assert.ok(configInputs.has(config));
      const result = { id: `jev-original-${cases.length + 1}`, source_test: current.id,
        config_arguments: configInputs.get(config), config_snapshot: snapshot(config), requests: [] };
      cases.push(result); return result;
    });
    super(config, { ...options, fetchImpl: async (...args) => {
      const request = observe(() => {
        healthy(); assert.ok(row.requests.length < 2);
        const [url, options] = args;
        const result = { url, method: options.method, redirect: options.redirect,
          headers: snapshot(options.headers), body: options.body,
          signal_aborted_before: options.signal.aborted };
        assert.equal(typeof result.body, 'string'); assert.ok(Buffer.byteLength(result.body) < 65536);
        row.requests.push(result); return result;
      });
      const response = await options.fetchImpl(...args);
      observe(() => {
        healthy(); assert.ok(responses.has(response));
        request.response_arguments = responses.get(response); request.status = response.status;
        request.signal_aborted_after = args[1].signal.aborted;
      });
      return response;
    } });
    this.captureRow = row; instances.push(this);
  }
  async route(...args) {
    const row = this.captureRow;
    observe(() => {
      healthy(); assert.equal(args.length, 1); assert.equal(row.body_json, undefined);
      row.body_json = JSON.stringify(args[0]); row.before = snapshot(args);
    });
    const result = await super.route(...args);
    observe(() => {
      healthy(); row.after = snapshot(args); assert.deepEqual(row.after, row.before);
      row.body_after_json = JSON.stringify(args[0]);
      const raw = snapshot(result), stable = { ...raw };
      for (const key of ['latency_ms', 'evaluation_latency_ms']) {
        assert.ok(Number.isFinite(raw[key]) && raw[key] >= 0); delete stable[key];
      }
      row.decision = stable; rawDecisions.push({ id: row.id, decision: raw });
      row.pending_evaluations = this.pendingEvaluations.size;
      row.subscribers = this.evaluationSubscribers;
    });
    return result;
  }
}
function counted(id, value) {
  return observe(() => {
    healthy(); assert.ok(assertionCalls.length < 128);
    const site = current.assertions.find(site => site.id === id); assert.ok(site);
    site.expanded_executions += 1; assertionCalls.push(id); return value;
  });
}
mkdirSync(output, { mode: 0o700 });
const deadline = setTimeout(() => { throw new Error('Capture exceeded declared 30-second bound'); }, 30000);
globalThis.fetch = () => observe(() => { throw new Error('Unexpected actual network request'); });
try {
  for (const [file, chosen] of [['test/redaction.test.mjs', 8], ['test/routing-compatibility.test.mjs', 1]]) {
    const source = readFileSync(join(reference, file), 'utf8');
    assert.equal(hash(source), baseline.files.find(row => row.path === file)?.sha256);
    const syntax = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
    assert.equal(syntax.parseDiagnostics.length, 0);
    const edits = [], helpers = []; let number = 0;
    for (const statement of syntax.statements) {
      if (ts.isImportDeclaration(statement)) { edits.push({ start: statement.getStart(syntax), end: statement.end, text: '' }); continue; }
      if (!ts.isExpressionStatement(statement) || !ts.isCallExpression(statement.expression)
        || statement.expression.expression.getText(syntax) !== 'test') {
        helpers.push({ statement: statement.getText(syntax), sha256: hash(statement.getText(syntax)) }); continue;
      }
      number += 1;
      if (number !== chosen) { edits.push({ start: statement.getStart(syntax), end: statement.end, text: '' }); continue; }
      const call = statement.expression, sites = [];
      function visit(node) {
        if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)
          && node.expression.expression.getText(syntax) === 'assert') sites.push(node);
        ts.forEachChild(node, visit);
      }
      visit(call.arguments.at(-1));
      current = { id: `${file}#${number}`, name: call.arguments[0].text,
        statement: statement.getText(syntax), sha256: hash(statement.getText(syntax)),
        line: syntax.getLineAndCharacterOfPosition(statement.getStart(syntax)).line + 1,
        assertions: sites.map((site, index) => ({ id: `${file}#${number}:assert-${index + 1}`,
          expression: site.getText(syntax), sha256: hash(site.getText(syntax)),
          line: syntax.getLineAndCharacterOfPosition(site.getStart(syntax)).line + 1, expanded_executions: 0 })) };
      definitions.push(current);
      sites.forEach((site, index) => edits.push({ start: site.getStart(syntax), end: site.end,
        text: `__assertion(${JSON.stringify(current.assertions[index].id)}, ${site.getText(syntax)})` }));
    }
    assert.ok(number >= chosen);
    let transformed = source;
    for (const edit of edits.sort((a, b) => b.start - a.start)) transformed = transformed.slice(0, edit.start) + edit.text + transformed.slice(edit.end);
    let callback;
    const test = (name, fn) => { assert.equal(name, current.name); assert.equal(callback, undefined); callback = fn; };
    Function('test', 'assert', 'readConfig', 'Router', 'Response', '__assertion', `"use strict";\n${transformed}`)(test, assert, readConfig, Router, CapturedResponse, counted);
    assert.equal(typeof callback, 'function'); await callback(); healthy(); current.passed = true;
    sources.push({ path: file, bytes: Buffer.byteLength(source), sha256: hash(source), helpers });
  }
  assert.equal(cases.length, 13); assert.equal(definitions.length, 2); assert.equal(assertionCalls.length, 63);
  assert.ok(cases.every(row => row.requests.length === 1 && row.pending_evaluations === 0 && row.subscribers === 0));
  assert.ok(cases.every(row => row.requests.every(call => !call.signal_aborted_before && !call.signal_aborted_after)));
  // The frozen Router has no shutdown API; all original calls already drained.
  assert.deepEqual(await verifyBaseline(reference), before);
  assert.equal(hash(readFileSync(import.meta.filename)), scriptHash); assert.equal(hash(readFileSync(process.execPath)), nodeHash);
  const bytes = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
  const report = { schema_version: 1, status: 'passed', kind: 'frozen_jev_routing_original_contracts',
    baseline: before, node: { version: process.version, sha256: nodeHash }, generator_sha256: scriptHash,
    sources, definitions, static_assertions: 8, expanded_assertions: assertionCalls.length, assertion_calls: assertionCalls,
    routes: cases.length, evaluator_calls: 13, cases_sha256: hash(bytes), actual_network_calls: 0,
    transformations: 'Complete selected callbacks and all non-import helper statements retained. Only imports/unselected definitions removed; assertion result wrappers count successful unchanged assertions. Recording facades delegate frozen config/Router and original mock fetch callbacks without extra response consumption or timer/clock changes.',
    projections: ['Only top-level latency_ms/evaluation_latency_ms are removed after finite nonnegative validation; complete raw decisions retained separately.', 'Undefined config fields retain a tagged undefined value.'],
    scope: 'Original route-only privacy test does not invoke Anthropic; actual upstream preservation needs a separate supplemental native gateway control. No production/network/performance/native-equivalence claim from capture.' };
  const metadata = JSON.stringify(report, null, 2) + '\n';
  assert.ok(Buffer.byteLength(bytes) + Buffer.byteLength(metadata) < 1048576);
  for (const [file, data] of [['jev-routing-original-contracts.jsonl', bytes], ['jev-routing-original-contracts.capture.json', metadata], ['raw-decisions.json', JSON.stringify(rawDecisions, null, 2) + '\n']]) writeFileSync(join(output, file), data, { flag: 'wx', mode: 0o600 });
  console.log(JSON.stringify({ output, cases: 13, static_assertions: 8, expanded_assertions: 63, cases_sha256: hash(bytes), capture_sha256: hash(metadata) }));
} catch (error) {
  writeFileSync(join(output, 'failure.json'), JSON.stringify({ error: { name: error.name, message: error.message, stack: error.stack }, recorder_fault: fault?.message, cases, definitions, assertion_calls: assertionCalls, rawDecisions }, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
  throw error;
} finally { clearTimeout(deadline); globalThis.fetch = originalFetch; }
