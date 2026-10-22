import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { EventEmitter } from 'node:events';
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync,
  rmSync, symlinkSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fetchLockedApks } from '../rust/distribution/ci/fetch-musl-apks.mjs';

const repository = 'https://dl-cdn.alpinelinux.org/alpine/v3.21/main';
const arches = ['aarch64', 'x86_64'];
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const body = (arch, index) => Buffer.from(`Synthetic SDK package ${arch} ${index}\n`);
const filenames = Array.from({ length: 61 }, (_, index) => `sdk-fixture-${String(index).padStart(2, '0')}-1.0-r0.apk`);

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'autorouter-musl-sdk-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const directory = join(root, 'packages'), proof = join(root, 'proof');
  mkdirSync(directory, { mode: 0o700 });
  mkdirSync(proof, { mode: 0o700 });
  const pinsPath = join(root, 'pins.json'), provenance = join(proof, 'inputs.json');
  const pins = { apk_package_set: { schema_version: 1, kind: 'closed_alpine_apk_sha256_set',
    repository, filenames: [...filenames], sha256_base64: Object.fromEntries(arches.map(arch =>
      [arch, filenames.map((_, index) => createHash('sha256').update(body(arch, index)).digest('base64'))])) } };
  const save = () => writeFileSync(pinsPath, JSON.stringify(pins));
  save();
  return { root, directory, proof, pinsPath, provenance, pins, save,
    expected: join(proof, 'apk-package-expected.sha256') };
}

// No sockets: this models https.get's asynchronous response and terminal events.
function network(arch, plan = () => ({})) {
  const calls = [], requests = [], responses = [];
  const get = (url, options, callback) => {
    const index = calls.length, request = new EventEmitter();
    const selected = plan(index);
    calls.push({ url, options });
    assert.equal(url, `${repository}/${arch}/${encodeURIComponent(filenames[index])}`);
    assert.deepEqual(options, { agent: false });
    request.destroyed = false;
    request.destroy = () => { request.destroyed = true; };
    requests.push(request);
    if (selected.throw) throw new Error('Synthetic request setup failure');
    queueMicrotask(() => {
      if (selected.requestError) { request.emit('error', new Error('Synthetic request failure')); return; }
      if (selected.noResponse) return;
      const response = new EventEmitter();
      response.statusCode = selected.status ?? 200;
      response.destroyed = false;
      response.destroy = () => { response.destroyed = true; };
      responses.push(response);
      callback(response);
      const chunks = selected.chunks ?? [body(arch, index)];
      for (const chunk of chunks) {
        if (response.destroyed) break;
        response.emit('data', chunk);
      }
      if (!response.destroyed) {
        if (selected.responseError) response.emit('error', new Error('Synthetic response failure'));
        else response.emit(selected.terminal ?? 'end');
      }
      selected.after?.();
    });
    return request;
  };
  return { get, calls, requests, responses };
}

function unpublished(f) {
  assert.equal(existsSync(f.provenance), false);
  assert.equal(existsSync(f.expected), false);
}

function timers(t) {
  const pending = new Map();
  t.mock.method(globalThis, 'setTimeout', (callback, duration) => {
    const token = {};
    pending.set(token, { callback, duration });
    return token;
  });
  t.mock.method(globalThis, 'clearTimeout', token => pending.delete(token));
  return pending;
}

test('musl collector source pin and Docker SDK roots match the checked-in manifest', () => {
  const pinsBytes = readFileSync(new URL('../rust/distribution/musl-builder-pins.json', import.meta.url));
  const pins = JSON.parse(pinsBytes);
  const collector = readFileSync(new URL('../rust/xtask/src/native_provenance_musl.rs', import.meta.url), 'utf8');
  const declarations = [...collector.matchAll(/^const PINS_SHA: &str = "([a-f0-9]{64})";$/gm)];
  assert.equal(declarations.length, 1, 'one explicit reviewed SDK manifest hash');
  assert.equal(declarations[0][1], sha256(pinsBytes), 'collector must admit the updated SDK bytes');
  const docker = readFileSync(new URL('../rust/distribution/ci/musl.Dockerfile', import.meta.url), 'utf8');
  for (const target of pins.targets) {
    for (const [name, version] of Object.entries(target.packages)) {
      assert.ok(docker.includes(`${name}-${version}`), `Docker SDK root ${name} must match the manifest`);
    }
  }
});

