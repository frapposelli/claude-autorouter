import {
  closeSync, constants, fchmodSync, fsyncSync, linkSync, lstatSync,
  mkdirSync, openSync, readFileSync, renameSync, unlinkSync, writeFileSync,
} from 'node:fs';
import { createHash, randomBytes } from 'node:crypto';
import { homedir } from 'node:os';
import { basename, dirname, isAbsolute, join, resolve } from 'node:path';

export const CONFIG_KEYS = Object.freeze([
  'AUTOROUTER_AUTH_MODE', 'AUTOROUTER_CLIENT_PROFILE',
  'ANTHROPIC_API_KEY', 'TYPESAFE_API_KEY', 'AUTOROUTER_TOKEN',
  'AUTOROUTER_UPSTREAM_URL', 'AUTOROUTER_JEV_URL', 'AUTOROUTER_JEV_MODEL',
  'AUTOROUTER_HAIKU_MODEL', 'AUTOROUTER_SONNET_MODEL', 'AUTOROUTER_OPUS_MODEL',
  'AUTOROUTER_PORT', 'AUTOROUTER_JEV_TIMEOUT_MS', 'AUTOROUTER_TOKEN_COUNT_TIMEOUT_MS',
  'AUTOROUTER_MIN_CONFIDENCE', 'AUTOROUTER_STATUSLINE', 'AUTOROUTER_DEBUG',
  'AUTOROUTER_SESSION_LOG_DIR', 'AUTOROUTER_SESSION_LOG_MODE',
  'ENABLE_TOOL_SEARCH', 'CLAUDE_CODE_STOP_HOOK_BLOCK_CAP',
  'AUTOROUTER_EVALUATOR', 'AUTOROUTER_OLLAMA_URL', 'AUTOROUTER_OLLAMA_MODEL',
  'AUTOROUTER_OLLAMA_TIMEOUT_MS', 'AUTOROUTER_OLLAMA_KEEP_ALIVE',
]);
export const SECRET_CONFIG_KEYS = Object.freeze(['ANTHROPIC_API_KEY', 'TYPESAFE_API_KEY', 'AUTOROUTER_TOKEN']);
const allowedKeys = new Set(CONFIG_KEYS);
const revision = content => createHash('sha256').update(content).digest('hex');
const SAFE_FS_CODES = new Set([
  'EACCES', 'EPERM', 'ENOENT', 'ENOTDIR', 'EISDIR', 'ENOSPC', 'EROFS',
  'EMFILE', 'ENFILE', 'ELOOP', 'EIO', 'EEXIST',
]);

function configError(message) {
  const error = new Error(message);
  error.code = 'AUTOROUTER_CONFIG_ERROR';
  return error;
}

function filesystemError(error, operation) {
  if (error?.code === 'AUTOROUTER_CONFIG_ERROR') return error;
  // Native filesystem errors include paths; JSON errors can include values.
  // Neither is needed to explain the failure, and either might hold a secret.
  const code = SAFE_FS_CODES.has(error?.code) ? ` (${error.code})` : '';
  return configError(`Could not ${operation} AutoRouter configuration${code}.`);
}

function validate(values) {
  if (!values || typeof values !== 'object' || Array.isArray(values)
    || ![Object.prototype, null].includes(Object.getPrototypeOf(values))) {
    throw configError('AutoRouter configuration must be a JSON object of string values.');
  }
  const validated = {};
  for (const key of Reflect.ownKeys(values)) {
    if (!allowedKeys.has(key)) {
      // An unknown key can itself contain a pasted credential.
      throw configError('AutoRouter configuration contains an unsupported key.');
    }
    const descriptor = Object.getOwnPropertyDescriptor(values, key);
    if (!descriptor || !('value' in descriptor) || typeof descriptor.value !== 'string') {
      throw configError('AutoRouter configuration values must be strings.');
    }
    validated[key] = descriptor.value;
  }
  return validated;
}

