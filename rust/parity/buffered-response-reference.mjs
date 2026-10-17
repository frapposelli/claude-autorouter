// One isolated synthetic reference scenario. No native implementation is executed.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { readFile, writeFile } from 'node:fs/promises';
import http from 'node:http';
import https from 'node:https';
import net from 'node:net';
import tls from 'node:tls';
import { join } from 'node:path';
import { setImmediate as immediate, setTimeout as delay } from 'node:timers/promises';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';
import { cases, limits } from './buffered-response-cases.mjs';

assert.equal(process.version, 'v22.14.0');
assert.equal(process.argv.length, 5, 'reference directory, declared case ID, result path');
const [reference, caseId, output] = process.argv.slice(2);
const spec = cases.find(value => value.id === caseId);
assert.ok(spec, 'Declared case');
const baseline = await verifyBaseline(reference);
const [{ createRouterServer, listen }, { readConfig }] = await Promise.all([
  import(pathToFileURL(join(reference, 'src/server.mjs')).href),
  import(pathToFileURL(join(reference, 'src/config.mjs')).href),
]);
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const builtinNames = ['_http_agent', '_http_client', '_http_common', '_http_incoming', '_http_server', 'internal/streams/readable', 'internal/streams/pipeline', 'net', 'https'];
const identity = { baseline, node: process.version, openssl: process.versions.openssl, node_sha256: hash(await readFile(process.execPath)), builtins: Object.fromEntries(builtinNames.map(name => [name, hash(process.binding('natives')[name])])) };
const trace = [], sockets = new Set(), servers = [], cleanupFailures = [], observations = [];
const stop = new AbortController();
let ordinal = 0, phase = 'scenario', failure, scheduleDone = false, interrupted, accepted = 0, incoming = 0;
let client, providerSocket, upstreamResponse, downstreamResponse, signal, releaseBody, releaseResponseCallback, providerEnded = false, holdObserved = false, upstreamPushBytes = 0, downstreamWrites = 0, downstreamWriteBytes = 0, consumerReleased = false;
let dataDepth = 0, acceptedWriteBytes = 0, synchronousWriteBytes = 0, heldWrite;
const handoff = { data_calls: 0, write_calls: 0, synchronous_write_calls: 0, false_writes: 0, at_abort: null };
let rejectObservation;
const observedFailure = new Promise((_, reject) => { rejectObservation = reject; }); observedFailure.catch(() => {});
const sanitize = error => ({ name: String(error?.name ?? 'Error').slice(0, 40), message: String(error?.message ?? error).slice(0, 200) });
const fail = error => { if (phase === 'cleanup') { if (cleanupFailures.length < 16) cleanupFailures.push(sanitize(error)); } else if (!stop.signal.aborted) { stop.abort(); rejectObservation(error); } };
const guard = fn => (...args) => { try { return fn(...args); } catch (error) { fail(error); } };
const emit = (event, fields = {}) => {
  assert.ok(trace.length < limits.events, 'Trace bound');
  trace.push({ ordinal: ordinal++, phase, event, ...fields });
};
const seen = event => trace.some(row => row.phase === 'scenario' && row.event === event);
const first = event => trace.find(row => row.phase === 'scenario' && row.event === event)?.ordinal;
const code = value => typeof value === 'string' && /^[A-Z0-9_]{1,60}$/.test(value) ? value : 'other';
async function bounded(promise, ms, label) {
  let timer;
  try { return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(Error(label)), ms); })]); }
  finally { clearTimeout(timer); }
}
async function wait(predicate, label, ms = limits.barrier_ms) {
  const deadline = Date.now() + ms;
  for (;;) {
    if (stop.signal.aborted) throw Error('Scenario cancelled');
    if (predicate()) return;
    assert.ok(Date.now() < deadline, label);
    await immediate(undefined, { signal: stop.signal });
  }
}
function track(socket, side) {
  assert.ok(sockets.size < limits.connections, 'Owned socket bound');
  sockets.add(socket);
  socket.on('end', guard(() => emit(`${side}.end`)));
  socket.on('error', guard(error => emit(`${side}.error`, { code: code(error.code) })));
  socket.on('close', guard(hadError => { sockets.delete(socket); emit(`${side}.close`, { had_error: hadError }); }));
  return socket;
}
// Incremental fixture-only response accounting. It never retains a full response.
function wireObserver() {
  let input = Buffer.alloc(0), headers, mode, needed = 0, total = 0, done = false, bodyBytes = 0, trailerBytes = 0;
  const digest = createHash('sha256');
  const data = bytes => { bodyBytes += bytes.length; assert.ok(bodyBytes <= 2 * 1024 * 1024, 'Synthetic body count bound'); digest.update(bytes); };
  const push = bytes => {
    total += bytes.length; assert.ok(total <= 3 * 1024 * 1024, 'Synthetic wire count bound');
    input = Buffer.concat([input, bytes]); assert.ok(input.length <= 262144, 'Incremental wire scratch bound');
    for (;;) {
      if (!headers) {
        const end = input.indexOf('\r\n\r\n'); if (end < 0) { assert.ok(input.length <= 16384); return; }
        headers = input.toString('latin1', 0, end).split('\r\n'); input = input.subarray(end + 4);
        assert.match(headers[0], /^HTTP\/1\.[01] [0-9]{3}/);
        const chunked = headers.some(line => /^transfer-encoding:\s*chunked$/i.test(line));
        const length = headers.find(line => /^content-length:/i.test(line));
        mode = chunked ? 'chunk-size' : length ? 'length' : 'close';
        if (length) needed = Number(length.split(':')[1].trim());
        if (/^HTTP\/1\.[01] (204|304)\b/.test(headers[0])) { mode = 'length'; needed = 0; }
      }
      if (mode === 'close') { data(input); input = Buffer.alloc(0); return; }
      if (mode === 'length') { const take = Math.min(needed, input.length); data(input.subarray(0, take)); needed -= take; input = input.subarray(take); if (needed === 0) { done = true; mode = 'done'; } else return; }
      if (mode === 'chunk-size') {
        const end = input.indexOf('\r\n'); if (end < 0) return;
        const size = input.toString('ascii', 0, end).split(';')[0]; assert.match(size, /^[0-9a-f]+$/i); needed = Number.parseInt(size, 16); input = input.subarray(end + 2); mode = needed === 0 ? 'trailers' : 'chunk-data';
      }
      if (mode === 'chunk-data') { const take = Math.min(needed, input.length); data(input.subarray(0, take)); needed -= take; input = input.subarray(take); if (needed > 0) return; mode = 'chunk-crlf'; }
      if (mode === 'chunk-crlf') { if (input.length < 2) return; assert.equal(input.toString('ascii', 0, 2), '\r\n'); input = input.subarray(2); mode = 'chunk-size'; continue; }
      if (mode === 'trailers') { const end = input.indexOf('\r\n'); if (end < 0) return; trailerBytes += end; input = input.subarray(end + 2); if (end === 0) { done = true; mode = 'done'; } else continue; }
      if (mode === 'done') { assert.equal(input.length, 0, 'One downstream response only'); return; }
      if (!input.length) return;
    }
  };
  return { push, bytes: () => bodyBytes, snapshot: () => ({ wire_bytes: total, body_bytes: bodyBytes, body_sha256: digest.copy().digest('hex'), complete_framing: done, trailer_bytes: trailerBytes, status: headers ? Number(headers[0].split(' ')[1]) : null, pending_wire_bytes: input.length }) };
}
const wire = wireObserver();
const transport = spec.protocol === 'HTTP' ? http : https;
const originalRequest = transport.request;
const onInterrupt = name => { interrupted = name; fail(Error(`Reference interrupted by ${name}`)); };
const interrupt = () => onInterrupt('SIGINT'), terminate = () => onInterrupt('SIGTERM');
process.once('SIGINT', interrupt); process.once('SIGTERM', terminate);
function snapshot(label) {
  const row = { label, buffered_bytes: upstreamResponse?.readableLength ?? 0, high_water_mark: upstreamResponse?.readableHighWaterMark ?? null, parser_complete: upstreamResponse?.complete ?? false, readable_ended: upstreamResponse?.readableEnded ?? false, upstream_abort_observed: seen('upstream.aborted'), signal_aborted: signal?.aborted ?? false, pushed_bytes: upstreamPushBytes, raw_trailer_pairs: (upstreamResponse?.rawTrailers?.length ?? 0) / 2, consumer_released: consumerReleased, ...wire.snapshot() };
  assert.ok(observations.length < 16); observations.push(row); emit('driver.snapshot', row); return row;
}
async function payload(socket, bytes) {
  const unit = spec.writes === 'tiny' ? 17 : Math.max(bytes, 1);
  for (let offset = 0; offset < bytes; offset += unit) {
    if (stop.signal.aborted) throw Error('Provider write cancelled');
    const value = Buffer.alloc(Math.min(unit, bytes - offset), 120);
    if (!socket.write(value)) await bounded(once(socket, 'drain'), limits.barrier_ms, 'Provider drain');
    if (spec.writes === 'tiny' && offset % 1088 === 0) await immediate(undefined, { signal: stop.signal });
  }
}
async function stimulus() {
  const short = spec.framing === 'short-length', chunked = !['short-length', 'exact-length'].includes(spec.framing);
  providerSocket.write(`HTTP/1.1 ${spec.response_status ?? 200} ${spec.response_status ? 'Synthetic' : 'OK'}\r\n${chunked ? 'transfer-encoding: chunked' : `content-length: ${spec.declared_length}`}\r\n\r\n`);
  if (!chunked) await payload(providerSocket, spec.bytes);
  else if (spec.framing === 'invalid-chunk-length') providerSocket.write('ZZ\r\n');
  else if (spec.framing === 'truncated-chunk') { providerSocket.write('10\r\n'); await payload(providerSocket, spec.bytes); }
  else {
    if (spec.bytes) { providerSocket.write(`${spec.bytes.toString(16)}\r\n`); await payload(providerSocket, spec.bytes); providerSocket.write('\r\n'); }
    if (spec.framing === 'chunked-trailers') providerSocket.write(spec.trailer_lines ? `0\r\n${'x:\r\n'.repeat(spec.trailer_lines)}\r\n` : '0\r\nX-Synthetic-Trailer: exact\r\n\r\n');
    else if (spec.framing === 'truncated-trailers') providerSocket.write('0\r\nX-Synthetic-Trailer: unfinished');
    else if (spec.framing === 'invalid-trailers') providerSocket.write('0\r\nInvalid Trailer\r\n\r\n');
    else assert.equal(spec.framing, 'missing-final-chunk');
  }
  return short;
}
async function work() {
  const onPeer = guard(socket => {
    assert.equal(++accepted, 1, 'One provider request/connection'); providerSocket = track(socket, 'provider'); emit('provider.connected', { resumed: socket.isSessionReused?.() ?? false });
    let head = Buffer.alloc(0), received = false;
    socket.on('data', guard(chunk => {
      assert.equal(received, false, 'Unexpected second upstream request'); head = Buffer.concat([head, chunk]); assert.ok(head.length <= limits.request_headers);
      if (!head.includes('\r\n\r\n')) return;
      assert.ok(head.toString('latin1').startsWith('GET /v1/models/session-0 HTTP/1.1\r\n'));
      received = true; head = Buffer.alloc(0); emit('provider.request');
    }));
  });
  const tlsOptions = spec.protocol === 'HTTP' ? undefined : { key: await readFile(process.env.AUTOROUTER_SYNTHETIC_TLS_KEY), cert: await readFile(process.env.AUTOROUTER_SYNTHETIC_TLS_CERT), minVersion: spec.protocol, maxVersion: spec.protocol };
  const provider = spec.protocol === 'HTTP' ? net.createServer({ allowHalfOpen: true }, onPeer) : tls.createServer({ ...tlsOptions, allowHalfOpen: true }, onPeer);
  servers.push(provider); provider.on('error', fail); provider.on('tlsClientError', guard(error => emit('provider.tls-error', { code: code(error.code) })));
  provider.listen(0, '127.0.0.1'); await bounded(once(provider, 'listening'), limits.barrier_ms, 'Provider listen');
  const origin = `${spec.protocol === 'HTTP' ? 'http' : 'https'}://127.0.0.1:${provider.address().port}`;
  const config = readConfig({ AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: 'synthetic-evaluator', ANTHROPIC_API_KEY: 'synthetic-provider', AUTOROUTER_TOKEN: 'synthetic-intent-fixture-token', AUTOROUTER_UPSTREAM_URL: origin, AUTOROUTER_JEV_URL: `${origin}/v1/systemone` });
  assert.equal(new URL(config.upstream).hostname, '127.0.0.1'); assert.equal(new URL(config.jevEndpoint).hostname, '127.0.0.1');
  transport.request = function(url, options, callback) {
    assert.equal(new URL(url).origin, origin); assert.equal(new URL(url).pathname, '/v1/models/session-0'); assert.equal(options.method, 'GET'); assert.ok(options.signal instanceof AbortSignal);
    signal = options.signal; emit('outgoing.created');
    const request = originalRequest.call(this, url, options, response => {
      upstreamResponse = response; emit('upstream.headers', { status: response.statusCode });
      response.on('aborted', guard(() => {
        if (spec.observe_handoff) handoff.at_abort = { accepted_write_bytes: acceptedWriteBytes, synchronous_write_bytes: synchronousWriteBytes, readable_flowing: response.readableFlowing, buffered_bytes: response.readableLength, writer_held: !!heldWrite };
        emit('upstream.aborted', { signal_aborted: signal.aborted, complete: response.complete });
      }));
      if (spec.observe_handoff) {
        const originalEmit = response.emit;
        response.emit = function(event, ...args) {
          if (event !== 'data') return originalEmit.call(this, event, ...args);
          handoff.data_calls++; dataDepth++;
          try { return originalEmit.call(this, event, ...args); }
          finally { dataDepth--; }
        };
      }
      response.on('end', guard(() => emit('upstream.end', { complete: response.complete, signal_aborted: signal.aborted })));
      response.on('error', guard(error => emit('upstream.error', { code: code(error.code), signal_aborted: signal.aborted })));
      response.on('close', guard(() => emit('upstream.close', { complete: response.complete, signal_aborted: signal.aborted })));
      const push = response.push;
      response.push = function(chunk, ...args) { if (chunk !== null) upstreamPushBytes += chunk.length; return push.call(this, chunk, ...args); };
      const resume = response.resume;
      response.resume = function() { if (!holdObserved) { holdObserved = true; emit('driver.resume_held', { high_water_mark: response.readableHighWaterMark }); } return this; };
      releaseBody = () => { assert.equal(consumerReleased, false); consumerReleased = true; response.resume = resume; emit('driver.release_body'); resume.call(response); };
      if (spec.schedule === 'error-before-gateway-callback') {
        releaseResponseCallback = () => { emit('driver.release_response_callback'); callback(response); };
        emit('driver.response_callback_held');
      } else callback(response);
    });
    signal.addEventListener('abort', guard(() => emit('signal.abort', { reason: signal.reason?.name ?? 'other', upstream_destroyed: request.destroyed, upstream_complete: upstreamResponse?.complete ?? false })), { once: true });
    request.on('socket', guard(socket => { track(socket, 'outgoing.socket'); emit('outgoing.assigned'); }));
    request.on('error', guard(error => emit('outgoing.error', { code: code(error.code), signal_aborted: signal.aborted })));
    request.on('finish', guard(() => emit('outgoing.finish'))); request.on('close', guard(() => emit('outgoing.close', { signal_aborted: signal.aborted })));
    return request;
  };
  const gateway = createRouterServer(config, { router: { route() { assert.fail('No inference in transport characterization'); } }, tokenCounter() { assert.fail('No token counting'); }, log: guard(row => emit('gateway.log', { kind: row.event, status: row.status ?? null })) });
  servers.push(gateway); gateway.on('error', fail);
  gateway.prependListener('connection', guard(socket => {
    track(socket, 'downstream.socket'); emit('downstream.connected');
    for (const method of ['_write', '_writev']) {
      const original = socket[method];
      socket[method] = function(...args) {
        const callback = args.at(-1); assert.equal(typeof callback, 'function'); downstreamWrites++;
        downstreamWriteBytes += method === '_write' ? args[0].length : args[0].reduce((sum, row) => sum + row.chunk.length, 0);
        args[args.length - 1] = function(error) { if (error) guard(() => emit('downstream.write_callback_error', { code: code(error.code), method }))(); return callback.apply(this, arguments); };
        if (spec.hold_socket_writes) {
          assert.equal(heldWrite, undefined, 'Only one physical write can be in flight');
          const bytes = method === '_write' ? args[0].length : args[0].reduce((sum, row) => sum + row.chunk.length, 0);
          assert.ok(bytes <= 262144, 'Held writer backing observation bound');
          heldWrite = { callback: args[args.length - 1], bytes };
          emit('driver.socket_write_held', { bytes, method });
          return;
        }
        return original.apply(this, args);
      };
    }
  }));
  gateway.prependListener('request', guard((req, res) => {
    assert.equal(++incoming, 1); downstreamResponse = res; emit('downstream.request');
    if (spec.observe_handoff) {
      const write = res.write;
      res.write = function(chunk, ...args) {
        const bytes = typeof chunk === 'string' ? Buffer.byteLength(chunk, typeof args[0] === 'string' ? args[0] : undefined) : chunk.length;
        handoff.write_calls++; acceptedWriteBytes += bytes;
        if (dataDepth) { handoff.synchronous_write_calls++; synchronousWriteBytes += bytes; }
        const accepted = write.call(this, chunk, ...args);
        if (!accepted) handoff.false_writes++;
        return accepted;
      };
    }
    req.on('aborted', guard(() => emit('downstream.request.aborted', { complete: req.complete })));
    req.on('end', guard(() => emit('downstream.request.end'))); req.on('error', guard(error => emit('downstream.request.error', { code: code(error.code) })));
    res.on('finish', guard(() => emit('downstream.response.finish', { writable_finished: res.writableFinished, signal_aborted: signal?.aborted ?? false })));
    res.on('error', guard(error => emit('downstream.response.error', { code: code(error.code) })));
    res.on('close', guard(() => emit('downstream.response.close.before', { writable_finished: res.writableFinished, signal_aborted: signal?.aborted ?? false })));
  }));
  gateway.on('request', guard((_req, res) => res.on('close', guard(() => emit('downstream.response.close.after', { writable_finished: res.writableFinished, signal_aborted: signal?.aborted ?? false })))));
  const address = await bounded(listen(gateway, 0), limits.barrier_ms, 'Gateway listen');
  client = track(net.createConnection({ host: '127.0.0.1', port: address.port, allowHalfOpen: true }), 'client');
  let prefixSeen = false;
  client.on('data', guard(chunk => { wire.push(chunk); if (!prefixSeen && wire.bytes()) { prefixSeen = true; emit('client.body-prefix', { bytes: wire.bytes() }); } }));
  await bounded(once(client, 'connect'), limits.barrier_ms, 'Client connect');
  client.write('GET /v1/models/session-0 HTTP/1.1\r\nHost: synthetic\r\nX-Api-Key: synthetic-intent-fixture-token\r\n\r\n');
  await wait(() => seen('provider.request'), 'Provider request');
  await stimulus();
  if (spec.schedule === 'error-before-gateway-callback') {
    await wait(() => seen('upstream.headers') && releaseResponseCallback, 'Response callback held after headers');
    emit('driver.provider_end'); providerEnded = true; providerSocket.end();
    await wait(() => seen('upstream.aborted'), 'Actual body failure before frozen forward callback');
    snapshot('body-error-before-callback'); releaseResponseCallback();
  } else await wait(() => seen('upstream.headers') && holdObserved, 'Installed body resume hold');
  if (spec.schedule === 'prefix-error') { releaseBody(); await wait(() => wire.bytes() === spec.bytes, 'Forwarded prefix before provider end'); snapshot('prefix-forwarded'); }
  if (spec.schedule === 'cancel-before-provider-end') { emit('driver.reset_before_provider_end'); client.resetAndDestroy(); await wait(() => signal.aborted, 'Cancellation before provider end'); }
  if (!providerEnded) { emit('driver.provider_end'); providerEnded = true; providerSocket.end(); }
  if (spec.schedule.startsWith('saturated-')) {
    await wait(() => upstreamResponse.readableLength >= upstreamResponse.readableHighWaterMark, 'Actual upstream buffer saturation');
    const stage = snapshot('saturated'); assert.equal(stage.upstream_abort_observed, false);
    if (spec.schedule === 'saturated-cancel') { emit('driver.reset_while_held'); client.resetAndDestroy(); } else releaseBody();
  } else if (spec.schedule === 'hold-observe-release') {
    await wait(() => upstreamResponse.complete || upstreamResponse.readableLength >= upstreamResponse.readableHighWaterMark || seen('upstream.aborted') || seen('outgoing.error'), 'Decoder terminal or actual saturation');
    snapshot('held-observation');
    if (!seen('upstream.aborted') && !seen('outgoing.error')) releaseBody();
  }
  const clean = ['exact-length', 'chunked-trailers'].includes(spec.framing);
  if (clean) {
    await wait(() => seen('downstream.response.finish') && seen('upstream.end') && wire.snapshot().complete_framing, 'Clean response delivery');
    assert.equal(wire.bytes(), spec.bytes); assert.equal(wire.snapshot().body_sha256, hash(Buffer.alloc(spec.bytes, 120))); assert.equal(wire.snapshot().trailer_bytes, 0); assert.equal(seen('upstream.aborted'), false);
  } else {
    await wait(() => seen('downstream.response.close.after') && seen('outgoing.socket.close'), 'Independent terminal propagation');
    assert.equal(seen('downstream.response.finish'), false, 'Failure must not complete downstream response');
    assert.ok(seen('upstream.aborted') || seen('outgoing.error'), 'Actual upstream failure evidence');
  }
  for (const relation of spec.relations) {
    if (relation === 'writer-held-before-upstream-error') assert.ok(first('driver.socket_write_held') < first('upstream.aborted'));
    if (relation === 'upstream-error-without-writer-release') assert.equal(handoff.at_abort?.writer_held, true);
    if (relation === 'no-wire-body') assert.equal(wire.bytes(), 0);
    if (relation === 'synchronous-data-consumer-write') assert.ok(handoff.synchronous_write_calls > 0);
    if (relation === 'all-provider-bytes-accepted-before-error') { assert.equal(handoff.at_abort?.accepted_write_bytes, spec.bytes); assert.ok(handoff.at_abort.synchronous_write_bytes > 0); }
    if (relation === 'body-error-before-gateway-callback') assert.ok(first('upstream.aborted') < first('driver.release_response_callback'));
    if (relation === 'successful-response-acquisition-not-502') {
      assert.ok(trace.some(row => row.phase === 'scenario' && row.event === 'gateway.log' && row.kind === 'upstream_response' && row.status === 200));
      assert.ok(wire.snapshot().status === null || wire.snapshot().status === 200);
    }
    if (relation === 'consumer-never-released') assert.equal(consumerReleased, false);
    if (relation === 'upstream-aborted-before-signal') { assert.ok(Number.isInteger(first('upstream.aborted'))); assert.ok(Number.isInteger(first('signal.abort'))); assert.ok(first('upstream.aborted') < first('signal.abort')); }
    if (relation === 'signal-before-upstream-abort') { assert.ok(Number.isInteger(first('signal.abort'))); assert.ok(Number.isInteger(first('upstream.aborted'))); assert.ok(first('signal.abort') < first('upstream.aborted')); }
    if (relation === 'forwarded-prefix-before-error') assert.ok(first('client.body-prefix') < first('upstream.aborted'));
  }
  snapshot('finished'); scheduleDone = true;
}
let workPromise;
try {
  workPromise = work(); workPromise.catch(() => {});
  await bounded(Promise.race([workPromise, observedFailure]), limits.scenario_ms, 'Scenario outer deadline');
} catch (error) { failure = sanitize(error); }
finally {
  phase = 'cleanup'; stop.abort(); transport.request = originalRequest;
  // Restoring the method releases the artificial gate without treating cleanup as scenario delivery.
  if (releaseBody && !consumerReleased && upstreamResponse && !upstreamResponse.destroyed) { try { releaseBody(); } catch (error) { cleanupFailures.push(sanitize(error)); } }
  for (const socket of sockets) socket.destroy(); http.globalAgent.destroy(); https.globalAgent.destroy();
  if (heldWrite) { const held = heldWrite; heldWrite = undefined; try { const error = Error('Synthetic held writer cancelled during cleanup'); error.code = 'ECONNRESET'; held.callback(error); } catch (error) { cleanupFailures.push(sanitize(error)); } }
  const closes = servers.map(server => new Promise(resolve => { if (!server.listening) resolve(); else server.close(resolve); }));
  try { await bounded(Promise.allSettled([workPromise, ...closes]), limits.cleanup_ms, 'Work/server cleanup'); } catch (error) { cleanupFailures.push(sanitize(error)); }
  try { await bounded((async () => { while (sockets.size) await delay(1); })(), limits.cleanup_ms, 'Owned socket cleanup'); } catch (error) { cleanupFailures.push(sanitize(error)); }
  process.off('SIGINT', interrupt); process.off('SIGTERM', terminate);
}
const report = { schema_version: 1, kind: 'buffered_response_node_reference_case', identity, case: spec, schedule_done: scheduleDone, passed: scheduleDone && !failure && !cleanupFailures.length && sockets.size === 0, failure, interrupted, observations, wire: wire.snapshot(), provider_ended: providerEnded, handoff, upstream_push_bytes: upstreamPushBytes, downstream_write_calls: downstreamWrites, downstream_write_bytes: downstreamWriteBytes, cleanup: { sockets: sockets.size, servers: servers.filter(server => server.listening).length, failures: cleanupFailures }, trace };
const reportBytes = JSON.stringify(report, null, 2) + '\n'; assert.ok(Buffer.byteLength(reportBytes) <= limits.control_bytes, 'Reference report bound');
await writeFile(output, reportBytes, { mode: 0o600 });
if (!report.passed) process.exitCode = interrupted === 'SIGINT' ? 130 : interrupted === 'SIGTERM' ? 143 : 1;
