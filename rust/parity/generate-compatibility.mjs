// Development-only synthetic corpus generator. No provider calls or user data.
// Run from any directory; stdout is JSONL suitable for cargo xtask parity --cases.
import { MODEL_CATALOG } from '../../src/model-catalog.mjs';

const models = [...Object.keys(MODEL_CATALOG), 'team/claude-opus-5-5'];
const extras = [{}, { future_contract: { opaque: true } }];
for (const key of ['speed', 'container', 'mcp_servers', 'compaction', 'cache_control', 'safeguards',
  'context_management', 'thinking', 'tool_choice', 'output_config', 'tools', 'max_tokens', 'stream']) {
  for (const value of [null, {}, [], true, 0, 1, 'standard', 'future']) extras.push({ [key]: value });
}
for (const type of ['disabled', 'adaptive', 'enabled', 'between_tools', 'future']) {
  extras.push({ thinking: { type } }, { thinking: { type, future: true } }, { thinking: { type, block_binding: null } },
    { thinking: { type, block_binding: { prefix_mismatch_behavior: 'error' } } });
  for (const effort of [null, 'low', 'high', 'xhigh', 'max', 'future']) extras.push({ thinking: { type }, output_config: { effort } });
}
for (const type of ['auto', 'none', 'any', 'tool', 'future']) {
  extras.push({ tool_choice: { type } }, { tool_choice: { type, name: 'Read' } }, { tool_choice: { type, disable_parallel_tool_use: true } });
}
for (const type of ['text', 'image', 'document', 'tool_use', 'tool_result', 'thinking', 'redacted_thinking',
  'tool_reference', 'tool_search_tool_result', 'tool_search_tool_search_result', 'tool_search_tool_result_error',
  'tool_addition', 'tool_removal', 'future']) {
  for (const block of [{ type }, { type, future: true }, { type, cache_control: null },
    { type, cache_control: { type: 'ephemeral', future: true } }]) {
    extras.push({ messages: [{ role: 'user', content: [block] }] }, { system: [block] },
      { messages: [{ role: 'user', content: [{ type: 'tool_result', content: [block] }] }] });
  }
}
for (const type of [null, 'custom', 'bash_20250124', 'text_editor_20250728', 'tool_search_tool_regex_20251119',
  'tool_search_tool_bm25_20251119', 'future']) {
  const tool = { type, name: 'Read', input_schema: { future: true } };
  for (const item of [tool, { ...tool, future: true }, { type, name: 'Read', cache_control: null }]) {
    extras.push({ tools: [item] }, { messages: [{ role: 'system', content: [
      { type: 'tool_addition', tool: { type: 'tool_definition', definition: item } },
    ] }] });
  }
}
for (const effort of [null, 'low', 'high', 'xhigh', 'max', 'future', 1, {}]) {
  extras.push({ output_config: { effort } }, { messages: [{ role: 'user', content: 'Task', output_config: { effort } }] },
    { thinking: { type: 'disabled' }, messages: [{ role: 'user', content: 'Task', output_config: { effort } }] });
}
for (const type of ['clear_thinking_20251015', 'clear_tool_uses_20250919', 'future']) {
  for (const edit of [{ type }, { type, keep: 'all' }, { type, trigger: { type: 'input_tokens', value: 100 } },
    { type, trigger: { type: 'input_tokens', future: true } }]) extras.push({ context_management: { edits: [edit] } });
}
for (const v of [1, 2, '1', null]) extras.push({ safeguards: [{ type: 'dangerous_tool_use', classifier_context: { v } }] });
for (const key of ['temperature', 'top_p', 'top_k']) for (const value of [null, 0, 0.5, 1, '1']) extras.push({ [key]: value });
extras.push({ messages: [] }, { messages: [null] }, { messages: [{ role: 'assistant', content: 'Prefill' }] },
  { messages: [{ role: 'system', content: 'Instruction' }] });

let sequence = 0;
const emit = (op, input) => process.stdout.write(`${JSON.stringify({ id: `generated-${sequence++}`, op, input })}\n`);
for (const model of models) emit('model_catalog', { model });
for (const source of ['claude-haiku-4-5', 'claude-sonnet-4-6', 'claude-sonnet-5', 'claude-sonnet-5-5', 'claude-opus-5', 'claude-opus-5-5']) {
  for (const extra of extras) {
    const body = { model: source, max_tokens: 32000, messages: [{ role: 'user', content: 'Synthetic task' }], ...extra };
    emit('validate_request', body);
    emit('safeguards', body);
    for (const target of models) for (const auto_mode of [false, true]) emit('target_compatibility', { body, target, auto_mode });
  }
}
