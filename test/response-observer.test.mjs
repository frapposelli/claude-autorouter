import test from 'node:test';
import assert from 'node:assert/strict';
import { once } from 'node:events';
import { createResponseObserver } from '../src/response-observer.mjs';

async function observe(chunks, options = {}) {
  const models = [];
  const errors = [];
  const usages = [];
  const executions = [];
  const completions = [];
  const received = [];
  const stream = createResponseObserver({ contentType: 'text/event-stream', onModel: value => models.push(value), onError: value => errors.push(value), onUsage: value => usages.push(value),
    onExecution: value => executions.push(value), onComplete: value => completions.push(value), ...options });
  stream.on('data', chunk => received.push(chunk));
  const complete = once(stream, 'end');
  for (const chunk of chunks) stream.write(chunk);
  stream.end();
  await complete;
  assert.deepEqual(Buffer.concat(received), Buffer.concat(chunks));
  return { models, errors, usages, executions, completions, received };
}

const frame = (type, fields = {}) => Buffer.from(`event: ${type}\ndata: ${JSON.stringify({ type, ...fields })}\n\n`);
const usageStart = usage => frame('message_start', { message: { model: 'claude-sonnet-5', usage } });
const usageDelta = (usage, stop_reason) => frame('message_delta', { usage, delta: { stop_reason } });

test('forwards an incomplete SSE frame immediately and reports the actual provider model once', async () => {
  const first = Buffer.from('event: message_start\ndata: {"type":"message_start",');
  const final = Buffer.from('"message":{"model":"claude-sonnet-5","content":[]}}\n\n');
  const models = [];
  const received = [];
  const stream = createResponseObserver({ contentType: 'text/event-stream; charset=utf-8', onModel: value => models.push(value) });
  stream.on('data', chunk => received.push(chunk));
  stream.write(first);
  assert.equal(received.length, 1);
  assert.equal(received[0], first);
  assert.deepEqual(models, []);
  stream.write(final);
  assert.deepEqual(models, [{ model: 'claude-sonnet-5' }]);
  const rest = Buffer.from('event: message_start\ndata: {"type":"message_start","message":{"model":"ignored"}}\n\ndata: PRIVATE_CONTENT\n\n');
  const complete = once(stream, 'end');
  stream.end(rest);
  await complete;
  assert.deepEqual(models, [{ model: 'claude-sonnet-5' }]);
  assert.deepEqual(Buffer.concat(received), Buffer.concat([first, final, rest]));
});

test('handles ping, malformed frames, multiline data, split UTF-8 and CRLF without changing bytes', async () => {
  const bytes = Buffer.from(': heartbeat\r\n\r\nevent: ping\r\ndata: {"type":"ping"}\r\n\r\n'
    + 'event: message_start\r\ndata: invalid JSON\r\n\r\n'
    + 'event: message_start\r\ndata: {"type":"message_start",\r\ndata: "message":{"model":"claude-模型","id":"é"}}\r\n\r\n');
  const chunks = [...bytes].map(byte => Buffer.from([byte]));
  const { models } = await observe(chunks);
  assert.deepEqual(models, [{ model: 'claude-模型' }]);
});

test('skips an oversized frame while forwarding bytes and observing subsequent frames', async () => {
  const chunks = [Buffer.from('data: ' + 'x'.repeat(200)), Buffer.from('\n\nevent: message_start\ndata: {"message":{"model":"observed"}}\n\n')];
  const { models } = await observe(chunks, { maxBufferBytes: 96 });
  assert.deepEqual(models, [{ model: 'observed' }]);
});

test('many small ping frames do not consume the per-frame observation budget', async () => {
  const ping = Buffer.from('event: ping\ndata: {"type":"ping"}\n\n');
  const start = Buffer.from('event: message_start\ndata: {"message":{"model":"claude-opus-5-5"}}\n\n');
  const { models } = await observe([...Array(1000).fill(ping), start], { maxBufferBytes: 128 });
  assert.deepEqual(models, [{ model: 'claude-opus-5-5' }]);
});

