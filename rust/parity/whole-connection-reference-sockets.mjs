// Bounded development-only sockets; no network or user/provider data is used.
import { Duplex } from 'node:stream';

export const MAX_REQUEST = 1024 * 1024;
export const MAX_RESPONSE = 128 * 1024;
export const RESPONSE = Buffer.from('{"id":"synthetic-benchmark-message","type":"message","role":"assistant","model":"claude-sonnet-5","content":[{"type":"text","text":"Synthetic benchmark response."}],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":32,"output_tokens":8}}');
export const PROVIDER_WIRE = Buffer.concat([Buffer.from(`HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: ${RESPONSE.length}\r\nX-Probe: synthetic\r\nConnection: keep-alive, x-hop\r\nX-Hop: removed\r\n\r\n`), RESPONSE]);

export class ImmediateSocket extends Duplex {
  constructor(write, limit = MAX_RESPONSE) {
    super({ allowHalfOpen: true });
    this.writeCallback = write;
    this.limit = limit;
    this.queuedBytes = 0;
    this.queued = [];
    this.readRequested = false;
    this.remoteAddress = '127.0.0.1';
    this.remotePort = 1;
    this.localAddress = '127.0.0.1';
    this.localPort = 1;
    this.connecting = false;
    this.holdWrites = false;
    this.heldDone = [];
  }
  feed(bytes) {
    if (this.destroyed || bytes.length > this.limit - this.queuedBytes) throw new Error('Synthetic socket bound');
    for (let offset = 0; offset < bytes.length; offset += 8192) this.queued.push(bytes.subarray(offset, offset + 8192));
    this.queuedBytes += bytes.length;
    if (this.readRequested) this._read();
  }
  _read() {
    this.readRequested = true;
    while (this.queued.length) {
      const bytes = this.queued.shift();
      this.queuedBytes -= bytes.length;
      if (!this.push(bytes)) { this.readRequested = false; break; }
    }
  }
  _write(bytes, _encoding, done) {
    try { this.writeCallback(Buffer.from(bytes), this); if (this.holdWrites) this.heldDone.push(done); else done(); } catch { done(new Error('Synthetic socket write failed')); }
  }
  releaseWrites() { this.holdWrites = false; for (const done of this.heldDone.splice(0)) done(); }
  setTimeout(_milliseconds, callback) { if (callback) this.once('timeout', callback); return this; }
  setNoDelay() { return this; }
  setKeepAlive() { return this; }
  ref() { return this; }
  unref() { return this; }
  _destroy(error, done) { this.queued.length = 0; this.queuedBytes = 0; for (const pending of this.heldDone.splice(0)) pending(error ?? new Error('Synthetic socket closed')); done(error); }
}

export function parseWire(bytes, request = false) {
  if (bytes.length > (request ? MAX_REQUEST : MAX_RESPONSE)) throw new Error('Synthetic wire bound');
  const offset = bytes.indexOf('\r\n\r\n');
  if (offset < 0) return undefined;
  if (offset > 16 * 1024) throw new Error('Synthetic header bound');
  const lines = bytes.subarray(0, offset).toString('latin1').split('\r\n');
  const first = lines.shift();
  const headers = {};
  for (const line of lines) {
    const colon = line.indexOf(':');
    if (colon <= 0) throw new Error('Invalid synthetic header');
    const name = line.slice(0, colon).toLowerCase();
    if (headers[name] !== undefined) throw new Error('Duplicate synthetic header');
    headers[name] = line.slice(colon + 1).trim();
  }
  let body;
  let consumed;
  const start = offset + 4;
  if (headers['transfer-encoding'] === 'chunked') {
    const chunks = [];
    let cursor = start;
    for (;;) {
      const end = bytes.indexOf('\r\n', cursor);
      if (end < 0) return undefined;
      const sizeText = bytes.subarray(cursor, end).toString('ascii');
      if (!/^[0-9a-f]+$/i.test(sizeText)) throw new Error('Invalid synthetic chunk');
      const size = Number.parseInt(sizeText, 16);
      cursor = end + 2;
      if (bytes.length < cursor + size + 2) return undefined;
      if (bytes.subarray(cursor + size, cursor + size + 2).toString() !== '\r\n') throw new Error('Invalid synthetic framing');
      if (size === 0) { consumed = cursor + 2; break; }
      chunks.push(bytes.subarray(cursor, cursor + size));
      cursor += size + 2;
    }
    body = Buffer.concat(chunks);
  } else {
    const length = Number(headers['content-length']);
    if (!Number.isSafeInteger(length) || length < 0) throw new Error('Missing synthetic length');
    consumed = start + length;
    if (bytes.length < consumed) return undefined;
    body = bytes.subarray(start, consumed);
  }
  const semantic = Object.fromEntries(Object.entries(headers).filter(([name]) => ![
    'date', 'host', 'connection', 'keep-alive', 'content-length', 'transfer-encoding',
  ].includes(name)).sort(([left], [right]) => left.localeCompare(right)));
  const status = request ? undefined : Number(first?.split(' ')[1]);
  if (!request && !Number.isInteger(status)) throw new Error('Invalid synthetic status');
  return { first, status, headers: semantic, body, consumed };
}

export class ProviderCodec {
  constructor(record) { this.bytes = Buffer.alloc(0); this.record = record; }
  write(bytes, socket) {
    if (bytes.length > MAX_REQUEST - this.bytes.length) throw new Error('Synthetic request bound');
    this.bytes = Buffer.concat([this.bytes, bytes]);
    const request = parseWire(this.bytes, true);
    if (!request) return;
    if (request.first !== 'POST /v1/messages?probe=whole HTTP/1.1') throw new Error('Forbidden synthetic endpoint');
    this.record(request);
    this.bytes = this.bytes.subarray(request.consumed);
    if (this.bytes.length) throw new Error('Unexpected synthetic pipelining');
    queueMicrotask(() => { if (!socket.destroyed) socket.feed(PROVIDER_WIRE); });
  }
}
