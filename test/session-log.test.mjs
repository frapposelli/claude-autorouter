import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { createSessionLog } from '../src/session-log.mjs';

const decision = (extra = {}) => ({ schema_version: 2, event: 'decision', timestamp: '2026-10-01T12:00:00.000Z',
  request_id: 'request-1', session_id: 'session-a', request_class: 'main', prompt_excerpt: 'Fix a typo', prompt_truncated: false,
  requested_model: 'claude-haiku-4-5-20251001', selected_model: 'claude-sonnet-5-5', decision_latency_ms: 12.5,
  source: 'jev', reason: 'low_confidence', evaluator: 'jev', classified_tier: 'haiku', ...extra });

async function fixture(t) {
  const root = await fs.mkdtemp(join(tmpdir(), 'autorouter-session-log-'));
  const directory = join(root, 'logs');
  const writers = [];
  t.after(async () => {
    await Promise.all(writers.map(writer => writer.close()));
    await fs.rm(root, { recursive: true, force: true });
  });
  return { root, directory, async create(options, path = directory) {
    const writer = await createSessionLog(path, options);
    writers.push(writer);
    return writer;
  } };
}

async function files(directory) {
  return Promise.all((await fs.readdir(directory)).sort().map(async name => {
    const text = await fs.readFile(join(directory, name), 'utf8');
    return { name, text, rows: text.trim() ? text.trimEnd().split('\n').map(line => JSON.parse(line)) : [] };
  }));
}

test('concurrent decisions stay ordered in one private file per session, including an anonymous session', async t => {
  const f = await fixture(t), warnings = [];
  const writer = await f.create({ warn: message => warnings.push(message) });
  await Promise.all(Array.from({ length: 120 }, async (_, index) => {
    await Promise.resolve();
    assert.equal(writer.record(decision({ request_id: `request-${index}`, session_id: index % 3 === 2 ? undefined : `session-${index % 3}`,
      agent_id: `agent-${index % 5}` })), true);
  }));
  await writer.close();
  const logs = await files(f.directory);
  assert.equal(logs.length, 3);
  assert.equal((await fs.stat(f.directory)).mode & 0o777, 0o700);
  for (const file of logs) {
    assert.match(file.name, /^autorouter-session-[A-Za-z0-9-]+\.jsonl$/);
    assert.ok(!file.name.includes('session-0') && !file.name.includes('session-1'));
    assert.equal((await fs.stat(join(f.directory, file.name))).mode & 0o777, 0o600);
    assert.equal(new Set(file.rows.map(row => row.session_id)).size, 1);
    assert.equal(file.rows.length, 40);
    const indexes = file.rows.map(row => Number(row.request_id.slice('request-'.length)));
    assert.deepEqual(indexes, [...indexes].sort((a, b) => a - b));
    assert.ok(file.rows.every(row => row.schema_version === 2));
  }
  assert.deepEqual(warnings, []);
});

test('separate launches never append to each other and existing directory modes are unchanged', async t => {
  const f = await fixture(t);
  await fs.mkdir(f.directory, { mode: 0o755 });
  await fs.chmod(f.directory, 0o755);
  const [first, second] = await Promise.all([f.create(), f.create()]);
  first.record(decision({ prompt_excerpt: 'First launch' }));
  second.record(decision({ prompt_excerpt: 'Second launch' }));
  const closing = first.close();
  assert.equal(first.close(), closing);
  assert.equal(first.record(decision()), false);
  await Promise.all([closing, second.close()]);
  assert.equal((await fs.stat(f.directory)).mode & 0o777, 0o755);
  const logs = await files(f.directory);
  assert.equal(logs.length, 2);
  assert.deepEqual(logs.map(file => file.rows[0].prompt_excerpt).sort(), ['First launch', 'Second launch']);
  assert.ok(logs.every(file => file.rows.length === 1));
});