export function getConfigPath(env = process.env) {
  if (env.AUTOROUTER_CONFIG !== undefined) {
    if (typeof env.AUTOROUTER_CONFIG !== 'string' || !env.AUTOROUTER_CONFIG.trim()) {
      throw configError('AUTOROUTER_CONFIG must be a non-empty path.');
    }
    return resolve(env.AUTOROUTER_CONFIG);
  }
  const xdg = env.XDG_CONFIG_HOME;
  if (xdg !== undefined && (typeof xdg !== 'string' || (xdg && !isAbsolute(xdg)))) {
    throw configError('XDG_CONFIG_HOME must be an absolute path when set.');
  }
  return join(xdg || join(homedir(), '.config'), 'claude-autorouter', 'config.json');
}

export function loadUserConfig(env = process.env, { allowMissing = false } = {}) {
  const path = getConfigPath(env);
  let content;
  try {
    content = readFileSync(path, 'utf8');
  } catch (error) {
    if (error.code === 'ENOENT') {
      if (env.AUTOROUTER_CONFIG === undefined || allowMissing) return { env: { ...env }, values: {}, path, exists: false, revision: null };
      throw configError('AUTOROUTER_CONFIG points to a missing configuration file.');
    }
    throw filesystemError(error, 'read');
  }
  let parsed;
  try { parsed = JSON.parse(content); }
  catch { throw configError('AutoRouter configuration must contain valid JSON.'); }
  const values = validate(parsed);
  return { env: { ...values, ...env }, values, path, exists: true, revision: revision(content) };
}

function existingFile(path) {
  let stat;
  try { stat = lstatSync(path); }
  catch (error) { if (error.code === 'ENOENT') return undefined; throw error; }
  if (stat.isSymbolicLink()) throw configError('Refusing to write a symbolic-link AutoRouter configuration.');
  if (!stat.isFile()) throw configError('AutoRouter configuration must be a regular file.');
  return stat;
}

export function saveUserConfig(values, { env = process.env, overwrite = false, expectedRevision } = {}) {
  const validated = validate(values);
  const path = getConfigPath(env);
  const parent = dirname(path);
  let temporary;
  let descriptor;
  const checkRevision = () => {
    if (expectedRevision === undefined) return;
    const current = existingFile(path) ? revision(readFileSync(path, 'utf8')) : null;
    if (current !== expectedRevision) throw configError('AutoRouter configuration changed while this operation was running. Retry with the current settings.');
  };
  try {
    if (existingFile(path) && !overwrite) {
      throw configError('AutoRouter configuration already exists; use overwrite to replace it.');
    }
    checkRevision();
    // mkdir leaves existing directory permissions unchanged. Only directories
    // created for this configuration receive the private creation mode.
    mkdirSync(parent, { recursive: true, mode: 0o700 });
    temporary = join(parent, `.${basename(path)}.${process.pid}.${randomBytes(12).toString('hex')}.tmp`);
    descriptor = openSync(temporary, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | (constants.O_NOFOLLOW ?? 0), 0o600);
    fchmodSync(descriptor, 0o600);
    writeFileSync(descriptor, `${JSON.stringify(validated, null, 2)}\n`, 'utf8');
    fsyncSync(descriptor);
    closeSync(descriptor);
    descriptor = undefined;

    // Check again after preparing the replacement. Renaming replaces the
    // directory entry rather than following a target that changes to a link.
    if (existingFile(path) && !overwrite) {
      throw configError('AutoRouter configuration already exists; use overwrite to replace it.');
    }
    checkRevision();
    if (overwrite) {
      renameSync(temporary, path);
      temporary = undefined;
    } else {
      // A hard link makes creation exclusive and atomic: a concurrent file or
      // symlink can never be replaced by the default save operation.
      linkSync(temporary, path);
    }
    return path;
  } catch (error) {
    if (error?.code === 'EEXIST') {
      throw configError('AutoRouter configuration already exists; use overwrite to replace it.');
    }
    throw filesystemError(error, 'save');
  } finally {
    if (descriptor !== undefined) { try { closeSync(descriptor); } catch {} }
    if (temporary) { try { unlinkSync(temporary); } catch {} }
  }
}
