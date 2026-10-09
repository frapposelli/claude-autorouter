// Paired executable resource contracts. All inputs/services are synthetic.
// Barriers establish ordering; timers only fail stalled fixtures. No timing,
// throughput, RSS or other performance acceptance is measured by this driver.
import assert from 'node:assert/strict';
import http from 'node:http';
import net from 'node:net';
import { once } from 'node:events';
import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { isDeepStrictEqual } from 'node:util';
import { transportHarness } from './transport-harness.mjs';

const harness = await transportHarness('autorouter-gateway-resources-');
const { root, token, identity, gateway, watch, close, cleanup } = harness;
const MODEL = 'claude-sonnet-5-5';
const SELECTED = 'claude-haiku-4-5-20251001';
const RESPONSE = Buffer.from(JSON.stringify({ type: 'message', model: SELECTED, stop_reason: 'end_turn', content: [], usage: { input_tokens: 32, output_tokens: 8 } }));
const DECISION = Buffer.from(JSON.stringify({ answers: { tier: { choice: 'haiku', confidence: 0.99 } } }));
const COUNT = Buffer.from('{"input_tokens":1000}');
const MAX_BODY = 1024 * 1024;
const DEADLINE = 10000;
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const scenarios = [
  { id: 'shared-evaluation-one-waiter-cancels', mode: 'one', large: true, expected: { evaluation: 1, count: 2, inference: 1 } },
  { id: 'shared-evaluation-last-waiter-cancels-and-retries', mode: 'last', large: true, expected: { evaluation: 2, count: 3, inference: 1 } },
  { id: 'shared-evaluation-completes-one-count-remains-cancellable', mode: 'count', large: true, expected: { evaluation: 1, count: 2, inference: 1 } },
  { id: 'forwarding-cancellation-keeps-classification-cache-usable', mode: 'forward', large: false, expected: { evaluation: 1, count: 0, inference: 2 } },
  { id: 'shutdown-cancels-shared-evaluation-and-independent-counts', mode: 'shutdown', large: true, expected: { evaluation: 1, count: 2, inference: 0 } },
];