test('observes bounded nonstream JSON across chunks and ignores oversized or malformed JSON', async () => {
  const first = Buffer.from('{"model":"claude-haiku-4-5-20251001",');
  const final = Buffer.from('"content":[{"type":"text","text":"answer"}]}');
  const { models } = await observe([first, final], { contentType: 'application/json' });
  assert.deepEqual(models, [{ model: 'claude-haiku-4-5-20251001' }]);
  assert.deepEqual((await observe([first, final], { contentType: 'application/json', maxBufferBytes: 16 })).models, []);
  assert.deepEqual((await observe([Buffer.from('{bad JSON')], { contentType: 'application/json' })).models, []);
});

test('unrecognized content types and callback failures cannot interfere with response delivery', async () => {
  const start = Buffer.from('event: message_start\ndata: {"message":{"model":"claude-sonnet-5"}}\n\n');
  assert.deepEqual((await observe([start], { contentType: 'application/octet-stream' })).models, []);
  await observe([start], { onModel: () => { throw new Error('logging failed'); } });
});

test('reports an SSE error after model confirmation and an oversized content delta without exposing error messages', async () => {
  const bytes = Buffer.from('event: message_start\r\ndata: {"message":{"model":"claude-sonnet-5"}}\r\n\r\n'
    + `event: content_block_delta\r\ndata: ${'PRIVATE_CONTENT'.repeat(40)}\r\n\r\n`
    + 'event: error\r\ndata: {"type":"error","error":{"type":"overloaded_error","message":"PRIVATE_REQUEST"}}\r\n\r\n');
  const { models, errors } = await observe([...bytes].map(byte => Buffer.from([byte])), { maxBufferBytes: 128 });
  assert.deepEqual(models, [{ model: 'claude-sonnet-5' }]);
  assert.deepEqual(errors, [{ error_type: 'overloaded_error' }]);
});

test('only allowlisted error categories escape observation, including bounded JSON errors', async () => {
  const error = { type: 'error', error: { type: 'PRIVATE_DATA\u001b[2J', message: 'PRIVATE_REQUEST' } };
  assert.deepEqual((await observe([Buffer.from(`event: error\ndata: ${JSON.stringify(error)}\n\n`)])).errors,
    [{ error_type: 'unknown_error' }]);
  assert.deepEqual((await observe([Buffer.from(JSON.stringify({ ...error, error: { type: 'rate_limit_error', message: 'PRIVATE_REQUEST' } }))],
    { contentType: 'application/json' })).errors, [{ error_type: 'rate_limit_error' }]);
  await observe([Buffer.from(`event: error\ndata: ${JSON.stringify(error)}\n\n`)], { onError: () => { throw new Error('UI unavailable'); } });
});

test('reports final cumulative usage once with mixed cache TTLs and only allowed metadata', async () => {
  const initial = { input_tokens: 120, output_tokens: 1, cache_creation_input_tokens: 90, cache_read_input_tokens: 300,
    cache_creation: { ephemeral_5m_input_tokens: 50, ephemeral_1h_input_tokens: 40, private: 'PRIVATE_CONTENT' },
    speed: 'standard', inference_geo: 'us', service_tier: 'standard', private: 'PRIVATE_CONTENT' };
  const update = { output_tokens: 30, input_tokens: 125, cache_creation: { ephemeral_5m_input_tokens: 55 }, cache_creation_input_tokens: 95 };
  const frames = [usageStart(initial), usageDelta({ output_tokens: 10 }), usageDelta(update, 'end_turn'), usageDelta(update, 'end_turn'), frame('message_stop'), frame('message_stop')];
  const bytes = Buffer.concat(frames);
  const { usages } = await observe([...bytes].map(byte => Buffer.from([byte])));
  assert.deepEqual(usages, [{ usage: { input_tokens: 125, output_tokens: 30, cache_creation_input_tokens: 95, cache_read_input_tokens: 300,
    cache_creation: { ephemeral_5m_input_tokens: 55, ephemeral_1h_input_tokens: 40 }, speed: 'standard', inference_geo: 'us', service_tier: 'standard' } }]);
  assert.ok(!JSON.stringify(usages).includes('PRIVATE_CONTENT'));
});

