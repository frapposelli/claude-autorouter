import { modelCapabilities } from './model-catalog.mjs';

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
  const adaptation = modelCapabilities(model)?.disabledThinkingAdaptation;
  if (model !== body.model && body.model === 'claude-sonnet-5-5' && body.thinking?.type === 'between_tools'
    && Object.keys(body.thinking).length === 1 && adaptation === 'adaptive') {
    request.thinking = { type: 'adaptive' };
    adjustments.push('adaptive_thinking_required');
  }
  if (model !== body.model && body.thinking?.type === 'disabled' && Object.keys(body.thinking).length === 1) {
    if (adaptation === 'between_tools') {
      const type = sonnetNeedsAdaptive(body) ? 'adaptive' : 'between_tools';
      request.thinking = { type };
      adjustments.push(type === 'adaptive' ? 'adaptive_thinking_required' : 'between_tools_thinking_required');
    } else if (adaptation === 'adaptive') {
      request.thinking = { type: 'adaptive' };
      adjustments.push('adaptive_thinking_required');
    }
  }
  return { request, adjustments };
}
