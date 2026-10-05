import { readConfig, parseSessionLogDir, parseStopHookBlockCap } from './config.mjs';
import { CONFIG_KEYS, SECRET_CONFIG_KEYS, loadUserConfig, saveUserConfig } from './user-config.mjs';
import { modelCapabilities } from './model-catalog.mjs';
import { askSecret } from './onboarding.mjs';

const secretKey = key => SECRET_CONFIG_KEYS.includes(key);
const providerFor = key => key.startsWith('AUTOROUTER_OLLAMA_') ? 'ollama'
  : key.startsWith('AUTOROUTER_JEV_') || key === 'AUTOROUTER_MIN_CONFIDENCE' || key === 'TYPESAFE_API_KEY' ? 'jev' : undefined;
const safeText = value => String(value).replace(/[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/g, '');

function effectiveValues(config, env) {
  return {
    AUTOROUTER_AUTH_MODE: config.authMode, AUTOROUTER_CLIENT_PROFILE: config.clientProfile,
    AUTOROUTER_EVALUATOR: config.evaluator, AUTOROUTER_UPSTREAM_URL: config.upstream,
    AUTOROUTER_HAIKU_MODEL: config.models.haiku, AUTOROUTER_SONNET_MODEL: config.models.sonnet,
    AUTOROUTER_OPUS_MODEL: config.models.opus, AUTOROUTER_PORT: config.port,
    AUTOROUTER_TOKEN_COUNT_TIMEOUT_MS: config.tokenCountTimeoutMs,
    AUTOROUTER_JEV_URL: config.jevEndpoint, AUTOROUTER_JEV_MODEL: config.jevModel,
    AUTOROUTER_JEV_TIMEOUT_MS: config.jevTimeoutMs, AUTOROUTER_MIN_CONFIDENCE: config.minConfidence,
    AUTOROUTER_OLLAMA_URL: config.ollamaEndpoint, AUTOROUTER_OLLAMA_MODEL: config.ollamaModel,
    AUTOROUTER_OLLAMA_TIMEOUT_MS: config.ollamaTimeoutMs, AUTOROUTER_OLLAMA_KEEP_ALIVE: config.ollamaKeepAlive,
    AUTOROUTER_SESSION_LOG_DIR: config.sessionLogDir ?? null,
    AUTOROUTER_SESSION_LOG_MODE: config.sessionLogMode,
    CLAUDE_CODE_STOP_HOOK_BLOCK_CAP: config.stopHookBlockCap ?? null,
    AUTOROUTER_STATUSLINE: env.AUTOROUTER_STATUSLINE !== '0',
    AUTOROUTER_DEBUG: env.AUTOROUTER_DEBUG === '1', ENABLE_TOOL_SEARCH: env.ENABLE_TOOL_SEARCH ?? 'true',
  };
}

/** Redacted effective configuration, independent of Claude launch arguments. */
export function configReport(loaded, env, { checkAll = false } = {}) {
  const config = readConfig(loaded.env);
  const values = effectiveValues(config, loaded.env);
  const settings = {};
  for (const key of CONFIG_KEYS) {
    const source = env[key] !== undefined ? 'environment' : Object.hasOwn(loaded.values, key) ? 'file' : 'default';
    const provider = providerFor(key);
    const active = (!provider || provider === config.evaluator)
      && (key !== 'ANTHROPIC_API_KEY' || config.authMode === 'api-key');
    const secret = secretKey(key);
    settings[key] = { source, active,
      ...(source === 'environment' && Object.hasOwn(loaded.values, key) ? { overrides_file: true } : {}),
      ...(secret ? { secret: true, present: typeof loaded.env[key] === 'string' && Boolean(loaded.env[key].trim()) }
        : { value: active ? values[key] ?? null : null }),
    };
  }
  const warnings = [];
  if (config.clientProfile === 'auto' && [config.models.sonnet, config.models.opus].some(model => !modelCapabilities(model)?.sharedAuto)) {
    warnings.push('Auto permission eligibility does not guarantee automatic switching. Older targets can retain the incoming model for shared execution features.');
  }
  let valid = true, error;
  if (checkAll) {
    try { readConfig(loaded.env, { validateAll: true }); }
    catch (failure) { valid = false; error = failure.message; }
  }
  return { schema_version: 1, config_path: loaded.path, config_exists: loaded.exists, valid,
    checked: checkAll ? 'all_evaluators' : 'active_evaluator', settings, warnings,
    ...(error ? { error } : {}),
  };
}

async function stdinSecret(input) {
  if (input.isTTY) throw new Error('Pipe the secret to --stdin, or omit --stdin to use the hidden prompt.');
  const chunks = [];
  let bytes = 0;
  try {
    for await (const chunk of input) {
      const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      bytes += buffer.length;
      if (bytes > 16384) throw new Error('Secret input exceeds the supported size.');
      chunks.push(buffer);
    }
  } catch { throw new Error('Could not read a bounded secret from stdin.'); }
  return Buffer.concat(chunks).toString('utf8').replace(/\r?\n$/, '');
}

function normalizedValue(key, value) {
  if (typeof value !== 'string') throw new Error('Configuration values must be strings.');
  if (secretKey(key)) {
    const normalized = value.trim();
    if (!normalized || /[\r\n\0]/.test(normalized)) throw new Error(`${key} must be a nonempty, single-line secret.`);
    if (key === 'AUTOROUTER_TOKEN' && normalized.length < 16) throw new Error('AUTOROUTER_TOKEN must contain at least 16 characters.');
    return normalized;
  }
  if (key === 'AUTOROUTER_SESSION_LOG_DIR') return parseSessionLogDir(value) ?? '';
  if (key === 'CLAUDE_CODE_STOP_HOOK_BLOCK_CAP') return String(parseStopHookBlockCap(value));
  if (/[\r\n\0\u001b]/.test(value)) throw new Error(`${key} must be a single-line setting.`);
  return value;
}

export async function configCommand(args, {
  env = process.env, write = console.log, input = process.stdin, promptSecret = askSecret,
} = {}) {
  const [operation, ...rest] = args;
  if (operation === 'show') {
    if (rest.some(arg => !['--json', '--check-all'].includes(arg))) throw new Error('Usage: claude-autorouter config show [--json] [--check-all]');
    let loaded, report;
    try {
      loaded = loadUserConfig(env, { allowMissing: true });
      report = configReport(loaded, env, { checkAll: rest.includes('--check-all') });
    }
    catch (error) {
      report = { schema_version: 1, ...(loaded ? { config_path: loaded.path, config_exists: loaded.exists } : {}), valid: false, error: error.message };
    }
    if (rest.includes('--json')) write(JSON.stringify(report, null, 2));
    else {
      if (loaded) write(`Config: ${loaded.path}${loaded.exists ? '' : ' (not saved)'}`);
      for (const [key, setting] of Object.entries(report.settings ?? {})) {
        const value = setting.secret ? setting.present ? '[set; hidden]' : '[unset]'
          : !setting.active ? '[inactive]' : setting.value === null ? '[unset]' : safeText(setting.value);
        write(`${key}=${value} [${setting.source}${setting.overrides_file ? '; overrides file' : ''}${!setting.active ? '; inactive' : ''}]`);
      }
      for (const warning of report.warnings ?? []) write(warning);
      if (report.error) write(`FAIL  ${report.error}`);
    }
    return report.valid;
  }
  if (!['set', 'unset'].includes(operation)) throw new Error('Usage: claude-autorouter config show|set|unset');
  const [key, value, ...extra] = rest;
  // Never echo an unknown key: it may itself be a pasted credential.
  if (!CONFIG_KEYS.includes(key)) throw new Error('Unsupported configuration key. Run claude-autorouter config show for supported settings.');
  if (extra.length || (operation === 'unset' && value !== undefined)) throw new Error(`Usage: claude-autorouter config ${operation} KEY${operation === 'set' ? ' VALUE' : ''}`);
  if (operation === 'set' && secretKey(key) && value !== undefined && value !== '--stdin') {
    throw new Error('Secret values are not accepted as command arguments. Use --stdin or the hidden prompt.');
  }
  if (operation === 'set' && !secretKey(key) && (value === undefined || value === '--stdin')) throw new Error('Nonsecret settings require a value argument.');
  const loaded = loadUserConfig(env, { allowMissing: true });
  const next = { ...loaded.values };
  if (operation === 'unset') delete next[key];
  else next[key] = normalizedValue(key, secretKey(key)
    ? value === '--stdin' ? await stdinSecret(input) : await promptSecret(key)
    : value);
  // Validate the persisted setting itself, not an environment value that could
  // mask it. An explicitly edited inactive provider is checked too.
  const provider = operation === 'set' ? providerFor(key) : undefined;
  readConfig({ ...next, ...(provider ? { AUTOROUTER_EVALUATOR: provider } : {}) });
  saveUserConfig(next, { env, overwrite: loaded.exists, expectedRevision: loaded.revision });
  write(`${operation === 'unset' ? 'Removed saved' : 'Saved'} ${key}. Other saved settings are unchanged.`);
  if (env[key] !== undefined) write(`The current environment still overrides ${key}; unset that environment variable to use the saved/default value.`);
  return true;
}
