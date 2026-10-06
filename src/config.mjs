// @ts-check
import { DEFAULT_OLLAMA_MODEL, defaultOllamaTimeoutMs, validateOllamaEndpoint, validateOllamaModel } from './ollama-models.mjs';
import { resolve } from 'node:path';
import { supportsAutoMode } from './model-catalog.mjs';

/** @type {import('./contracts.mjs').Tier[]} */
export const TIERS = ['haiku', 'sonnet', 'opus'];
/** @type {import('./contracts.mjs').ClientProfile[]} */
export const CLIENT_PROFILES = ['compatible', 'native', 'auto'];
export function parseStopHookBlockCap(value, name = 'CLAUDE_CODE_STOP_HOOK_BLOCK_CAP') {
  if (!['string', 'number'].includes(typeof value)
    || (typeof value === 'string' && !/^[0-9]+$/.test(value.trim()))
    || !Number.isSafeInteger(Number(value)) || Number(value) < 0) {
    throw new Error(`${name} requires a nonnegative safe integer (0 disables the Stop-hook continuation cap)`);
  }
  return Number(value);
}

export function parseSessionLogDir(value, name = 'AUTOROUTER_SESSION_LOG_DIR') {
  if (value === undefined || value === '') return undefined;
  if (typeof value !== 'string' || !value.trim() || /[\u0000-\u001f\u007f]/.test(value)) {
    throw new Error(`${name} must be a directory path, or an empty string to disable session logging`);
  }
  return resolve(value);
}

function number(env, key, fallback, min, max, integer = true) {
  const raw = env[key] === undefined ? fallback : env[key];
  const pattern = integer ? /^[0-9]+$/ : /^(?:[0-9]+(?:\.[0-9]+)?|\.[0-9]+)$/;
  const value = Number(raw);
  if (!['string', 'number'].includes(typeof raw) || (typeof raw === 'string' && !pattern.test(raw.trim()))
    || !Number.isFinite(value) || value < min || value > max || (integer && !Number.isSafeInteger(value))) {
    throw new Error(`${key} must be ${integer ? 'an integer' : 'a number'} between ${min} and ${max}`);
  }
  return value;
}

function endpoint(value, name) {
  let url;
  try { url = new URL(value); } catch { throw new Error(`${name} must be a valid URL`); }
  const local = ['127.0.0.1', '[::1]', 'localhost'].includes(url.hostname);
  if ((url.protocol !== 'https:' && !(local && url.protocol === 'http:')) || url.username || url.password || url.search || url.hash) {
    throw new Error(`${name} must use HTTPS (HTTP is allowed on loopback) with no credentials, query, or fragment`);
  }
  return url.href.replace(/\/$/, '');
}

function modelName(value, name) {
  if (typeof value !== 'string' || !value.trim() || /[\u0000-\u001f\u007f-\u009f]/.test(value)) {
    throw new Error(`${name} must be a nonempty model name without control characters`);
  }
  return value;
}

/**
 * @param {Record<string,string|undefined>} [env]
 * @param {{validateAll?:boolean}} [options]
 * @returns {import('./contracts.mjs').RouterConfig}
 */
