// Isolated source-only helper regressions. No sockets, network, timers or GC.
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import test from 'node:test';
import { createPassiveNodeLifecycle } from '../rust/parity/node-passive-lifecycle.mjs';

const marker = name => Object.freeze({ synthetic_result: name });
function fixture({ expectedRequests = 1, boundPush = false } = {}) {
  const calls = [], thrown = new Map(), originals = [];
  const result = Object.fromEntries(['agentAdd', 'agentReuse', 'agentEmit', 'requestEmit', 'responseEmit', 'socketEmit', 'push']
    .map(name => [name, marker(name)]));
  const agent = new EventEmitter(), request = new EventEmitter(), socket = new EventEmitter(), response = new EventEmitter();
  Object.assign(agent, { freeSockets: Object.create(null), sockets: Object.create(null), requests: Object.create(null) });
  Object.assign(request, { reusedSocket: false, destroyed: false });
  Object.assign(socket, { readableEnded: false, writable: true, writableEnded: false,
    writableFinished: false, destroyed: false, closed: false });
  Object.assign(response, { complete: false, readableEnded: false, readableLength: 0 });
  function spy(name, implementation) {
    return function(...args) {
      calls.push({ name, receiver: this, args });
      if (thrown.has(name)) throw thrown.get(name);
      return Reflect.apply(implementation, this, args);
    };
  }
  function install(target, name, value, flags = {}) {
    Object.defineProperty(target, name, { value, configurable: true, enumerable: false, writable: true, ...flags });
    originals.push({ target, name, descriptor: Object.getOwnPropertyDescriptor(target, name) });
  }
  install(agent, 'addRequest', spy('agentAdd', function() { return result.agentAdd; }), { writable: false });
  install(agent, 'reuseSocket', spy('agentReuse', function(_socket, selected) { selected.reusedSocket = true; return result.agentReuse; }));
  for (const [target, name] of [[agent, 'agentEmit'], [request, 'requestEmit'], [response, 'responseEmit'], [socket, 'socketEmit']]) {
    install(target, 'emit', spy(name, function(...args) {
      Reflect.apply(EventEmitter.prototype.emit, this, args);
      return result[name];
    }), { enumerable: name === 'requestEmit' });
  }
  let originalPush = spy('push', function() { return result.push; });
  if (boundPush) originalPush = originalPush.bind(response);
  install(response, 'push', originalPush, { writable: false });
  const receiver = marker('request-constructor-receiver');
  const options = { method: 'GET' }, callback = () => {}, extra = marker('extra-argument');
  const invocationArgs = ['synthetic-url-token', options, callback, extra];
  let addResult;
  const constructor = spy('invoke', function(_url, passedOptions, _callback, passedExtra) {
    addResult = agent.addRequest(request, passedOptions, passedExtra);
    return request;
  });
  const lifecycle = createPassiveNodeLifecycle({ agent,
    requestNumbering: 'zero_based_session_path_ordinal', expectedRequests });
  const invoke = () => lifecycle.invoke(constructor, receiver, invocationArgs, 0);
  function ready() {
    assert.equal(invoke(), request);
    assert.equal(request.emit('socket', socket), result.requestEmit);
    assert.equal(request.emit('response', response), result.requestEmit);
    return api;
  }
  const api = { agent, request, socket, response, result, calls, thrown, originals, receiver,
    options, callback, extra, invocationArgs, lifecycle, invoke, ready, addResult: () => addResult };
  return api;
}
function decoded(diagnostics) {
  return diagnostics.rows.map(row => {
    assert.equal(row.length, diagnostics.fields.length);
    return Object.fromEntries(diagnostics.fields.map((field, index) => [field, row[index]]));
  });
}
function onlyCall(f, name, receiver, args) {
  const rows = f.calls.filter(row => row.name === name);
  assert.equal(rows.length, 1, `${name} delegates exactly once`);
  assert.equal(rows[0].receiver, receiver, `${name} original receiver identity`);
  assert.equal(rows[0].args.length, args.length, `${name} original argument count`);
  for (let index = 0; index < args.length; index++) assert.equal(rows[0].args[index], args[index], `${name} argument ${index} identity`);
}
function assertRestored(f) {
  for (const item of f.originals) assert.deepEqual(Object.getOwnPropertyDescriptor(item.target, item.name), item.descriptor);
}