test('rows copy only bounded metadata, retain Auto safety reasons, and never serialize bodies or errors', async t => {
  const f = await fixture(t), writer = await f.create();
  const secret = 'PRIVATE_HEADER_BODY_ERROR';
  const entry = decision({ source: 'passthrough', reason: 'auto_mode_safeguards', classifier_error: 'timeout',
    body: { private: secret }, headers: { authorization: secret }, error: new Error(secret) });
  Object.defineProperty(entry, 'raw_response', { get() { throw new Error(secret); } });
  assert.equal(writer.record(entry), true);
  entry.prompt_excerpt = 'Changed after queuing';
  entry.selected_model = 'changed';
  await writer.close();
  const [file] = await files(f.directory), [row] = file.rows;
  assert.equal(file.text.includes(secret), false);
  assert.deepEqual(row, { schema_version: 2, event: 'decision', timestamp: '2026-10-01T12:00:00.000Z', request_id: 'request-1',
    session_id: 'session-a', prompt_excerpt: 'Fix a typo', prompt_truncated: false,
    requested_model: 'claude-haiku-4-5-20251001', selected_model: 'claude-sonnet-5-5', request_class: 'main',
    decision_latency_ms: 12.5, source: 'passthrough', reason: 'auto_mode_safeguards', evaluator: 'jev', classified_tier: 'haiku', classifier_error: 'timeout' });
});

test('prompt excerpts use at most 500 Unicode characters and auxiliary classes never retain prompt text', async t => {
  const f = await fixture(t), writer = await f.create();
  writer.record(decision({ prompt_excerpt: '😀'.repeat(501) + 'PRIVATE_TAIL' }));
  writer.record(decision({ request_id: 'already-truncated', prompt_excerpt: '短い', prompt_truncated: true }));
  writer.record(decision({ request_id: 'exact', prompt_excerpt: '😀'.repeat(500) }));
  writer.record(decision({ request_id: 'without-class', request_class: undefined, prompt_excerpt: 'Included foreground text' }));
  for (const request_class of ['auxiliary', 'compaction', 'safety_review']) writer.record(decision({
    request_id: request_class, request_class, prompt_excerpt: 'PRIVATE_BACKGROUND_PROMPT', prompt_truncated: true,
  }));
  await writer.close();
  const [file] = await files(f.directory);
  assert.equal(file.text.includes('PRIVATE_'), false);
  assert.equal([...file.rows[0].prompt_excerpt].length, 500);
  assert.equal(file.rows[0].prompt_excerpt, '😀'.repeat(500));
  assert.equal(file.rows[0].prompt_truncated, true);
  assert.equal(file.rows[1].prompt_truncated, true);
  assert.equal(file.rows[2].prompt_truncated, false);
  assert.equal(file.rows[3].prompt_excerpt, 'Included foreground text');
  for (const row of file.rows.slice(4)) assert.deepEqual([row.prompt_excerpt, row.prompt_truncated], ['', false]);
});

test('unlimited evaluator runtimes can retain finite decision latency beyond one hour', async t => {
  const f = await fixture(t), writer = await f.create();
  assert.equal(writer.record(decision({ decision_latency_ms: 7200000.125 })), true);
  await writer.close();
  assert.equal((await files(f.directory))[0].rows[0].decision_latency_ms, 7200000.125);
});

test('invalid IDs cannot traverse paths or merge into anonymous logs; malformed fields are omitted', async t => {
  const f = await fixture(t), writer = await f.create();
  for (const session_id of ['../../outside', 'bad\nsession', 'x'.repeat(201), {}, ['session']]) {
    assert.equal(writer.record(decision({ session_id })), false);
  }
  for (const entry of [null, [], { event: 'error' }, decision({ request_id: '../request' }), decision({ selected_model: 'bad\nmodel' }), decision({ agent_id: '../agent' }), decision({ prompt_id: 'p'.repeat(201) })]) {
    assert.equal(writer.record(entry), false);
  }
  const getter = decision();
  Object.defineProperty(getter, 'selected_model', { get() { throw new Error('PRIVATE_GETTER'); } });
  assert.doesNotThrow(() => assert.equal(writer.record(getter), false));
  assert.equal(writer.record(decision({ session_id: null, decision_latency_ms: Infinity,
    timestamp: 'PRIVATE_TIMESTAMP', source: 'PRIVATE SOURCE', reason: 'PRIVATE ERROR', classifier_error: 'PRIVATE ERROR', evaluator: 'other', classified_tier: 'other' })), true);
  await writer.close();
  const [file] = await files(f.directory);
  assert.equal((await fs.readdir(f.root)).length, 1);
  assert.equal(file.rows.length, 1);
  const [row] = file.rows;
  for (const key of ['session_id', 'agent_id', 'prompt_id', 'decision_latency_ms', 'source', 'reason', 'classifier_error', 'evaluator', 'classified_tier']) assert.equal(Object.hasOwn(row, key), false);
  assert.equal(file.text.includes('PRIVATE'), false);
  assert.match(row.timestamp, /^\d{4}-\d\d-\d\dT/);
});