test('collects bounded JSON usage together with the provider model and isolates callback failures', async () => {
  const payload = { model: 'claude-sonnet-5', usage: { input_tokens: 0, output_tokens: 0, cache_read_input_tokens: 99, speed: 'PRIVATE_CONTENT', inference_geo: 'PRIVATE_CONTENT', service_tier: 'PRIVATE_CONTENT' } };
  const bytes = Buffer.from(JSON.stringify(payload));
  const result = await observe([bytes.subarray(0, 10), bytes.subarray(10)], { contentType: 'application/json' });
  assert.deepEqual(result.models, [{ model: payload.model }]);
  assert.deepEqual(result.usages, [{ usage: { input_tokens: 0, output_tokens: 0, cache_read_input_tokens: 99, speed: 'unknown', inference_geo: 'unknown', service_tier: 'unknown' } }]);
  await observe([bytes], { contentType: 'application/json', onUsage: () => { throw new Error('Unavailable'); } });
  await observe([bytes], { contentType: 'application/json', onUsage: async () => { throw new Error('Unavailable'); } });
  assert.deepEqual((await observe([bytes], { contentType: 'application/json', maxBufferBytes: 16 })).usages, []);
});

test('clean EOF after a final stop reason supports gateways that omit message_stop', async () => {
  const result = await observe([usageStart({ input_tokens: 4, output_tokens: 1 }), usageDelta({ output_tokens: 12 }, 'tool_use')]);
  assert.deepEqual(result.usages, [{ usage: { input_tokens: 4, output_tokens: 12 } }]);
});

test('incomplete, errored, malformed and invalid usage streams never publish savings telemetry', async () => {
  const start = usageStart({ input_tokens: 4, output_tokens: 1 });
  const final = usageDelta({ output_tokens: 12 }, 'end_turn');
  const stop = frame('message_stop');
  for (const chunks of [
    [start], [start, usageDelta({ output_tokens: 12 })], [start, final, Buffer.from('event: incomplete')],
    [start, frame('content_block_delta', { delta: { type: 'text_delta', text: 'answer' } }), stop],
    [start, usageDelta({}, 'end_turn'), stop],
    [start, final, usageDelta({}, 'end_turn'), stop],
    [start, usageDelta(null, 'end_turn'), stop],
    [start, usageDelta('PRIVATE_USAGE', 'end_turn'), stop],
    [start, usageDelta([], 'end_turn'), stop],
    [start, final, frame('error', { error: { type: 'overloaded_error' } }), stop],
    [start, final, stop, frame('error', { error: { type: 'overloaded_error' } })],
    [start, Buffer.from('event: message_delta\ndata: broken\n\n'), stop],
    [usageStart({ input_tokens: 4 }), stop],
    [usageStart({ output_tokens: 1 }), final, stop],
    [start, usageDelta({ output_tokens: -1 }, 'end_turn'), stop],
    [start, usageDelta({ cache_read_input_tokens: 'PRIVATE_CONTENT' }, 'end_turn'), stop],
    [start, usageDelta({ cache_creation: { ephemeral_1h_input_tokens: 0.1 } }, 'end_turn'), stop],
    [start, usageDelta({ output_tokens: Number.MAX_SAFE_INTEGER + 1 }, 'end_turn'), stop],
  ]) assert.deepEqual((await observe(chunks)).usages, []);
  assert.deepEqual((await observe([Buffer.from(JSON.stringify({ type: 'error', usage: { input_tokens: 4, output_tokens: 12 } }))], { contentType: 'application/json' })).usages, []);
});

test('destroying a stream after its final delta does not report a completed response', async () => {
  const usages = [];
  const completions = [];
  const stream = createResponseObserver({ contentType: 'text/event-stream', onUsage: value => usages.push(value), onComplete: value => completions.push(value) });
  stream.resume();
  stream.write(usageStart({ input_tokens: 4, output_tokens: 1 }));
  stream.write(usageDelta({ output_tokens: 12 }, 'end_turn'));
  const closed = once(stream, 'close');
  stream.destroy();
  await closed;
  assert.deepEqual(usages, []);
  assert.deepEqual(completions, []);
});

test('oversized content does not lose final usage while oversized metadata is excluded', async () => {
  const chunks = [usageStart({ input_tokens: 4, output_tokens: 1 }),
    frame('content_block_delta', { delta: { type: 'text_delta', text: 'PRIVATE_CONTENT'.repeat(100) } }),
    usageDelta({ output_tokens: 12 }, 'end_turn'), frame('message_stop')];
  assert.deepEqual((await observe(chunks, { maxBufferBytes: 256 })).usages, [{ usage: { input_tokens: 4, output_tokens: 12 } }]);
  chunks.splice(2, 0, usageDelta({ output_tokens: 10, private: 'PRIVATE_CONTENT'.repeat(100) }));
  assert.deepEqual((await observe(chunks, { maxBufferBytes: 256 })).usages, []);
  chunks[2] = frame('content_block_start', { private: 'PRIVATE_CONTENT'.repeat(100), content_block: { type: 'fallback' } });
  assert.deepEqual((await observe(chunks, { maxBufferBytes: 256 })).usages, []);
});