test('musl SDK pins bind the reviewed CA family and all59 unchanged filename/hash pairs per architecture', () => {
  const pins = JSON.parse(readFileSync(new URL('../rust/distribution/musl-builder-pins.json', import.meta.url)));
  const set = pins.apk_package_set;
  assert.equal(set.filenames.length, 61);
  assert.deepEqual(set.filenames, [...new Set(set.filenames)].sort());
  assert.equal(sha256(set.filenames.join('\n') + '\n'), 'ee48fae7322bfd52ebcca8e7302d7ed56cdbe1825ebbf67b6edb00837754f70c');
  const inventories = { aarch64: '2b440f75d8f086b8a11bc2dfbc3be141f983b801e39f15a0575897250e8ba7b1',
    x86_64: 'd2fde33332a759154db8cfb37e21dc17e51c8fdc1e1a24de7918524cd16c0f77' };
  // Derived from the prior reviewed61 set after excluding only the two CA
  // entries; these independent pins retain curl and every other package tuple.
  const retainedInventories = { aarch64: '994e4335b0b41408e4a831ff44b16d7bcbc72c6ba99226cfc2aa447707efc023',
    x86_64: '2836290bd6e9b176f35baf1e645327cdf972f0e2d95f9b44bba16e003c8507a1' };
  const replacements = new Set(['ca-certificates-20261020-r0.apk', 'ca-certificates-bundle-20261020-r0.apk']);
  for (const target of pins.targets) {
    assert.equal(target.packages['ca-certificates'], '20261020-r0');
    assert.equal(target.packages.curl, '8.14.1-r3');
  }
  assert.deepEqual(set.filenames.filter(filename => replacements.has(filename)), [...replacements]);
  assert.deepEqual(Object.keys(set.sha256_base64).sort(), arches);
  for (const arch of arches) {
    assert.equal(set.sha256_base64[arch].length, 61);
    const hashes = set.sha256_base64[arch].map(value => {
      const bytes = Buffer.from(value, 'base64');
      assert.equal(bytes.length, 32);
      assert.equal(bytes.toString('base64'), value);
      return bytes.toString('hex');
    });
    assert.equal(sha256(hashes.join('\n') + '\n'), inventories[arch]);
    const retained = set.filenames.flatMap((filename, index) =>
      replacements.has(filename) ? [] : [`${filename}  ${hashes[index]}\n`]);
    assert.equal(retained.length, 59);
    assert.equal(sha256(retained.join('')), retainedInventories[arch]);
  }
});

for (const arch of arches) test(`musl SDK downloads exact61 synthetic packages for ${arch} and records admitted bytes`, async t => {
  const f = fixture(t), mock = network(arch, index => {
    const bytes = body(arch, index);
    return { chunks: [bytes.subarray(0, 3), bytes.subarray(3)] };
  });
  await fetchLockedApks(arch, f.pinsPath, f.directory, f.provenance, mock);
  assert.equal(mock.calls.length, 61);
  assert.ok(mock.requests.every(request => request.destroyed));
  assert.ok(mock.responses.every(response => response.destroyed));
  assert.deepEqual(readdirSync(f.directory).sort(), filenames);
  const files = filenames.map((filename, index) => {
    const bytes = body(arch, index), path = join(f.directory, filename);
    assert.deepEqual(readFileSync(path), bytes);
    assert.equal(lstatSync(path).mode & 0o7777, 0o600);
    return { filename, sha256: sha256(bytes), bytes: bytes.length };
  });
  assert.deepEqual(JSON.parse(readFileSync(f.provenance)), { schema_version: 1,
    kind: 'verified_closed_alpine_apk_set', arch, pins_sha256: sha256(readFileSync(f.pinsPath)),
    package_count: 61, total_bytes: files.reduce((sum, file) => sum + file.bytes, 0), files });
  assert.equal(readFileSync(f.expected, 'utf8'), files.map(file => `${file.sha256}  ${join(f.directory, file.filename)}\n`).join(''));
});

