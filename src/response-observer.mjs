import { Transform } from 'node:stream';

const ERROR_TYPES = new Set(['invalid_request_error', 'authentication_error', 'permission_error', 'not_found_error',
  'request_too_large', 'rate_limit_error', 'api_error', 'overloaded_error', 'billing_error', 'timeout_error']);
const TOKEN_FIELDS = ['input_tokens', 'output_tokens', 'cache_creation_input_tokens', 'cache_read_input_tokens'];
const CACHE_FIELDS = ['ephemeral_5m_input_tokens', 'ephemeral_1h_input_tokens'];
const STOP_REASONS = new Set(['end_turn', 'max_tokens', 'stop_sequence', 'tool_use', 'pause_turn', 'refusal', 'model_context_window_exceeded']);
const MAX_TRACKED_BLOCKS = 256;
const validModel = value => typeof value === 'string' && value.length > 0 && value.length <= 256 && !/[\x00-\x1f\x7f]/.test(value);
const validToolId = value => typeof value === 'string' && /^[a-zA-Z0-9_-]{1,256}$/.test(value);
const USAGE_ENUMS = {
  speed: new Set(['standard', 'fast']), inference_geo: new Set(['global', 'us', 'not_available']),
  service_tier: new Set(['standard', 'priority', 'batch', 'flex']),
};