test('null cache details with zero writes are valid while model changes make accounting unsupported', async () => {
  const start = usageStart({ input_tokens: 4, output_tokens: 1, cache_creation_input_tokens: 0, cache_creation: null });
  const final = usageDelta({ output_tokens: 12 }, 'end_turn');
  const expected = { input_tokens: 4, output_tokens: 12, cache_creation_input_tokens: 0 };
  assert.deepEqual((await observe([start, start, final, frame('message_stop')])).usages, [{ usage: expected }]);
  const changed = frame('message_start', { message: { model: 'claude-opus-5-5', usage: { input_tokens: 20, output_tokens: 1 } } });
  assert.deepEqual((await observe([start, changed, final, frame('message_stop')])).usages, [{ usage: { ...expected, pricing_unsupported: true } }]);
  const json = { model: 'claude-sonnet-5', usage: { ...expected, cache_creation: null } };
  assert.deepEqual((await observe([Buffer.from(JSON.stringify(json))], { contentType: 'application/json' })).usages, [{ usage: expected }]);
});

test('unsupported compaction, advisor, fallback and refusal accounting is marked without storing response text', async () => {
  for (const additions of [
    [usageDelta({ output_tokens: 12, iterations: [{ private: 'PRIVATE_CONTENT' }] }, 'end_turn')],
    [frame('content_block_start', { content_block: { type: 'fallback', private: 'PRIVATE_CONTENT' } }), usageDelta({ output_tokens: 12 }, 'end_turn')],
    [usageDelta({ output_tokens: 12 }, 'refusal')],
  ]) {
    const result = await observe([usageStart({ input_tokens: 4, output_tokens: 1 }), ...additions, frame('message_stop')]);
    assert.deepEqual(result.usages, [{ usage: { input_tokens: 4, output_tokens: 12, pricing_unsupported: true } }]);
  }
  const json = { model: 'claude-sonnet-5', stop_reason: 'refusal', usage: { input_tokens: 4, output_tokens: 12 } };
  assert.deepEqual((await observe([Buffer.from(JSON.stringify(json))], { contentType: 'application/json' })).usages,
    [{ usage: { input_tokens: 4, output_tokens: 12, pricing_unsupported: true } }]);
});

test('ordinary message iterations use cumulative top-level totals once, including metadata-only starts', async () => {
  const initial = { input_tokens: 4, output_tokens: 1, iterations: [{ type: 'message', model: 'claude-sonnet-5' }] };
  const final = { input_tokens: 15, output_tokens: 30, iterations: [
    { type: 'message', model: 'claude-sonnet-5', input_tokens: 4, output_tokens: 12 },
    { type: 'message', input_tokens: 11, output_tokens: 18, private: 'PRIVATE_CONTENT' },
  ] };
  const { usages } = await observe([usageStart(initial), usageDelta(final, 'end_turn'), frame('message_stop')]);
  assert.deepEqual(usages, [{ usage: { input_tokens: 15, output_tokens: 30 } }]);
  const json = { model: 'claude-sonnet-5', usage: final };
  assert.deepEqual((await observe([Buffer.from(JSON.stringify(json))], { contentType: 'application/json' })).usages, usages);
});

test('iteration type, shape and model mismatches remain unsupported', async () => {
  for (const iterations of [
    [{ type: 'compaction' }], [{ type: 'advisor_message' }], [{ type: 'fallback_message' }], [{ type: 'PRIVATE_TYPE' }],
    [{ type: 'message', model: 'claude-opus-5-5' }], [{ type: 'message', model: null }],
    [{ type: 'message', input_tokens: 'PRIVATE_TOKENS' }], [null], [['message']], 'PRIVATE_ITERATIONS',
    [{ type: 'message' }, { type: 'advisor_message' }],
  ]) {
    const result = await observe([usageStart({ input_tokens: 4, output_tokens: 1 }), usageDelta({ output_tokens: 12, iterations }, 'end_turn'), frame('message_stop')]);
    assert.deepEqual(result.usages, [{ usage: { input_tokens: 4, output_tokens: 12, pricing_unsupported: true } }]);
  }
});

