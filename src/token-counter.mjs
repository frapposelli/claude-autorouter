import { createHash } from 'node:crypto';

// Count-token API fields, including the beta fields used by Claude Code.
// Keep unknown extensions out of the count path: dropping new input context
// could produce an incorrectly low count and an unsafe routing decision.
const COUNT_FIELDS = new Set([
  'model', 'messages', 'system', 'tools', 'tool_choice', 'thinking',
  'cache_control', 'context_management', 'compaction', 'output_config',
  'output_format', 'speed',
]);
const GENERATION_FIELDS = new Set([
  'max_tokens', 'stream', 'temperature', 'top_p', 'top_k', 'stop_sequences',
  'metadata', 'service_tier', 'inference_geo',
]);
const CACHE_HEADERS = [
  'authorization', 'x-api-key', 'anthropic-beta', 'anthropic-version',
  'anthropic-workspace-id', 'anthropic-user-profile-id',
];

function canCount(body) {
  if (!body || !Array.isArray(body.messages)) return false;
  if (Object.keys(body).some(key => !COUNT_FIELDS.has(key) && !GENERATION_FIELDS.has(key))) return false;
  if (body.tools !== undefined && !Array.isArray(body.tools)) return false;
  if (body.tools?.some(tool => !tool || (tool.type && tool.type !== 'custom'
    && !/^(?:bash|text_editor|computer|memory)_\d{8}$/.test(tool.type)
    && tool.type !== 'advisor_20260301'))) return false;

  const pending = [body.system, ...body.messages.map(message => message?.content)];
  while (pending.length) {
    const content = pending.pop();
    if (!Array.isArray(content)) continue;
    for (const block of content) {
      if (!block || typeof block !== 'object') continue;
      // The count endpoint cannot fetch URL/File API sources. Do not drop
      // them or turn their tiny JSON representation into a token estimate.
      if (['image', 'document'].includes(block.type)) {
        if (['url', 'file'].includes(block.source?.type)) return false;
        if (block.source?.type === 'content') pending.push(block.source.content);
      }
      pending.push(block.content);
    }
  }
  return true;
}

async function readCount(response) {
  if (!response.ok) {
    await response.body?.cancel();
    return undefined;
  }
  if (!response.body) return undefined;
  // A successful response is tiny. Bound parsing even if an upstream gateway
  // returns an unexpected body; none of its content enters logs or the cache.
  const reader = response.body.getReader();
  const chunks = [];
  let size = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > 65536) { await reader.cancel(); return undefined; }
      chunks.push(value);
    }
    const count = JSON.parse(Buffer.concat(chunks, size).toString('utf8'))?.input_tokens;
    return Number.isSafeInteger(count) && count >= 0 ? count : undefined;
  } finally { reader.releaseLock(); }
}

// Credentials are supplied for this one request by the server. The cache
// retains only hashes, token counts, and expiry times; never prompts or keys.
export function createTokenCounter(config, { fetchImpl = fetch } = {}) {
  const timeoutMs = config.tokenCountTimeoutMs ?? 1500;
  const cacheEntries = config.tokenCountCacheEntries ?? 100;
  const cacheTtlMs = config.tokenCountCacheTtlMs ?? 5 * 60 * 1000;
  const cache = new Map();
  return async function countTokens(body, model, { headers = {}, signal, search = '' } = {}) {
    if (signal?.aborted || !canCount(body) || typeof model !== 'string' || !model) return undefined;
    let timer;
    let onAbort;
    const controller = new AbortController();
    try {
      const payload = { ...Object.fromEntries(Object.entries(body).filter(([key]) => COUNT_FIELDS.has(key))), model };
      const serialized = JSON.stringify(payload);
      const requestHeaders = new Headers(headers);
      requestHeaders.delete('content-length');
      requestHeaders.set('content-type', 'application/json');
      requestHeaders.set('accept', 'application/json');
      const url = new URL(`${config.upstream.replace(/\/$/, '')}/v1/messages/count_tokens`);
      url.search = search;
      const key = createHash('sha256').update(JSON.stringify([
        url.href, CACHE_HEADERS.map(name => requestHeaders.get(name)), serialized,
      ])).digest('hex');
      const cached = cache.get(key);
      if (cached) {
        cache.delete(key);
        if (cached.expires > Date.now()) { cache.set(key, cached); return cached.count; }
      }
      const cancelled = new Promise(resolve => {
        onAbort = () => { controller.abort(); resolve(undefined); };
        signal?.addEventListener('abort', onAbort, { once: true });
        timer = setTimeout(onAbort, timeoutMs);
      });
      const pending = (async () => {
        try {
          const response = await fetchImpl(url.href, {
            method: 'POST', redirect: 'error', headers: requestHeaders,
            body: serialized, signal: controller.signal,
          });
          const count = await readCount(response);
          if (controller.signal.aborted || count === undefined) return undefined;
          cache.set(key, { count, expires: Date.now() + cacheTtlMs });
          while (cache.size > cacheEntries) cache.delete(cache.keys().next().value);
          return count;
        } catch { return undefined; }
      })();
      // Cover response-body consumption and implementations that do not reject
      // promptly on abort. Unknown counts simply leave the router's fallback
      // policy in charge; no count failure can fail an inference request.
      return await Promise.race([pending, cancelled]);
    } catch { return undefined; }
    finally {
      clearTimeout(timer);
      if (onAbort) signal?.removeEventListener('abort', onAbort);
    }
  };
}
