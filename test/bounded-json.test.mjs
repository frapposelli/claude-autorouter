import test from 'node:test';
import assert from 'node:assert/strict';
import { readBoundedJson, DECISION_RESPONSE_LIMIT, MODEL_METADATA_LIMIT } from '../src/bounded-json.mjs';

test('shared reader accepts chunked UTF-8 exactly at the byte boundary', async () => {
  const bytes = Buffer.from(JSON.stringify({ text: '🦊é' }));
  const response = new Response(new ReadableStream({ start(controller) {
    for (let index = 0; index < bytes.length; index++) controller.enqueue(bytes.subarray(index, index + 1));
    controller.close();
  } }));
  assert.deepEqual(await readBoundedJson(response, { limit: bytes.length }), { text: '🦊é' });
  assert.equal(response.body.locked, false);
  assert.equal(DECISION_RESPONSE_LIMIT, 65536);
  assert.equal(MODEL_METADATA_LIMIT, 1048576);
});

test('stream bytes override missing or inaccurate headers and oversized bodies are cancelled', async () => {
  for (const headers of [{}, { 'content-length': '1' }, { 'content-length': '65537' }]) {
    let cancelled = false;
    const response = new Response(new ReadableStream({ start(controller) { controller.enqueue(new Uint8Array(65537)); },
      cancel() { cancelled = true; } }), { headers });
    await assert.rejects(readBoundedJson(response), /classifier_invalid_response/);
    assert.equal(cancelled, true);
    assert.equal(response.body.locked, false);
  }
});

test('malformed JSON exposes only the stable category and releases the body', async () => {
  const response = new Response('{"PRIVATE_SECRET":"unterminated');
  await assert.rejects(readBoundedJson(response), error => error.message === 'classifier_invalid_response');
  assert.equal(response.body.locked, false);
  await assert.rejects(readBoundedJson(new Response(null)), /classifier_invalid_response/);
});

test('caller cancellation interrupts stalled reads even when source cancellation never resolves', { timeout: 1000 }, async () => {
  let cancelled = 0;
  const controller = new AbortController();
  const response = new Response(new ReadableStream({ cancel() { cancelled++; return new Promise(() => {}); } }));
  const reason = new Error('caller stopped');
  const pending = readBoundedJson(response, { signal: controller.signal });
  const rejected = assert.rejects(pending, error => error === reason);
  controller.abort(reason);
  await rejected;
  assert.equal(cancelled, 1);
  assert.equal(response.body.locked, false);
});

test('already aborted and oversized reads do not await uncooperative cancellation', { timeout: 1000 }, async () => {
  for (const aborted of [false, true]) {
    let cancelled = false;
    const controller = new AbortController();
    if (aborted) controller.abort(new Error('already stopped'));
    const response = new Response(new ReadableStream({ start(stream) { stream.enqueue(new Uint8Array(65537)); },
      cancel() { cancelled = true; return new Promise(() => {}); } }));
    await assert.rejects(readBoundedJson(response, { signal: controller.signal }), aborted ? /already stopped/ : /classifier_invalid_response/);
    assert.equal(cancelled, true);
    assert.equal(response.body.locked, false);
  }
});
