import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const directory = path.dirname(fileURLToPath(import.meta.url));
const cases = [];
const emit = (op, body, extra = {}) => cases.push({
  id: `authoritative-guard-${cases.length}`, op,
  input: { bytes: [...Buffer.from(body)], ...extra },
  source_tests: ['test/auto-routing.test.mjs', 'test/model-request.test.mjs'],
});
const base = '"model":"claude-sonnet-5-5","max_tokens":32000,"messages":[{"role":"user","content":"Synthetic task"}]';
const fragments = [
  '"cache_control":1e999', '"tools":[{"type":1e999}]',
  '"tools":[{"type":"custom","cache_control":1e999}]',
  '"context_management":1e999', '"output_config":{"effort":1e999}',
  '"output_config":{"format":1e999}', '"output_config":{"task_budget":1e999}',
  '"thinking":{"type":"adaptive","block_binding":1e999}',
  '"messages":[{"role":"user","content":"Synthetic task","output_config":1e999}]',
  '"safeguards":[{"type":"dangerous_tool_use","classifier_context":{"v":1e999}}]',
  '"tools":[{"type":"custom","input_schema":{"type":"object","opaque":"\\ud800","overflow":1e999}}]',
  '"messages":[{"role":"user","content":[{"type":"tool_use","id":"synthetic","input":{"opaque":"\\ud800","overflow":1e999}}]}]',
  '"tools":[{"type":"custom","\\ud800":true}]',
];
for (const fragment of fragments) for (const auto_mode of [false, true]) {
  emit('target_compatibility_json', `{${base},${fragment}}`, { target: 'claude-opus-5-5', auto_mode });
}
for (const depth of [32, 64, 128, 512, 2048, 4096]) for (const leaf of [
  '[{"type":"text","text":"Synthetic \\ud800 text"}]',
  '[{"type":"future_extension","opaque":true}]',
  '[{"type":"text","cache_control":1e999}]',
]) {
  const nested = '[{"type":"tool_result","content":'.repeat(depth) + leaf + '}]'.repeat(depth);
  const body = `{${base},"messages":[{"role":"user","content":${nested}}]}`;
  emit('target_compatibility_json', body, { target: 'claude-opus-5-5', auto_mode: false });
  emit('can_route_auto_json', body, { target: 'claude-opus-5-5' });
}
for (const model of ['synthetic-\\ud800', 'synthetic-\\udfff', 'synthetic-�']) {
  emit('target_compatibility_json', `{"model":"${model}","messages":[]}`, { target: 'synthetic-�', auto_mode: false });
}
for (const context of ['{"v":1,"opaque":"\\ud800"}', '{"v":1e999}', '{"v":1,"deep":' + '['.repeat(512) + '"\\udfff"' + ']'.repeat(512) + '}']) {
  emit('safeguards_json', `{${base},"safeguards":[{"type":"dangerous_tool_use","classifier_context":${context}}]}`);
}
for (const source of ['claude-haiku-4-5-20251001', 'claude-sonnet-5-5']) for (const effort of ['"high"', '"\\ud800"', '1e999', '{}']) {
  const body = `{"model":"${source}","thinking":{"type":"disabled"},"output_config":{"effort":${effort}},"messages":[{"role":"user","content":"Synthetic \\ud800 task","output_config":{"effort":${effort}}}],"opaque":{"2":1e999,"1":-0}}`;
  emit('prepare_request_json', body, { target: 'claude-sonnet-5-5' });
}
fs.writeFileSync(path.join(directory, 'cases/guards-json.jsonl'), cases.map(value => JSON.stringify(value)).join('\n') + '\n');
console.log(`Wrote ${cases.length} authoritative JSON guard cases`);
