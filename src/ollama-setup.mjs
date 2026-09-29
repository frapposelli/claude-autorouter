import { validateOllamaEndpoint, validateOllamaModel } from './ollama-models.mjs';
import { buildOllamaState, evaluateOllama, OLLAMA_VERSION_MESSAGE } from './ollama-evaluator.mjs';

const MAX_JSON_BYTES = 1024 * 1024;
const MAX_PULL_BYTES = 16 * 1024 * 1024;
const MAX_LINE_BYTES = 64 * 1024;

function failure(code, message) {
  const error = new Error(message);
  error.code = code;
  return error;
}

async function operation({ signal, timeoutMs }, action) {
  const controller = new AbortController();
  const cancel = () => controller.abort(failure('OLLAMA_CANCELLED', 'Ollama setup cancelled.'));
  if (signal?.aborted) cancel();
  else signal?.addEventListener('abort', cancel, { once: true });
  const timer = setTimeout(() => controller.abort(failure('OLLAMA_TIMEOUT', 'Ollama operation timed out. Check Ollama and retry.')), timeoutMs);
  try {
    controller.signal.throwIfAborted();
    return await action(controller.signal);
  } catch (error) {
    if (controller.signal.aborted) throw controller.signal.reason;
    if (typeof error?.code === 'string' && error.code.startsWith('OLLAMA_')) throw error;
    throw failure('OLLAMA_UNAVAILABLE', 'Cannot reach local Ollama. Install Ollama from https://ollama.com/download and start it, then retry.');
  } finally {
    clearTimeout(timer);
    signal?.removeEventListener('abort', cancel);
  }
}

// Bound reads independently of fetch's own cancellation, including responses
// returned by injected clients. Errors never include provider response text.
async function readChunk(reader, signal) {
  signal.throwIfAborted();
  let rejectAbort;
  const aborted = new Promise((_, reject) => { rejectAbort = reject; });
  const cancel = () => { void reader.cancel().catch(() => {}); rejectAbort(signal.reason); };
  signal.addEventListener('abort', cancel, { once: true });
  try { return await Promise.race([reader.read(), aborted]); }
  finally { signal.removeEventListener('abort', cancel); }
}

async function readJson(response, signal) {
  if (!response.body) throw failure('OLLAMA_RESPONSE', 'Ollama returned an invalid response.');
  const reader = response.body.getReader();
  const chunks = [];
  let length = 0;
  try {
    for (;;) {
      const { done, value } = await readChunk(reader, signal);
      if (done) break;
      length += value.byteLength;
      if (length > MAX_JSON_BYTES) throw failure('OLLAMA_RESPONSE', 'Ollama returned an oversized response.');
      chunks.push(value);
    }
    try { return JSON.parse(Buffer.concat(chunks).toString('utf8')); }
    catch { throw failure('OLLAMA_RESPONSE', 'Ollama returned invalid JSON.'); }
  } finally { await reader.cancel().catch(() => {}); reader.releaseLock(); }
}

async function fetchResponse(fetchImpl, url, signal, body) {
  const response = await fetchImpl(url, {
    method: body === undefined ? 'GET' : 'POST', redirect: 'error', signal,
    ...(body === undefined ? {} : { headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) }),
  });
  if (!response.ok || response.redirected) {
    await response.body?.cancel().catch(() => {});
    throw failure('OLLAMA_HTTP', 'Local Ollama rejected the request. Check the selected model and Ollama version.');
  }
  return response;
}

