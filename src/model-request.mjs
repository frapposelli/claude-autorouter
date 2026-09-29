// These model IDs require adaptive thinking even when the client's source
// model supports disabling it. Keep the adaptation explicit and narrow.
const ALWAYS_ADAPTIVE = new Set(['claude-opus-5', 'claude-opus-5-5']);

export function prepareRequest(body, model) {
  const request = { ...body, model };
  const adjustments = [];
  if (model !== body.model && ALWAYS_ADAPTIVE.has(model) && body.thinking?.type === 'disabled') {
    request.thinking = { type: 'adaptive' };
    adjustments.push('adaptive_thinking_required');
  }
  return { request, adjustments };
}