// Observe only bounded model/tool ownership, token counts and safe metadata.
// Forward original Buffer objects immediately, even when observation fails.
// onExecution is an observation, not a successful response. onComplete runs at
// clean protocol EOF; its optional continuation_model is the commit candidate.
// The caller must also await successful downstream transport before committing.
// https://platform.claude.com/docs/en/build-with-claude/refusals-and-fallback
export function createResponseObserver({ contentType = '', onModel = () => {}, onError = () => {}, onUsage = () => {},
  onExecution = () => {}, onComplete = () => {}, maxBufferBytes = 64 * 1024 } = {}) {
  if (!Number.isSafeInteger(maxBufferBytes) || maxBufferBytes < 1) throw new Error('maxBufferBytes must be a positive integer');
  const mediaType = contentType.split(';', 1)[0].trim().toLowerCase();
  const mode = mediaType === 'text/event-stream' ? 'sse'
    : mediaType === 'application/json' || mediaType.endsWith('+json') ? 'json' : undefined;
  let active = Boolean(mode);
  let buffer;
  let size = 0;
  let reportedModel;
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
  let servingModel;
  let stopReason;
  let invalidExecution = false;
  let ambiguousExecution = false;
  const openBlocks = new Map();
  let toolUses = [];

  const emit = (callback, value) => {
    // A rejected asynchronous observer must be just as harmless as a throw.
    try { Promise.resolve(callback(value)).catch(() => {}); } catch {}
  };

  const stop = () => { active = false; buffer = undefined; size = 0; };
  const report = (model, source) => {
    if (!validModel(model)) { invalidExecution = true; return; }
    servingModel = model;
    if (reportedModel === model) return;
    reportedModel = model;
    emit(onModel, { model });
    emit(onExecution, { model, source });
  };
  const observeIterations = value => {
    if (!Array.isArray(value?.iterations)) return;
    const fallback = value.iterations.findLast(iteration => iteration?.type === 'fallback_message');
    if (!fallback) return;
    if (!validModel(fallback.model)) { ambiguousExecution = true; return; }
    if (fallback.model !== servingModel && (toolUses.length || openBlocks.size)) ambiguousExecution = true;
    // The final fallback iteration also identifies sticky routing, which can
    // omit a boundary block. It never retroactively changes a tool's owner.
    report(fallback.model, 'usage_iterations');
  };
  const observeFallback = block => {
    usage.pricing_unsupported = true;
    if (!validModel(block?.to?.model)) { invalidExecution = true; return; }
    if (openBlocks.size) invalidExecution = true;
    // Client tools before the final fallback boundary must not be continued.
    // See Anthropic's refusals-and-fallback "Continuing the conversation".
    toolUses = [];
    report(block.to.model, 'fallback');
  };
  const openBlock = (index, block) => {
    if (!started || completed || finalDelta || !Number.isSafeInteger(index) || index < 0
      || !block || typeof block.type !== 'string' || openBlocks.has(index) || openBlocks.size >= MAX_TRACKED_BLOCKS) {
      invalidExecution = true; return;
    }
    if (block.type === 'fallback') observeFallback(block);
    let tool;
    if (block.type === 'tool_use') {
      if (!validToolId(block.id) || !validModel(servingModel) || toolUses.length >= MAX_TRACKED_BLOCKS) invalidExecution = true;
      else tool = { id: block.id, model: servingModel };
    }
    openBlocks.set(index, { tool });
  };
  const closeBlock = index => {
    const block = openBlocks.get(index);
    if (!block) { invalidExecution = true; return; }
    if (block.tool) {
      if (toolUses.some(tool => tool.id === block.tool.id) || toolUses.length >= MAX_TRACKED_BLOCKS) invalidExecution = true;
      else toolUses.push(block.tool);
    }
    openBlocks.delete(index);
  };
  const reportCompletion = () => {
    if (!started || invalidExecution || openBlocks.size || !validModel(servingModel) || !stopReason) return;
    const stop_reason = STOP_REASONS.has(stopReason) ? stopReason : 'unknown';
    const actionable = stop_reason === 'tool_use' && !ambiguousExecution ? toolUses : [];
    const toolEvidence = stop_reason !== 'tool_use' || (actionable.length > 0 && actionable.every(tool => tool.model === servingModel));
    const continuation = stop_reason !== 'refusal' && stop_reason !== 'unknown' && !ambiguousExecution && toolEvidence;
    emit(onComplete, { model: servingModel, ...(continuation ? { continuation_model: servingModel } : {}),
      stop_reason, tool_uses: actionable });
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
    emit(onError, { error_type });
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
        if (!started) {
          started = true;
          startedModel = payload?.message?.model;
          report(startedModel, 'message_start');
          updateUsage(payload?.message?.usage);
          observeIterations(payload?.message?.usage);
        } else {
          if (payload?.message?.model !== startedModel) usage.pricing_unsupported = true;
          invalidExecution = true;
        }
      } else if (payload?.type === 'message_delta' || event === 'message_delta') {
        // Provider deltas are cumulative: replace reported fields rather than adding them.
        if (started && !completed) {
          updateUsage(payload?.usage);
          observeIterations(payload?.usage);
          deltaOutputKnown = Number.isSafeInteger(payload?.usage?.output_tokens) && payload.usage.output_tokens >= 0;
          finalDelta = typeof payload?.delta?.stop_reason === 'string' && Boolean(payload.delta.stop_reason);
          if (finalDelta && openBlocks.size) invalidExecution = true;
          stopReason = finalDelta ? payload.delta.stop_reason : undefined;
          if (payload?.delta?.stop_reason === 'refusal') usage.pricing_unsupported = true;
        }
      } else if (payload?.type === 'message_stop' || event === 'message_stop') completed = started;
      else if (payload?.type === 'content_block_start' || event === 'content_block_start') {
        if (payload?.content_block?.type === 'fallback') usage.pricing_unsupported = true;
        openBlock(payload?.index, payload?.content_block);
      } else if (payload?.type === 'content_block_stop' || event === 'content_block_stop') closeBlock(payload?.index);
    } catch { if (data.length) { invalidUsage = true; invalidExecution = true; } }
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
          if (!/^event:\s*(?:content_block_delta|ping)\r?$/m.test(prefix)) invalidExecution = true;
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
            started = true;
            startedModel = payload?.model;
            report(payload?.model, 'message');
            updateUsage(payload?.usage, payload?.model);
            if (Array.isArray(payload?.content)) {
              let finalFallbackModel;
              for (const block of payload.content) {
                if (!block || typeof block.type !== 'string') invalidExecution = true;
                if (block?.type === 'fallback') {
                  usage.pricing_unsupported = true;
                  toolUses = [];
                  if (!validModel(block?.to?.model)) ambiguousExecution = true;
                  else finalFallbackModel = block.to.model;
                } else if (block?.type === 'tool_use') {
                  if (!validToolId(block.id) || toolUses.length >= MAX_TRACKED_BLOCKS || toolUses.some(tool => tool.id === block.id)) invalidExecution = true;
                  else toolUses.push({ id: block.id, model: servingModel });
                }
              }
              // A JSON response names its final serving model. Intermediate
              // boundaries can differ; only tools after the last boundary
              // remain actionable. Malformed earlier boundaries stay unknown.
              if (finalFallbackModel !== undefined && finalFallbackModel !== payload.model) ambiguousExecution = true;
            } else invalidExecution = true;
            observeIterations(payload?.usage);
            stopReason = typeof payload?.stop_reason === 'string' && payload.stop_reason || undefined;
            if (stopReason === 'refusal') usage.pricing_unsupported = true;
            reportUsage();
            reportCompletion();
          }
        } catch {}
      } else if (active && mode === 'sse' && !size && !discarding && (completed || finalDelta)) {
        // Wait for clean EOF so an error or interrupted transport cannot count
        // partial output. Claude gateways may finish with the final stop_reason
        // delta instead of a message_stop event.
        if (deltaOutputKnown) reportUsage();
        reportCompletion();
      }
      stop();
      callback();
    },
    destroy(error, callback) { stop(); callback(error); },
  });
}