const modelIdentity = model => {
  const canonical = model.replace(/^registry\.ollama\.ai\//, '').replace(/^library\//, '');
  return canonical.slice(canonical.lastIndexOf('/') + 1).includes(':') ? canonical : `${canonical}:latest`;
};

function supportsDecisions(version) {
  const match = typeof version === 'string' && /^(\d+)\.(\d+)\.(\d+)(?:-[A-Za-z0-9.-]+)?(?:\+[A-Za-z0-9.-]+)?$/.exec(version);
  if (!match) return false;
  const numbers = match.slice(1, 4).map(Number);
  return numbers.every(Number.isSafeInteger) && (numbers[0] > 0 || numbers[1] >= 35);
}

export async function inspectOllama(config, { fetchImpl = fetch, signal, timeoutMs = 5000 } = {}) {
  const endpoint = validateOllamaEndpoint(config.ollamaEndpoint);
  const model = validateOllamaModel(config.ollamaModel);
  return operation({ signal, timeoutMs }, async requestSignal => {
    const versionResponse = await fetchResponse(fetchImpl, `${endpoint}/api/version`, requestSignal);
    const version = await readJson(versionResponse, requestSignal);
    if (!supportsDecisions(version?.version)) throw failure('OLLAMA_VERSION', OLLAMA_VERSION_MESSAGE);
    const response = await fetchResponse(fetchImpl, `${endpoint}/api/tags`, requestSignal);
    const body = await readJson(response, requestSignal);
    if (!body || !Array.isArray(body.models) || body.models.some(item => !item || typeof (item.name ?? item.model) !== 'string')) {
      throw failure('OLLAMA_RESPONSE', 'Ollama returned an invalid model list.');
    }
    const entry = body.models.find(item => [item.name, item.model].some(name => typeof name === 'string' && modelIdentity(name) === modelIdentity(model)));
    if (entry?.remote_model || entry?.remote_host) throw failure('OLLAMA_CLOUD', 'The selected Ollama model uses a remote service. Choose a local model.');
    if (entry) {
      const detailsResponse = await fetchResponse(fetchImpl, `${endpoint}/api/show`, requestSignal, { model });
      const details = await readJson(detailsResponse, requestSignal);
      if (!details || typeof details !== 'object' || Array.isArray(details) || details.error) throw failure('OLLAMA_RESPONSE', 'Ollama returned invalid model details.');
      if (details.remote_model || details.remote_host) throw failure('OLLAMA_CLOUD', 'The selected Ollama model uses a remote service. Choose a local model.');
      if (typeof details.details?.parameter_size !== 'string' || !details.details.parameter_size.trim()) {
        throw failure('OLLAMA_RESPONSE', 'Ollama did not identify a local model. Check the selected model and Ollama version.');
      }
    }
    return { model, installed: Boolean(entry) };
  });
}

async function pullOllama(config, { fetchImpl, signal, write, timeoutMs }) {
  const endpoint = validateOllamaEndpoint(config.ollamaEndpoint);
  const reported = new Set();
  const progress = text => { if (!reported.has(text)) { reported.add(text); write(text); } };
  return operation({ signal, timeoutMs }, async requestSignal => {
    const response = await fetchResponse(fetchImpl, `${endpoint}/api/pull`, requestSignal, { model: config.ollamaModel, stream: true });
    if (!response.body) throw failure('OLLAMA_RESPONSE', 'Ollama returned an invalid download response.');
    const reader = response.body.getReader();
    let buffer = Buffer.alloc(0);
    let length = 0;
    let success = false;
    function line(raw) {
      if (!raw.toString('utf8').trim()) return;
      if (raw.byteLength > MAX_LINE_BYTES) throw failure('OLLAMA_RESPONSE', 'Ollama returned an oversized download update.');
      let update;
      try { update = JSON.parse(raw.toString('utf8')); }
      catch { throw failure('OLLAMA_RESPONSE', 'Ollama returned an invalid download update.'); }
      if (!update || typeof update !== 'object' || Array.isArray(update) || update.error) {
        throw failure('OLLAMA_PULL', 'Ollama model download failed. Check Ollama and the selected model, then retry.');
      }
      if (update.status === 'success') { success = true; progress('Ollama model download complete.'); }
      else if (update.status === 'pulling manifest') progress('Ollama: downloading model manifest.');
      else if (typeof update.status === 'string' && update.status.startsWith('pulling ') && Number.isSafeInteger(update.completed)
        && Number.isSafeInteger(update.total) && update.total > 0 && update.completed >= 0 && update.completed <= update.total) {
        progress(`Ollama: downloading model data (${Math.floor(update.completed / update.total * 10) * 10}%).`);
      } else if (update.status === 'verifying sha256 digest') progress('Ollama: verifying model data.');
      else if (update.status === 'writing manifest') progress('Ollama: saving model manifest.');
    }
    try {
      for (;;) {
        const { done, value } = await readChunk(reader, requestSignal);
        if (done) break;
        length += value.byteLength;
        if (length > MAX_PULL_BYTES) throw failure('OLLAMA_RESPONSE', 'Ollama returned too many download updates.');
        buffer = Buffer.concat([buffer, value]);
        let newline;
        while ((newline = buffer.indexOf(10)) !== -1) {
          line(buffer.subarray(0, newline));
          buffer = buffer.subarray(newline + 1);
        }
        if (buffer.byteLength > MAX_LINE_BYTES) throw failure('OLLAMA_RESPONSE', 'Ollama returned an oversized download update.');
      }
      if (buffer.length) line(buffer);
      if (!success) throw failure('OLLAMA_PULL', 'Ollama download ended before completion. Retry setup with --pull.');
    } finally { await reader.cancel().catch(() => {}); reader.releaseLock(); }
  });
}

async function warmOllama(config, { fetchImpl, signal, timeoutMs }) {
  return operation({ signal, timeoutMs }, async requestSignal => {
    // Prime the same question policy used for real classifications.
    // Only this fixed synthetic task is sent; startup never reads a user task.
    const state = buildOllamaState({ messages: [{ role: 'user', content: 'Return the literal word ready.' }] });
    try {
      await evaluateOllama(state, {
        ...config, ollamaTimeoutMs: timeoutMs, ollamaKeepAlive: config.ollamaKeepAlive ?? '5m',
      }, { fetchImpl, signal: requestSignal });
    } catch (error) {
      requestSignal.throwIfAborted();
      if (error?.code === 'OLLAMA_VERSION') throw failure('OLLAMA_VERSION', OLLAMA_VERSION_MESSAGE);
      throw failure('OLLAMA_WARMUP', 'Ollama could not prepare the local evaluator. Check the selected model and available memory, then retry.');
    }
  });
}

export async function setupOllama(config, {
  pull = false, warm = true, write = console.log, fetchImpl = fetch, signal,
  timeoutMs = 5000, pullTimeoutMs = 15 * 60 * 1000, warmTimeoutMs = 60000,
} = {}) {
  const inspection = await inspectOllama(config, { fetchImpl, signal, timeoutMs });
  let pulled = false;
  if (!inspection.installed) {
    if (!pull) throw failure('OLLAMA_MODEL_MISSING', `The selected Ollama model is not installed. Run ollama pull ${inspection.model}, or rerun setup --evaluator ollama --pull (add --force if already configured).`);
    write(`Downloading ${inspection.model} with local Ollama. Model files are fetched from the model registry.`);
    await pullOllama(config, { fetchImpl, signal, write, timeoutMs: pullTimeoutMs });
    const installed = await inspectOllama(config, { fetchImpl, signal, timeoutMs });
    if (!installed.installed) throw failure('OLLAMA_MODEL_MISSING', 'Ollama finished downloading but the selected model is not available. Check Ollama and retry.');
    pulled = true;
  }
  if (warm) {
    write(`Preloading ${inspection.model} in local Ollama.`);
    await warmOllama(config, { fetchImpl, signal, timeoutMs: warmTimeoutMs });
  }
  return { model: inspection.model, pulled, warmed: warm };
}
