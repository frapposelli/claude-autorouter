// Keep adaptations explicit: models in the same family can have different
// thinking contracts. Preserve the existing adaptive Opus upgrade behavior.
const ADAPTIVE_TARGETS = new Set(['claude-opus-5', 'claude-opus-5-5']);

function sonnetNeedsAdaptive(body) {
  const effort = body.output_config?.effort ?? 'high';
  // Sonnet 5.5's between_tools mode accepts high effort or below, and cannot
  // change effort through per-message overrides. Preserve those overrides by
  // selecting adaptive thinking instead of dropping or lowering the effort.
  return ['xhigh', 'max'].includes(effort) || body.messages?.some(message =>
    message.output_config?.effort !== undefined && message.output_config.effort !== effort);
}

export function prepareRequest(body, model) {
  const request = { ...body, model };
  const adjustments = [];
  if (model !== body.model && body.thinking?.type === 'disabled') {
    if (model === 'claude-sonnet-5-5') {
      const type = sonnetNeedsAdaptive(body) ? 'adaptive' : 'between_tools';
      request.thinking = { type };
      adjustments.push(type === 'adaptive' ? 'adaptive_thinking_required' : 'between_tools_thinking_required');
    } else if (ADAPTIVE_TARGETS.has(model)) {
      request.thinking = { type: 'adaptive' };
      adjustments.push('adaptive_thinking_required');
    }
  }
  return { request, adjustments };
}