test('unavailable or nullable response geography is preserved without accepting unknown pricing values', async () => {
  for (const inference_geo of ['not_available', null]) {
    const initial = { input_tokens: 4, output_tokens: 1, inference_geo, speed: null, service_tier: null };
    assert.deepEqual((await observe([usageStart(initial), usageDelta({ output_tokens: 12 }, 'end_turn'), frame('message_stop')])).usages,
      [{ usage: { input_tokens: 4, output_tokens: 12, inference_geo: 'not_available', speed: 'unknown', service_tier: 'unknown' } }]);
  }
});

const opus = 'claude-opus-5-5';
const sonnet = 'claude-sonnet-5-5';
const startModel = model => frame('message_start', { message: { model, usage: { input_tokens: 4, output_tokens: 1 } } });
const blockStart = (index, content_block) => frame('content_block_start', { index, content_block });
const blockStop = index => frame('content_block_stop', { index });
const tool = (index, id) => [blockStart(index, { type: 'tool_use', id, name: 'PRIVATE_TOOL', input: {} }), blockStop(index)];
const fallback = (index, from, to) => [blockStart(index, { type: 'fallback', from: { model: from }, to: { model: to } }), blockStop(index)];

test('execution observation is immediate but continuation evidence waits for clean EOF', async () => {
  const executions = [];
  const completions = [];
  const stream = createResponseObserver({ contentType: 'text/event-stream', onExecution: value => executions.push(value), onComplete: value => completions.push(value) });
  stream.resume();
  stream.write(startModel(opus));
  assert.deepEqual(executions, [{ model: opus, source: 'message_start' }]);
  assert.deepEqual(completions, []);
  stream.write(usageDelta({ output_tokens: 4 }, 'end_turn'));
  stream.write(frame('message_stop'));
  assert.deepEqual(completions, []);
  const complete = once(stream, 'end');
  stream.end();
  await complete;
  assert.deepEqual(completions, [{ model: opus, continuation_model: opus, stop_reason: 'end_turn', tool_uses: [] }]);
});

test('before-output and sticky fallbacks report the model actually serving the response', async () => {
  for (const boundaries of [fallback(0, opus, sonnet), []]) {
    const result = await observe([startModel(sonnet), ...boundaries,
      usageDelta({ output_tokens: 12, iterations: [{ type: 'fallback_message', model: sonnet }] }, 'end_turn'), frame('message_stop')]);
    assert.deepEqual(result.models, [{ model: sonnet }]);
    assert.deepEqual(result.completions, [{ model: sonnet, continuation_model: sonnet, stop_reason: 'end_turn', tool_uses: [] }]);
    assert.equal(result.usages[0].usage.pricing_unsupported, true);
  }
});

test('midstream fallback preserves all bytes and assigns only post-boundary client tools to the new model', async () => {
  const bytes = Buffer.concat([startModel(opus), ...tool(0, 'toolu_before'),
    ...fallback(1, opus, sonnet), ...tool(2, 'toolu_after'),
    usageDelta({ output_tokens: 12, iterations: [
      { type: 'message', model: opus }, { type: 'fallback_message', model: sonnet },
    ] }, 'tool_use'), frame('message_stop')]);
  const result = await observe([...bytes].map(byte => Buffer.from([byte])));
  assert.deepEqual(result.models, [{ model: opus }, { model: sonnet }]);
  assert.deepEqual(result.executions, [{ model: opus, source: 'message_start' }, { model: sonnet, source: 'fallback' }]);
  assert.deepEqual(result.completions, [{ model: sonnet, continuation_model: sonnet, stop_reason: 'tool_use', tool_uses: [{ id: 'toolu_after', model: sonnet }] }]);
  assert.equal(result.usages[0].usage.pricing_unsupported, true);
  assert.ok(!JSON.stringify(result.completions).includes('PRIVATE_TOOL'));
});

