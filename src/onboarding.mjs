import { execFile } from 'node:child_process';
import { createInterface } from 'node:readline';
import { Writable } from 'node:stream';
import { promisify } from 'node:util';
import { CLIENT_PROFILES, readConfig, requireKeys, parseStopHookBlockCap, parseSessionLogDir } from './config.mjs';
import { buildClaudeEnv, conflictingProviders, LOCAL_AUTH_HEADER } from './auth.mjs';
import { CONFIG_KEYS, SECRET_CONFIG_KEYS, loadUserConfig, saveUserConfig } from './user-config.mjs';
import { DEFAULT_OLLAMA_MODEL, validateOllamaModel } from './ollama-models.mjs';
import { inspectOllama, setupOllama } from './ollama-setup.mjs';

const execute = promisify(execFile);

export const ollamaDeadlineText = timeoutMs => timeoutMs === 0
  ? 'routing deadline disabled' : `routing deadline ${timeoutMs} ms per request`;

const stopHookCapText = cap => cap === 0
  ? 'Claude Stop/SubagentStop continuation cap disabled (0).'
  : `Claude Stop/SubagentStop cap: ${cap} continuations without tool use.`;

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
  env = process.env, write = console.log, prompt = askSecret, fetchImpl = fetch, signal,
} = {}) {
  if (signal?.aborted) throw new Error('Setup cancelled');
  const loaded = loadUserConfig(env, { allowMissing: true });
  const replace = args.includes('--replace');
  const mergeExisting = loaded.exists && !replace;
  // Updating one preference must not turn unrelated runtime overrides into
  // saved defaults. First setup/replacement still supports environment-only
  // configuration; an explicitly selected backend can use its supplied key.
  const effectiveEnv = mergeExisting ? loaded.values : env;
  let explicitEvaluator = false, explicitAuthMode = false;
  let authMode = effectiveEnv.AUTOROUTER_AUTH_MODE ?? 'subscription';
  let clientProfile = effectiveEnv.AUTOROUTER_CLIENT_PROFILE ?? 'compatible';
  let evaluator = effectiveEnv.AUTOROUTER_EVALUATOR ?? 'jev';
  let model;
  let ollamaTimeoutMs;
  let stopHookBlockCap = effectiveEnv.CLAUDE_CODE_STOP_HOOK_BLOCK_CAP;
  let sessionLogDir = effectiveEnv.AUTOROUTER_SESSION_LOG_DIR;
  let sessionLogMode = effectiveEnv.AUTOROUTER_SESSION_LOG_MODE;
  let pull = false;
  let overwrite = replace;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === '--auth-mode') { authMode = args[++i]; explicitAuthMode = true; }
    else if (args[i] === '--client-profile') clientProfile = args[++i];
    else if (args[i] === '--evaluator') { evaluator = args[++i]; explicitEvaluator = true; }
    else if (args[i] === '--ollama-model') { model = args[++i]; if (model === undefined) throw new Error('--ollama-model requires a model tag'); }
    else if (args[i] === '--ollama-timeout-ms') {
      const value = args[++i];
      if (typeof value !== 'string' || !/^[0-9]+$/.test(value.trim()) || Number(value) > 30000) {
        throw new Error('--ollama-timeout-ms requires an integer between 0 and 30000 (0 disables the routing deadline)');
      }
      ollamaTimeoutMs = String(Number(value));
    }
    else if (args[i] === '--stop-hook-block-cap') {
      stopHookBlockCap = parseStopHookBlockCap(args[++i], '--stop-hook-block-cap');
    }
    else if (args[i] === '--session-log-dir') {
      sessionLogDir = args[++i];
      if (sessionLogDir === undefined || sessionLogDir.startsWith('--')) throw new Error('--session-log-dir requires a directory path');
      parseSessionLogDir(sessionLogDir, '--session-log-dir');
    }
    else if (args[i] === '--session-log-mode') {
      sessionLogMode = args[++i];
      if (!['metadata', 'prompts'].includes(sessionLogMode)) throw new Error('--session-log-mode must be metadata or prompts');
    }
    else if (args[i] === '--pull') pull = true;
    else if (args[i] === '--force') overwrite = true;
    else if (args[i] === '--replace') continue;
    else throw new Error('Usage: claude-autorouter setup [--auth-mode subscription|api-key] [--client-profile compatible|native|auto] [--evaluator jev|ollama] [--ollama-model TAG] [--ollama-timeout-ms N] [--stop-hook-block-cap N] [--session-log-dir DIR] [--session-log-mode metadata|prompts] [--pull] [--force|--replace]');
  }
  if (!['subscription', 'api-key'].includes(authMode)) throw new Error('--auth-mode must be subscription or api-key');
  if (!CLIENT_PROFILES.includes(clientProfile)) throw new Error('--client-profile must be compatible, native or auto');
  if (!['jev', 'ollama'].includes(evaluator)) throw new Error('--evaluator must be jev or ollama');
  if (evaluator !== 'ollama' && (model !== undefined || ollamaTimeoutMs !== undefined || pull)) throw new Error('Ollama model, deadline and download options require --evaluator ollama');
  if (stopHookBlockCap !== undefined) stopHookBlockCap = parseStopHookBlockCap(stopHookBlockCap);
  if (sessionLogDir !== undefined) sessionLogDir = parseSessionLogDir(sessionLogDir) ?? '';
  const path = loaded.path;
  if (!overwrite && loaded.exists) throw new Error('AutoRouter configuration already exists. Use setup --force to update it while preserving unrelated settings.');
  write(evaluator === 'ollama'
    ? 'AutoRouter evaluates bounded prompt excerpts locally with Ollama. Complete requests still go to Anthropic.'
    : 'AutoRouter sends bounded prompt excerpts to TypeSafe Jev and complete requests to Anthropic.');
  const values = replace ? {} : { ...loaded.values };
  const selectedBackendKeys = explicitEvaluator ? CONFIG_KEYS.filter(key => evaluator === 'ollama'
    ? key.startsWith('AUTOROUTER_OLLAMA_') : key.startsWith('AUTOROUTER_JEV_') || key === 'AUTOROUTER_MIN_CONFIDENCE') : [];
  for (const key of mergeExisting ? selectedBackendKeys : CONFIG_KEYS) {
    if (!SECRET_CONFIG_KEYS.includes(key) && env[key] !== undefined) values[key] = env[key];
  }
  Object.assign(values, { AUTOROUTER_AUTH_MODE: authMode, AUTOROUTER_CLIENT_PROFILE: clientProfile, AUTOROUTER_EVALUATOR: evaluator });
  if (stopHookBlockCap !== undefined) values.CLAUDE_CODE_STOP_HOOK_BLOCK_CAP = String(stopHookBlockCap);
  if (sessionLogDir !== undefined) values.AUTOROUTER_SESSION_LOG_DIR = sessionLogDir;
  if (sessionLogMode !== undefined) values.AUTOROUTER_SESSION_LOG_MODE = sessionLogMode;
  if (evaluator === 'ollama') {
    values.AUTOROUTER_OLLAMA_MODEL = validateOllamaModel(model
      ?? (explicitEvaluator ? env.AUTOROUTER_OLLAMA_MODEL : undefined) ?? effectiveEnv.AUTOROUTER_OLLAMA_MODEL ?? DEFAULT_OLLAMA_MODEL);
    for (const key of ['AUTOROUTER_OLLAMA_URL', 'AUTOROUTER_OLLAMA_TIMEOUT_MS', 'AUTOROUTER_OLLAMA_KEEP_ALIVE']) {
      if ((!mergeExisting || explicitEvaluator) && env[key] !== undefined) values[key] = env[key];
    }
    if (ollamaTimeoutMs !== undefined) values.AUTOROUTER_OLLAMA_TIMEOUT_MS = ollamaTimeoutMs;
  }
  const keys = [...(evaluator === 'jev' ? ['TYPESAFE_API_KEY'] : []), ...(authMode === 'api-key' ? ['ANTHROPIC_API_KEY'] : [])];
  // Reject invalid settings before inviting secret input or making local calls.
  readConfig(values);
  for (const key of keys) {
    if (signal?.aborted) throw new Error('Setup cancelled');
    const explicitlySelected = !mergeExisting || (key === 'TYPESAFE_API_KEY' ? explicitEvaluator : explicitAuthMode);
    const value = ((explicitlySelected ? env[key]?.trim() : '') || effectiveEnv[key]?.trim() || env[key]?.trim() || await prompt(key)).trim();
    if (signal?.aborted) throw new Error('Setup cancelled');
    if (!value || /[\r\n\0]/.test(value)) throw new Error(`${key} must be a nonempty, single-line key`);
    values[key] = value;
  }
  const config = readConfig(values);
  requireKeys(config);
  if (signal?.aborted) throw new Error('Setup cancelled');
  if (config.clientProfile === 'auto') write('Auto-compatible profile: Sonnet/Opus task routing. Claude controls permission-mode availability and safety checks.');
  if (config.stopHookBlockCap !== undefined) write(stopHookCapText(config.stopHookBlockCap));
  if (config.sessionLogDir) write(config.sessionLogMode === 'metadata' ? 'Session logs enabled with metadata only; prompt excerpts are omitted.'
    : 'Session decision logs enabled; files include up to 500 characters of user prompt text per decision.');
  if (evaluator === 'ollama') {
    write(`Local evaluator: ${config.ollamaModel}; ${ollamaDeadlineText(config.ollamaTimeoutMs)}.`);
    const controller = new AbortController();
    const cancel = () => controller.abort();
    if (!signal) for (const name of ['SIGINT', 'SIGTERM']) process.once(name, cancel);
    try { await setupOllama(config, { pull, warm: true, write, fetchImpl, signal: signal ?? controller.signal }); }
    finally { if (!signal) for (const name of ['SIGINT', 'SIGTERM']) process.removeListener(name, cancel); }
  }
  if (signal?.aborted) throw new Error('Setup cancelled');
  saveUserConfig(values, { env, overwrite, expectedRevision: loaded.revision });
  write(`Saved ${authMode} configuration to ${path}`);
  write(`${keys.length ? 'Keys and settings are' : 'Settings are'} stored locally in this file with owner-only permissions. Environment variables take precedence.`);
  write('Next: claude-autorouter doctor, then claude-autorouter claude from your project.');
}

