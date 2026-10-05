const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const text = value => typeof value === 'string' && value.length > 0;
const valid = Object.freeze({ valid: true });
const invalid = field => ({ valid: false, error: `Invalid Messages API request shape: ${field}` });

// Validate containers and scalar fields read by routing/excerpt extraction.
// This intentionally is not the provider's full schema: unknown fields and
// block/tool types remain untouched, and opaque inputs/schemas are not walked.
export function validateRequestShape(body) {
  if (!object(body) || !text(body.model) || !body.model.trim()) return invalid('model');
  if (!Array.isArray(body.messages)) return invalid('messages');
  const pending = [];
  const content = (value, field, depth = 0) => {
    if (typeof value === 'string') return true;
    if (!Array.isArray(value)) return false;
    pending.push({ blocks: value, field, depth });
    return true;
  };
  if (body.system !== undefined && !content(body.system, 'system')) return invalid('system');
  for (const message of body.messages) {
    if (!object(message) || !['user', 'assistant', 'system'].includes(message.role)) return invalid('messages');
    if (!content(message.content, 'message content')) return invalid('message content');
    if (message.output_config !== undefined && message.output_config !== null && !object(message.output_config)) return invalid('message output_config');
  }
  if (body.tools !== undefined && (!Array.isArray(body.tools) || body.tools.some(tool =>
    !object(tool) || (tool.type !== undefined && tool.type !== null && !text(tool.type))
    || (tool.name !== undefined && typeof tool.name !== 'string')))) return invalid('tools');
  for (const field of ['thinking', 'tool_choice', 'output_config', 'context_management']) {
    // Context management is explicitly nullable in the beta Messages schema.
    if (field === 'context_management' && body[field] === null) continue;
    if (body[field] !== undefined && !object(body[field])) return invalid(field);
  }
  for (const field of ['thinking', 'tool_choice']) {
    if (body[field] !== undefined && !text(body[field].type)) return invalid(field);
  }
  // The Messages API permits zero for cache population without generation.
  // Reviewed 2026-10-05: https://platform.claude.com/docs/en/api/beta/messages/create
  if (body.max_tokens !== undefined && (!Number.isSafeInteger(body.max_tokens) || body.max_tokens < 0)) return invalid('max_tokens');
  if (body.stream !== undefined && typeof body.stream !== 'boolean') return invalid('stream');
  while (pending.length) {
    const { blocks, field, depth } = pending.pop();
    // Excerpt extraction recursively reads tool results. Bound this known
    // content nesting before it can exhaust the stack; never echo field data.
    if (depth > 32) return invalid('content nesting');
    for (const block of blocks) {
      if (!object(block) || !text(block.type)) return invalid(field);
      if (block.type === 'text' && typeof block.text !== 'string') return invalid('text content');
      if (block.type === 'tool_use' && block.name !== undefined && typeof block.name !== 'string') return invalid('tool name');
      if (block.type === 'tool_result' && block.content !== undefined
        && !content(block.content, 'tool result content', depth + 1)) return invalid('tool result content');
    }
  }
  return valid;
}
