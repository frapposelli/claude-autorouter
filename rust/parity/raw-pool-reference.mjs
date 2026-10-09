// Source-only observations around the verified frozen gateway and actual global agent.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import http from 'node:http';
import https from 'node:https';
import net from 'node:net';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

assert.equal(process.version, 'v22.14.0');
assert.equal(process.argv.length, 3);
const reference = process.argv[2];
const baseline = await verifyBaseline(reference);
const [{ createRouterServer, listen }, { readConfig }] = await Promise.all([
  import(pathToFileURL(join(reference, 'src/server.mjs')).href),
  import(pathToFileURL(join(reference, 'src/config.mjs')).href),
]);
const config = readConfig(process.env);
for (const value of [config.upstream, config.jevEndpoint]) assert.equal(new URL(value).hostname, '127.0.0.1');
const [host, port] = process.env.AUTOROUTER_SYNTHETIC_POOL_CONTROL.split(':');
assert.equal(host, '127.0.0.1');
const transport = new URL(config.upstream).protocol === 'https:' ? https : http;
const agent = transport.globalAgent;
assert.equal(agent.options.timeout, 5000);
assert.equal(agent.maxFreeSockets, 256);
assert.equal(agent.scheduling, 'lifo');
assert.equal(agent.maxSockets, Infinity);
assert.equal(agent.maxTotalSockets, Infinity);
const rows = [], sockets = new Map(), identities = new WeakMap(), responses = new Map(), held = new Set();
const started = performance.now();
let submitted = 0, connections = 0;
const trace = (event, fields = {}) => { assert.ok(rows.length < 1024, 'raw pool trace bound'); rows.push({ event, ...fields, ms: Math.round(performance.now() - started) }); };
const free = socket => Object.values(agent.freeSockets).some(values => values.includes(socket));
const snapshot = () => ({
  rows, connections, live: sockets.size, requests: submitted,
  free: Object.values(agent.freeSockets).map(values => values.map(socket => identities.get(socket))),
  active: Object.values(agent.sockets).map(values => values.map(socket => identities.get(socket))),
  queued: Object.values(agent.requests).reduce((n, values) => n + values.length, 0),
  sockets: [...sockets].map(([id, socket]) => ({ id, timeout: socket.timeout, free: free(socket), destroyed: socket.destroyed })),
  held: [...responses.keys()],
});
const original = transport.request;
transport.request = function(url, options, callback) {
  const target = new URL(url);
  assert.equal(target.origin, new URL(config.upstream).origin);
  assert.match(target.pathname, /^\/v1\/models\/pool-[0-9]{1,3}$/);
  assert.equal(options.method, 'GET');
  assert.ok(++submitted <= 520, 'raw pool request bound');
  const ordinal = Number(target.pathname.split('-').at(-1));
  const req = original.call(this, url, options, response => {
    if (held.has(ordinal)) {
      const resume = response.resume;
      response.resume = function() { return this; };
      responses.set(ordinal, () => { response.resume = resume; responses.delete(ordinal); resume.call(response); });
    }
    callback(response);
  });
  req.once('error', error => trace('request-error', { request: ordinal, code: /^[A-Z0-9_]{1,60}$/.test(error.code ?? '') ? error.code : 'other' }));
  req.once('socket', socket => {
    let id = identities.get(socket);
    if (id === undefined) {
      id = ++connections; assert.ok(id <= 260, 'raw pool connection bound');
      identities.set(socket, id); sockets.set(id, socket);
      if (process.env.AUTOROUTER_SYNTHETIC_POOL_TIMERS === '1') {
        const setTimeout = socket.setTimeout, refresh = socket._unrefTimer;
        socket.setTimeout = function(ms, ...args) { trace('timer-set', { connection: id, before: this.timeout, after: ms }); return setTimeout.call(this, ms, ...args); };
        socket._unrefTimer = function(...args) { trace('timer-refresh', { connection: id, timeout: this.timeout }); return refresh.apply(this, args); };
      }
      socket.on('free', () => trace('free', { connection: id, kept: free(socket), timeout: socket.timeout }));
      socket.on('timeout', () => trace('timeout', { connection: id, free: free(socket), destroyed: socket.destroyed }));
      socket.once('close', hadError => { trace('close', { connection: id, had_error: hadError }); sockets.delete(id); });
      socket.on('error', () => {});
    }
    trace('assigned', { request: ordinal, connection: id, reused: req.reusedSocket });
  });
  return req;
};
const gateway = createRouterServer(config, { router: { route() { assert.fail('pool fixture must not classify'); } }, tokenCounter() { assert.fail('pool fixture must not count'); }, log() {}, onStatus() {} });
const incoming = new Set();
gateway.on('connection', socket => { assert.ok(incoming.size < 260); incoming.add(socket); socket.once('close', () => incoming.delete(socket)); });
const address = await listen(gateway, 0);
const control = net.createConnection({ host, port: Number(port) });
await once(control, 'connect');
const reply = value => { const line = JSON.stringify(value); assert.ok(Buffer.byteLength(line) <= 131072, 'pool control response bound'); control.write(line + '\n'); };
reply({ port: address.port, baseline, builtins: Object.fromEntries(['_http_agent', '_http_client', '_http_incoming', 'net', 'https'].map(name => [name, createHash('sha256').update(process.binding('natives')[name]).digest('hex')])) });
let stopping = false, buffer = '', commands = 0;
async function cleanup() {
  if (stopping) return;
  stopping = true;
  for (const release of responses.values()) release();
  transport.request = original;
  const closed = [...sockets.values(), ...incoming].map(socket => new Promise(resolve => { socket.once('close', resolve); socket.destroy(); }));
  http.globalAgent.destroy(); https.globalAgent.destroy();
  closed.push(new Promise(resolve => gateway.close(resolve)));
  let timer;
  try { await Promise.race([Promise.all(closed), new Promise((_, reject) => { timer = setTimeout(() => reject(Error('raw pool owned cleanup deadline')), 2000); })]); }
  finally { clearTimeout(timer); }
  assert.equal(sockets.size, 0); assert.equal(incoming.size, 0); assert.equal(responses.size, 0);
  reply({ cleanup: true, ...snapshot() }); control.end();
}
control.on('data', chunk => {
  buffer += chunk.toString('utf8'); assert.ok(Buffer.byteLength(buffer) <= 4096, 'pool control command bound');
  while (buffer.includes('\n')) {
    assert.ok(++commands <= 1024, 'pool control count bound');
    const end = buffer.indexOf('\n'), command = JSON.parse(buffer.slice(0, end)); buffer = buffer.slice(end + 1);
    if (command.op === 'stop') { void cleanup(); return; }
    if (command.op === 'hold') { assert.ok(Number.isInteger(command.request) && command.request >= 0 && command.request < 520); held.add(command.request); }
    else if (command.op === 'release') { const release = responses.get(command.request); assert.ok(release); release(); }
    else assert.equal(command.op, 'snapshot');
    reply(snapshot());
  }
});
control.on('end', () => { void cleanup(); });
control.on('error', () => { void cleanup(); });
process.once('SIGTERM', () => { void cleanup(); });
