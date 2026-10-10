// Development-only capture: four unchanged frozen callbacks and original streams.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/local-setup-pull-${Date.now()}`));
assert.ok(process.argv.length <= 4, 'Usage: capture-local-setup-pull-contracts.mjs [reference] [new-output]');
const hash = value => createHash('sha256').update(value).digest('hex');
const before = await verifyBaseline(reference);
assert.equal(before.baseline_commit, 'ea930c247626ce2af5ccdad721b5121417bf4ad8');
assert.equal(before.files_verified, 111);
const baseline = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json')));
const sourceFile = 'test/ollama-setup.test.mjs';
const source = readFileSync(join(reference, sourceFile), 'utf8');
assert.equal(hash(source), baseline.files.find(row => row.path === sourceFile).sha256);
const ts = createRequire(join(root, 'package.json'))('typescript');
const syntax = ts.createSourceFile(sourceFile, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
assert.equal(syntax.parseDiagnostics.length, 0);
const selectedNumbers = [4, 6, 7, 13];
const bounds = { whole_run_ms: 60_000, cases: 32, requests_per_call: 16, request_bytes: 65_536,
  response_bytes: 1_048_576, row_bytes: 1_048_576, aggregate_bytes: 8_388_608,
  progress_per_call: 64, chunks_per_response: 64, assertions: 256, depth: 32, nodes: 4096 };
const definitions = [], edits = [], callbacks = [], cases = [], assertionCalls = [];
let current, nextFetch = 0, captureFailure;
const fetchIds = new WeakMap(), responses = new WeakMap(), streams = new WeakMap();
const scriptHash = hash(readFileSync(import.meta.filename));
const nodeHash = hash(readFileSync(process.execPath));
const descriptor = path => { const bytes = readFileSync(join(root, path)); return { path, bytes: bytes.length, sha256: hash(bytes) }; };
const helpers = [];
for (const statement of syntax.statements) {
  if (ts.isImportDeclaration(statement)) {
    edits.push({ start: statement.getStart(syntax), end: statement.end, text: '' });
    continue;
  }
  if (!ts.isExpressionStatement(statement) || !ts.isCallExpression(statement.expression)
    || statement.expression.expression.getText(syntax) !== 'test') {
    helpers.push({ text: statement.getText(syntax), sha256: hash(statement.getText(syntax)) });
    continue;
  }
  const call = statement.expression, number = definitions.length + 1, selected = selectedNumbers.includes(number);
  const sites = [];
  const visit = node => {
    if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)
      && node.expression.expression.getText(syntax) === 'assert') sites.push(node);
    ts.forEachChild(node, visit);
  };
  visit(call.arguments.at(-1));
  const row = { id: `${sourceFile}#${number}`, number, name: call.arguments[0].text, selected,
    statement: statement.getText(syntax), statement_sha256: hash(statement.getText(syntax)),
    source_line: syntax.getLineAndCharacterOfPosition(statement.getStart(syntax)).line + 1,
    assertions: sites.map((node, index) => ({ id: `${sourceFile}#${number}:assert-${index + 1}`,
      expression: node.getText(syntax), sha256: hash(node.getText(syntax)),
      line: syntax.getLineAndCharacterOfPosition(node.getStart(syntax)).line + 1, expanded_executions: 0 })) };
  definitions.push(row);
  if (!selected) edits.push({ start: statement.getStart(syntax), end: statement.end, text: '' });
  else for (const [index, node] of sites.entries()) {
    edits.push({ start: node.getStart(syntax), end: node.getStart(syntax), text: `__assertion(${JSON.stringify(row.assertions[index].id)}, ` });
    edits.push({ start: node.end, end: node.end, text: ')' });
  }
}
assert.equal(definitions.length, 14);
assert.equal(definitions.filter(row => row.selected).reduce((n, row) => n + row.assertions.length, 0), 28);
let transformed = source;
for (const edit of edits.sort((a, b) => b.start - a.start || b.end - a.end)) {
  transformed = transformed.slice(0, edit.start) + edit.text + transformed.slice(edit.end);
}
assert.doesNotMatch(transformed, /^\s*import\b/m);
function observe(action) {
  try { return action(); } catch (error) { captureFailure ??= error; throw error; }
}
function plain(value) {
  let nodes = 0;
  const visit = (value, depth) => {
    assert.ok(++nodes <= bounds.nodes && depth <= bounds.depth);
    if (value === null || typeof value === 'string' || typeof value === 'boolean') return value;
    if (typeof value === 'number') { assert.ok(Number.isFinite(value)); return value; }
    assert.equal(typeof value, 'object');
    assert.ok(Array.isArray(value) || Object.getPrototypeOf(value) === Object.prototype);
    if (Array.isArray(value)) return value.map(v => visit(v, depth + 1));
    const result = {};
    for (const [key, d] of Object.entries(Object.getOwnPropertyDescriptors(value))) {
      assert.ok(Object.hasOwn(d, 'value'), 'Capture must not invoke getters');
      if (d.enumerable) result[key] = visit(d.value, depth + 1);
    }
    return result;
  };
  return visit(value, 0);
}
function errorRecord(error) {
  assert.equal(error.name, 'Error');
  assert.equal(typeof error.message, 'string');
  assert.ok(error.code === undefined || typeof error.code === 'string');
  const row = { name: error.name, message: error.message, code_present: error.code !== undefined,
    code: error.code ?? null, cause_present: error.cause !== undefined };
  assert.ok(!JSON.stringify(row).includes('PRIVATE_') && !row.cause_present);
  return row;
}
function startCase(kind, fields) {
  assert.ok(cases.length < bounds.cases);
  const row = { id: `baseline-local-setup-pull-${current.number}-${current.case_ids.length + 1}`,
    source_test: current.id, kind, ...fields };
  cases.push(row); current.case_ids.push(row.id); return row;
}
function validator(kind, original) {
  return value => {
    const row = observe(() => startCase(kind, { input: plain(value) }));
    try {
      const result = original(value);
      observe(() => { row.node_expected = { ok: true, result: plain(result) }; });
      return result;
    } catch (error) {
      observe(() => { row.node_expected = { ok: false, error: errorRecord(error) }; });
      throw error;
    }
  };
}
const NativeStream = globalThis.ReadableStream, NativeResponse = globalThis.Response;
const ReadableStream = new Proxy(NativeStream, { construct(target, args) {
  return observe(() => {
    assert.equal(args.length, 1);
    const original = args[0];
    assert.deepEqual(Object.keys(original), ['start']);
    const record = { chunks_hex: [], total_bytes: 0, closed: false, starts: 0 };
    const result = Reflect.construct(target, [{ start(controller) {
      record.starts++;
      return Reflect.apply(original.start, original, [{ enqueue(bytes) {
        return observe(() => {
          assert.ok(bytes instanceof Uint8Array && record.chunks_hex.length < bounds.chunks_per_response);
          record.total_bytes += bytes.byteLength;
          assert.ok(record.total_bytes <= bounds.response_bytes);
          record.chunks_hex.push(Buffer.from(bytes).toString('hex'));
          return controller.enqueue(bytes);
        });
      }, close() { record.closed = true; return controller.close(); }, error(error) { return controller.error(error); } }]);
    } }], target);
    assert.equal(record.starts, 1); assert.ok(record.closed);
    streams.set(result, record); return result;
  });
} });
function remember(response, body) {
  observe(() => {
    const record = { status: response.status, headers: Object.fromEntries(response.headers), body };
    responses.set(response, record);
  });
  return response;
}
const Response = new Proxy(NativeResponse, {
  construct(target, args) {
    return observe(() => {
      assert.ok(streams.has(args[0]), 'Only original observed streams are accepted');
      return remember(Reflect.construct(target, args, target), { kind: 'stream', ...plain(streams.get(args[0])) });
    });
  },
  get(target, key) {
    if (key !== 'json') return Reflect.get(target, key);
    return (value, init) => observe(() => {
      const text = JSON.stringify(plain(value));
      assert.ok(Buffer.byteLength(text) <= bounds.response_bytes);
      return remember(target.json(value, init), { kind: 'json', text });
    });
  },
});
function operation(kind, original) {
  return async (config, options = {}) => {
    const row = observe(() => {
      assert.equal(typeof options.fetchImpl, 'function');
      assert.ok(Object.keys(options).every(key => ['pull', 'fetchImpl', 'write'].includes(key)));
      if (!fetchIds.has(options.fetchImpl)) fetchIds.set(options.fetchImpl, ++nextFetch);
      return startCase(kind, { config: plain(config), options: Object.hasOwn(options, 'pull') ? { pull: options.pull } : {},
        option_functions: Object.keys(options).filter(key => typeof options[key] === 'function'),
        fetch_id: fetchIds.get(options.fetchImpl), requests: [], progress: [] });
    });
    const fetchImpl = async (url, request) => {
      const call = observe(() => {
        assert.ok(row.requests.length < bounds.requests_per_call);
        assert.equal(request.redirect, 'error');
        assert.ok(Buffer.byteLength(request.body ?? '') <= bounds.request_bytes);
        const headers = Object.fromEntries(new Headers(request.headers));
        assert.ok(!('authorization' in headers) && !('x-api-key' in headers));
        assert.ok(request.signal instanceof AbortSignal && !request.signal.aborted);
        const call = { url: String(url), method: request.method, headers, redirect: request.redirect,
          signal: { present: true, aborted_at_request: false },
          body: request.body === undefined ? null : JSON.parse(request.body) };
        row.requests.push(call); return call;
      });
      const response = await options.fetchImpl(url, request);
      observe(() => { assert.ok(responses.has(response)); call.response = plain(responses.get(response)); });
      return response;
    };
    const write = line => {
      observe(() => {
        assert.ok(row.progress.length < bounds.progress_per_call);
        assert.equal(typeof line, 'string'); assert.ok(Buffer.byteLength(line) <= bounds.request_bytes);
        row.progress.push(line);
      });
      return options.write?.(line);
    };
    try {
      const result = await original(config, { ...options, fetchImpl, ...(kind === 'setup' ? { write } : {}) });
      observe(() => { row.node_expected = { ok: true, result: plain(result) }; });
      return result;
    } catch (error) {
      observe(() => { row.node_expected = { ok: false, error: errorRecord(error) }; });
      throw error;
    } finally { observe(() => { assert.deepEqual(plain(config), row.config, 'Original config remains unchanged'); }); }
  };
}
function test(name, ...args) {
  const original = definitions.filter(row => row.selected)[callbacks.length];
  assert.equal(name, original.name);
  callbacks.push({ ...original, case_ids: [], callback: args.at(-1) });
}
function counted(id, result) {
  observe(() => {
    assert.ok(assertionCalls.length < bounds.assertions);
    const site = current.assertions.find(row => row.id === id); assert.ok(site);
    site.expanded_executions++; assertionCalls.push(id);
  });
  return result;
}
mkdirSync(output);
writeFileSync(join(output, 'generator.mjs'), readFileSync(import.meta.filename), { flag: 'wx', mode: 0o600 });
writeFileSync(join(output, 'transformed.mjs'), transformed, { flag: 'wx', mode: 0o600 });
const realFetch = globalThis.fetch;
const deadline = setTimeout(() => { process.stderr.write('Setup capture whole-run deadline exceeded\n'); process.exit(1); }, bounds.whole_run_ms);
try {
  globalThis.fetch = () => { throw new Error('Capture forbids actual network'); };
  const setup = await import(pathToFileURL(join(reference, 'src/ollama-setup.mjs')).href);
  const models = await import(pathToFileURL(join(reference, 'src/ollama-models.mjs')).href);
  const evaluator = await import(pathToFileURL(join(reference, 'src/ollama-evaluator.mjs')).href);
  const values = { test, assert, __assertion: counted, Response, ReadableStream,
    inspectOllama: operation('inspect', setup.inspectOllama), setupOllama: operation('setup', setup.setupOllama),
    validateOllamaEndpoint: validator('endpoint', models.validateOllamaEndpoint),
    validateOllamaModel: validator('model', models.validateOllamaModel),
    DEFAULT_OLLAMA_MODEL: models.DEFAULT_OLLAMA_MODEL, OLLAMA_QUESTIONS: evaluator.OLLAMA_QUESTIONS };
  Function(...Object.keys(values), transformed)(...Object.values(values));
  assert.equal(callbacks.length, 4);
  for (current of callbacks) await current.callback();
  if (captureFailure) throw captureFailure;
  assert.ok(cases.every(row => row.node_expected));
  assert.ok(cases.every(row => (row.requests ?? []).every(request => request.response)));
  const serialized = cases.map(row => {
    const text = JSON.stringify(row); assert.ok(Buffer.byteLength(text) <= bounds.row_bytes); return text;
  }).join('\n') + '\n';
  assert.ok(Buffer.byteLength(serialized) <= bounds.aggregate_bytes);
  const after = await verifyBaseline(reference); assert.deepEqual(after, before);
  assert.equal(scriptHash, hash(readFileSync(import.meta.filename)));
  assert.equal(nodeHash, hash(readFileSync(process.execPath)));
  const report = { schema_version: 1, baseline_commit: before.baseline_commit, baseline_files_verified: before.files_verified,
    source_file: sourceFile, source_sha256: hash(source), generator_sha256: scriptHash, node_sha256: nodeHash,
    node_version: process.version, typescript_version: ts.version, verifier: descriptor('scripts/rust-reference.mjs'),
    baseline_manifest: descriptor('rust/parity/baseline.json'), cases_sha256: hash(serialized), selected_definitions: selectedNumbers,
    definitions: definitions.map(row => {
      const captured = callbacks.find(c => c.number === row.number);
      return captured ? Object.fromEntries(Object.entries(captured).filter(([key]) => key !== 'callback')) : row;
    }), helpers, counts: { cases: cases.length, fetch_groups: nextFetch, requests: cases.reduce((n, row) => n + (row.requests?.length ?? 0), 0),
      static_assertions: 28, executed_assertions: assertionCalls.length,
      response_streams: cases.flatMap(row => row.requests ?? []).filter(row => row.response.body.kind === 'stream').length },
    assertion_calls: assertionCalls, bounds,
    boundaries: ['Selected callbacks/helpers/assertion expressions unchanged except import rewiring and value-returning assertion wrappers; unselected definitions removed.',
      'Original stream start is forwarded to real controller enqueue/close/error without added body reads, clones, tees or cancellation. Recorded chunks are copies at original enqueue boundaries.',
      'Original real 10ms mock delay remains; no timers or clocks patched and no timing field normalized.',
      'Fetch redirect option and actual AbortSignal presence are recorded; native transport contract and CancellationToken APIs require explicit adaptation, not invented request fields.',
      'Source validator and invalid-inspect errors have no code; native validator String and inspect SetupError INVALID_CONFIGURATION are explicit envelope adaptations. Exact original safe message/rejection and zero I/O are compared.',
      'Private synthetic configuration fields remain supplied and unchanged; only actual request/progress projections may establish absence from outgoing data.',
      'No real model, service, provider, system configuration, download or numerical benchmark is invoked.'] };
  writeFileSync(join(output, 'cases.jsonl'), serialized, { flag: 'wx', mode: 0o600 });
  writeFileSync(join(output, 'capture.json'), JSON.stringify(report, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
  console.log(JSON.stringify({ ...report.counts, cases_sha256: report.cases_sha256 }));
} finally { globalThis.fetch = realFetch; clearTimeout(deadline); }
