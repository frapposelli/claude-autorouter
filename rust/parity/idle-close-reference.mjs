// Source-only wrapper around the hash-verified frozen gateway. Synthetic controls
// disclose their socket pause and request _flush gate; no production import.
import assert from 'node:assert/strict';
import net from 'node:net';
import https from 'node:https';
import http from 'node:http';
import { once } from 'node:events';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

assert.equal(process.version, 'v22.14.0');
assert.equal(process.argv.length, 3);
const reference = process.argv[2];
await verifyBaseline(reference);
const [{ createRouterServer, listen }, { readConfig }] = await Promise.all([
  import(pathToFileURL(join(reference, 'src/server.mjs')).href),
  import(pathToFileURL(join(reference, 'src/config.mjs')).href),
]);
const config = readConfig(process.env);
for (const value of [config.upstream, config.jevEndpoint]) {
  const url = new URL(value);
  assert.equal(url.hostname, '127.0.0.1');
}
const [host, portText] = process.env.AUTOROUTER_SYNTHETIC_IDLE_CONTROL.split(':');
assert.equal(host, '127.0.0.1');
const rows = [], sockets = new Map(), identities = new WeakMap(), selected = new Map(), holds = new Map();
let requests = 0, connections = 0;
const emit = row => { assert.ok(rows.length < 256, 'idle trace bound'); rows.push(row); };
const original = https.request;
https.request = function(url, options, callback) {
  assert.equal(new URL(url).origin, new URL(config.upstream).origin);
  assert.equal(options.method, 'GET');
  const request = ++requests;
  assert.ok(request <= 16, 'idle request bound');
  emit(`Submitted(${request})`);
  const req = original.call(this, url, options, response => {
    emit(`ResponseHead(${request})`);
    response.on('end', () => emit(`BodyComplete(${request})`));
    callback(response);
  });
  req.once('error', error => {
    emit(`ResponseError(${request})`);
    emit(`RequestErrorCode(${request}, ${/^[A-Z0-9_]{1,60}$/.test(error.code ?? '') ? error.code : 'other'})`);
  });
  req.once('socket', socket => {
    let id = identities.get(socket);
    if (id === undefined) {
      id = ++connections;
      assert.ok(id <= 8, 'idle connection bound');
      identities.set(socket, id); sockets.set(id, socket);
      emit(`Connected(${id})`);
      socket.once('end', () => emit(`ReadEof(${id})`));
      socket.once('error', error => emit(`SocketError(${id}, ${/^[A-Z0-9_]{1,60}$/.test(error.code ?? '') ? error.code : 'other'})`));
      socket.once('close', hadError => { emit(`SocketClose(${id}, ${hadError})`); emit(`Released(${id})`); sockets.delete(id); });
    }
    selected.set(request, id);
    emit(`AssignmentObserved(${request}, ${id})`);
    const hold = holds.get(`${id}:write`);
    if (hold) {
      assert.equal(hold.release, undefined);
      const flush = req._flush;
      let called = false;
      req._flush = function() { assert.equal(called, false, 'idle _flush gate called twice'); called = true; emit(`WriteHeld(${id})`); };
      hold.release = () => { req._flush = flush; if (called && !req.destroyed) flush.call(req); };
    }
  });
  return req;
};
const gateway = createRouterServer(config, {
  router: { route() { assert.fail('idle fixture must not classify'); } },
  tokenCounter() { assert.fail('idle fixture must not count'); },
  log() {}, onStatus() {},
});
const inbound = new Set();
gateway.on('connection', socket => { inbound.add(socket); socket.once('close', () => inbound.delete(socket)); });
const address = await listen(gateway, 0);
const control = net.createConnection({ host, port: Number(portText) });
await once(control, 'connect');
const reply = value => { const line = JSON.stringify(value); assert.ok(Buffer.byteLength(line) < 32768); control.write(line + '\n'); };
reply({ port: address.port });
let buffer = '', stopping = false;
const release = () => { for (const held of holds.values()) held.release?.(); holds.clear(); };
async function cleanup() {
  if (stopping) return;
  stopping = true;
  release(); https.request = original;
  const closed = [...sockets.values(), ...inbound].map(socket => new Promise(resolve => { socket.once('close', resolve); socket.destroy(); }));
  http.globalAgent.destroy(); https.globalAgent.destroy();
  closed.push(new Promise(resolve => gateway.close(resolve)));
  let deadline;
  try { await Promise.race([Promise.all(closed), new Promise((_, reject) => { deadline = setTimeout(() => reject(Error('idle socket cleanup deadline')), 2000); })]); }
  finally { clearTimeout(deadline); }
  assert.equal(sockets.size, 0); assert.equal(inbound.size, 0);
  emit('Cleanup'); reply({ cleanup: true, rows, active: 0, tasks: 0, shutdown_handles: 0 });
  control.end();
}
control.on('data', chunk => {
  buffer += chunk.toString('utf8');
  assert.ok(Buffer.byteLength(buffer) <= 512, 'idle control command bound');
  while (buffer.includes('\n')) {
    const end = buffer.indexOf('\n'), line = buffer.slice(0, end); buffer = buffer.slice(end + 1);
    const command = JSON.parse(line);
    if (command.op === 'stop') { void cleanup(); return; }
    if (command.op === 'hold') {
      const id = command.connection, socket = sockets.get(id);
      assert.ok(socket && !socket.destroyed);
      const key = `${id}:${command.kind}`; assert.equal(holds.has(key), false);
      if (command.kind === 'read') { socket.pause(); holds.set(key, { release: () => socket.resume() }); emit(`ReadHeld(${id})`); }
      else { assert.equal(command.kind, 'write'); holds.set(key, {}); }
    } else if (command.op === 'release') release();
    else assert.equal(command.op, 'snapshot');
    reply({ rows, selected: [...selected], live: sockets.size });
  }
});
control.on('end', () => { void cleanup(); });
control.on('error', () => { void cleanup(); });
process.once('SIGTERM', () => { void cleanup(); });
