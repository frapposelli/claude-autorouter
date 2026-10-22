#!/usr/bin/env node
import { writeFile } from 'node:fs/promises';
const stamp = '2026-10-05T12:00:00.000Z';
const decision = (request_id, extra = {}) => ({ schema_version: 2, event: 'decision', timestamp: stamp, request_id, session_id: 's', requested_model: 'claude-opus-5-5', selected_model: 'claude-haiku-4-5', source: 'jev', reason: 'classified', decision_latency_ms: 12, prompt_excerpt: 'Synthetic task', ...extra });
const outcome = (request_id, extra = {}) => ({ schema_version: 2, event: 'outcome', timestamp: stamp, request_id, session_id: 's', status: 'completed', http_status: 200, completion_confirmed: true, confirmed_model: 'claude-haiku-4-5', baseline_model: 'claude-opus-5-5', pricing_version: '2026-09-29.1', usage_complete: true, usage: { input_tokens: 1000, output_tokens: 100 }, total_latency_ms: 600, ...extra });
const rows = [];
const add = (bytes, limits = {}) => rows.push({ id: `history-${rows.length}`, op: 'history_session', input: { bytes: [...Buffer.from(bytes)], limits, text: true }, source_tests: ['test/session-history.test.mjs'] });
const line = row => JSON.stringify(row) + '\n';
const variants = [decision('a'), outcome('a'), decision('b', { schema_version: 1, source: 'fallback', classifier_error: 'timeout', decision_latency_ms: 1500 }), outcome('b', { status: 'error', http_status: 429, completion_confirmed: false }), outcome('c', { completion_confirmed: false }), outcome('d', { status: 'cancelled' }), outcome('e', { pricing_version: 'future.9' }), outcome('f', { pricing_version: undefined }), outcome('g', { baseline_model: 'claude-opus-5' }), outcome('a', { confirmed_model: 'claude-opus-5-5' }), decision('foreign', { session_id: 'other' }), decision('bad', { schema_version: 99 }), decision('badtime', { timestamp: 'invalid' }), decision('meta', { prompt_excerpt: undefined }), decision('private', { prompt_excerpt: '\x1b[31m\u202eSynthetic\n😀'.repeat(101), prompt_truncated: true, body: { secret: 'synthetic-canary' } }), {}, null, ['bad'], decision('date', { timestamp: '2026-02-30T00:00:00.000Z' })];
for (const row of variants) add(line(row));
let seed = 0x61c88647;
const next = () => (seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0);
for (let i = 0; i < 250; i++) {
  const content = Array.from({ length: 15 }, () => line(variants[next() % variants.length])).join('');
  const limits = [{}, { maxRecords: 3 }, { maxLines: 4 }, { maxLineBytes: 300 }, { maxFileBytes: 700 }, { maxTotalBytes: 1400 }][i % 6];
  add(content + (i % 7 === 0 ? '{incomplete' : ''), limits);
}
for (const value of [0, -1, 1.5, '1', null, 5001]) add(line(decision('a')), { maxRecords: value });
add(line(decision('a')), { unknown: 1 });
add(['zeta','Zeta','aB','Ab','_custom','-custom',':custom','.custom','/custom','2','10'].map((model,index) => line(decision('sort-' + index, { selected_model: model }))).join(''));
add(''); add('\n'); add('{bad}\n'); add(Buffer.from([0xff, 10])); add(' '.repeat(17000) + '\n' + line(decision('a')));
const path = process.argv[2] ?? '/private/tmp/autorouter-history-differential.jsonl';
await writeFile(path, rows.map(row => JSON.stringify(row)).join('\n') + '\n');
console.log(JSON.stringify({ cases: rows.length, path }));