export function readConfig(env = process.env, { validateAll = false } = {}) {
  const sessionLogMode = env.AUTOROUTER_SESSION_LOG_MODE === undefined ? 'metadata' : env.AUTOROUTER_SESSION_LOG_MODE;
  if (sessionLogMode !== 'metadata' && sessionLogMode !== 'prompts') throw new Error('AUTOROUTER_SESSION_LOG_MODE must be metadata or prompts');
  for (const key of ['AUTOROUTER_STATUSLINE', 'AUTOROUTER_DEBUG']) {
    if (env[key] !== undefined && !['0', '1'].includes(env[key])) throw new Error(`${key} must be 0 or 1`);
  }
  const evaluator = env.AUTOROUTER_EVALUATOR ?? 'ollama';
  if (evaluator !== 'jev' && evaluator !== 'ollama') throw new Error('AUTOROUTER_EVALUATOR must be jev or ollama');
  // A stale inactive backend must not stop the selected evaluator. Config
  // inspection can request validation of both providers explicitly.
  const ollamaEnv = evaluator === 'ollama' || validateAll ? env : {};
  const jevEnv = evaluator === 'jev' || validateAll ? env : {};
  const ollamaModel = validateOllamaModel(ollamaEnv.AUTOROUTER_OLLAMA_MODEL ?? DEFAULT_OLLAMA_MODEL);
  const rawOllamaTimeout = ollamaEnv.AUTOROUTER_OLLAMA_TIMEOUT_MS;
  // Zero is an explicit opt-out. Reject blanks, coercible non-numbers, and
  // non-integer strings (including exponents that could underflow to zero).
  if (rawOllamaTimeout !== undefined && (!['string', 'number'].includes(typeof rawOllamaTimeout)
    || (typeof rawOllamaTimeout === 'string' && !/^[0-9]+$/.test(rawOllamaTimeout.trim())))) {
    throw new Error('AUTOROUTER_OLLAMA_TIMEOUT_MS must be an integer between 0 and 30000 (0 disables the runtime deadline)');
  }
  const ollamaKeepAlive = ollamaEnv.AUTOROUTER_OLLAMA_KEEP_ALIVE ?? '5m';
  if (!/^(?:0|[1-9]\d{0,3}(?:s|m|h))$/.test(ollamaKeepAlive)) {
    throw new Error('AUTOROUTER_OLLAMA_KEEP_ALIVE must be 0 or a positive duration such as 5m');
  }
  const authMode = env.AUTOROUTER_AUTH_MODE ?? 'api-key';
  if (authMode !== 'api-key' && authMode !== 'subscription') {
    throw new Error('AUTOROUTER_AUTH_MODE must be api-key or subscription');
  }
  const clientProfile = env.AUTOROUTER_CLIENT_PROFILE ?? 'compatible';
  if (clientProfile !== 'compatible' && clientProfile !== 'native' && clientProfile !== 'auto') {
    throw new Error('AUTOROUTER_CLIENT_PROFILE must be compatible, native or auto');
  }
  const models = {
    haiku: env.AUTOROUTER_HAIKU_MODEL === undefined ? 'claude-haiku-4-5-20251001' : env.AUTOROUTER_HAIKU_MODEL,
    sonnet: env.AUTOROUTER_SONNET_MODEL === undefined ? (clientProfile === 'auto' ? 'claude-sonnet-5-5' : 'claude-sonnet-5') : env.AUTOROUTER_SONNET_MODEL,
    opus: env.AUTOROUTER_OPUS_MODEL === undefined ? 'claude-opus-5-5' : env.AUTOROUTER_OPUS_MODEL,
  };
  for (const tier of TIERS) modelName(models[tier], `AUTOROUTER_${tier.toUpperCase()}_MODEL`);
  if (clientProfile === 'auto') {
    for (const tier of ['sonnet', 'opus']) {
      if (!supportsAutoMode(models[tier])) {
        throw new Error(`AUTOROUTER_${tier.toUpperCase()}_MODEL must be a known Auto-mode-capable Sonnet or Opus model for the auto profile`);
      }
    }
  }
  const upstream = endpoint(env.AUTOROUTER_UPSTREAM_URL ?? 'https://api.anthropic.com', 'AUTOROUTER_UPSTREAM_URL');
  if (authMode === 'subscription' && upstream !== 'https://api.anthropic.com') {
    throw new Error('Subscription mode requires https://api.anthropic.com as AUTOROUTER_UPSTREAM_URL');
  }
  return {
    evaluator,
    authMode,
    clientProfile,
    sessionLogDir: parseSessionLogDir(env.AUTOROUTER_SESSION_LOG_DIR),
    sessionLogMode,
    stopHookBlockCap: env.CLAUDE_CODE_STOP_HOOK_BLOCK_CAP === undefined
      ? undefined : parseStopHookBlockCap(env.CLAUDE_CODE_STOP_HOOK_BLOCK_CAP),
    anthropicKey: authMode === 'api-key' ? env.ANTHROPIC_API_KEY : undefined,
    jevKey: env.TYPESAFE_API_KEY,
    localToken: env.AUTOROUTER_TOKEN,
    upstream,
    jevEndpoint: endpoint(jevEnv.AUTOROUTER_JEV_URL ?? 'https://api.typesafe.ai/v1/systemone', 'AUTOROUTER_JEV_URL'),
    jevModel: modelName(jevEnv.AUTOROUTER_JEV_MODEL === undefined ? 'jev-latest' : jevEnv.AUTOROUTER_JEV_MODEL, 'AUTOROUTER_JEV_MODEL'),
    ollamaEndpoint: validateOllamaEndpoint(ollamaEnv.AUTOROUTER_OLLAMA_URL ?? 'http://127.0.0.1:11434'),
    ollamaModel,
    ollamaTimeoutMs: number(ollamaEnv, 'AUTOROUTER_OLLAMA_TIMEOUT_MS', defaultOllamaTimeoutMs(ollamaModel), 0, 30000),
    ollamaStateChars: 3000,
    ollamaKeepAlive,
    models,
    port: number(env, 'AUTOROUTER_PORT', 8787, 0, 65535),
    jevTimeoutMs: number(jevEnv, 'AUTOROUTER_JEV_TIMEOUT_MS', 1500, 1, 10000),
    tokenCountTimeoutMs: number(env, 'AUTOROUTER_TOKEN_COUNT_TIMEOUT_MS', 1500, 1, 10000),
    minConfidence: number(jevEnv, 'AUTOROUTER_MIN_CONFIDENCE', 0.75, 0, 1, false),
    stateChars: 12000,
    maxBodyBytes: 32 * 1024 * 1024,
    cacheEntries: 1000,
    cacheTtlMs: 5 * 60 * 1000,
    turnTtlMs: 30 * 60 * 1000,
    upstreamTimeoutMs: 10 * 60 * 1000,
  };
}

/** @param {import('./contracts.mjs').RouterConfig} config */
export function requireKeys(config) {
  const keys = config.evaluator === 'ollama' ? [] : [['TYPESAFE_API_KEY', config.jevKey]];
  if (config.authMode === 'api-key') keys.push(['ANTHROPIC_API_KEY', config.anthropicKey]);
  for (const [key, value] of keys) {
    if (!value?.trim()) throw new Error(`Set ${key} before starting the router`);
  }
}
