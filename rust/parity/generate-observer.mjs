#!/usr/bin/env node
// Synthetic observer cases; never captures a user session or calls a provider.
import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const corpus = JSON.parse(await readFile(new URL('../../test/fixtures/claude-protocol-v1.json', import.meta.url), 'utf8'));
const cases = [];
const row = (id, bytes, options = {}, destination = cases) => destination.push({ id, op: 'response_observer', input: {
  content_type: 'text/event-stream', chunks: [[...bytes]], ...options,
}, source_tests: ['test/response-observer.test.mjs', 'test/protocol-fixtures.test.mjs'] });
const frame = (type, fields = {}) => Buffer.from(`event: ${type}\ndata: ${JSON.stringify({ type, ...fields })}\n\n`);
const join = (...parts) => Buffer.concat(parts);
const usage = { input_tokens: 100, output_tokens: 0, cache_creation_input_tokens: 0, cache_read_input_tokens: 20 };
const start = frame('message_start', { message: { model: 'claude-sonnet-5', content: [], usage } });
const delta = frame('message_delta', { delta: { stop_reason: 'end_turn' }, usage: { output_tokens: 12 } });
const stop = frame('message_stop');

for (const scenario of corpus.cases) for (const step of scenario.steps) {
  const response = step.response;
  const bytes = Buffer.from(response.prelude + response.events.map(event =>
    `event: ${event.type}${response.line_ending}data: ${JSON.stringify(event)}${response.line_ending}${response.line_ending}`).join(''));
  const id = `protocol-${scenario.id}-${step.id}`;
  row(`${id}-whole`, bytes, { content_type: response.content_type });
  row(`${id}-single-byte`, bytes, { content_type: response.content_type, chunks: [...bytes].map(byte => [byte]) });
  row(`${id}-destroy`, bytes, { content_type: response.content_type, action: 'destroy' });
}

row('observer-clean-eof-without-message-stop', join(start, delta));
row('observer-truncated-after-start', start);
row('observer-destroy-after-delta', join(start, delta), { action: 'destroy' });
row('observer-sse-error', join(start, frame('error', { error: { type: 'rate_limit_error', message: 'synthetic-private-message' } }), delta, stop));
row('observer-unknown-error', join(start, frame('error', { error: { type: 'synthetic-private-category' } }), stop));
row('observer-invalid-usage', join(start, frame('message_delta', { delta: { stop_reason: 'end_turn' }, usage: { output_tokens: -1 } }), stop));
row('observer-unknown-stop', join(start, frame('message_delta', { delta: { stop_reason: 'future_stop' }, usage: { output_tokens: 1 } }), stop));
row('observer-malformed-after-start', join(start, Buffer.from('event: message_delta\ndata: invalid json\n\n'), delta, stop));
row('observer-multiline-crlf', Buffer.from(': heartbeat\r\n\r\nevent: message_start\r\ndata: {"type":"message_start",\r\ndata: "message":{"model":"claude-模型","id":"é"}}\r\n\r\n'));
row('observer-oversize-recovery', Buffer.from('data: ' + 'x'.repeat(200) + '\n\nevent: message_start\ndata: {"message":{"model":"observed"}}\n\n'), { max_buffer_bytes: 96 });
row('observer-large-content', join(start, frame('content_block_delta', { index: 0, delta: { type: 'text_delta', text: '😀'.repeat(300) } }), delta, stop), { max_buffer_bytes: 256 });
row('observer-oversize-ownership', join(start, frame('content_block_start', { index: 0, content_block: { type: 'tool_use', id: 'tool', name: 'Read', input: { opaque: 'x'.repeat(1000) } } }), frame('content_block_stop', { index: 0 }), delta, stop), { max_buffer_bytes: 256 });
row('observer-many-pings', join(...Array.from({ length: 100 }, () => frame('ping')), start, delta, stop), { max_buffer_bytes: 256 });
row('observer-unknown-content-type', join(start, delta, stop), { content_type: 'application/octet-stream' });
row('observer-zero-buffer', Buffer.alloc(0), { max_buffer_bytes: 0 });

const message = { model: 'claude-sonnet-5', content: [{ type: 'text', text: 'Synthetic answer 😀' }], stop_reason: 'end_turn', usage: { ...usage, output_tokens: 12 } };
const variants = [
  ['ordinary', message], ['tool', { ...message, stop_reason: 'tool_use', content: [{ type: 'tool_use', id: 'read', name: 'Read', input: {} }] }],
  ['wrong-model-type', { ...message, model: 42 }], ['null-model', { ...message, model: null }],
  ['unknown-stop', { ...message, stop_reason: 'future_stop' }], ['refusal', { ...message, stop_reason: 'refusal' }],
  ['negative-usage', { ...message, usage: { input_tokens: -1, output_tokens: 1 } }],
  ['fractional-usage', { ...message, usage: { input_tokens: 1.5, output_tokens: 1 } }],
  ['missing-usage', { ...message, usage: null }], ['fast-usage', { ...message, usage: { ...message.usage, speed: 'fast' } }],
  ['error', { error: { type: 'overloaded_error', message: 'synthetic-private-message' } }],
];
for (const [id, value] of variants) {
  const bytes = Buffer.from(JSON.stringify(value));
  row(`observer-json-${id}`, bytes, { content_type: 'application/json' });
  row(`observer-json-${id}-split`, bytes, { content_type: 'application/json', chunks: [...bytes].map(byte => [byte]) });
}
row('observer-json-malformed', Buffer.from('{not json'), { content_type: 'application/json' });
row('observer-json-oversize', Buffer.from(JSON.stringify(message)), { content_type: 'application/json', max_buffer_bytes: 32 });
row('observer-json-invalid-utf8', join(Buffer.from('{"unknown":"'), Buffer.from([255]), Buffer.from('","model":"claude-sonnet-5"}')), { content_type: 'application/json' });

row('observer-json-lone-surrogate-opaque', Buffer.from('{"unknown":"\\ud800",' + JSON.stringify(message).slice(1)), { content_type: 'application/json' });
row('observer-json-deep-opaque', Buffer.from('{"unknown":' + '['.repeat(150) + '0' + ']'.repeat(150) + ',' + JSON.stringify(message).slice(1)), { content_type: 'application/json' });
row('observer-json-overflow-opaque', Buffer.from('{"unknown":1e400,' + JSON.stringify(message).slice(1)), { content_type: 'application/json' });
// Lossless callback-string encoding checks exact JavaScript UTF-16 identities;
// it never normalizes a model value into an apparently equivalent replacement.
row('observer-json-lone-surrogate-model', Buffer.from('{"model":"\\ud800","content":[],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}'), { content_type: 'application/json', utf16_strings: true });
row('observer-sse-lone-surrogate-model', Buffer.from('event: message_start\ndata: {"type":"message_start","message":{"model":"\\ud800","content":[],"usage":{"input_tokens":1,"output_tokens":0}}}\n\nevent: message_delta\ndata: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}\n\nevent: message_stop\ndata: {"type":"message_stop"}\n\n'), { utf16_strings: true });

await writeFile(new URL('cases/observer.jsonl', import.meta.url), cases.map(value => JSON.stringify(value)).join('\n') + '\n');
console.log(JSON.stringify({ cases: cases.length, directory: fileURLToPath(new URL('cases/', import.meta.url)) }));