function bounded(promise, label) {
  let timer;
  return Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(Error(label)), DEADLINE); })]).finally(() => clearTimeout(timer));
}
function barriers() {
  const pending = new Set();
  let failure;
  function notify() {
    for (const entry of [...pending]) {
      if (failure) { pending.delete(entry); entry.reject(failure); }
      else if (entry.ready()) { pending.delete(entry); entry.resolve(); }
    }
  }
  return {
    notify,
    fail() { failure = Error('Synthetic mock failed'); notify(); },
    wait(ready, label) {
      if (failure) return Promise.reject(failure);
      if (ready()) return Promise.resolve();
      let entry;
      const promise = new Promise((resolve, reject) => { entry = { ready, resolve, reject }; pending.add(entry); });
      return bounded(promise, label).finally(() => pending.delete(entry));
    },
  };
}
async function readBounded(stream) {
  const chunks = [];
  let size = 0;
  for await (const chunk of stream) {
    size += chunk.length;
    if (size > MAX_BODY) throw Error('Synthetic fixture body exceeds bound');
    chunks.push(chunk);
  }
  return Buffer.concat(chunks);
}
function fixture(scenario) {
  return Buffer.from(JSON.stringify({
    model: MODEL, max_tokens: 128,
    messages: [{ role: 'user', content: `Synthetic resource task ${scenario.id}. Correct teh typo.` }],
    ...(scenario.large ? { tools: [{ name: 'synthetic_tool', description: 'x'.repeat(160000), input_schema: { type: 'object', properties: {} } }] } : {}),
  }));
}
function client(port, id, body, clients, notify) {
  let cancelled = false, received = false, settled = false;
  let finish;
  const result = new Promise(resolve => { finish = value => { if (!settled) { settled = true; resolve(value); } }; });
  const req = http.request({ host: '127.0.0.1', port, path: `/v1/messages?fixture=${id}`, method: 'POST', agent: false,
    headers: { 'x-api-key': token, 'content-type': 'application/json', 'content-length': body.length,
      'x-claude-code-session-id': 'synthetic-resource-session', 'x-claude-code-agent-id': id, 'x-claude-code-prompt-id': id } }, res => {
    received = true;
    void readBounded(res).then(bytes => finish({ status: res.statusCode, content_type: res.headers['content-type'], bytes: bytes.toString('base64') }),
      () => finish(cancelled ? { cancelled: true, received_response: received } : { error: 'response_failed' }));
  });
  req.on('error', () => finish(cancelled ? { cancelled: true, received_response: received } : { error: 'request_failed' }));
  req.on('close', () => { clients.delete(req); notify(); if (!settled && !received) finish(cancelled ? { cancelled: true, received_response: false } : { error: 'request_failed' }); });
  clients.add(req);
  req.end(body);
  return { result: () => bounded(result, `Client ${id} did not finish`), cancel() { cancelled = true; req.destroy(); } };
}
function snapshot(records) {
  const project = row => ({ id: row.id, request: row.request, ...(row.kind === 'inference' ? { request_bytes: row.request_bytes } : {}), reply: row.released, closed: row.closed, cancelled: row.cancelled });
  return Object.fromEntries(['evaluation', 'count', 'inference'].map(kind => [kind, records.filter(row => row.kind === kind).map(project).sort((a, b) => a.id < b.id ? -1 : a.id > b.id ? 1 : 0)]));
}
async function engine(name, scenario) {
  const sync = barriers(), records = [], sockets = new Set(), clients = new Set(), responses = {};
  const bytes = fixture(scenario);
  let running, failure, listenerRebound = false;
  const start = id => client(running.port, id, bytes, clients, sync.notify);
  const mock = watch(http.createServer((req, res) => {
    void (async () => {
      const body = await readBounded(req);
      const url = new URL(req.url, 'http://synthetic.invalid');
      const kind = url.pathname === '/v1/systemone' ? 'evaluation' : url.pathname === '/v1/messages/count_tokens' ? 'count' : url.pathname === '/v1/messages' ? 'inference' : undefined;
      assert.ok(kind, 'Unexpected synthetic endpoint');
      const id = kind === 'evaluation' ? String(records.filter(row => row.kind === kind).length) : url.searchParams.get('fixture');
      assert.ok(id !== null, 'Missing synthetic request identity');
      const row = { kind, id, request: JSON.parse(body), request_bytes: body.toString('base64'), released: false, closed: false, cancelled: false, res };
      res.on('close', () => { row.closed = true; row.cancelled = !res.writableFinished; sync.notify(); });
      res.on('error', () => {});
      records.push(row); sync.notify();
    })().catch(() => { sync.fail(); res.destroy(); });
  }));
  mock.on('connection', socket => { sockets.add(socket); socket.once('close', () => { sockets.delete(socket); sync.notify(); }); });
  const record = (kind, id) => records.find(row => row.kind === kind && row.id === id);
  const entered = (kind, id) => sync.wait(() => Boolean(record(kind, id)), `Missing ${kind} admission ${id}`);
  const cancelled = (kind, id) => sync.wait(() => record(kind, id)?.cancelled === true, `Missing ${kind} cancellation ${id}`);
  function release(kind, id) {
    const row = record(kind, id);
    assert.ok(row && !row.closed && !row.released, `Cannot release ${kind} ${id}`);
    row.released = true;
    row.res.writeHead(200, { 'content-type': 'application/json' });
    row.res.end(kind === 'evaluation' ? DECISION : kind === 'count' ? COUNT : RESPONSE);
  }
  async function successful(request, id) {
    const value = await request.result();
    assert.deepEqual(value, { status: 200, content_type: 'application/json', bytes: RESPONSE.toString('base64') }, `Unexpected response ${id}`);
    responses[id] = value;
  }
  async function abandon(request, id) {
    request.cancel(); responses[id] = await request.result();
    assert.deepEqual(responses[id], { cancelled: true, received_response: false }, `Unexpected cancellation ${id}`);
  }
  async function serve(request, id) {
    await entered('inference', id); release('inference', id); await successful(request, id);
  }
  try {
    mock.listen(0, '127.0.0.1'); await once(mock, 'listening');
    const base = `http://127.0.0.1:${mock.address().port}`;
    running = await gateway(name, base, { AUTOROUTER_JEV_URL: `${base}/v1/systemone`, AUTOROUTER_JEV_TIMEOUT_MS: '10000', AUTOROUTER_TOKEN_COUNT_TIMEOUT_MS: '10000', AUTOROUTER_CLIENT_PROFILE: 'compatible' });
    const first = start('a');
    await entered('evaluation', '0');
    if (scenario.mode === 'forward') {
      release('evaluation', '0'); await entered('inference', 'a');
      await abandon(first, 'a'); await cancelled('inference', 'a');
      const replacement = start('c');
      await serve(replacement, 'c');
    } else {
      await entered('count', 'a');
      const second = start('b');
      await entered('count', 'b');
      // Admission barrier: early counts are independent and held open. Both
      // executable runtimes register the classifier subscriber synchronously
      // before yielding to the count task's network I/O. The native executable
      // uses a current-thread executor. Thus the second observed count proves
      // the second waiter is registered, without a sleep or product test hook.
      assert.equal(records.filter(row => row.kind === 'evaluation').length, 1);
      if (scenario.mode === 'one' || scenario.mode === 'last') {
        await abandon(first, 'a'); await cancelled('count', 'a');
        assert.equal(record('evaluation', '0').closed, false, 'One waiter cancelled shared work');
        if (scenario.mode === 'last') {
          await abandon(second, 'b');
          await cancelled('count', 'b'); await cancelled('evaluation', '0');
          const replacement = start('c');
          await entered('evaluation', '1'); await entered('count', 'c');
          release('count', 'c'); release('evaluation', '1'); await serve(replacement, 'c');
        } else {
          release('count', 'b'); release('evaluation', '0'); await serve(second, 'b');
        }
      } else if (scenario.mode === 'count') {
        release('evaluation', '0'); release('count', 'b'); await serve(second, 'b');
        // A completed peer proves the shared classification has settled while
        // the first request's separate advisory count is still blocked.
        assert.equal(record('count', 'a').closed, false);
        await abandon(first, 'a'); await cancelled('count', 'a');
      } else {
        await running.stop();
        responses.a = await first.result(); responses.b = await second.result();
        assert.deepEqual(responses.a, { error: 'request_failed' });
        assert.deepEqual(responses.b, { error: 'request_failed' });
        await cancelled('count', 'a'); await cancelled('count', 'b'); await cancelled('evaluation', '0');
      }
    }
    const counts = Object.fromEntries(['evaluation', 'count', 'inference'].map(kind => [kind, records.filter(row => row.kind === kind).length]));
    assert.deepEqual(counts, scenario.expected, 'Unexpected stage call counts');
    for (const row of records.filter(row => row.kind === 'inference')) assert.equal(row.request.model, SELECTED);
  } catch (error) {
    failure = error instanceof assert.AssertionError ? error.message.split('\n')[0] : error.message;
  } finally {
    for (const req of clients) req.destroy();
    if (running) {
      try {
        await running.stop();
        assert.deepEqual(running.exit(), { code: 0, signal: null }, 'Gateway did not exit cleanly');
        await sync.wait(() => sockets.size === 0, 'Gateway retained mock connections after exit');
        await sync.wait(() => clients.size === 0, 'Fixture retained downstream requests after exit');
        assert.ok(records.every(row => row.closed), 'Mock request stayed open after gateway exit');
        assert.deepEqual(Object.fromEntries(['evaluation', 'count', 'inference'].map(kind => [kind, records.filter(row => row.kind === kind).length])), scenario.expected, 'Stage calls changed during cleanup');
        const rebound = net.createServer();
        rebound.listen(running.port, '127.0.0.1'); await once(rebound, 'listening'); await close(rebound); listenerRebound = true;
      } catch (error) { failure ??= error.message.split('\n')[0]; }
    }
    for (const socket of sockets) socket.destroy();
    await close(mock).catch(() => { failure ??= 'Mock server cleanup failed'; });
  }
  return { passed: !failure, ...(failure ? { failure } : {}), fixture_sha256: hash(bytes), fixture_bytes: bytes.length, observations: snapshot(records), responses,
    cleanup: { exit: running?.exit() ?? null, listener_rebound: listenerRebound, active_mock_sockets: sockets.size, active_downstream_requests: clients.size, all_mock_requests_closed: records.every(row => row.closed) } };
}