test('constructor and Agent methods preserve receiver, arguments, return and real free admission order', { concurrency: false }, () => {
  const f = fixture();
  assert.equal(f.invoke(), f.request);
  onlyCall(f, 'invoke', f.receiver, f.invocationArgs);
  onlyCall(f, 'agentAdd', f.agent, [f.request, f.options, f.extra]);
  assert.equal(f.addResult(), f.result.agentAdd);
  f.calls.length = 0;
  const extra = marker('reuse-extra');
  assert.equal(f.agent.reuseSocket(f.socket, f.request, extra), f.result.agentReuse);
  onlyCall(f, 'agentReuse', f.agent, [f.socket, f.request, extra]);
  assert.equal(f.request.reusedSocket, true);
  assert.equal(f.request.emit('socket', f.socket), f.result.requestEmit);
  f.agent.on('free', socket => { f.agent.freeSockets.synthetic = [socket]; });
  f.calls.length = 0;
  const options = marker('free-options');
  assert.equal(f.agent.emit('free', f.socket, options), f.result.agentEmit);
  onlyCall(f, 'agentEmit', f.agent, ['free', f.socket, options]);
  const diagnostics = f.lifecycle.finish();
  assert.equal(diagnostics.complete, true);
  const rows = decoded(diagnostics);
  const before = rows.find(row => row.event === 'agent_reuse_socket_before');
  const after = rows.find(row => row.event === 'agent_reuse_socket_after');
  assert.equal(before.request, 0); assert.equal(before.last_socket_owner_request, null);
  assert.equal(before.request_reused_socket, false); assert.equal(after.request_reused_socket, true);
  const admitted = rows.find(row => row.event === 'agent_free_after_emit');
  assert.equal(admitted.agent_free_member, true); assert.equal(admitted.agent_free_count, 1);
  assert.equal(admitted.last_socket_owner_request, 0);
  assertRestored(f);
});

test('producer null completion and later consumer end remain distinct scalar boundaries', { concurrency: false }, () => {
  const f = fixture().ready();
  f.calls.length = 0;
  const body = Buffer.from('SYNTHETIC_PRIVATE_BODY_SENTINEL');
  assert.equal(f.response.push(body, 'synthetic-encoding'), f.result.push);
  onlyCall(f, 'push', f.response, [body, 'synthetic-encoding']);
  f.calls.length = 0;
  f.response.complete = true;
  assert.equal(f.response.push(null, 'synthetic-null-encoding'), f.result.push);
  onlyCall(f, 'push', f.response, [null, 'synthetic-null-encoding']);
  f.response.readableEnded = true;
  const detail = marker('end-detail');
  f.calls.length = 0;
  assert.equal(f.response.emit('end', detail), f.result.responseEmit);
  onlyCall(f, 'responseEmit', f.response, ['end', detail]);
  const diagnostics = f.lifecycle.finish();
  assert.equal(diagnostics.complete, true);
  const rows = decoded(diagnostics);
  const producer = rows.filter(row => row.event.startsWith('producer_complete_'));
  const consumer = rows.filter(row => row.event.startsWith('response_end_'));
  assert.equal(producer.length, 2); assert.equal(consumer.length, 2);
  assert.ok(producer.every(row => row.response_complete === true && row.response_readable_ended === false));
  assert.ok(consumer.every(row => row.response_readable_ended === true));
  assert.ok(producer.at(-1).sequence < consumer[0].sequence);
  assert.equal(JSON.stringify(diagnostics).includes(body.toString()), false);
  assertRestored(f);
});

for (const outcome of ['return', 'throw']) test(`throwing complete getter cannot replace original push ${outcome}`, { concurrency: false }, () => {
  const f = fixture().ready();
  const observerFailure = marker('observer-getter-threw'), originalFailure = marker('original-push-threw');
  let getterCalls = 0;
  Object.defineProperty(f.response, 'complete', { configurable: true, get() { getterCalls++; throw observerFailure; } });
  if (outcome === 'throw') f.thrown.set('push', originalFailure);
  f.calls.length = 0;
  const args = [null, marker('unchanged-push-encoding')];
  if (outcome === 'return') assert.equal(Reflect.apply(f.response.push, f.response, args), f.result.push);
  else assert.throws(() => Reflect.apply(f.response.push, f.response, args), error => error === originalFailure);
  assert.equal(getterCalls, 1);
  onlyCall(f, 'push', f.response, args);
  const diagnostics = f.lifecycle.finish();
  assert.equal(diagnostics.complete, false);
  assert.equal(diagnostics.failure, 'observer_invariant');
  assertRestored(f);
});

