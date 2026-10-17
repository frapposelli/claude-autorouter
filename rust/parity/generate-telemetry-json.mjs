#!/usr/bin/env node
// Synthetic raw JSON probes. Bytes preserve values JSON.stringify cannot carry.
import { writeFile } from 'node:fs/promises';
const rows = [];
const add = (op, text, extra = {}) => rows.push({ id: `telemetry-json-${rows.length}`, op, input: { bytes: [...Buffer.from(text)], ...extra }, source_tests: ['test/telemetry-event.test.mjs', 'test/session-history.test.mjs'] });
const base = { schema_version: 2, event: 'decision', request_id: 'synthetic-request', timestamp: '2026-10-09T12:00:00.000Z', session_id: 'synthetic-session', requested_model: 'claude-opus-5-5', selected_model: 'claude-haiku-4-5', source: 'jev', prompt_excerpt: 'Synthetic prompt' };
const fields = ['schema_version','request_id','session_id','agent_id','prompt_id','request_class','timestamp','requested_model','selected_model','confirmed_model','model','baseline_model','pricing_version','reason','compatibility_reason','continuity_state','source','evaluator','tier','classified_tier','latency_ms','evaluation_latency_ms','routing_latency_ms','decision_latency_ms','first_response_ms','upstream_latency_ms','total_latency_ms','classifier_error','classifier_status','http_status','error_type','context_check','counted_input_tokens','model_transitions','model_transitions_truncated','usage','pricing_context','usage_complete','pricing_eligible','completion_confirmed','unpriced_reason','savings','savings_coverage','prompt_excerpt','prompt_truncated','status'];
const values = ['1e400','-1e400','null','true','false','0','-1','1.5','9007199254740992','""','"\\ud800"','"custom-\\udfff"','"\\ufffd"','"😀"','[]','{}','[1e400]','{"opaque":1e400}','"synthetic-session"','"completed"'];
const object = (entry, key, raw) => JSON.stringify({ ...entry, [key]: undefined }).slice(0, -1) + `,${JSON.stringify(key)}:${raw}}`;
for (const field of fields) for (const value of values) {
  add('normalize_telemetry_json', object({ ...base, event: 'route' }, field, value));
  add('normalize_session_json', object(base, field, value));
  add('normalize_session_json', object({ ...base, event: 'outcome', status: 'completed' }, field, value));
}
for (const key of ['usage','pricing_context','savings','savings_coverage']) {
  const nested = ['input_tokens','output_tokens','cache_creation_input_tokens','cache_read_input_tokens','cache_creation','ephemeral_5m_input_tokens','ephemeral_1h_input_tokens','speed','inference_geo','service_tier','pricing_unsupported','baseline_model','actual_usd','baseline_usd','saved_usd','percent','requests','priced_requests','unpriced_requests','partial','pricing_version','pricing_date','pricing_source','unpriced_reasons'];
  for (const field of nested) for (const value of values.slice(0, 18)) add('normalize_session_json', object({ ...base, event: 'outcome', status: 'completed' }, key, `{${JSON.stringify(field)}:${value}}`));
}
for (const raw of ['1e400','-1e400','"\\ud800"','null','""']) {
  for (const field of ['session_id','agent_id','prompt_id','request_class','prompt_excerpt','requested_model','selected_model']) add('history_session', object(base, field, raw) + '\n', { text: true });
}
for (const value of ['["\\ud800","claude-sonnet-4-6","\\ud801"]','[1e400,"claude-sonnet-4-6"]','["claude-sonnet-4-6",null]']) add('normalize_session_json', object({ ...base, event: 'outcome', status: 'completed' }, 'model_transitions', value));
for (const opaque of ['"\\ud800"','1e400','['.repeat(512) + '0' + ']'.repeat(512)]) {
  add('normalize_session_json', object(base, 'unknown_extension', opaque));
  add('history_session', object(base, 'unknown_extension', opaque) + '\n', { text: true });
}
for (const text of ['\\ud800A\\udfff', '😀'.repeat(501), 'a'.repeat(499)+'\\ud800tail', 'api_key=synthetic-secret \\ud800']) {
  const literal = `"${text}"`;
  add('normalize_session_json', object(base, 'prompt_excerpt', literal));
  add('normalize_session_json', object(base, 'prompt_excerpt', literal), { include_prompts: false });
  add('history_session', object(base, 'prompt_excerpt', literal) + '\n', { text: true });
}
const path = process.argv[2] ?? '/private/tmp/autorouter-telemetry-json-differential.jsonl';
await writeFile(path, rows.map(row => JSON.stringify(row)).join('\n') + '\n');
console.log(JSON.stringify({ cases: rows.length, path }));
