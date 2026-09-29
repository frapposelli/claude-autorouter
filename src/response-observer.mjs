import { Transform } from 'node:stream';

const ERROR_TYPES = new Set(['invalid_request_error', 'authentication_error', 'permission_error', 'not_found_error',
  'request_too_large', 'rate_limit_error', 'api_error', 'overloaded_error', 'billing_error', 'timeout_error']);
const TOKEN_FIELDS = ['input_tokens', 'output_tokens', 'cache_creation_input_tokens', 'cache_read_input_tokens'];
const CACHE_FIELDS = ['ephemeral_5m_input_tokens', 'ephemeral_1h_input_tokens'];
const USAGE_ENUMS = {
  speed: new Set(['standard', 'fast']), inference_geo: new Set(['global', 'us', 'not_available']),
  service_tier: new Set(['standard', 'priority', 'batch', 'flex']),
};

// Observe only provider model, token counts and safe pricing/error metadata. Forward the original Buffer objects
// immediately; neither parsing failures nor oversized frames affect delivery.
export function createResponseObserver({ contentType = '', onModel = () => {}, onError = () => {}, onUsage = () => {}, maxBufferBytes = 64 * 1024 } = {}) {
  if (!Number.isSafeInteger(maxBufferBytes) || maxBufferBytes < 1) throw new Error('maxBufferBytes must be a positive integer');
  const mediaType = contentType.split(';', 1)[0].trim().toLowerCase();
  const mode = mediaType === 'text/event-stream' ? 'sse'
    : mediaType === 'application/json' || mediaType.endsWith('+json') ? 'json' : undefined;
  let active = Boolean(mode);
  let buffer;
  let size = 0;
  let modelReported = false;
  let discarding = false;
  let lineBytes = 0;
  let previousByte;
  const usage = {};
  let invalidUsage = false;
  let started = false;
  let startedModel;
  let completed = false;
  let finalDelta = false;
  let deltaOutputKnown = false;
  let usageReported = false;

  const stop = () => { active = false; buffer = undefined; size = 0; };
  const report = model => {
    if (modelReported || typeof model !== 'string' || !model) return;
    modelReported = true;
    // Observability must never turn a successful provider stream into an error.
    try { onModel({ model }); } catch {}
  };
  const updateUsage = (value, providerModel = startedModel) => {
    if (value === undefined) return;
    if (!value || typeof value !== 'object' || Array.isArray(value)) { invalidUsage = true; return; }
    // Ordinary same-model iterations are already included in the top-level
    // totals. Compaction, advisor and fallback iterations have separate billing.
    if (value.iterations != null && (!Array.isArray(value.iterations) || value.iterations.some(iteration =>
      !iteration || typeof iteration !== 'object' || Array.isArray(iteration) || iteration.type !== 'message'
      || (iteration.model !== undefined && (typeof iteration.model !== 'string' || iteration.model !== providerModel))
      || TOKEN_FIELDS.some(field => Object.hasOwn(iteration, field) && (!Number.isSafeInteger(iteration[field]) || iteration[field] < 0))))) usage.pricing_unsupported = true;
    for (const field of TOKEN_FIELDS) {
      if (!Object.hasOwn(value, field)) continue;
      if (!Number.isSafeInteger(value[field]) || value[field] < 0) invalidUsage = true;
      else usage[field] = value[field];
    }
    if (value.cache_creation !== undefined && value.cache_creation !== null) {
      if (typeof value.cache_creation !== 'object' || Array.isArray(value.cache_creation)) invalidUsage = true;
      else for (const field of CACHE_FIELDS) {
        if (!Object.hasOwn(value.cache_creation, field)) continue;
        const count = value.cache_creation[field];
        if (!Number.isSafeInteger(count) || count < 0) invalidUsage = true;
        else (usage.cache_creation ??= {})[field] = count;
      }
    }
    for (const [field, allowed] of Object.entries(USAGE_ENUMS)) {
      if (Object.hasOwn(value, field)) usage[field] = field === 'inference_geo' && value[field] === null ? 'not_available'
        : allowed.has(value[field]) ? value[field] : 'unknown';
    }
  };
  const reportUsage = () => {
    if (usageReported || invalidUsage || !Number.isSafeInteger(usage.input_tokens) || !Number.isSafeInteger(usage.output_tokens)) return;
    usageReported = true;
    try { Promise.resolve(onUsage({ usage })).catch(() => {}); } catch {}
  };
  const reportError = type => {
    stop();
    // Provider messages and unknown type strings may contain private data.
    const error_type = ERROR_TYPES.has(type) ? type : 'unknown_error';
    try { onError({ error_type }); } catch {}
  };
  const parseFrame = () => {
    const data = [];
    let event;
    for (const line of buffer.toString('utf8', 0, size).split(/\r?\n/)) {
      if (line.startsWith('data:')) data.push(line.slice(5).replace(/^ /, ''));
      else if (line === 'data') data.push('');
      else if (line.startsWith('event:')) event = line.slice(6).trim();
    }
    size = 0;
    try {
      const payload = JSON.parse(data.join('\n'));
      if (payload?.type === 'error' || event === 'error') reportError(payload?.error?.type);
      else if (payload?.type === 'message_start' || event === 'message_start') {
        report(payload?.message?.model);
        if (!started) {
          started = true;
          startedModel = payload?.message?.model;
          updateUsage(payload?.message?.usage);
        } else if (payload?.message?.model !== startedModel) usage.pricing_unsupported = true;
      } else if (payload?.type === 'message_delta' || event === 'message_delta') {
        // Provider deltas are cumulative: replace reported fields rather than adding them.
        if (started && !completed) {
          updateUsage(payload?.usage);
          deltaOutputKnown = Number.isSafeInteger(payload?.usage?.output_tokens) && payload.usage.output_tokens >= 0;
          finalDelta = typeof payload?.delta?.stop_reason === 'string' && Boolean(payload.delta.stop_reason);
          if (payload?.delta?.stop_reason === 'refusal') usage.pricing_unsupported = true;
        }
      } else if (payload?.type === 'message_stop' || event === 'message_stop') completed = started;
      else if ((payload?.type === 'content_block_start' || event === 'content_block_start') && payload?.content_block?.type === 'fallback') usage.pricing_unsupported = true;
    } catch { if (data.length) invalidUsage = true; }
  };
  const observe = chunk => {
    if (!active) return;
    buffer ??= Buffer.allocUnsafe(maxBufferBytes);
    if (mode === 'json') {
      if (size + chunk.length > maxBufferBytes) return stop();
      chunk.copy(buffer, size);
      size += chunk.length;
      return;
    }
    // Parse line delimiters as bytes so split UTF-8 code points stay intact.
    // Buffer only one bounded SSE frame, including any ping/comment frames.
    for (const byte of chunk) {
      if (!discarding) {
        if (size === maxBufferBytes) {
          // Content can be arbitrarily large. Skipping a frame containing usage
          // or an error, however, leaves the final accounting uncertain.
          const prefix = buffer.toString('utf8', 0, size);
          if (!/^event:\s*(?:content_block_(?:delta|stop)|ping)\r?$/m.test(prefix)) invalidUsage = true;
          discarding = true; size = 0;
        }
        else buffer[size++] = byte;
      }
      const frameEnd = byte === 10 && (lineBytes === 0 || (lineBytes === 1 && previousByte === 13));
      lineBytes = byte === 10 ? 0 : Math.min(2, lineBytes + 1);
      previousByte = byte;
      if (frameEnd) {
        // Skip an oversized frame, then resume at its boundary so a later
        // error is still visible without buffering a large content delta.
        if (discarding) { discarding = false; size = 0; }
        else parseFrame();
        if (!active) return;
      }
    }
  };

  return new Transform({
    transform(chunk, encoding, callback) {
      this.push(chunk);
      try { observe(chunk); } catch { stop(); }
      callback();
    },
    flush(callback) {
      if (active && mode === 'json' && size) {
        try {
          const payload = JSON.parse(buffer.toString('utf8', 0, size));
          if (payload?.type === 'error') reportError(payload?.error?.type);
          else {
            report(payload?.model);
            updateUsage(payload?.usage, payload?.model);
            if (payload?.stop_reason === 'refusal' || (Array.isArray(payload?.content) && payload.content.some(block => block?.type === 'fallback'))) usage.pricing_unsupported = true;
            reportUsage();
          }
        } catch {}
      } else if (active && mode === 'sse' && !size && !discarding && deltaOutputKnown && (completed || finalDelta)) {
        // Wait for clean EOF so an error or interrupted transport cannot count
        // partial output. Claude gateways may finish with the final stop_reason
        // delta instead of a message_stop event.
        reportUsage();
      }
      stop();
      callback();
    },
    destroy(error, callback) { stop(); callback(error); },
  });
}
