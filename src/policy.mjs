import { lstatSync, readFileSync } from 'node:fs';
import { dirname } from 'node:path';

// Optional organization policy. It lives at a fixed system path that no
// environment variable can redirect, must be owned by root and not writable by
// anyone else, and is applied after the saved file and the environment, so
// neither can override it. This guards against configuration drift and
// environment-driven changes (direnv, devcontainers, CI variables). It does not
// stop someone who can run modified code; pair it with device management.
const ALLOWLISTS = Object.freeze({
  allowed_evaluators: { key: 'AUTOROUTER_EVALUATOR', fallback: 'ollama', values: ['jev', 'ollama'] },
  allowed_auth_modes: { key: 'AUTOROUTER_AUTH_MODE', fallback: 'api-key', values: ['api-key', 'subscription'] },
});
const LOCKS = Object.freeze({
  session_log_mode: { key: 'AUTOROUTER_SESSION_LOG_MODE', values: ['metadata', 'prompts'] },
  upstream_url: { key: 'AUTOROUTER_UPSTREAM_URL' },
  jev_url: { key: 'AUTOROUTER_JEV_URL' },
});
export const POLICY_KEYS = Object.freeze([...Object.keys(ALLOWLISTS), ...Object.keys(LOCKS)]);

function policyError(message) {
  const error = new Error(message);
  error.code = 'AUTOROUTER_CONFIG_ERROR';
  return error;
}

export function defaultPolicyPath(platform = process.platform) {
  return platform === 'darwin'
    ? '/Library/Application Support/claude-autorouter/policy.json'
    : '/etc/claude-autorouter/policy.json';
}

function checkTrusted(path, trustedUid) {
  for (const [target, kind] of [[path, 'file'], [dirname(path), 'directory']]) {
    const stat = lstatSync(target);
    const typeOk = kind === 'file' ? stat.isFile() : stat.isDirectory();
    if (stat.isSymbolicLink() || !typeOk || stat.uid !== trustedUid || (stat.mode & 0o022) !== 0) {
      throw policyError(`The AutoRouter organization policy ${kind} must be a regular ${kind} owned by root and not writable by group or others. Refusing to start.`);
    }
  }
}

function validate(parsed) {
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw policyError('The AutoRouter organization policy must be a JSON object.');
  const policy = {};
  for (const name of Object.keys(parsed)) {
    if (!POLICY_KEYS.includes(name)) throw policyError('The AutoRouter organization policy contains an unsupported key.');
  }
  for (const [name, { values }] of Object.entries(ALLOWLISTS)) {
    if (parsed[name] === undefined) continue;
    const list = parsed[name];
    if (!Array.isArray(list) || !list.length || list.some(value => !values.includes(value))) {
      throw policyError(`Policy ${name} must be a nonempty list of: ${values.join(', ')}.`);
    }
    policy[name] = [...new Set(list)];
  }
  for (const [name, { values }] of Object.entries(LOCKS)) {
    if (parsed[name] === undefined) continue;
    const value = parsed[name];
    const valid = values ? values.includes(value)
      : typeof value === 'string' && /^https?:\/\/[^\s]+$/.test(value) && !/[\u0000-\u001f\u007f]/.test(value);
    if (!valid) throw policyError(`Policy ${name} has an invalid value.`);
    policy[name] = value;
  }
  return policy;
}

/**
 * Returns undefined when no policy file exists. A file that exists but cannot
 * be trusted or parsed is an error: policy fails closed.
 */
export function loadPolicy({ path = defaultPolicyPath(), trustedUid = 0 } = {}) {
  let content;
  try {
    lstatSync(path);
  } catch (error) {
    if (error.code === 'ENOENT' || error.code === 'ENOTDIR') return undefined;
    throw policyError(`Could not read the AutoRouter organization policy (${error.code ?? 'error'}). Refusing to start.`);
  }
  try {
    checkTrusted(path, trustedUid);
    content = readFileSync(path, 'utf8');
  } catch (error) {
    if (error?.code === 'AUTOROUTER_CONFIG_ERROR') throw error;
    throw policyError('Could not read the AutoRouter organization policy. Refusing to start.');
  }
  let parsed;
  try { parsed = JSON.parse(content); }
  catch { throw policyError('The AutoRouter organization policy must contain valid JSON.'); }
  return { path, values: validate(parsed) };
}

/**
 * Enforce allowlists (an unlisted choice is an error) and apply locks (the
 * policy value replaces whatever the file or environment supplied). Commands
 * that repair a configuration pass allowlists: false and check new values
 * themselves, so a disallowed saved setting can still be corrected.
 */
export function applyPolicy(env, policy, { allowlists = true } = {}) {
  const result = { ...env };
  const locked = [];
  for (const [name, { key, fallback }] of Object.entries(ALLOWLISTS)) {
    const allowed = policy[name];
    if (!allowed || !allowlists) continue;
    const value = result[key] ?? fallback;
    if (!allowed.includes(value)) {
      throw policyError(`${key} is not permitted by the AutoRouter organization policy. Allowed: ${allowed.join(', ')}.`);
    }
  }
  for (const [name, { key }] of Object.entries(LOCKS)) {
    if (policy[name] === undefined) continue;
    result[key] = policy[name];
    locked.push(key);
  }
  return { env: result, locked };
}