test('queue backpressure is bounded in UTF-8 bytes and shutdown drains every accepted line', async t => {
  const f = await fixture(t), warnings = [];
  const writer = await f.create({ warn: message => warnings.push(message) });
  let accepted = 0;
  // JSON escaping expands these characters, so raw string length is not a
  // sufficient bound for the retained queue or eventual file bytes.
  for (let index = 0; index < 5000; index++) {
    if (!writer.record(decision({ request_id: `request-${index}`, prompt_excerpt: '\0'.repeat(500) }))) break;
    accepted++;
  }
  assert.ok(accepted > 100 && accepted < 1000);
  assert.equal(writer.record(decision()), false);
  assert.deepEqual(warnings, ['AutoRouter session logging disabled.']);
  await writer.close();
  const [file] = await files(f.directory);
  assert.ok(Buffer.byteLength(file.text) <= 1024 * 1024);
  assert.equal(file.rows.length, accepted);
  assert.deepEqual(file.rows.map(row => row.request_id), Array.from({ length: accepted }, (_, index) => `request-${index}`));
});

test('at most 128 sessions and descriptors are retained, and accepted sessions drain on overflow', async t => {
  const f = await fixture(t), warnings = [], handles = [];
  const originalOpen = fs.open;
  t.mock.method(fs, 'open', async (...args) => { const handle = await originalOpen(...args); handles.push(handle); return handle; });
  const writer = await f.create({ warn: message => warnings.push(message) });
  for (let index = 0; index < 128; index++) assert.equal(writer.record(decision({ session_id: `session-${index}` })), true);
  assert.equal(writer.record(decision({ session_id: 'session-overflow' })), false);
  assert.equal(writer.record(decision({ session_id: 'session-0' })), false);
  await writer.close();
  assert.equal(handles.length, 128);
  assert.ok(handles.every(handle => handle.fd === -1));
  assert.equal((await files(f.directory)).length, 128);
  assert.deepEqual(warnings, ['AutoRouter session logging disabled.']);
});

test('existing and dangling log-directory symlinks fail open without following their targets', async t => {
  const f = await fixture(t);
  const target = join(f.root, 'target');
  await fs.mkdir(target);
  await fs.writeFile(join(target, 'unchanged'), 'untouched');
  for (const [name, destination] of [['existing', target], ['dangling', join(f.root, 'missing')]]) {
    const path = join(f.root, name), warnings = [];
    await fs.symlink(destination, path);
    const writer = await f.create({ warn: message => warnings.push(message) }, path);
    assert.equal(writer.record(decision()), false);
    await writer.close();
    assert.deepEqual(warnings, ['AutoRouter session logging disabled.']);
    assert.equal((await fs.lstat(path)).isSymbolicLink(), true);
  }
  assert.deepEqual(await fs.readdir(target), ['unchanged']);
  assert.equal(await fs.readFile(join(target, 'unchanged'), 'utf8'), 'untouched');
  await assert.rejects(fs.stat(join(f.root, 'missing')), { code: 'ENOENT' });
});