test('multiple fallback boundaries retain only the last serving model and its actionable tools', async () => {
  const final = 'claude-sonnet-5';
  const result = await observe([startModel(opus), ...fallback(0, opus, sonnet), ...tool(1, 'toolu_old'),
    ...fallback(2, sonnet, final), ...tool(3, 'toolu_final'), usageDelta({ output_tokens: 12 }, 'tool_use')]);
  assert.deepEqual(result.models, [{ model: opus }, { model: sonnet }, { model: final }]);
  assert.deepEqual(result.completions, [{ model: final, continuation_model: final, stop_reason: 'tool_use', tool_uses: [{ id: 'toolu_final', model: final }] }]);
});

test('final fallback iterations identify serving model without retroactively reassigning tools', async () => {
  const delta = usageDelta({ output_tokens: 12, iterations: [{ type: 'message', model: opus }, { type: 'fallback_message', model: sonnet }] }, 'end_turn');
  const result = await observe([startModel(opus), delta]);
  assert.deepEqual(result.executions, [{ model: opus, source: 'message_start' }, { model: sonnet, source: 'usage_iterations' }]);
  assert.equal(result.completions[0].continuation_model, sonnet);
  const conflicting = await observe([startModel(opus), ...tool(0, 'toolu_unknown_boundary'),
    usageDelta({ output_tokens: 12, iterations: [{ type: 'fallback_message', model: sonnet }] }, 'tool_use')]);
  assert.deepEqual(conflicting.completions, [{ model: sonnet, stop_reason: 'tool_use', tool_uses: [] }]);
});

test('JSON completion uses returned model and final-boundary tool ownership without changing content', async () => {
  const payload = { type: 'message', model: sonnet, stop_reason: 'tool_use', content: [
    { type: 'fallback', from: { model: opus }, to: { model: sonnet } },
    { type: 'tool_use', id: 'toolu_json', name: 'PRIVATE_NAME', input: { secret: 'PRIVATE_INPUT' } },
  ], usage: { input_tokens: 4, output_tokens: 12, iterations: [{ type: 'fallback_message', model: sonnet }] } };
  const result = await observe([Buffer.from(JSON.stringify(payload))], { contentType: 'application/json' });
  assert.deepEqual(result.executions, [{ model: sonnet, source: 'message' }]);
  assert.deepEqual(result.completions, [{ model: sonnet, continuation_model: sonnet, stop_reason: 'tool_use', tool_uses: [{ id: 'toolu_json', model: sonnet }] }]);
  assert.equal(result.usages[0].usage.pricing_unsupported, true);
  assert.ok(!JSON.stringify(result.completions).includes('PRIVATE_'));
});

test('JSON multiple fallbacks retain only final-boundary tool ownership, while malformed history stays unknown', async () => {
  const payload = { type: 'message', model: opus, stop_reason: 'tool_use', content: [
    { type: 'fallback', from: { model: opus }, to: { model: sonnet } },
    { type: 'tool_use', id: 'toolu_superseded', name: 'Read', input: {} },
    { type: 'fallback', from: { model: sonnet }, to: { model: opus } },
    { type: 'tool_use', id: 'toolu_final', name: 'Read', input: {} },
  ], usage: { input_tokens: 4, output_tokens: 12 } };
  const bytes = Buffer.from(JSON.stringify(payload));
  const result = await observe([...bytes].map(byte => Buffer.from([byte])), { contentType: 'application/json' });
  assert.deepEqual(result.completions, [{ model: opus, continuation_model: opus, stop_reason: 'tool_use',
    tool_uses: [{ id: 'toolu_final', model: opus }] }]);
  assert.equal(result.usages[0].usage.pricing_unsupported, true);
  for (const [index, model] of [[0, 'invalid\nmodel'], [2, 'invalid\nmodel'], [2, sonnet]]) {
    const uncertain = structuredClone(payload);
    uncertain.content[index].to.model = model;
    const observed = await observe([Buffer.from(JSON.stringify(uncertain))], { contentType: 'application/json' });
    assert.deepEqual(observed.completions, [{ model: opus, stop_reason: 'tool_use', tool_uses: [] }]);
  }
});

test('refusal and unknown terminal reasons never provide a successful continuation pin', async () => {
  for (const reason of ['refusal', 'PRIVATE_STOP_REASON']) {
    const result = await observe([startModel(opus), ...tool(0, 'toolu_denied'), usageDelta({ output_tokens: 12 }, reason), frame('message_stop')]);
    assert.deepEqual(result.completions, [{ model: opus, stop_reason: reason === 'refusal' ? reason : 'unknown', tool_uses: [] }]);
  }
});

