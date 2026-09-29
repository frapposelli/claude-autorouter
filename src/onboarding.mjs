import { execFile } from 'node:child_process';
import { existsSync } from 'node:fs';
import { createInterface } from 'node:readline';
import { Writable } from 'node:stream';
import { promisify } from 'node:util';
import { readConfig, requireKeys } from './config.mjs';
import { buildClaudeEnv, conflictingProviders, LOCAL_AUTH_HEADER } from './auth.mjs';
import { getConfigPath, loadUserConfig, saveUserConfig } from './user-config.mjs';
import { selectOllamaModel } from './ollama-models.mjs';
import { inspectOllama, setupOllama } from './ollama-setup.mjs';

const execute = promisify(execFile);

// Readline manages editing and restores terminal state; its output is discarded
// so neither typing nor pasted credentials are echoed to the terminal.
export async function askSecret(label, { input = process.stdin, output = process.stderr } = {}) {
  if (!input.isTTY || !output.isTTY) throw new Error(`Set ${label} in the environment for noninteractive setup`);
  const muted = new Writable({ write(_chunk, _encoding, done) { done(); } });
  const rl = createInterface({ input, output: muted, terminal: true, historySize: 0 });
  try {
    return await new Promise((resolve, reject) => {
      rl.once('SIGINT', () => reject(new Error('Setup cancelled')));
      rl.once('close', () => reject(new Error('Setup cancelled')));
      rl.question('', resolve);
      output.write(`${label} (hidden): `);
    });
  } finally {
    rl.close();
    output.write('\n');
  }
}

export async function setup(args, {
  env = process.env, write = console.log, prompt = askSecret, fetchImpl = fetch, signal, totalMemory,
} = {}) {
  let authMode = env.AUTOROUTER_AUTH_MODE ?? 'subscription';
  let evaluator = env.AUTOROUTER_EVALUATOR ?? 'jev';
  let preset = 'compact';
  let explicitPreset = false;
  let model;
  let pull = false;
  let overwrite = false;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === '--auth-mode') authMode = args[++i];
    else if (args[i] === '--evaluator') evaluator = args[++i];
    else if (args[i] === '--ollama-preset') { preset = args[++i]; explicitPreset = true; if (preset === undefined) throw new Error('--ollama-preset requires compact, quality, or auto'); }
    else if (args[i] === '--ollama-model') { model = args[++i]; if (model === undefined) throw new Error('--ollama-model requires a model tag'); }
    else if (args[i] === '--pull') pull = true;
    else if (args[i] === '--force') overwrite = true;
    else throw new Error('Usage: claude-autorouter setup [--auth-mode subscription|api-key] [--evaluator jev|ollama] [--ollama-preset compact|quality|auto] [--ollama-model TAG] [--pull] [--force]');
  }
  if (!['subscription', 'api-key'].includes(authMode)) throw new Error('--auth-mode must be subscription or api-key');
  if (!['jev', 'ollama'].includes(evaluator)) throw new Error('--evaluator must be jev or ollama');
  if (evaluator !== 'ollama' && (explicitPreset || model !== undefined || pull)) throw new Error('Ollama model and download options require --evaluator ollama');
  const path = getConfigPath(env);
  if (!overwrite && existsSync(path)) throw new Error('AutoRouter configuration already exists. Use setup --force to replace it.');
  write(evaluator === 'ollama'
    ? 'AutoRouter evaluates bounded prompt excerpts locally with Ollama. Complete requests still go to Anthropic.'
    : 'AutoRouter sends bounded prompt excerpts to TypeSafe Jev and complete requests to Anthropic.');
  const values = { AUTOROUTER_AUTH_MODE: authMode, AUTOROUTER_CLIENT_PROFILE: 'compatible', AUTOROUTER_EVALUATOR: evaluator };
  if (evaluator === 'ollama') {
    values.AUTOROUTER_OLLAMA_MODEL = selectOllamaModel({ preset, model: model ?? (explicitPreset ? undefined : env.AUTOROUTER_OLLAMA_MODEL), totalMemory });
    for (const key of ['AUTOROUTER_OLLAMA_URL', 'AUTOROUTER_OLLAMA_TIMEOUT_MS', 'AUTOROUTER_OLLAMA_KEEP_ALIVE']) {
      if (env[key] !== undefined) values[key] = env[key];
    }
    write(`Local evaluator model: ${values.AUTOROUTER_OLLAMA_MODEL}.`);
  }
  const keys = [...(evaluator === 'jev' ? ['TYPESAFE_API_KEY'] : []), ...(authMode === 'api-key' ? ['ANTHROPIC_API_KEY'] : [])];
  for (const key of keys) {
    const value = (env[key]?.trim() || await prompt(key)).trim();
    if (!value || /[\r\n\0]/.test(value)) throw new Error(`${key} must be a nonempty, single-line key`);
    values[key] = value;
  }
  const config = readConfig(values);
  requireKeys(config);
  if (evaluator === 'ollama') {
    const controller = new AbortController();
    const cancel = () => controller.abort();
    if (!signal) for (const name of ['SIGINT', 'SIGTERM']) process.once(name, cancel);
    try { await setupOllama(config, { pull, warm: true, write, fetchImpl, signal: signal ?? controller.signal }); }
    finally { if (!signal) for (const name of ['SIGINT', 'SIGTERM']) process.removeListener(name, cancel); }
  }
  if (signal?.aborted) throw new Error('Setup cancelled');
  saveUserConfig(values, { env, overwrite });
  write(`Saved ${authMode} configuration to ${path}`);
  write(`${keys.length ? 'Keys and settings are' : 'Settings are'} stored locally in this file with owner-only permissions. Environment variables take precedence.`);
  write('Next: claude-autorouter doctor, then claude-autorouter claude from your project.');
}