for (const outcome of ['return', 'throw']) test(`wrong push receiver cannot replace original ${outcome} or read its complete getter`, { concurrency: false }, () => {
  const f = fixture().ready();
  const originalFailure = marker('original-wrong-receiver-threw');
  let getterCalls = 0;
  const wrong = Object.defineProperty({}, 'complete', { get() { getterCalls++; throw marker('wrong-observer-getter'); } });
  if (outcome === 'throw') f.thrown.set('push', originalFailure);
  const args = [null, marker('wrong-receiver-encoding')];
  f.calls.length = 0;
  if (outcome === 'return') assert.equal(Reflect.apply(f.response.push, wrong, args), f.result.push);
  else assert.throws(() => Reflect.apply(f.response.push, wrong, args), error => error === originalFailure);
  assert.equal(getterCalls, 0);
  onlyCall(f, 'push', wrong, args);
  assert.equal(f.lifecycle.finish().complete, false);
  assertRestored(f);
});

const methodCases = [
  ['agentAdd', f => [f.agent, 'addRequest', [f.request, f.options, f.extra]]],
  ['agentReuse', f => [f.agent, 'reuseSocket', [f.socket, f.request, f.extra]]],
  ['agentEmit', f => [f.agent, 'emit', ['free', f.socket, f.options]]],
  ['requestEmit', f => [f.request, 'emit', ['finish', f.extra]]],
  ['responseEmit', f => [f.response, 'emit', ['end', f.extra]]],
  ['socketEmit', f => [f.socket, 'emit', ['finish', f.extra]]],
  ['push', f => [f.response, 'push', [null, f.extra]]],
];
for (const [name, select] of methodCases) test(`${name} preserves the original thrown object exactly once`, { concurrency: false }, () => {
  const f = fixture().ready();
  f.response.complete = true;
  const sentinel = marker(`thrown-${name}`);
  f.thrown.set(name, sentinel);
  const [target, method, args] = select(f);
  f.calls.length = 0;
  assert.throws(() => Reflect.apply(target[method], target, args), error => error === sentinel);
  onlyCall(f, name, target, args);
  f.lifecycle.finish();
  assertRestored(f);
});

test('original constructor throw is preserved even before it binds a request', { concurrency: false }, () => {
  const f = fixture(), sentinel = marker('original-constructor-threw');
  f.thrown.set('invoke', sentinel);
  assert.throws(() => f.invoke(), error => error === sentinel);
  onlyCall(f, 'invoke', f.receiver, f.invocationArgs);
  assert.equal(f.calls.some(row => row.name === 'agentAdd'), false);
  assert.equal(f.lifecycle.finish().complete, true);
  assertRestored(f);
});

test('constructor-internal Agent addRequest preserves its original thrown object', { concurrency: false }, () => {
  const f = fixture(), sentinel = marker('original-constructor-addRequest-threw');
  f.thrown.set('agentAdd', sentinel);
  assert.throws(() => f.invoke(), error => error === sentinel);
  onlyCall(f, 'invoke', f.receiver, f.invocationArgs);
  onlyCall(f, 'agentAdd', f.agent, [f.request, f.options, f.extra]);
  assert.equal(f.lifecycle.finish().complete, true);
  assertRestored(f);
});

for (const [name, select] of methodCases.filter(([name]) => name !== 'push')) test(`${name} wrong receiver does not change the original return`, { concurrency: false }, () => {
  const f = fixture().ready(), wrong = new EventEmitter();
  const [target, method, args] = select(f);
  f.calls.length = 0;
  assert.equal(Reflect.apply(target[method], wrong, args), f.result[name]);
  onlyCall(f, name, wrong, args);
  assert.equal(f.lifecycle.finish().complete, false);
  assertRestored(f);
});

test('observing error emission adds no listener and preserves EventEmitter unhandled error identity', { concurrency: false }, () => {
  const f = fixture().ready();
  for (const [target, name] of [[f.request, 'requestEmit'], [f.response, 'responseEmit'], [f.socket, 'socketEmit'], [f.agent, 'agentEmit']]) {
    assert.equal(target.listenerCount('error'), 0);
    const sentinel = new Error(`synthetic-original-error-${name}`);
    f.calls.length = 0;
    assert.throws(() => target.emit('error', sentinel), error => error === sentinel);
    onlyCall(f, name, target, ['error', sentinel]);
    assert.equal(target.listenerCount('error'), 0);
  }
  assert.equal(f.lifecycle.finish().complete, true);
  assertRestored(f);
});

