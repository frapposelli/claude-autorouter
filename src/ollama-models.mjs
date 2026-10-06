export const DEFAULT_OLLAMA_MODEL = 'nimble:9b-q4_K_M';

// Known model families need different runtime budgets. Restrict recognition to
// the official library and standard quantization tags; custom namespaces and
// unknown variants retain the short default. Explicit configuration wins.
const QUANTIZATION = '(?:q[2-8]_(?:0|1|k(?:_[sml])?)|iq[1-4]_(?:xxs|xs|s|m|nl)|f16|bf16|f32)';
const TEV_4B = new RegExp(`^(?:latest|4b(?:-${QUANTIZATION})?)$`, 'i');
const NIMBLE_9B = new RegExp(`^(?:latest|9b(?:-${QUANTIZATION})?)$`, 'i');

export function defaultOllamaTimeoutMs(model) {
  const canonical = model.replace(/^registry\.ollama\.ai\//, '').replace(/^library\//, '');
  const match = /^(tev1|nimble)(?::([^:]+))?$/.exec(canonical);
  if (match?.[1] === 'tev1' && TEV_4B.test(match[2] ?? 'latest')) return 15000;
  if (match?.[1] === 'nimble' && NIMBLE_9B.test(match[2] ?? 'latest')) return 30000;
  return 1500;
}

export function validateOllamaEndpoint(value) {
  let endpoint;
  try { endpoint = new URL(value); } catch {}
  if (!endpoint || !['http:', 'https:'].includes(endpoint.protocol)
    || !['127.0.0.1', '[::1]', 'localhost'].includes(endpoint.hostname)
    || endpoint.username || endpoint.password || endpoint.search || endpoint.hash || endpoint.pathname !== '/') {
    throw new Error('Ollama must use a loopback base URL without a path, credentials, a query, or a fragment.');
  }
  // Connect to the loopback address itself. Where `localhost` resolves comes
  // from /etc/hosts and the resolver, which are not part of this check.
  if (endpoint.hostname === 'localhost') endpoint.hostname = '127.0.0.1';
  return endpoint.origin;
}

export function validateOllamaModel(model) {
  if (typeof model !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_./:-]{0,199}$/.test(model)
    || model.includes('://') || /(?:-cloud|:cloud)$/i.test(model)) {
    throw new Error('Ollama requires a valid local model tag; cloud models are not supported.');
  }
  return model;
}
