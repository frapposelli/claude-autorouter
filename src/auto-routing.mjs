// Shared execution capabilities, not a replacement for Claude's permission
// classifier. Keep the complete safeguards contract and signed history on wire.
const MODELS = new Set(['claude-sonnet-5', 'claude-sonnet-5-5', 'claude-opus-5', 'claude-opus-5-5']);
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const SHARED_TOOLS = new Set(['custom', 'tool_search_tool_regex_20251119', 'tool_search_tool_bm25_20251119',
  'bash_20250124', 'text_editor_20250728']);
const CONTEXT_EDITS = new Set(['clear_thinking_20251015', 'clear_tool_uses_20250919']);
const sharedTool = tool => object(tool) && (tool.type === undefined || SHARED_TOOLS.has(tool.type));

export function hasRoutableSafeguards(body) {
  return MODELS.has(body.model) && Array.isArray(body.safeguards) && body.safeguards.length > 0
    && body.safeguards.every(entry => object(entry) && entry.type === 'dangerous_tool_use'
      && object(entry.classifier_context) && entry.classifier_context.v === 1);
}

export function canRouteAutoRequest(body, target) {
  if (!MODELS.has(body.model) || !MODELS.has(target)) return false;
  if (body.safeguards !== undefined && !hasRoutableSafeguards(body)) return false;
  if (body.max_tokens !== undefined && (!Number.isSafeInteger(body.max_tokens) || body.max_tokens < 1 || body.max_tokens > 128000)) return false;
  // Retain model-specific execution facilities whose contracts differ between
  // models. Ordinary Claude Code tools and native context editing are shared.
  if (body.speed !== undefined && body.speed !== 'standard') return false;
  if (body.container !== undefined || body.mcp_servers !== undefined || body.compaction !== undefined) return false;
  if (body.tools !== undefined && (!Array.isArray(body.tools) || !body.tools.every(sharedTool))) return false;
  for (const message of body.messages) {
    if (message.role !== 'system' || !Array.isArray(message.content)) continue;
    for (const block of message.content) {
      if (!['tool_addition', 'tool_removal'].includes(block.type)) continue;
      const tool = block.tool;
      if (!object(tool) || (tool.type !== 'tool_reference'
        && !(block.type === 'tool_addition' && tool.type === 'tool_definition' && sharedTool(tool.definition)))) return false;
    }
  }
  if (body.tool_choice !== undefined && (!object(body.tool_choice) || !['auto', 'none'].includes(body.tool_choice.type))) return false;
  if (body.context_management !== undefined) {
    const context = body.context_management;
    if (!object(context) || Object.keys(context).some(key => key !== 'edits') || !Array.isArray(context.edits)
      || context.edits.some(edit => !object(edit) || !CONTEXT_EDITS.has(edit.type))) return false;
  }
  if (body.thinking !== undefined) {
    const thinking = body.thinking;
    if (!object(thinking) || !['adaptive', 'disabled', 'between_tools'].includes(thinking.type)) return false;
    // between_tools is Sonnet 5.5-specific. A routed Opus request uses
    // adaptive thinking; prepareRequest makes that explicit without touching
    // any prior thinking blocks or the conversation prefix they sign.
    if (thinking.type === 'between_tools' && (body.model !== 'claude-sonnet-5-5'
      || Object.keys(thinking).some(key => key !== 'type') || target === 'claude-sonnet-5')) return false;
  }
  // Sonnet 5 lacks mid-conversation system/tool/effort updates. Never flatten
  // those messages into the top-level prompt: that invalidates signed history.
  if (target === 'claude-sonnet-5' && (body.output_config?.task_budget !== undefined
    || body.messages.some(message => message.role === 'system' || message.output_config !== undefined))) return false;
  return true;
}