try {
  const results = [];
  for (const scenario of scenarios) {
    const node = await engine('node', scenario), rust = await engine('rust', scenario);
    const matched = isDeepStrictEqual(node, rust);
    results.push({ id: scenario.id, expected_calls: scenario.expected, matched, passed: matched && node.passed && rust.passed, node, rust });
  }
  const report = { schema_version: 1, kind: 'paired_executable_gateway_resource_contracts', identity,
    source_sha256: hash(await readFile(import.meta.filename)), harness_sha256: hash(await readFile(join(root, 'rust/parity/transport-harness.mjs'))),
    fixture_manifest_sha256: hash(JSON.stringify(scenarios)), passed: results.every(row => row.passed), scenarios: results.length, results,
    barrier: 'Observed independent speculative count requests establish registered shared-classifier waiters on the two current-thread executable runtimes; explicit mock response release and cancellation/close events order every later stage.',
    comparison: 'Exact downstream response bytes, upstream inference request bytes, parsed evaluator/count JSON, stage call counts, cancellation ownership, process exit and connection cleanup. No timing fields are normalized or measured.',
    scope: 'Synthetic loopback only; no provider/model/quality or numerical performance evidence. Detached descendants, descriptor-count soak, and other resource workloads remain outside this corpus.' };
  let reportPath = join(root, 'artifacts/rust-rewrite/parity-gateway-resources.json');
  const reportBytes = JSON.stringify(report, null, 2) + '\n';
  try { await writeFile(reportPath, reportBytes, { mode: 0o600, flag: 'wx' }); }
  catch (error) {
    if (error.code !== 'EEXIST') throw error;
    // Keep failed observations as well as successful reruns; never replace a
    // historical report merely because the fixture or implementation changed.
    reportPath = join(root, `artifacts/rust-rewrite/parity-gateway-resources-${Date.now()}-${process.pid}.json`);
    await writeFile(reportPath, reportBytes, { mode: 0o600, flag: 'wx' });
  }
  console.log(JSON.stringify({ passed: report.passed, scenarios: report.scenarios, report: reportPath, failures: results.filter(row => !row.passed).map(row => ({ id: row.id, matched: row.matched, node: row.node.failure, rust: row.rust.failure })) }, null, 2));
  if (!report.passed) process.exitCode = 1;
} finally { await cleanup(); }
