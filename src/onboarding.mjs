import { execFile } from 'node:child_process';
import { existsSync } from 'node:fs';
import { createInterface } from 'node:readline';
import { Writable } from 'node:stream';
import { promisify } from 'node:util';
import { readConfig, requireKeys } from './config.mjs';
import { buildClaudeEnv, conflictingProviders, LOCAL_AUTH_HEADER } from './auth.mjs';
import { getConfigPath, loadUserConfig, saveUserConfig } from './user-config.mjs';

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

export async function setup(args, { env = process.env, write = console.log, prompt = askSecret } = {}) {
  let authMode = env.AUTOROUTER_AUTH_MODE ?? 'subscription';
  let overwrite = false;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === '--auth-mode') authMode = args[++i];
    else if (args[i] === '--force') overwrite = true;
    else throw new Error('Usage: claude-autorouter setup [--auth-mode subscription|api-key] [--force]');
  }
  if (!['subscription', 'api-key'].includes(authMode)) throw new Error('--auth-mode must be subscription or api-key');
  const path = getConfigPath(env);
  if (!overwrite && existsSync(path)) throw new Error('AutoRouter configuration already exists. Use setup --force to replace it.');
  write('AutoRouter sends bounded prompt excerpts to TypeSafe Jev and complete requests to Anthropic.');
  const values = { AUTOROUTER_AUTH_MODE: authMode, AUTOROUTER_CLIENT_PROFILE: 'compatible' };
  const keys = ['TYPESAFE_API_KEY', ...(authMode === 'api-key' ? ['ANTHROPIC_API_KEY'] : [])];
  for (const key of keys) {
    const value = (env[key]?.trim() || await prompt(key)).trim();
    if (!value || /[\r\n\0]/.test(value)) throw new Error(`${key} must be a nonempty, single-line key`);
    values[key] = value;
  }
  requireKeys(readConfig(values));
  saveUserConfig(values, { env, overwrite });
  write(`Saved ${authMode} configuration to ${path}`);
  write('Keys are stored locally in this file with owner-only permissions. Environment variables take precedence.');
  write('Next: claude-autorouter doctor, then claude-autorouter claude from your project.');
}

export async function doctor({ env = process.env, write = console.log, run = execute } = {}) {
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
  write('Local checks only; provider connectivity, key validity, model access, and quota are not tested.');
  return healthy;
}