export async function doctor({ env = process.env, write = console.log, run = execute, fetchImpl = fetch, signal } = {}) {
  let healthy = true;
  const report = (ok, message) => { if (!ok) healthy = false; write(`${ok ? 'OK' : 'FAIL'}  ${message}`); };
  report(Number(process.versions.node.split('.')[0]) >= 22, `Node.js ${process.versions.node} (requires 22+)`);
  let config;
  let effectiveEnv = env;
  try {
    const loaded = loadUserConfig(env);
    effectiveEnv = loaded.env;
    write(`Config: ${loaded.path}${loaded.exists ? '' : ' (absent; using environment)'}`);
    config = readConfig(effectiveEnv);
    requireKeys(config);
    report(true, `Configuration and required keys present (${config.authMode})`);
  } catch (error) {
    report(false, `${error.message}. Run claude-autorouter setup.`);
  }
  for (const key of conflictingProviders(effectiveEnv)) {
    report(false, `Unset ${key}; AutoRouter uses the Anthropic Messages API`);
  }
  if (config?.evaluator === 'ollama') {
    try {
      const result = await inspectOllama(config, { fetchImpl, signal });
      report(result.installed, result.installed
        ? `Local Ollama model available (${result.model})`
        : `Ollama model missing (${result.model}). Run ollama pull ${result.model}, or setup --evaluator ollama --pull --force.`);
    } catch (error) {
      report(false, error.message);
    }
  }
  // Use the launcher's credential cleanup for auth inspection too. Claude owns
  // saved credentials; never read its credential files or print its JSON output.
  const childEnv = config ? buildClaudeEnv({ ...config, localToken: '' }, 'http://127.0.0.1:1', effectiveEnv) : { ...effectiveEnv };
  for (const key of ['ANTHROPIC_BASE_URL', 'AUTOROUTER_CONFIG', 'TYPESAFE_API_KEY', 'AUTOROUTER_TOKEN',
    'ANTHROPIC_API_KEY', 'ANTHROPIC_AUTH_TOKEN', 'CLAUDE_CODE_OAUTH_TOKEN', 'AUTOROUTER_STATUS_FILE']) delete childEnv[key];
  const headers = String(childEnv.ANTHROPIC_CUSTOM_HEADERS ?? '').split(/\r?\n/)
    .filter(line => line.trim() && ![LOCAL_AUTH_HEADER, 'authorization', 'x-api-key'].includes(line.split(':', 1)[0].trim().toLowerCase()));
  if (headers.length) childEnv.ANTHROPIC_CUSTOM_HEADERS = headers.join('\n');
  else delete childEnv.ANTHROPIC_CUSTOM_HEADERS;
  let claudeAvailable = false;
  try {
    const { stdout } = await run('claude', ['--version'], { env: childEnv, timeout: 10000, maxBuffer: 64 * 1024 });
    const version = /\b\d+\.\d+\.\d+\b/.exec(stdout)?.[0];
    report(Boolean(version), version ? `Claude Code ${version}` : 'Could not recognize Claude Code version');
    claudeAvailable = Boolean(version);
  } catch {
    report(false, 'Claude Code unavailable. Install claude and ensure it is on PATH.');
  }
  if (claudeAvailable && config?.authMode === 'subscription') {
    try {
      const { stdout } = await run('claude', ['auth', 'status', '--json'], { env: childEnv, timeout: 10000, maxBuffer: 64 * 1024 });
      const status = JSON.parse(stdout);
      const subscription = status.loggedIn === true && status.authMethod === 'claude.ai';
      report(subscription, subscription ? 'Claude subscription login found' : 'Claude subscription login not found. Run claude auth login.');
    } catch {
      report(false, 'Could not verify Claude subscription login. Run claude auth login (or update Claude Code).');
    }
  }
  write('Local checks only; external provider connectivity, key validity, Anthropic model access, and quota are not tested.');
  return healthy;
}