test('exclusive no-follow creation rejects a file symlink inserted before open and preserves the target', async t => {
  const f = await fixture(t), warnings = [];
  const target = join(f.root, 'PRIVATE_TARGET');
  await fs.writeFile(target, 'untouched', { mode: 0o644 });
  const originalOpen = fs.open;
  t.mock.method(fs, 'open', async (...args) => {
    await fs.symlink(target, args[0]);
    return originalOpen(...args);
  });
  const writer = await f.create({ warn: message => warnings.push(message) });
  assert.equal(writer.record(decision()), true);
  await writer.close();
  assert.equal(await fs.readFile(target, 'utf8'), 'untouched');
  assert.equal((await fs.stat(target)).mode & 0o777, 0o644);
  assert.deepEqual(warnings, ['AutoRouter session logging disabled.']);
  const [name] = await fs.readdir(f.directory);
  assert.equal((await fs.lstat(join(f.directory, name))).isSymbolicLink(), true);
});

test('a replaced directory is rejected before any session file is created', async t => {
  const f = await fixture(t), warnings = [];
  const writer = await f.create({ warn: message => warnings.push(message) });
  const target = join(f.root, 'replacement');
  await fs.mkdir(target);
  await fs.rename(f.directory, join(f.root, 'original'));
  await fs.symlink(target, f.directory);
  assert.equal(writer.record(decision()), true);
  await writer.close();
  assert.deepEqual(await fs.readdir(target), []);
  assert.deepEqual(warnings, ['AutoRouter session logging disabled.']);
});

test('filesystem and warning failures never reject the factory, record, or close', async t => {
  const f = await fixture(t), warnings = [];
  const path = join(f.root, 'PRIVATE_INVALID_PATH');
  await fs.writeFile(path, 'not a directory');
  const writer = await f.create({ warn: message => { warnings.push(message); throw new Error('PRIVATE_CALLBACK'); } }, path);
  assert.equal(writer.record(decision({ prompt_excerpt: 'PRIVATE_PROMPT' })), false);
  await writer.close();
  await writer.close();
  assert.deepEqual(warnings, ['AutoRouter session logging disabled.']);
  const rejectedWarning = await f.create({ warn: async () => { throw new Error('PRIVATE_ASYNC_CALLBACK'); } }, path);
  assert.equal(rejectedWarning.record(decision()), false);
  await rejectedWarning.close();
});

test('a write failure disables logging once and shutdown still closes its descriptor', async t => {
  const f = await fixture(t), warnings = [], handles = [];
  let signalWarning;
  const warned = new Promise(resolve => { signalWarning = resolve; });
  const originalOpen = fs.open;
  t.mock.method(fs, 'open', async (...args) => {
    const handle = await originalOpen(...args);
    handles.push(handle);
    t.mock.method(handle, 'writeFile', async () => { throw new Error('PRIVATE_DISK_ERROR'); });
    return handle;
  });
  const writer = await f.create({ warn: message => { warnings.push(message); signalWarning(); } });
  assert.equal(writer.record(decision({ prompt_excerpt: 'PRIVATE_PROMPT' })), true);
  await warned;
  assert.equal(writer.record(decision()), false);
  await writer.close();
  assert.equal(handles.length, 1);
  assert.equal(handles[0].fd, -1);
  assert.deepEqual(warnings, ['AutoRouter session logging disabled.']);
});

test('close waits for accepted in-flight writes, closes once, and rejects later records', async t => {
  const f = await fixture(t);
  let beginWrite, releaseWrite;
  const started = new Promise(resolve => { beginWrite = resolve; });
  const released = new Promise(resolve => { releaseWrite = resolve; });
  const originalOpen = fs.open;
  t.mock.method(fs, 'open', async (...args) => {
    const handle = await originalOpen(...args);
    const originalWrite = handle.writeFile.bind(handle);
    t.mock.method(handle, 'writeFile', async (...writeArgs) => { beginWrite(); await released; return originalWrite(...writeArgs); });
    return handle;
  });
  const writer = await f.create();
  for (let index = 0; index < 3; index++) assert.equal(writer.record(decision({ request_id: `request-${index}` })), true);
  await started;
  const closing = writer.close();
  assert.equal(writer.close(), closing);
  assert.equal(writer.record(decision({ request_id: 'too-late' })), false);
  let closed = false;
  closing.then(() => { closed = true; });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(closed, false);
  releaseWrite();
  await closing;
  const [file] = await files(f.directory);
  assert.deepEqual(file.rows.map(row => row.request_id), ['request-0', 'request-1', 'request-2']);
});