test('errors, malformed or incomplete protocol and open tools never report completed execution', async () => {
  const final = usageDelta({ output_tokens: 12 }, 'tool_use');
  for (const chunks of [
    [startModel(opus)],
    [startModel(opus), ...tool(0, 'toolu_a'), final, frame('error', { error: { type: 'overloaded_error' } })],
    [startModel(opus), ...tool(0, 'toolu_a'), final, Buffer.from('event: partial')],
    [startModel(opus), blockStart(0, { type: 'tool_use', id: 'toolu_a' }), final, frame('message_stop')],
    [startModel(opus), blockStart(0, { type: 'tool_use', id: 'toolu_a' }), final, blockStop(0)],
    [startModel(opus), ...tool(0, 'toolu_a'), frame('message_stop')],
    [startModel(opus), ...tool(0, 'toolu_a'), Buffer.from('data: malformed\n\n'), final],
    [startModel(opus), ...tool(0, 'toolu_a'), startModel(sonnet), final],
    [startModel(opus), ...tool(0, 'toolu_a'), startModel(opus), final],
    [startModel(opus), ...tool(0, 'toolu_a'), ...fallback(1, opus, 'invalid\nmodel'), final],
    [startModel(opus), blockStart(0, { type: 'tool_use', id: 'toolu_a' }), ...fallback(1, opus, sonnet), blockStop(0), final],
  ]) assert.deepEqual((await observe(chunks)).completions, []);
  for (const content of [undefined, null, {}, [null], ['PRIVATE_CONTENT']]) {
    const payload = { model: opus, stop_reason: 'end_turn', content };
    assert.deepEqual((await observe([Buffer.from(JSON.stringify(payload))], { contentType: 'application/json' })).completions, []);
  }
});

test('server tools are not client continuation work and missing tool evidence is never invented', async () => {
  const result = await observe([startModel(opus), blockStart(0, { type: 'server_tool_use', id: 'srvtoolu_123' }), blockStop(0),
    usageDelta({ output_tokens: 12 }, 'end_turn')]);
  assert.deepEqual(result.completions, [{ model: opus, continuation_model: opus, stop_reason: 'end_turn', tool_uses: [] }]);
  const missing = await observe([startModel(opus), usageDelta({ output_tokens: 12 }, 'tool_use')]);
  assert.deepEqual(missing.completions, [{ model: opus, stop_reason: 'tool_use', tool_uses: [] }]);
});

test('bounded execution metadata rejects duplicate, malformed and excessive actionable tools', async () => {
  for (const tools of [[...tool(0, 'toolu_a'), ...tool(1, 'toolu_a')], tool(0, 'PRIVATE\u001b[2J'),
    Array.from({ length: 257 }, (_, index) => tool(index, `toolu_${index}`)).flat()]) {
    const result = await observe([startModel(opus), ...tools, usageDelta({ output_tokens: 12 }, 'tool_use')]);
    assert.deepEqual(result.completions, []);
  }
});

test('oversized ordinary output is forwardable but skipped ownership metadata cannot confirm a turn', async () => {
  const output = frame('content_block_delta', { index: 0, delta: { type: 'text_delta', text: 'PRIVATE_TEXT'.repeat(1000) } });
  const result = await observe([startModel(opus), blockStart(0, { type: 'text', text: '' }), output, blockStop(0),
    usageDelta({ output_tokens: 12 }, 'end_turn')], { maxBufferBytes: 256 });
  assert.equal(result.completions[0].continuation_model, opus);
  const oversizedTool = blockStart(0, { type: 'tool_use', id: 'toolu_a', input: { private: 'x'.repeat(1000) } });
  const unknown = await observe([startModel(opus), oversizedTool, blockStop(0), usageDelta({ output_tokens: 12 }, 'tool_use')], { maxBufferBytes: 256 });
  assert.deepEqual(unknown.completions, []);
});

test('execution callback throws and rejections cannot interfere with original response bytes', async () => {
  const chunks = [startModel(opus), usageDelta({ output_tokens: 12 }, 'end_turn')];
  for (const callback of [() => { throw new Error('PRIVATE_ERROR'); }, async () => { throw new Error('PRIVATE_ERROR'); }]) {
    await observe(chunks, { onExecution: callback, onComplete: callback, onModel: callback });
  }
});