export async function doctor({ env = process.env, write = console.log, run = execute, fetchImpl = fetch, signal,
  evaluateLocal = false, json = false, diagnosticRunner } = {}) {
  if (evaluateLocal) {
    try {
      const config = readConfig(loadUserConfig(env).env);
      if (config.evaluator !== 'ollama') throw new Error('Local evaluation requires AUTOROUTER_EVALUATOR=ollama. It does not call Jev or Anthropic.');
      const diagnostic = await import('./local-diagnostic.mjs');
      const onProgress = json ? () => {} : progress => {
        if (progress.event === 'preflight') write('Checking the local evaluator; no Claude authentication or cloud requests are used.');
        else if (progress.event === 'startup') write('Preparing the local model with a separate 60-second startup deadline…');
        else if (progress.event === 'case_start') write('Checking a synthetic routing case…');
        else if (progress.event === 'case_complete') write('Synthetic routing case finished.');
      };
      const result = await (diagnosticRunner ?? diagnostic.runLocalDiagnostic)(config, { fetchImpl, signal, onProgress });
      if (json) write(JSON.stringify(result));
      else for (const line of diagnostic.formatLocalDiagnostic(result)) write(line);
      return result.passed;
    } catch (error) {
      if (signal?.aborted) throw error;
      if (json) write(JSON.stringify({ schema_version: 1, type: 'local_evaluator_diagnostic', passed: false,
        error: { code: 'configuration_error', message: error.message } }));
      else write(`FAIL  ${error.message}`);
      return false;
    }
  }
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
    report(false, `${error.message}. Use claude-autorouter config show --check-all to inspect settings, or setup for first-time configuration.`);
  }
  for (const key of conflictingProviders(effectiveEnv)) {
    report(false, `Unset ${key}; AutoRouter uses the Anthropic Messages API`);
  }
  if (config?.stopHookBlockCap !== undefined) write(stopHookCapText(config.stopHookBlockCap));
  if (config) {
    write(`Routing profile: ${config.clientProfile}; Haiku ${config.models.haiku}; Sonnet ${config.models.sonnet}; Opus ${config.models.opus}.`);
    if (config.evaluator === 'jev') write(`Evaluator: Jev ${config.jevModel}; routing deadline ${config.jevTimeoutMs} ms; confidence floor ${config.minConfidence}.`);
  }
  if (config?.clientProfile === 'auto') write('Auto-compatible profile: Sonnet/Opus task routing. Claude controls permission-mode availability and safety checks.');
  if (config?.sessionLogDir) write(config.sessionLogMode === 'metadata' ? 'Session logs enabled with metadata only; prompt excerpts are omitted.'
    : 'Session decision logs enabled; files include up to 500 characters of user prompt text per decision.');
  if (config?.evaluator === 'ollama') {
    write(`Local evaluator: ${config.ollamaModel}; ${ollamaDeadlineText(config.ollamaTimeoutMs)}.`);
    write('Model availability is checked below; classification speed and accuracy are not tested.');
    try {
      const result = await inspectOllama(config, { fetchImpl, signal });
      report(result.installed, result.installed
        ? `Local Ollama model available (${result.model})`
        : `Ollama model missing (${result.model}). Run claude-autorouter setup --evaluator ollama --ollama-model ${result.model} --pull --force.`);
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
    if (version) write(`Historical integration observations cover Claude Code 2.1.284 and 2.1.285; finding an executable does not certify its full compatibility${['2.1.284', '2.1.285'].includes(version) ? '.' : ' (installed version differs).'}`);
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