test('continuous arrivals during writes drain in order without dropping accepted records', async t => {
  const f = await fixture(t), warnings = [];
  let writer, written = 0, finish;
  const completed = new Promise(resolve => { finish = resolve; });
  const originalOpen = fs.open;
  t.mock.method(fs, 'open', async (...args) => {
    const handle = await originalOpen(...args);
    const originalWrite = handle.writeFile.bind(handle);
    t.mock.method(handle, 'writeFile', async (...writeArgs) => {
      // Keep a new record waiting whenever the previous record completes.
      if (written < 799) assert.equal(writer.record(decision({ request_id: `request-${written + 1}` })), true);
      await originalWrite(...writeArgs);
      if (++written === 800) finish();
    });
    return handle;
  });
  writer = await f.create({ warn: message => warnings.push(message) });
  assert.equal(writer.record(decision({ request_id: 'request-0' })), true);
  await completed;
  await writer.close();
  const [file] = await files(f.directory);
  assert.deepEqual(file.rows.map(row => row.request_id), Array.from({ length: 800 }, (_, index) => `request-${index}`));
  assert.deepEqual(warnings, []);
});

test('metadata mode omits prompt fields and retains correlated safe outcomes', async t => {
  const f = await fixture(t), writer = await f.create({ includePrompts: false });
  assert.equal(writer.record(decision({ prompt_excerpt: 'PRIVATE_PROMPT' })), true);
  assert.equal(writer.record({ schema_version: 2, event: 'outcome', timestamp: '2026-10-01T12:00:01.000Z',
    request_id: 'request-1', session_id: 'session-a', status: 'completed', http_status: 200,
    requested_model: 'claude-haiku-4-5-20251001', selected_model: 'claude-sonnet-5-5', confirmed_model: 'claude-sonnet-5-5',
    completion_confirmed: true, usage_complete: true, usage: { input_tokens: 100, output_tokens: 20, private: 'PRIVATE_USAGE' },
    baseline_model: 'claude-opus-5-5', pricing_version: '2026-09-29.1', total_latency_ms: 650,
    body: { text: 'PRIVATE_BODY' }, prompt_excerpt: 'PRIVATE_PROMPT', headers: { authorization: 'PRIVATE_AUTH' },
  }), true);
  await writer.close();
  const [file] = await files(f.directory);
  assert.equal(file.rows.length, 2);
  assert.ok(file.rows.every(row => row.schema_version === 2 && !Object.hasOwn(row, 'prompt_excerpt') && !Object.hasOwn(row, 'prompt_truncated')));
  assert.equal(file.rows[0].request_id, file.rows[1].request_id);
  assert.equal(file.rows[1].confirmed_model, 'claude-sonnet-5-5');
  assert.equal(file.rows[1].completion_confirmed, true);
  assert.deepEqual(file.rows[1].usage, { input_tokens: 100, output_tokens: 20 });
  assert.ok(!file.text.includes('PRIVATE_'));
});

test('pre-routing failures can be recorded without selected models and auxiliary records never retain excerpts', async t => {
  const f = await fixture(t), writer = await f.create();
  assert.equal(writer.record({ schema_version: 2, event: 'outcome', request_id: 'bad-request', status: 'error', http_status: 400,
    session_id: 'session-a', error_type: 'invalid_request_error', error: 'PRIVATE_RAW_ERROR' }), true);
  assert.equal(writer.record(decision({ agent_id: 'agent-a', request_class: 'subagent', prompt_excerpt: 'PRIVATE_AGENT_TEXT' })), true);
  await writer.close();
  const [file] = await files(f.directory);
  assert.equal(file.rows[0].selected_model, undefined);
  assert.equal(file.rows[0].status, 'error');
  assert.equal(file.rows[1].prompt_excerpt, '');
  assert.ok(!file.text.includes('PRIVATE_'));
});
