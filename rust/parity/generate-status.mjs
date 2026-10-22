#!/usr/bin/env node
// Deterministic synthetic coverage of status ownership, privacy and width.
import { writeFile } from 'node:fs/promises';
const rows = [];
const add = (op, input, test) => rows.push({ id: `${op}-${rows.length}`, op, input, source_tests: [test] });
const phases = ['routing', 'connecting', 'streaming', 'ready', 'error', 'cancelled', 'future'];
const models = [{}, { selected_model: 'claude-sonnet-4-6' }, { actual_model: 'claude-opus-5-5' }, { last_model: 'claude-haiku-4-5' }, { selected_model: 'claude-sonnet-4-6', actual_model: 'claude-opus-5-5', last_model: 'claude-haiku-4-5' }, { selected_model: '\x1b]0;synthetic-secret\x07custom\x1b[31m model\u202Ename' }, { actual_model: '自定义型号😀' }, { selected_model: 'CLAUDE-SONNET-4-61-x' }];
const details = [{}, { source: 'ollama', evaluation_latency_ms: 23.5, routing_latency_ms: 60.8, classified_tier: 'haiku' }, { source: 'cache', evaluator: 'jev', latency_ms: 12 }, { source: 'fallback', evaluator: 'ollama', classifier_error: 'timeout' }, { source: 'fallback', evaluator: 'jev', classifier_error: 'http_error', classifier_status: 503 }, { source: 'fallback', classifier_error: '/synthetic-private/raw-error' }, { reason: 'auto_mode_floor' }, { reason: 'model_incompatible', compatibility_reason: 'safeguards' }, { reason: 'context_capacity', context_check: 'count_unavailable' }, { continuity_state: 'capacity_exhausted' }, { continuity_state: 'unknown' }, { error_type: 'overloaded_error', status: 529 }, { completion_confirmed: false }];
for (const phase of phases) for (const model of models) for (const detail of details) for (const columns of [1, 8, 17, 28, 40, 60, 90, 160]) {
  const state = { phase, ...model, ...detail };
  add('render_statusline', { input: { session_id: 's' }, snapshot: { version: 1, pid: 123, heartbeat_at: 100000, sessions: { s: state } }, options: { now: 100000, color: columns === 160, columns } }, 'test/statusline.test.mjs');
}
const savings = [null, {}, { requests: 0, unpriced_requests: 0 }, { requests: 0, unpriced_requests: 2 }, ...[-0.001, 0, 0.001, 0.01, 0.125, 1.005, 12.345].map(saved_usd => ({ requests: 2, unpriced_requests: 0, actual_usd: 1, baseline_usd: 3, saved_usd, percent: 66.5 })), { requests: 2, unpriced_requests: 3, actual_usd: 1, baseline_usd: 3, saved_usd: 2, percent: 66.6, partial: true }];
for (const saving of savings) for (const columns of [15, 35, 70, 120, 240]) for (const phase of ['ready', 'routing']) {
  const state = { phase, actual_model: 'claude-opus-5-5', last_context_usage: { model: 'claude-haiku-4-5', input_tokens: 120000 }, context_usage: { model: 'claude-opus-5-5', input_tokens: 250000 } };
  for (const context_window of [{}, { used_percentage: 20, context_window_size: 200000 }, { used_percentage: 25.5, context_window_size: 1000000 }, { used_percentage: 0, current_usage: null }, { used_percentage: '5' }]) add('render_statusline', { input: { session_id: 's', context_window }, snapshot: { version: 1, pid: 123, heartbeat_at: 100000, sessions: { s: state }, savings: { s: saving } }, options: { now: 100000, color: false, columns } }, 'test/statusline.test.mjs');
}
for (const snapshot of [null, {}, { version: 1, pid: 123, heartbeat_at: 100000 }, { version: 1, pid: 123, heartbeat_at: 79999 }, { version: 1, pid: 123, heartbeat_at: 105001 }, { version: 1, pid: 0, heartbeat_at: 100000 }]) for (const columns of [null, true, false, 0, -1, '40', '0x20', [], [40], {}, 1.8]) add('render_statusline', { input: {}, snapshot, options: { now: 100000, color: false, columns } }, 'test/statusline.test.mjs');
const event = (kind, fields = {}) => ({ event: kind, session_id: 's', request_id: 'r', ...fields });
const variants = [event('route', { selected_model: 'claude-sonnet-4-6', source: 'ollama', evaluation_latency_ms: 12.5 }), event('upstream_response', { status: 200 }), event('upstream_response', { status: 529 }), event('upstream_model', { model: 'claude-sonnet-4-6' }), event('upstream_usage', { usage: { input_tokens: 100, output_tokens: 20 } }), event('upstream_usage', { usage: { input_tokens: 100, cache_creation_input_tokens: 30, cache_read_input_tokens: 40, output_tokens: 20 } }), event('upstream_error', { error_type: 'overloaded_error' }), event('request_complete', { completion_confirmed: true }), event('request_complete', { completion_confirmed: false }), event('request_cancelled'), event('request_error'), event('upstream_model', { request_id: 'old', model: 'claude-opus-5-5' }), event('request_start', { request_id: 'agent', agent_id: 'worker' }), event('request_start', { request_id: 'new' }), event('route', { request_class: 'count', model: 'claude-opus-5-5' })];
let seed = 0x61c88647;
const next = () => (seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0);
for (let i = 0; i < 350; i++) {
  const steps = [{ op: 'event', event: event('request_start', { requested_model: 'claude-opus-5-5', prompt_id: 'synthetic-task' }) }];
  for (let j = 0; j < 15; j++) steps.push({ op: 'now', value: 100000 + j }, { op: 'event', event: variants[next() % variants.length] }, { op: 'snapshot' });
  add('status_state', { steps }, 'test/status-state.test.mjs');
}
add('status_state', { steps: Array.from({ length: 110 }, (_, i) => ({ op: 'event', event: event('request_start', { session_id: `s${i}` }) })).concat({ op: 'snapshot' }) }, 'test/status-state.test.mjs');
const target = process.argv[2] ?? '/private/tmp/autorouter-status-differential.jsonl';
await writeFile(target, rows.map(row => JSON.stringify(row)).join('\n') + '\n');
console.log(JSON.stringify({ cases: rows.length, path: target }));
