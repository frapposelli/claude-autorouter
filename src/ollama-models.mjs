export const DEFAULT_OLLAMA_MODEL = 'nimble:9b-q4_K_M';

export function validateOllamaEndpoint(value) {
  let endpoint;
  try { endpoint = new URL(value); } catch {}
  if (!endpoint || !['http:', 'https:'].includes(endpoint.protocol)
    || !['127.0.0.1', '[::1]', 'localhost'].includes(endpoint.hostname)
    || endpoint.username || endpoint.password || endpoint.search || endpoint.hash || endpoint.pathname !== '/') {
    throw new Error('Ollama must use a loopback base URL without a path, credentials, a query, or a fragment.');
  }
  return endpoint.origin;
}

export function validateOllamaModel(model) {
  if (typeof model !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_./:-]{0,199}$/.test(model)
    || model.includes('://') || /(?:-cloud|:cloud)$/i.test(model)) {
    throw new Error('Ollama requires a valid local model tag; cloud models are not supported.');
  }
  return model;
}
