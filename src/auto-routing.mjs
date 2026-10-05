import { modelCapabilities } from './model-catalog.mjs';
import { prepareRequest } from './model-request.mjs';

// Shared execution capabilities, not a replacement for Claude's permission
// classifier. Keep the complete safeguards contract and signed history on wire.
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const CONTEXT_EDITS = new Set(['clear_thinking_20251015', 'clear_tool_uses_20250919']);
// Reviewed 2026-10-05 against the Messages API/tool reference and context-edit
// contract. These are semantic envelopes; input schemas/examples and document
// data remain opaque and are never inspected or rewritten.
// https://platform.claude.com/docs/en/api/beta/messages/create
// https://platform.claude.com/docs/en/build-with-claude/mid-conversation-system-messages
// https://github.com/anthropics/anthropic-sdk-typescript/blob/main/src/resources/beta/messages/messages.ts
const CUSTOM_TOOL_FIELDS = new Set(['type', 'name', 'description', 'input_schema', 'cache_control',
  'defer_loading', 'strict', 'input_examples', 'allowed_callers', 'eager_input_streaming']);
const SEARCH_TOOL_FIELDS = new Set(['type', 'name', 'allowed_callers', 'cache_control', 'defer_loading', 'strict']);
const BASH_TOOL_FIELDS = new Set([...SEARCH_TOOL_FIELDS, 'input_examples']);
const EDITOR_TOOL_FIELDS = new Set([...BASH_TOOL_FIELDS, 'max_characters']);
const TOOL_FIELDS = new Map([
  ['custom', CUSTOM_TOOL_FIELDS], ['bash_20250124', BASH_TOOL_FIELDS],
  ['text_editor_20250728', EDITOR_TOOL_FIELDS],
  ['tool_search_tool_regex_20251119', SEARCH_TOOL_FIELDS], ['tool_search_tool_bm25_20251119', SEARCH_TOOL_FIELDS],
]);
const TOOL_CHOICE_FIELDS = new Map([
  ['auto', new Set(['type', 'disable_parallel_tool_use'])], ['any', new Set(['type', 'disable_parallel_tool_use'])],
  ['tool', new Set(['type', 'name', 'disable_parallel_tool_use'])], ['none', new Set(['type'])],
]);
const MESSAGE_FIELDS = new Set(['role', 'content', 'output_config', 'clear_at']);
const THINKING_EDIT_FIELDS = new Set(['type', 'keep']);
const TOOL_EDIT_FIELDS = new Set(['type', 'clear_at_least', 'clear_tool_inputs', 'exclude_tools', 'keep', 'trigger']);
const EDIT_VALUE_FIELDS = new Set(['type', 'value']);
const knownFields = (value, fields) => Object.keys(value).every(key => fields.has(key));
const optionalEnvelope = (value, fields) => value == null || (object(value) && knownFields(value, fields));
const sharedTool = tool => object(tool) && TOOL_FIELDS.has(tool.type ?? 'custom')
  && knownFields(tool, TOOL_FIELDS.get(tool.type ?? 'custom')) && optionalEnvelope(tool.cache_control, CACHE_FIELDS);
const sharedEdit = edit => object(edit) && CONTEXT_EDITS.has(edit.type)
  && knownFields(edit, edit.type === 'clear_thinking_20251015' ? THINKING_EDIT_FIELDS : TOOL_EDIT_FIELDS)
  && ['keep', 'trigger', 'clear_at_least'].every(key => !object(edit[key]) || knownFields(edit[key], EDIT_VALUE_FIELDS));
const REQUEST_FIELDS = new Set(['model', 'messages', 'system', 'max_tokens', 'metadata', 'stream',
  'stop_sequences', 'temperature', 'top_p', 'top_k', 'tools', 'tool_choice', 'thinking', 'output_config',
  'context_management', 'speed', 'container', 'mcp_servers', 'compaction', 'safeguards', 'service_tier',
  'inference_geo', 'cache_control']);
const CONTENT_TYPES = new Set(['text', 'image', 'document', 'tool_use', 'tool_result', 'thinking',
  'redacted_thinking', 'tool_reference', 'tool_search_tool_result', 'tool_search_tool_search_result',
  'tool_addition', 'tool_removal']);