const invalidManifests = [
  ['schema', set => { set.schema_version = 2; }],
  ['kind', set => { set.kind = 'mutable_index'; }],
  ['origin', set => { set.repository = 'https://example.invalid/mirror'; }],
  ['extra architecture', set => { set.sha256_base64.armv7 = [...set.sha256_base64.aarch64]; }],
  ['missing architecture', set => { delete set.sha256_base64.aarch64; }],
  ['missing filename', set => { set.filenames.pop(); }],
  ['duplicate filename', set => { set.filenames[1] = set.filenames[0]; }],
  ['unsorted filenames', set => { set.filenames.reverse(); }],
  ['filename traversal', set => { set.filenames[0] = '../escape-r0.apk'; set.filenames.sort(); }],
  ['absolute filename', set => { set.filenames[0] = '/escape-r0.apk'; set.filenames.sort(); }],
  ['encoded separator', set => { set.filenames[0] = 'bad%2fescape-r0.apk'; set.filenames.sort(); }],
  ['newline filename', set => { set.filenames[0] = 'bad\nescape-r0.apk'; set.filenames.sort(); }],
  ['non-string filename', set => { set.filenames[0] = 1; }],
];
for (const arch of arches) {
  invalidManifests.push([`${arch} missing digest`, set => { set.sha256_base64[arch].pop(); }]);
  for (const [label, value] of [['non-string', 7], ['short', Buffer.alloc(31).toString('base64')],
    ['long', Buffer.alloc(33).toString('base64')], ['unpadded', Buffer.alloc(32).toString('base64').replace(/=+$/, '')],
    ['noncanonical pad bits', Buffer.alloc(32).toString('base64').replace(/A=$/, 'B=')],
    ['whitespace', Buffer.alloc(32).toString('base64') + '\n'], ['invalid alphabet', '!'.repeat(44)]]) {
    invalidManifests.push([`${arch} ${label} digest`, set => { set.sha256_base64[arch][0] = value; }]);
  }
}
for (const [label, mutate] of invalidManifests) test(`musl SDK rejects ${label} before fetching`, async t => {
  const f = fixture(t), mock = network('x86_64');
  mutate(f.pins.apk_package_set);
  f.save();
  await assert.rejects(fetchLockedApks('x86_64', f.pinsPath, f.directory, f.provenance, mock));
  assert.equal(mock.calls.length, 0);
  assert.deepEqual(readdirSync(f.directory), []);
  unpublished(f);
});

test('musl SDK rejects unsupported architecture and oversized or symlinked pins without fetching', async t => {
  for (const kind of ['architecture', 'oversized', 'symlink']) {
    const f = fixture(t), mock = network('x86_64');
    if (kind === 'oversized') writeFileSync(f.pinsPath, ' '.repeat(65537));
    if (kind === 'symlink') {
      const target = join(f.root, 'real-pins.json');
      writeFileSync(target, readFileSync(f.pinsPath)); unlinkSync(f.pinsPath); symlinkSync(target, f.pinsPath);
    }
    await assert.rejects(fetchLockedApks(kind === 'architecture' ? 'armv7' : 'x86_64', f.pinsPath, f.directory, f.provenance, mock));
    assert.equal(mock.calls.length, 0);
    unpublished(f);
  }
});

test('musl SDK rejects occupied or symlinked output directories and existing proof files without overwriting', async t => {
  for (const kind of ['occupied', 'directory symlink', 'provenance', 'expected sidecar']) {
    const f = fixture(t), mock = network('x86_64'), sentinel = Buffer.from('keep existing owned bytes');
    let path;
    if (kind === 'directory symlink') {
      const target = join(f.root, 'other'); mkdirSync(target); rmSync(f.directory, { recursive: true }); symlinkSync(target, f.directory);
      path = join(target, 'sentinel'); writeFileSync(path, sentinel);
    } else {
      path = kind === 'occupied' ? join(f.directory, filenames[0]) : kind === 'provenance' ? f.provenance : f.expected;
      writeFileSync(path, sentinel);
    }
    await assert.rejects(fetchLockedApks('x86_64', f.pinsPath, f.directory, f.provenance, mock));
    assert.equal(mock.calls.length, 0);
    assert.deepEqual(readFileSync(path), sentinel);
  }
});

