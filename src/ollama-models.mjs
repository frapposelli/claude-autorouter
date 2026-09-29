import { totalmem } from 'node:os';

export const OLLAMA_PRESETS = Object.freeze({ compact: 'qwen3:1.7b', quality: 'qwen3:4b' });

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

export function selectOllamaModel({ preset = 'compact', model, totalMemory = totalmem() } = {}) {
  if (!['compact', 'quality', 'auto'].includes(preset)) throw new Error('--ollama-preset must be compact, quality, or auto');
  if (model !== undefined) return validateOllamaModel(model);
  const selected = preset === 'auto' ? (Number.isFinite(totalMemory) && totalMemory > 24 * 1024 ** 3 ? 'quality' : 'compact') : preset;
  return OLLAMA_PRESETS[selected];
}