test('restoration preserves exact descriptors and is idempotent after observation fails', { concurrency: false }, () => {
  const f = fixture().ready();
  const wrong = new EventEmitter();
  assert.equal(Reflect.apply(f.socket.emit, wrong, ['end']), f.result.socketEmit);
  const first = f.lifecycle.finish();
  assert.equal(first.complete, false);
  assertRestored(f);
  const rows = first.rows.map(row => [...row]);
  const second = f.lifecycle.finish();
  assert.deepEqual(second.rows, rows);
  assert.equal(second.complete, false);
  assertRestored(f);
  f.calls.length = 0;
  assert.equal(f.socket.emit('end', f.extra), f.result.socketEmit);
  onlyCall(f, 'socketEmit', f.socket, ['end', f.extra]);
  assert.deepEqual(first.rows, rows, 'restored originals append no diagnostic rows');
});

test('restoration metadata weakly references owner, installed callable and bound original callable', { concurrency: false }, () => {
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, 'WeakRef');
  const NativeWeakRef = globalThis.WeakRef, weak = [];
  class ObservedWeakRef {
    constructor(value) { this.native = new NativeWeakRef(value); weak.push(this); }
    deref() { return this.native.deref(); }
  }
  let f;
  try {
    Object.defineProperty(globalThis, 'WeakRef', { ...descriptor, value: ObservedWeakRef });
    f = fixture({ boundPush: true }).ready();
    for (const item of f.originals) {
      const installed = item.target[item.name];
      assert.ok(weak.some(reference => reference.native.deref() === item.target), 'owner has a weak restoration reference');
      assert.ok(weak.some(reference => reference.native.deref() === installed), 'installed callable has a weak restoration reference');
      assert.ok(weak.some(reference => reference.native.deref() === item.descriptor.value), 'original callable has a weak restoration reference');
    }
    assert.equal(f.lifecycle.finish().complete, true);
    assertRestored(f);
    assert.equal(f.lifecycle.finish().complete, true);
  } finally {
    try { f?.lifecycle.finish(); }
    finally { Object.defineProperty(globalThis, 'WeakRef', descriptor); }
  }
  // This verifies construction/restoration through weak handles, not GC timing
  // or the absence of every possible strong edge. No collection is forced.
});

test('real encoded-byte overflow invalidates the trace while original effects keep executing', { concurrency: false }, () => {
  const f = fixture().ready();
  f.calls.length = 0;
  for (let index = 0; index < 600; index++) assert.equal(f.socket.emit('end', f.extra), f.result.socketEmit);
  assert.equal(f.calls.filter(row => row.name === 'socketEmit').length, 600);
  const diagnostics = f.lifecycle.finish();
  assert.equal(diagnostics.complete, false); assert.equal(diagnostics.failure, 'trace_bound');
  assert.ok(diagnostics.rows.length <= 512);
  assert.ok(Buffer.byteLength(JSON.stringify(diagnostics.rows)) <= 24 * 1024);
  assert.ok(diagnostics.rows.length < 512, 'real row encoding reaches the byte cap first');
  assertRestored(f);
});

test('512-row cap fails closed under isolated synthetic zero byte charges without changing original effects', { concurrency: false }, () => {
  const f = fixture().ready();
  const descriptor = Object.getOwnPropertyDescriptor(Buffer, 'byteLength');
  const original = Buffer.byteLength;
  let injected = 0, diagnostics;
  f.calls.length = 0;
  try {
    Object.defineProperty(Buffer, 'byteLength', { ...descriptor, value: function(value) {
      // Only a helper's individual fixed scalar row is charged zero. This
      // synchronous fault injection covers the independent row-count branch;
      // the preceding separate test is the evidence for real encoded bytes.
      if (typeof value === 'string' && /^\[\d+,"[a-z_]+",/.test(value)) { injected++; return 0; }
      return Reflect.apply(original, this, arguments);
    } });
    for (let index = 0; index < 600; index++) assert.equal(f.socket.emit('end', f.extra), f.result.socketEmit);
    diagnostics = f.lifecycle.finish();
  } finally {
    Object.defineProperty(Buffer, 'byteLength', descriptor);
    f.lifecycle.finish();
  }
  assert.ok(injected > 0);
  assert.equal(f.calls.filter(row => row.name === 'socketEmit').length, 600);
  assert.equal(diagnostics.complete, false); assert.equal(diagnostics.failure, 'trace_bound');
  assert.equal(diagnostics.rows.length, 512);
  assertRestored(f);
});

test('a partial fixed request inventory is rejected without replacing original effects', { concurrency: false }, () => {
  const f = fixture({ expectedRequests: 3 });
  assert.equal(f.invoke(), f.request);
  onlyCall(f, 'invoke', f.receiver, f.invocationArgs);
  assert.equal(f.lifecycle.finish().complete, false);
  assertRestored(f);
});