const failedResponses = [
  ['changed bytes', () => ({ chunks: [Buffer.from('changed')] }), /Locked APK digest/],
  ['incomplete bytes', () => ({ chunks: [body('x86_64', 1).subarray(0, 5)] }), /Locked APK digest/],
  ['wrong architecture bytes', () => ({ chunks: [body('aarch64', 1)] }), /Locked APK digest/],
  ['missing package', () => ({ status: 404 }), /Locked APK unavailable/],
  ['redirect', () => ({ status: 302 }), /Locked APK unavailable/],
  ['aborted response', () => ({ terminal: 'aborted' }), /Locked APK response aborted/],
  ['response error', () => ({ responseError: true }), /Locked APK response failed/],
  ['request error', () => ({ requestError: true }), /Locked APK request failed/],
  ['oversized package', () => ({ chunks: [Buffer.alloc(64 * 1024 * 1024 + 1)] }), /Locked APK file bound/],
];
for (const [label, response, error] of failedResponses) test(`musl SDK fails closed on ${label} and removes only the active partial`, async t => {
  const f = fixture(t), mock = network('x86_64', index => index === 1 ? response() : {});
  await assert.rejects(fetchLockedApks('x86_64', f.pinsPath, f.directory, f.provenance, mock), error);
  assert.equal(mock.calls.length, 2);
  assert.deepEqual(readdirSync(f.directory), [filenames[0]]);
  assert.deepEqual(readFileSync(join(f.directory, filenames[0])), body('x86_64', 0));
  assert.ok(mock.requests.every(request => request.destroyed));
  assert.ok(mock.responses.every(value => value.destroyed));
  unpublished(f);
});

test('musl SDK reports only HTTP status on unavailable packages without retry or response-body leaks', async t => {
  const pending = timers(t);
  const marker = 'synthetic-private-response-body-never-in-diagnostics';
  for (const status of [302, 404, 429, 503]) {
    const f = fixture(t), mock = network('x86_64', index => index === 1
      ? { status, chunks: [Buffer.from(marker)] } : {});
    await assert.rejects(fetchLockedApks('x86_64', f.pinsPath, f.directory, f.provenance, mock), error => {
      assert.equal(error.message, `Locked APK unavailable (HTTP ${status}): ${filenames[1]}`);
      assert.equal(error.message.includes(marker), false);
      return true;
    });
    assert.equal(mock.calls.length, 2);
    assert.ok(mock.requests.every(request => request.destroyed));
    assert.ok(mock.responses.every(response => response.destroyed));
    assert.equal(pending.size, 0);
    assert.deepEqual(readdirSync(f.directory), [filenames[0]]);
    assert.deepEqual(readFileSync(join(f.directory, filenames[0])), body('x86_64', 0));
    unpublished(f);
  }
});

test('musl SDK exclusive creation preserves a later colliding symlink and its target', async t => {
  const f = fixture(t), target = join(f.root, 'sentinel'), sentinel = Buffer.from('preserve target');
  writeFileSync(target, sentinel);
  const mock = network('x86_64', index => index === 0 ? { after: () => symlinkSync(target, join(f.directory, filenames[1])) } : {});
  await assert.rejects(fetchLockedApks('x86_64', f.pinsPath, f.directory, f.provenance, mock), { code: 'EEXIST' });
  assert.equal(mock.calls.length, 1);
  assert.equal(lstatSync(join(f.directory, filenames[1])).isSymbolicLink(), true);
  assert.deepEqual(readFileSync(target), sentinel);
  assert.deepEqual(readFileSync(join(f.directory, filenames[0])), body('x86_64', 0));
  unpublished(f);
});

test('musl SDK synchronous request failure clears its deadline and removes its partial file', async t => {
  const f = fixture(t), pending = timers(t), mock = network('x86_64', () => ({ throw: true }));
  await assert.rejects(fetchLockedApks('x86_64', f.pinsPath, f.directory, f.provenance, mock), /Synthetic request setup failure/);
  assert.equal(pending.size, 0);
  assert.deepEqual(readdirSync(f.directory), []);
  unpublished(f);
});