// The beta API distinguishes semantic block envelopes from opaque payloads.
// See its BetaContentBlockParam union and the matching official SDK types:
// https://github.com/anthropics/anthropic-sdk-python/tree/main/src/anthropic/types/beta
const CONTENT_FIELDS = Object.fromEntries(Object.entries({
  text: ['type', 'text', 'cache_control', 'citations'],
  image: ['type', 'source', 'cache_control', 'transformations'],
  document: ['type', 'source', 'cache_control', 'citations', 'context', 'title'],
  tool_use: ['type', 'id', 'input', 'name', 'cache_control', 'caller', 'toolset_name'],
  tool_result: ['type', 'tool_use_id', 'content', 'is_error', 'cache_control', 'toolset_name'],
  thinking: ['type', 'signature', 'thinking'],
  redacted_thinking: ['type', 'data'],
  tool_reference: ['type', 'tool_name', 'cache_control'],
  tool_search_tool_result: ['type', 'content', 'tool_use_id', 'cache_control'],
  tool_search_tool_search_result: ['type', 'tool_references'],
  tool_search_tool_result_error: ['type', 'error_code', 'error_message'],
  tool_addition: ['type', 'tool', 'cache_control'],
  tool_removal: ['type', 'tool', 'cache_control'],
}).map(([type, fields]) => [type, new Set(fields)]));
const CACHE_FIELDS = new Set(['type', 'ttl']);
const TRANSFORMATION_FIELDS = new Set(['oversized_image']);
const TOOL_REFERENCE_FIELDS = new Set(['type', 'name']);
const TOOL_DEFINITION_FIELDS = new Set(['type', 'definition']);
const OUTPUT_FIELDS = new Set(['effort', 'format', 'task_budget']);
const OUTPUT_FORMAT_FIELDS = new Set(['type', 'schema']);
const TASK_BUDGET_FIELDS = new Set(['type', 'total', 'remaining']);
const THINKING_BINDING_FIELDS = new Set(['prefix_mismatch_behavior']);
const THINKING_FIELDS = new Map([
  ['enabled', new Set(['type', 'budget_tokens', 'display', 'block_binding'])],
  ['adaptive', new Set(['type', 'display', 'block_binding'])],
  ['disabled', new Set(['type'])], ['between_tools', new Set(['type'])],
]);
const CALLER_FIELDS = new Map([
  ['direct', new Set(['type'])], ['code_execution_20250825', new Set(['type', 'tool_id'])],
  ['code_execution_20260120', new Set(['type', 'tool_id'])],
]);
// Claude's versioned classifier context is opaque permission-review data;
// only its outer contract and version determine whether routing is supported.
const SAFEGUARD_FIELDS = new Set(['type', 'classifier_context']);
const compatible = Object.freeze({ compatible: true });
const incompatible = reason => ({ compatible: false, reason });

export function hasRoutableSafeguards(body) {
  return modelCapabilities(body?.model)?.sharedAuto === true && Array.isArray(body.safeguards) && body.safeguards.length > 0
    && body.safeguards.every(entry => object(entry) && entry.type === 'dangerous_tool_use'
      && knownFields(entry, SAFEGUARD_FIELDS) && object(entry.classifier_context) && entry.classifier_context.v === 1);
}

export function canRouteAutoRequest(body, target) {
  return checkTarget(body, target, true).compatible;
}

/**
 * Check a proposed model change after the same explicit thinking adaptation
 * used by inference and token counting. Native same-model requests belong to
 * the provider: this is a routing guard, not a replacement API validator.
 * @returns {{compatible: boolean, reason?: string}}
 */
export function targetCompatibility(body, target, { autoMode = false } = {}) {
  if (typeof target === 'string' && body?.model === target) return compatible;
  return checkTarget(body, target, autoMode);
}

