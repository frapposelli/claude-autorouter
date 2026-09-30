import { DEFAULT_OLLAMA_MODEL, defaultOllamaTimeoutMs, validateOllamaEndpoint, validateOllamaModel } from './ollama-models.mjs';

export const TIERS = ['haiku', 'sonnet', 'opus'];

export function parseStopHookBlockCap(value, name = 'CLAUDE_CODE_STOP_HOOK_BLOCK_CAP') {
  if (!['string', 'number'].includes(typeof value)
    || (typeof value === 'string' && !/^[0-9]+$/.test(value.trim()))
    || !Number.isSafeInteger(Number(value)) || Number(value) < 0) {
    throw new Error(`${name} requires a nonnegative safe integer (0 disables the Stop-hook continuation cap)`);
  }
  return Number(value);
}

function number(env, key, fallback, min, max, integer = true) {
  const value = Number(env[key] ?? fallback);
  if (!Number.isFinite(value) || value < min || value > max || (integer && !Number.isInteger(value))) {
    throw new Error(`${key} must be ${integer ? 'an integer' : 'a number'} between ${min} and ${max}`);
  }
  return value;
}

function endpoint(value, name) {
  const url = new URL(value);
  const local = ['127.0.0.1', '[::1]', 'localhost'].includes(url.hostname);
  if ((url.protocol !== 'https:' && !(local && url.protocol === 'http:')) || url.username || url.password || url.search || url.hash) {
    throw new Error(`${name} must use HTTPS (HTTP is allowed on loopback) with no credentials, query, or fragment`);
  }
  return url.href.replace(/\/$/, '');
}

export function readConfig(env = process.env) {
  const evaluator = env.AUTOROUTER_EVALUATOR ?? 'jev';
  if (!['jev', 'ollama'].includes(evaluator)) throw new Error('AUTOROUTER_EVALUATOR must be jev or ollama');
  const ollamaModel = validateOllamaModel(env.AUTOROUTER_OLLAMA_MODEL ?? DEFAULT_OLLAMA_MODEL);
  const rawOllamaTimeout = env.AUTOROUTER_OLLAMA_TIMEOUT_MS;
  // Zero is an explicit opt-out. Reject blanks, coercible non-numbers, and
  // non-integer strings (including exponents that could underflow to zero).
  if (rawOllamaTimeout !== undefined && (!['string', 'number'].includes(typeof rawOllamaTimeout)
    || (typeof rawOllamaTimeout === 'string' && !/^[0-9]+$/.test(rawOllamaTimeout.trim())))) {
    throw new Error('AUTOROUTER_OLLAMA_TIMEOUT_MS must be an integer between 0 and 30000 (0 disables the runtime deadline)');
  }
  const ollamaKeepAlive = env.AUTOROUTER_OLLAMA_KEEP_ALIVE ?? '5m';
  if (!/^(?:0|[1-9]\d{0,3}(?:s|m|h))$/.test(ollamaKeepAlive)) {
    throw new Error('AUTOROUTER_OLLAMA_KEEP_ALIVE must be 0 or a positive duration such as 5m');
  }
  const authMode = env.AUTOROUTER_AUTH_MODE ?? 'api-key';
  if (!['api-key', 'subscription'].includes(authMode)) {
    throw new Error('AUTOROUTER_AUTH_MODE must be api-key or subscription');
  }
  const clientProfile = env.AUTOROUTER_CLIENT_PROFILE ?? 'compatible';
  if (!['compatible', 'native'].includes(clientProfile)) {
    throw new Error('AUTOROUTER_CLIENT_PROFILE must be compatible or native');
  }
  const upstream = endpoint(env.AUTOROUTER_UPSTREAM_URL ?? 'https://api.anthropic.com', 'AUTOROUTER_UPSTREAM_URL');
  if (authMode === 'subscription' && upstream !== 'https://api.anthropic.com') {
    throw new Error('Subscription mode requires https://api.anthropic.com as AUTOROUTER_UPSTREAM_URL');
  }
  return {
    evaluator,
    authMode,
    clientProfile,
    stopHookBlockCap: env.CLAUDE_CODE_STOP_HOOK_BLOCK_CAP === undefined
      ? undefined : parseStopHookBlockCap(env.CLAUDE_CODE_STOP_HOOK_BLOCK_CAP),
    anthropicKey: authMode === 'api-key' ? env.ANTHROPIC_API_KEY : undefined,
    jevKey: env.TYPESAFE_API_KEY,
    localToken: env.AUTOROUTER_TOKEN,
    upstream,
    jevEndpoint: endpoint(env.AUTOROUTER_JEV_URL ?? 'https://api.typesafe.ai/v1/systemone', 'AUTOROUTER_JEV_URL'),
    jevModel: env.AUTOROUTER_JEV_MODEL ?? 'jev-latest',
    ollamaEndpoint: validateOllamaEndpoint(env.AUTOROUTER_OLLAMA_URL ?? 'http://127.0.0.1:11434'),
    ollamaModel,
    ollamaTimeoutMs: number(env, 'AUTOROUTER_OLLAMA_TIMEOUT_MS', defaultOllamaTimeoutMs(ollamaModel), 0, 30000),
    ollamaStateChars: 3000,
    ollamaKeepAlive,
    models: {
      haiku: env.AUTOROUTER_HAIKU_MODEL ?? 'claude-haiku-4-5-20251001',
      sonnet: env.AUTOROUTER_SONNET_MODEL ?? 'claude-sonnet-5',
      opus: env.AUTOROUTER_OPUS_MODEL ?? 'claude-opus-5-5',
    },
    port: number(env, 'AUTOROUTER_PORT', 8787, 0, 65535),
    jevTimeoutMs: number(env, 'AUTOROUTER_JEV_TIMEOUT_MS', 1500, 1, 10000),
    tokenCountTimeoutMs: number(env, 'AUTOROUTER_TOKEN_COUNT_TIMEOUT_MS', 1500, 1, 10000),
    minConfidence: number(env, 'AUTOROUTER_MIN_CONFIDENCE', 0.75, 0, 1, false),
    stateChars: 12000,
    maxBodyBytes: 32 * 1024 * 1024,
    cacheEntries: 1000,
    cacheTtlMs: 5 * 60 * 1000,
    turnTtlMs: 30 * 60 * 1000,
    upstreamTimeoutMs: 10 * 60 * 1000,
  };
}

export function requireKeys(config) {
  const keys = config.evaluator === 'ollama' ? [] : [['TYPESAFE_API_KEY', config.jevKey]];
  if (config.authMode === 'api-key') keys.push(['ANTHROPIC_API_KEY', config.anthropicKey]);
  for (const [key, value] of keys) {
    if (!value?.trim()) throw new Error(`Set ${key} before starting the router`);
  }
}