test('musl SDK file deadline cancels a stalled request, clears its timer and removes its partial', async t => {
  const f = fixture(t), pending = timers(t), mock = network('x86_64', () => ({ noResponse: true }));
  const download = fetchLockedApks('x86_64', f.pinsPath, f.directory, f.provenance, mock);
  const rejected = assert.rejects(download, /Locked APK download deadline/);
  assert.equal(pending.size, 1);
  const timer = [...pending.values()][0];
  assert.ok(timer.duration > 0 && timer.duration <= 60000);
  timer.callback();
  await rejected;
  assert.equal(pending.size, 0);
  assert.equal(mock.requests[0].destroyed, true);
  assert.deepEqual(readdirSync(f.directory), []);
  unpublished(f);
});

test('musl SDK total deadline stops before opening or requesting another package', async t => {
  const f = fixture(t);
  let now = 0;
  t.mock.method(performance, 'now', () => now);
  const mock = network('x86_64', index => index === 0 ? { after: () => { now = 600001; } } : {});
  await assert.rejects(fetchLockedApks('x86_64', f.pinsPath, f.directory, f.provenance, mock), /Locked APK set download deadline/);
  assert.equal(mock.calls.length, 1);
  assert.deepEqual(readdirSync(f.directory), [filenames[0]]);
  unpublished(f);
});

test('musl SDK accepts an exact64MiB package before the next controlled404 and removes fixtures', async t => {
  const f = fixture(t), chunk = Buffer.alloc(64 * 1024 * 1024, 0x62);
  const digest = createHash('sha256').update(chunk).digest('base64');
  for (const arch of arches) f.pins.apk_package_set.sha256_base64[arch][0] = digest;
  f.save();
  const mock = network('x86_64', index => index === 0 ? { chunks: [chunk] } : { status: 404 });
  try {
    await assert.rejects(fetchLockedApks('x86_64', f.pinsPath, f.directory, f.provenance, mock), /Locked APK unavailable/);
    assert.equal(mock.calls.length, 2);
    assert.deepEqual(readdirSync(f.directory), [filenames[0]]);
    const retained = join(f.directory, filenames[0]);
    assert.equal(lstatSync(retained).size, chunk.length);
    assert.equal(lstatSync(retained).mode & 0o7777, 0o600);
    assert.equal(sha256(readFileSync(retained)), Buffer.from(digest, 'base64').toString('hex'));
    assert.ok(mock.requests.every(request => request.destroyed));
    assert.ok(mock.responses.every(response => response.destroyed));
    unpublished(f);
  } finally {
    rmSync(f.root, { recursive: true, force: true });
    assert.equal(existsSync(f.root), false);
  }
});

test('musl SDK enforces aggregate256MiB on the ninth package without retaining its overrun', async t => {
  const f = fixture(t), chunk = Buffer.alloc(32 * 1024 * 1024, 0x61);
  const digest = createHash('sha256').update(chunk).digest('base64');
  for (const arch of arches) for (let index = 0; index < 8; index++) f.pins.apk_package_set.sha256_base64[arch][index] = digest;
  f.save();
  const mock = network('x86_64', index => ({ chunks: [index < 8 ? chunk : Buffer.from('x')] }));
  try {
    await assert.rejects(fetchLockedApks('x86_64', f.pinsPath, f.directory, f.provenance, mock), /Locked APK set bound/);
    assert.equal(mock.calls.length, 9);
    assert.deepEqual(readdirSync(f.directory).sort(), filenames.slice(0, 8));
    for (const filename of filenames.slice(0, 8)) {
      assert.equal(lstatSync(join(f.directory, filename)).size, chunk.length);
      assert.equal(lstatSync(join(f.directory, filename)).mode & 0o7777, 0o600);
    }
    unpublished(f);
  } finally {
    rmSync(f.root, { recursive: true, force: true });
    assert.equal(existsSync(f.root), false);
  }
});