function checkTarget(body, target, autoMode) {
  const sourceFacts = modelCapabilities(body?.model);
  const targetFacts = modelCapabilities(target);
  if (!sourceFacts || !targetFacts) return incompatible('unknown_model');
  if (!object(body) || !Array.isArray(body.messages)) return incompatible('invalid_request_shape');
  if (autoMode && (!sourceFacts.sharedAuto || !targetFacts.sharedAuto)) return incompatible('auto_model');
  if (Object.keys(body).some(key => !REQUEST_FIELDS.has(key))) return incompatible('request_extension');
  if (!optionalEnvelope(body.cache_control, CACHE_FIELDS)) return incompatible('request_extension');
  if (body.safeguards !== undefined && (!sourceFacts.sharedAuto || !targetFacts.sharedAuto || !hasRoutableSafeguards(body))) return incompatible('safeguards');
  if (body.max_tokens !== undefined && (!Number.isSafeInteger(body.max_tokens) || body.max_tokens < 0
    || body.max_tokens > targetFacts.maxOutputTokens)) return incompatible('output_limit');
  // Retain model-specific execution facilities whose contracts differ between
  // models. Ordinary Claude Code tools and native context editing are shared.
  if (body.speed !== undefined && body.speed !== 'standard') return incompatible('speed');
  if (body.container !== undefined || body.mcp_servers !== undefined || body.compaction !== undefined) return incompatible('execution_facility');
  if (body.tools !== undefined && (!Array.isArray(body.tools) || !body.tools.every(sharedTool))) return incompatible('tool_type');
  const contents = Array.isArray(body.system) ? [body.system] : [];
  for (const message of body.messages) {
    if (!object(message)) return incompatible('invalid_request_shape');
    if (!knownFields(message, MESSAGE_FIELDS)) return incompatible('content_extension');
    if (message.role === 'system' && !targetFacts.midConversationSystem) return incompatible('system_message');
    if (message.output_config != null && !targetFacts.perMessageEffort) return incompatible('message_effort');
    if (message.output_config != null && (!object(message.output_config)
      || Object.keys(message.output_config).some(key => key !== 'effort')
      || (message.output_config.effort != null && !targetFacts.effortLevels.includes(message.output_config.effort)))) return incompatible('message_effort');
    if (Array.isArray(message.content)) contents.push(message.content);
    if (message.role !== 'system' || !Array.isArray(message.content)) continue;
    for (const block of message.content) {
      if (!object(block)) return incompatible('invalid_request_shape');
      if (!['tool_addition', 'tool_removal'].includes(block.type)) continue;
      const tool = block.tool;
      if (!object(tool) || (tool.type !== 'tool_reference'
        && !(block.type === 'tool_addition' && tool.type === 'tool_definition' && sharedTool(tool.definition)))
        || !knownFields(tool, tool.type === 'tool_reference' ? TOOL_REFERENCE_FIELDS : TOOL_DEFINITION_FIELDS)) return incompatible('inline_tool');
    }
  }
  // Inspect known content containers only; tool input and document payloads
  // are opaque. Unknown block types retain their source model, never stripped.
  while (contents.length) for (const block of contents.pop()) {
    if (!object(block) || !CONTENT_TYPES.has(block.type) || !knownFields(block, CONTENT_FIELDS[block.type])) return incompatible('content_extension');
    if (!optionalEnvelope(block.cache_control, CACHE_FIELDS)) return incompatible('content_extension');
    if (block.type === 'tool_use' && block.caller !== undefined && (!object(block.caller)
      || !CALLER_FIELDS.has(block.caller.type) || !knownFields(block.caller, CALLER_FIELDS.get(block.caller.type)))) return incompatible('content_extension');
    if (block.type === 'image' && object(block.transformations)
      && !knownFields(block.transformations, TRANSFORMATION_FIELDS)) return incompatible('content_extension');
    if (block.type === 'tool_result' && Array.isArray(block.content)) contents.push(block.content);
    if (block.type === 'tool_search_tool_result') {
      const result = block.content;
      if (!object(result) || !['tool_search_tool_search_result', 'tool_search_tool_result_error'].includes(result.type)
        || !knownFields(result, CONTENT_FIELDS[result.type])) return incompatible('content_extension');
      if (result.type === 'tool_search_tool_search_result') contents.push([result]);
    }
    if (block.type === 'tool_search_tool_search_result') {
      if (!Array.isArray(block.tool_references)) return incompatible('content_extension');
      contents.push(block.tool_references);
    }
  }
  if (!targetFacts.assistantPrefill && body.messages.at(-1)?.role === 'assistant') return incompatible('assistant_prefill');
  if (body.tool_choice !== undefined) {
    if (!object(body.tool_choice) || !TOOL_CHOICE_FIELDS.has(body.tool_choice.type)
      || !knownFields(body.tool_choice, TOOL_CHOICE_FIELDS.get(body.tool_choice.type))) return incompatible('tool_choice');
    if (['any', 'tool'].includes(body.tool_choice.type) && (autoMode || !targetFacts.forcedToolChoice
      || body.thinking?.type === 'enabled')) return incompatible('forced_tool_choice');
  }
  if (body.context_management != null) {
    const context = body.context_management;
    if (!sourceFacts.sharedAuto || !targetFacts.sharedAuto || !object(context)
      || Object.keys(context).some(key => key !== 'edits') || !Array.isArray(context.edits)
      || !context.edits.every(sharedEdit)) return incompatible('context_management');
  }
  if (body.thinking !== undefined) {
    const thinking = body.thinking;
    if (!object(thinking) || (autoMode && !['adaptive', 'disabled', 'between_tools'].includes(thinking.type))) return incompatible('thinking_mode');
    if (!THINKING_FIELDS.has(thinking.type) || !knownFields(thinking, THINKING_FIELDS.get(thinking.type))
      || !optionalEnvelope(thinking.block_binding, THINKING_BINDING_FIELDS)) return incompatible('thinking_extension');
    // between_tools is Sonnet 5.5-specific. A routed Opus request uses
    // adaptive thinking; prepareRequest makes that explicit without touching
    // any prior thinking blocks or the conversation prefix they sign.
    if (thinking.type === 'between_tools' && (body.model !== 'claude-sonnet-5-5'
      || Object.keys(thinking).some(key => key !== 'type') || target === 'claude-sonnet-5')) return incompatible('thinking_mode');
    const adapted = prepareRequest(body, target).request.thinking;
    if (!targetFacts.thinkingTypes.includes(adapted.type)) return incompatible('thinking_mode');
    if (adapted.type === 'between_tools' && (Object.keys(adapted).length !== 1
      || ['xhigh', 'max'].includes(body.output_config?.effort)
      || body.messages.some(message => message.output_config?.effort !== undefined
        && message.output_config.effort !== (body.output_config?.effort ?? 'high')))) return incompatible('thinking_effort');
    if (target === 'claude-opus-5' && adapted.type === 'disabled'
      && ['xhigh', 'max'].includes(body.output_config?.effort)) return incompatible('thinking_effort');
  }
  if (body.output_config !== undefined) {
    const output = body.output_config;
    if (!object(output) || Object.keys(output).some(key => !OUTPUT_FIELDS.has(key))) return incompatible('output_extension');
    if (!optionalEnvelope(output.format, OUTPUT_FORMAT_FIELDS)
      || (output.format != null && output.format.type !== 'json_schema')) return incompatible('output_extension');
    if (!optionalEnvelope(output.task_budget, TASK_BUDGET_FIELDS)
      || (output.task_budget != null && output.task_budget.type !== 'tokens')) return incompatible('task_budget');
    if (output.effort != null && !targetFacts.effortLevels.includes(output.effort)) return incompatible('effort');
    if (output.task_budget != null && !targetFacts.taskBudget) return incompatible('task_budget');
  }
  // Modern models reject non-default sampling. Shared modern source requests
  // retain their own validation; upgrading an older model must not introduce
  // this new restriction. Do not guess an undocumented numeric top_k default.
  if (targetFacts.defaultSamplingOnly && !sourceFacts.defaultSamplingOnly
    && ((body.temperature !== undefined && body.temperature !== 1)
      || (body.top_p !== undefined && body.top_p !== 1) || body.top_k !== undefined)) return incompatible('sampling');
  return compatible;
}
