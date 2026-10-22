// Development-only execution of three complete frozen identity/signed-safety HTTP server callbacks.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { gzipSync } from 'node:zlib';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2]);
const output = resolve(process.argv[3]);
assert.equal(process.argv.length, 4, 'Pass frozen reference and a new output directory');
const hash = value => createHash('sha256').update(value).digest('hex');
const before = await verifyBaseline(reference);
assert.equal(before.files_verified, 111);
const scriptHash = hash(readFileSync(import.meta.filename)), nodeHash = hash(readFileSync(process.execPath));
const ts = createRequire(join(root, 'package.json'))('typescript');
const sourcePath = 'test/server.test.mjs', source = readFileSync(join(reference, sourcePath), 'utf8');
const baseline = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json')));
assert.equal(hash(source), baseline.files.find(row => row.path === sourcePath)?.sha256);
const syntax = ts.createSourceFile(sourcePath, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
assert.equal(syntax.parseDiagnostics.length, 0);
const [configuration, gateway, routing] = await Promise.all(['config', 'server', 'router'].map(file => import(pathToFileURL(join(reference, `src/${file}.mjs`)).href)));
const originalFetch = globalThis.fetch;
const raw = [], definitions = [], helpers = [], assertionCalls = [], projections = [];
let current, fault;
const unhandled = error => { fault ??= error instanceof Error ? error : Error(String(error)); };
process.on('unhandledRejection', unhandled);
const servers = new Map();
function observe(fn) { try { return fn(); } catch (error) { fault ??= error; throw error; } }
function healthy() { if (fault) throw fault; }
function snap(value, depth = 0) {
  assert.ok(depth < 40);
  if (value === undefined) return { $js_type: 'undefined' };
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return value;
  if (typeof value === 'number') { assert.ok(Number.isFinite(value)); return value; }
  assert.equal(typeof value, 'object');
  if (Array.isArray(value)) return value.map(item => snap(item, depth + 1));
  assert.ok(Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null);
  const entries = Object.entries(Object.getOwnPropertyDescriptors(value)); assert.ok(entries.length < 256);
  return Object.fromEntries(entries.filter(([, descriptor]) => descriptor.enumerable).map(([key, descriptor]) => {
    assert.ok(Object.hasOwn(descriptor, 'value')); return [key, snap(descriptor.value, depth + 1)];
  }));
}
function utf8(buffer) { const value = buffer.toString('utf8'); assert.ok(Buffer.from(value).equals(buffer), 'Finite fixtures are exact UTF-8'); return value; }
function bytes(value, encoding) {
  if (value === undefined || value === null || typeof value === 'function') return '';
  const result = typeof value === 'string' ? Buffer.from(value, typeof encoding === 'string' ? encoding : undefined) : Buffer.from(value);
  assert.ok(result.length <= 2 * 1024 * 1024); return result.toString('base64');
}
function event(kind, fields = {}) { observe(() => { assert.ok(current.events.length < 1024); current.events.push({ sequence: current.events.length, kind, ...fields }); }); }
function register(server, role) {
  const owner = current, row = { role, address: null, requests: [], closed: false, sockets: new Set() };
  assert.ok(owner.servers.length < 3); owner.servers.push(row); servers.set(server, row);
  server.on('connection', socket => { observe(() => { assert.ok(row.sockets.size < 12); row.sockets.add(socket); }); socket.once('close', () => row.sockets.delete(socket)); });
  server.once('close', () => { row.closed = true; });
  server.on('error', unhandled);
  owner.owners.push(server); return server;
}
const httpFacade = { ...http, createServer(handler) {
  const role = 'upstream';
  let row;
  const server = http.createServer((req, res) => {
    const record = observe(() => {
      healthy(); assert.ok(row.requests.length < 8);
      const value = { method: req.method, path: req.url, headers: snap(req.headers), request_chunks: [], response_writes: [], response_status: null, response_headers: null };
      row.requests.push(value); event('peer-request', { role, index: row.requests.length - 1 }); return value;
    });
    const iterator = req[Symbol.asyncIterator].bind(req);
    req[Symbol.asyncIterator] = async function* () {
      for await (const chunk of iterator()) {
        observe(() => { assert.ok(record.request_chunks.length < 256); record.request_chunks.push(bytes(chunk)); }); yield chunk;
      }
    };
    const write = res.write.bind(res), end = res.end.bind(res), writeHead = res.writeHead.bind(res);
    res.writeHead = (...args) => {
      observe(() => { record.response_status = args[0]; record.response_headers = snap(typeof args[1] === 'string' ? args[2] : args[1]); }); return writeHead(...args);
    };
    res.write = (...args) => { observe(() => { assert.ok(record.response_writes.length < 16); record.response_writes.push({ op: 'write', base64: bytes(args[0], args[1]) }); event('peer-write', { role }); }); return write(...args); };
    res.end = (...args) => { observe(() => { assert.ok(record.response_writes.length < 16); record.response_writes.push({ op: 'end', base64: bytes(args[0], args[1]) }); event('peer-end', { role }); }); return end(...args); };
    try { Promise.resolve(handler(req, res)).catch(error => { fault ??= error; res.destroy(); }); }
    catch (error) { fault ??= error; res.destroy(); }
  });
  register(server, role); row = servers.get(server); return server;
} };
async function listen(...args) {
  const address = await gateway.listen(...args);
  observe(() => { assert.equal(address.address, '127.0.0.1'); servers.get(args[0]).address = snap(address); }); return address;
}
function readConfig(...args) { const result = configuration.readConfig(...args); observe(() => current.config_arguments.push(snap(args))); return result; }
function createRouterServer(config, options = {}) {
  observe(() => current.gateway_config = snap(config));
  const wrapped = { ...options };
  for (const name of ['log', 'onStatus', 'onDecision', 'onRecord']) {
    if (options[name]) wrapped[name] = (...args) => { observe(() => { assert.ok(current.sinks[name].length < 128); current.sinks[name].push(snap(args[0])); }); return options[name](...args); };
  }
  if (options.router) {
    const router = options.router;
    wrapped.router = { ...router, route(...args) {
      const row = observe(() => {
        const [document, context] = args;
        assert.ok(current.route_calls.length < 8);
        const options = Object.fromEntries(Object.entries(Object.getOwnPropertyDescriptors(context)).filter(([, value]) => value.enumerable).map(([key, descriptor]) => {
          assert.ok(Object.hasOwn(descriptor, 'value'));
          const value = descriptor.value;
          if (key === 'signal') { assert.ok(value instanceof AbortSignal); return [key, { kind: 'AbortSignal', aborted: value.aborted }]; }
          if (key === 'countTokens') { assert.equal(typeof value, 'function'); return [key, { kind: 'function' }]; }
          return [key, snap(value)];
        }));
        const row = { document: snap(document), options }; current.route_calls.push(row); return row;
      });
      const result = Reflect.apply(router.route, router, args);
      Promise.resolve(result).then(value => observe(() => { row.decision = snap(value); row.signal_aborted_after = args[1].signal.aborted; }), unhandled).catch(unhandled);
      return result;
    }, complete(...args) {
      const row = observe(() => { assert.ok(current.completions.length < 32); const row = { request_id: args[0], evidence: snap(args[1]) }; current.completions.push(row); return row; });
      const result = Reflect.apply(router.complete, router, args); observe(() => { row.result = snap(result); }); return result;
    } };
  }
  return register(gateway.createRouterServer(config, wrapped), 'gateway');
}
class Router extends routing.Router {
  constructor(config, options) {
    observe(() => { current.router_config = snap(config); assert.equal(typeof options.fetchImpl, 'function'); });
    super(config, { ...options, fetchImpl: async (...args) => {
      const row = observe(() => { assert.ok(current.mocked_fetch.length < 4); const [url, options] = args; const value = { url, method: options.method, headers: snap(options.headers), body: options.body, reads: [], signal_aborted_before: options.signal.aborted }; current.mocked_fetch.push(value); return value; });
      const result = await options.fetchImpl(...args);
      observe(() => { row.status = result.status; row.signal_aborted_after = args[1].signal.aborted; }); return recordResponse(result, row);
    } });
  }
}
async function capturedFetch(...args) {
  const row = observe(() => {
    healthy(); const url = new URL(args[0]); assert.equal(url.hostname, '127.0.0.1');
    const owner = current.servers.find(server => server.address?.port === Number(url.port)); assert.ok(owner, 'Unexpected external HTTP request');
    const options = args[1] ?? {}, value = { role: owner.role, url: String(url), method: options.method ?? 'GET', headers: options.headers instanceof Headers ? Object.fromEntries(options.headers) : snap(options.headers ?? {}), body: options.body === undefined ? null : String(options.body), reads: [] };
    assert.ok(current.fetches.length < 16); current.fetches.push(value); return value;
  });
  const response = await originalFetch(...args);
  return recordResponse(response, row);
}
function recordResponse(response, row) {
  observe(() => { row.status = response.status; row.response_headers = Object.fromEntries(response.headers); });
  for (const name of ['text', 'json', 'arrayBuffer']) {
    const original = response[name].bind(response);
    response[name] = async (...args) => { const value = await original(...args); observe(() => row.reads.push(name === 'arrayBuffer' ? { kind: name, base64: Buffer.from(value).toString('base64') } : { kind: name, value: snap(value) })); return value; };
  }
  if (response.body) {
    const getReader = response.body.getReader.bind(response.body);
    response.body.getReader = (...args) => {
      const reader = getReader(...args), read = reader.read.bind(reader);
      reader.read = async (...args) => {
        const value = await read(...args); observe(() => { assert.ok(row.reads.length < 256); row.reads.push({ kind: 'read', done: value.done, base64: bytes(value.value) }); event('fetch-read', { role: row.role, done: value.done }); }); return value;
      }; return reader;
    };
  }
  return response;
}
function counted(id, value) {
  observe(() => { healthy(); const site = current.definition.assertions.find(row => row.id === id); assert.ok(site); site.executions++; assert.ok(assertionCalls.length < 256); assertionCalls.push(id); }); return value;
}
function find(node, predicate) { const results = []; const walk = item => { if (predicate(item)) results.push(item); ts.forEachChild(item, walk); }; walk(node); return results; }
const isTest = node => ts.isCallExpression(node) && node.expression.getText(syntax) === 'test';
let index = 0;
const edits = [], selected = new Set([15, 16, 17]);
for (const statement of syntax.statements) {
  if (ts.isImportDeclaration(statement)) { edits.push({ start: statement.getStart(syntax), end: statement.end, text: '' }); continue; }
  const calls = find(statement, isTest);
  if (!calls.length) { helpers.push({ statement: statement.getText(syntax), sha256: hash(statement.getText(syntax)) }); continue; }
  index++;
  if (!selected.has(index)) { edits.push({ start: statement.getStart(syntax), end: statement.end, text: '' }); continue; }
  assert.equal(calls.length, 1); const call = calls[0], sites = find(call.arguments.at(-1), node => ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression) && node.expression.expression.getText(syntax) === 'assert');
  const definition = { number: index, id: `${sourcePath}#${index}`, name: call.arguments[0].text, statement: statement.getText(syntax), sha256: hash(statement.getText(syntax)), assertions: sites.map((site, i) => ({ id: `server#${index}:assert-${i + 1}`, expression: site.getText(syntax), sha256: hash(site.getText(syntax)), line: syntax.getLineAndCharacterOfPosition(site.getStart(syntax)).line + 1, executions: 0 })) };
  definitions.push(definition);
  sites.forEach((site, i) => edits.push({ start: site.getStart(syntax), end: site.end, text: `__assertion(${JSON.stringify(definition.assertions[i].id)}, ${site.getText(syntax)})` }));
}
assert.equal(helpers.length, 5); assert.deepEqual(definitions.map(row => row.assertions.length), [18, 15, 19]);
let transformed = source;
for (const edit of edits.sort((a, b) => b.start - a.start)) transformed = transformed.slice(0, edit.start) + edit.text + transformed.slice(edit.end);
const callbacks = [];
const test = (name, options, callback) => { assert.equal(name, definitions[callbacks.length].name); const fn = typeof options === 'function' ? options : callback; definitions[callbacks.length].options = typeof options === 'function' ? null : snap(options); assert.equal(typeof fn, 'function'); callbacks.push(fn); };
Function('test', 'assert', 'http', 'gzipSync', 'readConfig', 'createRouterServer', 'listen', 'Router', '__assertion', `"use strict";\n${transformed}`)(test, assert, httpFacade, gzipSync, readConfig, createRouterServer, listen, Router, counted);
assert.equal(callbacks.length, 3);
mkdirSync(output, { mode: 0o700 });
function serializable(row) { const { owners, after, definition, ...result } = row; return { ...result, servers: row.servers.map(({ sockets, ...server }) => ({ ...server, remaining_sockets: sockets.size })) }; }
async function cleanup(row) {
  for (const fn of row.after) { try { await fn(); } catch (error) { fault ??= error; } }
  await Promise.all(row.owners.map(server => new Promise(resolve => {
    if (servers.get(server).closed) { resolve(); return; }
    server.once('close', resolve); server.closeAllConnections(); server.close();
  })));
  // close callbacks run before all owned socket close events on some Node versions.
  for (let i = 0; i < 100 && row.servers.some(server => server.sockets.size); i++) await new Promise(resolve => setTimeout(resolve, 5));
  assert.ok(row.servers.every(server => server.closed && server.sockets.size === 0), 'Owned server/socket cleanup');
}
function project(value, path, authorities, ids) {
  if (Array.isArray(value)) return value.map((v, i) => project(v, `${path}/${i}`, authorities, ids));
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, v]) => {
    const pointer = `${path}/${key}`;
    if (key.endsWith('latency_ms') || ['first_response_ms', 'total_latency_ms'].includes(key)) {
      if (v && typeof v === 'object' && v.$js_type === 'undefined') return [key, v];
      assert.ok(typeof v === 'number' && Number.isFinite(v) && v >= 0); projections.push({ path: pointer, raw: v, stable: 0, kind: 'validated_latency' }); return [key, 0];
    }
    if (key === 'request_id' || key === 'requestId') { assert.match(v, /^[0-9a-f]{8}-[0-9a-f-]{27}$/); if (!ids.has(v)) ids.set(v, `request-${ids.size + 1}`); projections.push({ path: pointer, raw: v, stable: ids.get(v), kind: 'generated_request_id' }); return [key, ids.get(v)]; }
    if (key === 'date') { assert.ok(typeof v === 'string' && Number.isFinite(Date.parse(v))); projections.push({ path: pointer, raw: v, stable: '<http-date>', kind: 'automatic_http_date' }); return [key, '<http-date>']; }
    return [key, project(v, pointer, authorities, ids)];
  }));
  if (typeof value === 'string') {
    let result = value;
    for (const [authority, name] of authorities) result = result.replaceAll(authority, name);
    if (result !== value) projections.push({ path, raw: value, stable: result, kind: 'loopback_authority' });
    return result;
  }
  return value;
}
try {
  globalThis.fetch = capturedFetch;
  for (let i = 0; i < callbacks.length; i++) {
    current = { number: definitions[i].number, id: `server-identity-safety-${definitions[i].number}`, source_test: definitions[i].id, definition: definitions[i], config_arguments: [], gateway_config: null, router_config: null, mocked_fetch: [], route_calls: [], completions: [], servers: [], fetches: [], events: [], sinks: { log: [], onStatus: [], onDecision: [], onRecord: [] }, owners: [], after: [] };
    raw.push(current);
    let timer;
    try { await Promise.race([callbacks[i]({ after(fn) { current.after.push(fn); } }), new Promise((_, reject) => { timer = setTimeout(() => reject(Error('Original callback exceeded 5-second bound')), 5000); })]); healthy(); current.definition.passed = true; }
    finally { clearTimeout(timer); await Promise.race([cleanup(current), new Promise((_, reject) => { timer = setTimeout(() => reject(Error('Owned cleanup exceeded 3-second bound')), 3000); })]).finally(() => clearTimeout(timer)); }
    healthy();
  }
  assert.equal(assertionCalls.length, 82); assert.deepEqual(definitions.map(row => row.assertions.reduce((n, site) => n + site.executions, 0)), [20, 26, 36]);
  const first = raw[0];
  const firstRead = first.events.findIndex(event => event.kind === 'fetch-read' && event.role === 'gateway' && !event.done);
  const finalWrite = first.events.findIndex(event => event.kind === 'peer-end' && event.role === 'upstream');
  assert.ok(firstRead >= 0 && finalWrite > firstRead, 'Original first-read must precede provider final write');
  const cases = raw.map(row => {
    const authorities = row.servers.map(server => [`127.0.0.1:${server.address.port}`, `${server.role}.invalid`]);
    const stable = { id: row.id, source_test: row.source_test, config_arguments: row.config_arguments, gateway_config: row.gateway_config, router_config: row.router_config, mocked_fetch: row.mocked_fetch, route_calls: row.route_calls, completions: row.completions,
      downstream: row.fetches.filter(fetch => fetch.role === 'gateway').map(({ url, method, headers, body, status, response_headers, reads }) => ({ url, method, headers, body, status, response_headers, reads })),
      peers: row.servers.filter(server => server.role !== 'gateway').map(server => ({ role: server.role, requests: server.requests.map(({ request_chunks, ...request }) => ({ ...request, request_body_utf8: utf8(Buffer.concat(request_chunks.map(chunk => Buffer.from(chunk, 'base64')))) })) })),
      logs: row.sinks.log, statuses: row.sinks.onStatus, ...(row.number === 15 ? { streaming_barrier: { first_read_before_final_write: true } } : {}) };
    return project(stable, row.id, authorities, new Map());
  });
  const corpus = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
  assert.ok(Buffer.byteLength(corpus) <= 4 * 1024 * 1024);
  assert.deepEqual(await verifyBaseline(reference), before); assert.equal(hash(readFileSync(import.meta.filename)), scriptHash); assert.equal(hash(readFileSync(process.execPath)), nodeHash);
  const report = { schema_version: 1, status: 'passed', kind: 'complete_frozen_server_identity_safety_callbacks', baseline: before, node: { version: process.version, sha256: nodeHash }, generator_sha256: scriptHash, baseline_file_sha256: hash(source), helpers, definitions, static_assertions: 52, expanded_assertions: 82, assertion_execution_order_file: 'assertion-executions.json', assertion_order_scope: 'Raw successful assertion order retained separately. Per-site and per-callback counts are exact; no scheduling or chunk partition equivalence is inferred.', cases_sha256: hash(corpus), callbacks_executed: 3, network_scope: 'Actual synthetic loopback HTTP only; original #16/#17 evaluator fetches remain supplied mocks.', capture_semantics: 'Complete selected callbacks and five helper statements restored unchanged except successful assertion-result wrappers. Recording delegates actual HTTP and original return objects; no clone/drain, substituted verdict, timing threshold or source clock changes. Per-peer complete bodies preserve bytes; raw chunks and chronological reads remain in raw observations.', projection_kinds: ['validated_latency', 'generated_request_id', 'automatic_http_date', 'loopback_authority'], typed_observations: ['Headers inputs are enumerated into their complete normalized field map.', 'Route signal observation records actual AbortSignal type and aborted state; no serialized object identity is claimed. countTokens is recorded as a function capability only.', 'Individual response chunks remain base64, including intentionally split UTF-8. Only complete request bodies use UTF-8 after exact roundtrip validation.'], raw_observations_file: 'raw-observations.json', projection_ledger_file: 'projections.json', cleanup: raw.map(row => ({ id: row.id, servers: row.servers.length, all_closed: row.servers.every(server => server.closed), remaining_sockets: row.servers.reduce((n, server) => n + server.sockets.size, 0) })) };
  for (const [name, data] of [['server-identity-safety-contracts.jsonl', corpus], ['server-identity-safety-contracts.capture.json', JSON.stringify(report, null, 2) + '\n'], ['raw-observations.json', JSON.stringify(raw.map(serializable), null, 2) + '\n'], ['assertion-executions.json', JSON.stringify(assertionCalls, null, 2) + '\n'], ['projections.json', JSON.stringify(projections, null, 2) + '\n']]) writeFileSync(join(output, name), data, { flag: 'wx', mode: 0o600 });
  console.log(JSON.stringify({ cases: 3, static_assertions: 52, expanded_assertions: 82, corpus_sha256: hash(corpus), output }));
} catch (error) {
  writeFileSync(join(output, 'failure.json'), JSON.stringify({ error: { name: error.name, message: error.message, stack: error.stack }, recorder_fault: fault?.message, definitions, assertionCalls, raw: raw.map(serializable) }, null, 2) + '\n', { flag: 'wx', mode: 0o600 }); throw error;
} finally { globalThis.fetch = originalFetch; process.removeListener('unhandledRejection', unhandled); }
