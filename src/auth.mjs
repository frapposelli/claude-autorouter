export const LOCAL_AUTH_HEADER = 'x-autorouter-token';

// Leave Claude's permission selection and policy enforcement to Claude. This
// only chooses a compatible routing profile for an explicit Auto-mode launch.
export function clientProfileForLaunch(profile, args) {
  let permissionMode;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === '--') break;
    if (args[i] === '--permission-mode') permissionMode = args[++i];
    else if (args[i].startsWith('--permission-mode=')) permissionMode = args[i].slice('--permission-mode='.length);
  }
  return permissionMode === 'auto' ? 'auto' : profile;
}

export function conflictingProviders(env = process.env) {
  return ['CLAUDE_CODE_USE_BEDROCK', 'CLAUDE_CODE_USE_VERTEX', 'CLAUDE_CODE_USE_FOUNDRY',
    'CLAUDE_CODE_USE_MANTLE', 'CLAUDE_CODE_USE_ANTHROPIC_AWS']
    .filter(key => ['1', 'true'].includes(String(env[key]).toLowerCase()));
}

// This recognizes the subscription wire format, not the validity of the
// credential. Anthropic validates the bearer token on every forwarded request.
export function isSubscriptionRequest(headers) {
  return !headers['x-api-key']
    && /^Bearer \S+$/i.test(headers.authorization ?? '')
    && String(headers['anthropic-beta'] ?? '').split(',').some(value => /^oauth-/.test(value.trim()));
}

export function buildClaudeEnv(config, baseUrl, parent = process.env) {
  const env = { ...parent, ANTHROPIC_BASE_URL: baseUrl, CLAUDE_CODE_GATEWAY_HINT_HEADERS: '1' };
  // Opt into Claude's own loop guard without installing hooks or altering
  // their verdicts. An unset cap leaves Claude's default in control.
  if (env.CLAUDE_CODE_STOP_HOOK_BLOCK_CAP === undefined && config.stopHookBlockCap !== undefined) {
    env.CLAUDE_CODE_STOP_HOOK_BLOCK_CAP = String(config.stopHookBlockCap);
  }
  // Claude Code otherwise disables MCP tool search for a non-first-party
  // base URL and loads every schema into context. This proxy preserves both
  // tool_reference blocks and their beta headers. Respect explicit choices.
  if (env.ENABLE_TOOL_SEARCH === undefined) env.ENABLE_TOOL_SEARCH = 'true';
  // Start with a request format all three tiers accept. Jev still chooses the
  // upstream model. Native mode preserves the client's full feature selection.
  if (config.clientProfile === 'compatible') {
    env.ANTHROPIC_MODEL = config.models.haiku;
    env.MAX_THINKING_TOKENS = '0';
  } else if (config.clientProfile === 'auto') {
    // Haiku cannot run Auto permission mode. Leave explicit client choices
    // and thinking settings to Claude; it still enforces account/admin gates.
    env.ANTHROPIC_MODEL ??= config.models.sonnet;
  }
  const subscription = config.authMode === 'subscription';
  // Keep unrelated custom headers. Remove stale router credentials and, in
  // subscription mode, inherited custom auth that could override the login.
  const headers = String(env.ANTHROPIC_CUSTOM_HEADERS ?? '').split(/\r?\n/).filter(line => {
    const name = line.split(':', 1)[0].trim().toLowerCase();
    return line.trim() && name !== LOCAL_AUTH_HEADER
      && !(subscription && ['authorization', 'x-api-key'].includes(name));
  });
  if (subscription) {
    delete env.ANTHROPIC_API_KEY;
    delete env.ANTHROPIC_AUTH_TOKEN;
    delete env.CLAUDE_CODE_OAUTH_TOKEN;
    headers.push(`X-Autorouter-Token: ${config.localToken}`);
  } else {
    env.ANTHROPIC_API_KEY = config.localToken;
    env.ANTHROPIC_AUTH_TOKEN = config.localToken;
  }
  if (headers.length) env.ANTHROPIC_CUSTOM_HEADERS = headers.join('\n');
  else delete env.ANTHROPIC_CUSTOM_HEADERS;
  delete env.TYPESAFE_API_KEY;
  delete env.AUTOROUTER_TOKEN;
  return env;
}
