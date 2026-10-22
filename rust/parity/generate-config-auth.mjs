// Shared scalar configuration/authentication assertions from the frozen tests.
// This source-only generator is never included in the installed product.
import { writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
const cases = [];
const emit = (file, assertion, op, input) => cases.push({ id: `${file}-${assertion}-${cases.length}`, op, input,
  source_tests: [`test/${file}.test.mjs`], source_test_id: `test/${file}.test.mjs#${assertion}` });
const config = (assertion, env, extra = {}) => emit('config', assertion, 'read_config', { env, ...extra });
for (const value of [undefined, '', './local session logs', ' ', 'private\npath', 'private\0path', false, null, 0, {}]) config(1, value === undefined ? {} : { AUTOROUTER_SESSION_LOG_DIR: value });
for (const value of [undefined, 'prompts', 'metadata', '', ' ', 'private-value', false, null]) config(2, value === undefined ? {} : { AUTOROUTER_SESSION_LOG_MODE: value });
const profiles = ['compatible', 'native', 'auto'];
for (const profile of [...profiles, '', 'Auto', 'automatic', 'unknown']) config(3, { AUTOROUTER_CLIENT_PROFILE: profile });
config(3, {});
for (const [sonnet, opus] of [['claude-sonnet-4-6', 'claude-opus-4-6'], ['claude-sonnet-5', 'claude-opus-4-7'], ['claude-sonnet-5-5', 'claude-opus-5-5']]) config(3, { AUTOROUTER_CLIENT_PROFILE: 'auto', AUTOROUTER_SONNET_MODEL: sonnet, AUTOROUTER_OPUS_MODEL: opus });
for (const key of ['AUTOROUTER_SONNET_MODEL', 'AUTOROUTER_OPUS_MODEL']) for (const model of ['claude-haiku-4-5-20251001', 'claude-sonnet-4-5', 'claude-opus-4-5', 'sonnet', 'opus', 'custom/claude-sonnet-5', 'claude-opus-5-5-custom', 'claude-sonnet-99']) for (const profile of profiles) config(4, { AUTOROUTER_CLIENT_PROFILE: profile, [key]: model });
config(5, {});
for (const value of [0, '0', 2, '2', '002', Number.MAX_SAFE_INTEGER, String(Number.MAX_SAFE_INTEGER)]) for (const evaluator of ['jev', 'ollama']) config(5, { AUTOROUTER_EVALUATOR: evaluator, CLAUDE_CODE_STOP_HOOK_BLOCK_CAP: value });
// NaN/Infinity cannot be represented by JSON; the Rust library's typed numeric
// contract is reviewed separately, never silently counted as this JSON corpus.
for (const value of [-1, '-1', 1.5, '1.5', '2.0', '1e2', '1e-999', '-1e-999', '0x2', '+2', '', ' ', '2\n3', null, false, true, [], {}, 'Infinity', 'invalid', Number.MAX_SAFE_INTEGER + 1, '9007199254740992']) config(6, { CLAUDE_CODE_STOP_HOOK_BLOCK_CAP: value });
for (const env of [{}, { AUTOROUTER_EVALUATOR: 'jev' }, { AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_OLLAMA_MODEL: 'team/decision-model:v1' }]) config(7, env);
for (const prefix of ['', 'library/', 'registry.ollama.ai/', 'registry.ollama.ai/library/']) for (const tag of ['tev1', 'tev1:latest', 'tev1:4b', 'tev1:4b-q4_K_M', 'tev1:4b-q8_0', 'tev1:4b-f16', 'tev1:0.8b', 'tev1:0.8b-q8_0', 'nimble', 'nimble:latest', 'nimble:9b', 'nimble:9b-q4_K_M', 'nimble:9b-q8_0', 'nimble:9b-f16']) config(8, { AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_OLLAMA_MODEL: prefix + tag });
for (const model of ['team/nimble', 'team/tev1:4b', 'registry.ollama.ai/team/nimble:9b-q4_K_M', 'registry.example/library/nimble:9b', 'library/team/tev1:4b', 'tev1:40b', 'tev1:4b-experimental', 'nimble:small', 'nimble:9b-custom', 'nimble:9b-q4_K_M-extra', 'nimble-other:9b', 'custom:v1']) config(9, { AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_OLLAMA_MODEL: model });
for (const model of ['tev1:0.8b', 'tev1:4b', 'nimble:9b-q4_K_M', 'team/nimble']) for (const timeout of [0, 1, 1500, 18000, 30000]) config(10, { AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_OLLAMA_MODEL: model, AUTOROUTER_OLLAMA_TIMEOUT_MS: String(timeout) });
for (const value of ['-1', '30001', '1.5', '1e-999', '-1e-999', 'Infinity', 'unknown', '', ' ', null, false, true, [], {}]) config(10, { AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_OLLAMA_TIMEOUT_MS: value });
for (const env of [{ AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_AUTH_MODE: 'subscription' }, { AUTOROUTER_AUTH_MODE: 'subscription' }, { AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_AUTH_MODE: 'subscription' }, { AUTOROUTER_EVALUATOR: 'ollama' }, { AUTOROUTER_EVALUATOR: 'ollama', ANTHROPIC_API_KEY: 'test-api' }]) config(11, env, { require_keys: true });
for (const env of [{ AUTOROUTER_EVALUATOR: 'auto' }, { AUTOROUTER_OLLAMA_URL: 'https://example.com' }, { AUTOROUTER_OLLAMA_URL: 'http://127.0.0.1:11434/redirect' }, { AUTOROUTER_OLLAMA_MODEL: 'model:cloud' }, { AUTOROUTER_OLLAMA_TIMEOUT_MS: '-1' }, { AUTOROUTER_OLLAMA_KEEP_ALIVE: '-1' }]) config(12, { AUTOROUTER_EVALUATOR: 'ollama', ...env });
for (const env of [{ AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_OLLAMA_URL: 'https://private-sentinel:secret@example.test', AUTOROUTER_OLLAMA_TIMEOUT_MS: '' }, { AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_JEV_URL: 'private-invalid-url', AUTOROUTER_MIN_CONFIDENCE: '' }]) for (const validate_all of [false, true]) config(13, env, { validate_all });
for (const key of ['AUTOROUTER_PORT', 'AUTOROUTER_JEV_TIMEOUT_MS', 'AUTOROUTER_TOKEN_COUNT_TIMEOUT_MS', 'AUTOROUTER_MIN_CONFIDENCE']) for (const value of ['', ' ', null, false, '0x10', '1e2']) config(14, { AUTOROUTER_EVALUATOR: 'jev', [key]: value });
for (const env of [{ AUTOROUTER_PORT: '0' }, { AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_MIN_CONFIDENCE: '0' }, { AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_MIN_CONFIDENCE: '.5' }]) config(14, env);
for (const key of ['AUTOROUTER_STATUSLINE', 'AUTOROUTER_DEBUG']) for (const value of ['', 'false', 'true', '2', false, '0', '1']) config(15, { [key]: value });
for (const key of ['AUTOROUTER_HAIKU_MODEL', 'AUTOROUTER_SONNET_MODEL', 'AUTOROUTER_OPUS_MODEL', 'AUTOROUTER_JEV_MODEL']) for (const value of ['', ' ', 'private\nsentinel', null, 'custom/team-model:v1']) config(15, { AUTOROUTER_EVALUATOR: 'jev', [key]: value });
config(15, { ENABLE_TOOL_SEARCH: 'auto:5' });
const auth = (assertion, op, input) => emit('auth', assertion, op, input);
const envCase = (assertion, config, parent = {}) => auth(assertion, 'build_claude_env', { config: { ...config, AUTOROUTER_TOKEN: 'test-local-token-123456789' }, parent });
for (const key of ['CLAUDE_CODE_USE_BEDROCK', 'CLAUDE_CODE_USE_VERTEX', 'CLAUDE_CODE_USE_FOUNDRY', 'CLAUDE_CODE_USE_MANTLE', 'CLAUDE_CODE_USE_ANTHROPIC_AWS']) for (const enabled of ['1', 'true', 'TRUE', '0', 'false', '', undefined]) auth(1, 'conflicting_providers', enabled === undefined ? {} : { [key]: enabled });
for (const env of [{ AUTOROUTER_AUTH_MODE: 'subscription', TYPESAFE_API_KEY: 'test-jev', ANTHROPIC_API_KEY: 'ignored' }, { AUTOROUTER_AUTH_MODE: 'subscription' }, { TYPESAFE_API_KEY: 'test-jev' }, { TYPESAFE_API_KEY: 'test-jev', ANTHROPIC_API_KEY: 'test-api' }, { AUTOROUTER_AUTH_MODE: 'automatic' }]) auth(2, 'read_config', { env: { AUTOROUTER_EVALUATOR: 'jev', ...env }, require_keys: true });
for (const upstream of ['https://example.com', 'https://api.anthropic.com.example.com', 'https://api.anthropic.com/other', 'http://127.0.0.1:1234']) auth(3, 'read_config', { env: { AUTOROUTER_AUTH_MODE: 'subscription', AUTOROUTER_UPSTREAM_URL: upstream } });
envCase(4, { AUTOROUTER_AUTH_MODE: 'subscription' }, { ANTHROPIC_API_KEY: 'old-api-key', ANTHROPIC_AUTH_TOKEN: 'old-bearer', CLAUDE_CODE_OAUTH_TOKEN: 'old-setup-token', TYPESAFE_API_KEY: 'jev-secret', AUTOROUTER_TOKEN: 'old-router-token', ANTHROPIC_CUSTOM_HEADERS: 'X-Team: coding\r\nAuthorization: Bearer stale\r\nx-API-key: stale\r\nX-Autorouter-Token: stale', CLAUDE_CONFIG_DIR: '/custom/claude', PATH: '/usr/bin' });
envCase(5, {}, { ANTHROPIC_API_KEY: 'upstream-secret', TYPESAFE_API_KEY: 'jev-secret' });
for (const profile of ['compatible', 'native']) envCase(6, { AUTOROUTER_CLIENT_PROFILE: profile }, { ANTHROPIC_MODEL: 'claude-opus-5-5', MAX_THINKING_TOKENS: '10000' });
auth(6, 'read_config', { env: { AUTOROUTER_CLIENT_PROFILE: 'unknown' } });
for (const profile of profiles) for (const args of [[], ['--permission-mode', 'auto'], ['--permission-mode=auto'], ['--permission-mode', 'auto', '--permission-mode=plan'], ['--permission-mode=manual', '--permission-mode', 'auto'], ['--', '--permission-mode', 'auto'], ['--permission-mode', 'auto', '--', '--permission-mode=manual'], ['--settings', '{"permissions":{"defaultMode":"auto"}}'], ['--permission-mode', 'Auto']]) auth(7, 'client_profile', { profile, args });
for (const mode of ['subscription', 'api-key']) for (const parent of [{}, { ANTHROPIC_MODEL: 'claude-opus-5-5', MAX_THINKING_TOKENS: '10000', CLAUDE_CODE_AUTO_MODE_SERVER: '0', CLAUDE_CONFIG_DIR: '/synthetic/managed-claude' }]) envCase(8, { AUTOROUTER_AUTH_MODE: mode, AUTOROUTER_CLIENT_PROFILE: 'auto' }, parent);
for (const profile of profiles) for (const value of [undefined, 'true', 'false', 'auto', 'auto:5', '']) envCase(9, { AUTOROUTER_CLIENT_PROFILE: profile }, value === undefined ? {} : { ENABLE_TOOL_SEARCH: value });
for (const mode of ['subscription', 'api-key']) for (const profile of profiles) for (const cap of [undefined, 0, 2]) for (const explicit of [undefined, '0', '7']) envCase(10, { AUTOROUTER_AUTH_MODE: mode, AUTOROUTER_CLIENT_PROFILE: profile, ...(cap === undefined ? {} : { CLAUDE_CODE_STOP_HOOK_BLOCK_CAP: cap }) }, { CLAUDE_CODE_GOAL_CHECKIN_MINUTES: '0', CLAUDE_CONFIG_DIR: '/test/claude', UNRELATED_SETTING: 'keep', ANTHROPIC_CUSTOM_HEADERS: 'X-Team: coding', ...(explicit === undefined ? {} : { CLAUDE_CODE_STOP_HOOK_BLOCK_CAP: explicit }) });
const valid = { authorization: 'Bearer fake-login-token', 'anthropic-beta': 'some-beta, oauth-2025-04-20, future-beta' };
for (const headers of [valid, {}, { authorization: valid.authorization }, { ...valid, 'x-api-key': 'api-key' }, { ...valid, authorization: 'Basic abc' }, { ...valid, authorization: 'Bearer ' }, { ...valid, 'anthropic-beta': 'not-oauth-2025-04-20' }]) auth(11, 'subscription_request', headers);
const policy = { allowed_evaluators: ['ollama'], allowed_auth_modes: ['subscription'], session_log_mode: 'metadata', upstream_url: 'https://api.anthropic.com' };
for (const input of [{ env: { AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_AUTH_MODE: 'subscription' }, policy }, { env: {}, policy }, { env: { AUTOROUTER_AUTH_MODE: 'subscription' }, policy: { allowed_evaluators: ['jev'] } }, { env: { AUTOROUTER_AUTH_MODE: 'subscription', AUTOROUTER_SESSION_LOG_MODE: 'prompts', AUTOROUTER_UPSTREAM_URL: 'https://evil.example', KEEP: '1' }, policy }, { env: { AUTOROUTER_EVALUATOR: 'jev' }, policy, allowlists: false }]) emit('policy', 4, 'apply_policy', input);
// Additive URL-value boundary cases; these are API coercion regressions, not
// additional executions or inferred assertions of any original test definition.
// Append immediately before destination/writeFileSync in generate-config-auth.
const urlBoundary = (key, label, raw, extra = {}) => {
  const backend = key === 'AUTOROUTER_JEV_URL' ? 'jev' : 'ollama';
  const env = { AUTOROUTER_EVALUATOR: backend, ...(raw === undefined ? {} : { [key]: raw }), ...(extra.env ?? {}) };
  cases.push({ id: `config-url-value-${key.slice(11).toLowerCase()}-${label}`, op: 'read_config',
    input: { env, ...(extra.validate_all === undefined ? {} : { validate_all: extra.validate_all }) },
    source_tests: ['test/config.test.mjs'],
    source_basis: 'Additional frozen readConfig JSON-value API boundary; no original definition or assertion attribution' });
};
for (const key of ['AUTOROUTER_UPSTREAM_URL', 'AUTOROUTER_JEV_URL', 'AUTOROUTER_OLLAMA_URL']) {
  const valid = key === 'AUTOROUTER_OLLAMA_URL' ? 'http://localhost:11434/' : 'https://example.test/v1/systemone/';
  const shared = [
    ['absent', undefined], ['null-default', null], ['valid-string', valid],
    ['empty-string', ''], ['empty-array', []], ['null-array', [null]],
    ['singleton-empty-array', [[]]], ['singleton-valid', [valid]],
    ['nested-singleton-valid', [[[valid]]]], ['singleton-null-default-is-not-null', [[null]]],
    ['null-leading', [null, valid]], ['null-trailing', [valid, null]],
    ['nested-null-trailing', [valid, [null]]], ['empty-array-trailing', [valid, []]],
    ['https-public', ['https://example.test/']],
    ['http-public', ['http://example.test/']],
    ['http-ipv4-loopback', ['http://127.0.0.1:11434/']],
    ['http-ipv6-loopback', ['http://[::1]:11434/']],
    ['credential', ['https://synthetic-user:synthetic-pass@example.test/']],
    ['loopback-credential', ['http://synthetic-user:synthetic-pass@127.0.0.1:11434/']],
    ['query', ['http://127.0.0.1:11434/?synthetic-query']],
    ['fragment', ['http://127.0.0.1:11434/#synthetic-fragment']],
    ['path', ['http://127.0.0.1:11434/path']],
    ['bare-query', ['http://127.0.0.1:11434/?']],
    ['bare-fragment', ['http://127.0.0.1:11434/#']],
    ['false-scalar', false], ['number-scalar', 1e20], ['plain-object', {}],
    ['object-tostring-null', { toString: null }],
    ['object-valueof-null', { valueOf: null }],
    ['object-both-null', { toString: null, valueOf: null }],
    ['object-tostring-url-is-noncallable', { toString: valid }],
    ['path-object-default', ['https://example.test/path', {}]],
    ['path-object-tostring-null', ['https://example.test/path', { toString: null }]],
    ['path-object-valueof-null', ['https://example.test/path', { valueOf: null }]],
    ['path-object-both-null', ['https://example.test/path', { toString: null, valueOf: null }]],
    ['path-object-tostring-array', ['https://example.test/path', { toString: [] }]],
    ['path-array-tostring-word', ['https://example.test/path', { toString: 'synthetic' }]],
    ['path-negative-zero', ['https://example.test/path', -0]],
    ['path-small-number', ['https://example.test/path', 1e-7]],
    ['path-large-decimal-number', ['https://example.test/path', 1e20]],
    ['path-large-exponent-number', ['https://example.test/path', 1e21]],
    ['path-number-threshold', ['https://example.test/path', 1e-6]],
    ['path-nested-number-array', ['https://example.test/path', [1e-7, 1e20, 1e21, null]]],
    ['path-small-number-string', ['https://example.test/path', '1e-7']],
    ['path-large-decimal-string', ['https://example.test/path', '100000000000000000000']],
    ['path-large-exponent-string', ['https://example.test/path', '1e+21']],
  ];
  for (const [label, raw] of shared) urlBoundary(key, label, raw);
}
for (const key of ['AUTOROUTER_JEV_URL', 'AUTOROUTER_OLLAMA_URL']) {
  const inactive = key === 'AUTOROUTER_JEV_URL' ? 'ollama' : 'jev';
  const valid = key === 'AUTOROUTER_OLLAMA_URL' ? ['http://localhost:11434/'] : ['https://example.test/v1/systemone/'];
  for (const [label, raw] of [['valid-array', valid], ['invalid-url-array', ['not-a-url']],
    ['unsafe-array', ['http://example.test/']], ['throwing-object-path', ['https://example.test/path', { toString: null }]]]) {
    for (const validate_all of [false, true]) urlBoundary(key, `inactive-${label}-all-${validate_all}`, raw,
      { env: { AUTOROUTER_EVALUATOR: inactive }, validate_all });
  }
}
for (const [label, raw] of [
  ['official-string', 'https://api.anthropic.com/'], ['official-singleton', ['https://api.anthropic.com/']],
  ['official-nested', [[['https://API.ANTHROPIC.COM:443/']]]],
  ['foreign-array', ['https://example.test/']], ['path-array', ['https://api.anthropic.com/path']],
  ['credential-array', ['https://synthetic-user:synthetic-pass@api.anthropic.com/']],
  ['query-array', ['https://api.anthropic.com/?synthetic-query']],
  ['fragment-array', ['https://api.anthropic.com/#synthetic-fragment']],
  ['null-default', null], ['absent-default', undefined],
]) urlBoundary('AUTOROUTER_UPSTREAM_URL', `subscription-${label}`, raw,
  { env: { AUTOROUTER_AUTH_MODE: 'subscription' } });

// Additive credential-value API controls, appended after the existing 627 cases.
// These preserve the retained public read_config controls and do not attribute
// new assertions to any original test definition. The 16 build_claude_env cases
// stay separate: 10 still expose genuine pending OS credential coercion gaps.
const credentialControl = (id, input, control_group) => cases.push({ id, op: 'read_config', input,
  source_tests: ['test/config.test.mjs', 'test/auth.test.mjs'],
  source_basis: 'Additional public credential-value API control; no original assertion attribution', control_group });
const credentialValueText = 'synthetic-credential-value-123456789';
const credentialBaseEnv = { ANTHROPIC_API_KEY: 'synthetic-anthropic-valid',
  TYPESAFE_API_KEY: 'synthetic-jev-valid', AUTOROUTER_TOKEN: 'synthetic-local-token-123456789' };
const credentialValues = [
  ['absent', undefined], ['null', null], ['string', credentialValueText],
  ['empty-string', ''], ['blank-string', ' \t\n'], ['false', false],
  ['zero', 0], ['number', 42], ['true', true], ['array', [credentialValueText]],
  ['empty-array', []], ['nested-array', [[credentialValueText]]],
  ['object', { synthetic: 'credential-input' }],
  ['object-trim-null', { trim: null, synthetic: 'credential-input' }],
];
for (const key of ['ANTHROPIC_API_KEY', 'TYPESAFE_API_KEY', 'AUTOROUTER_TOKEN']) {
  for (const mode of ['api-key', 'subscription']) for (const evaluator of ['jev', 'ollama']) {
    for (const [label, raw] of credentialValues) {
      const env = { ...credentialBaseEnv, AUTOROUTER_AUTH_MODE: mode, AUTOROUTER_EVALUATOR: evaluator };
      if (raw === undefined) delete env[key]; else env[key] = raw;
      for (const require_keys of [false, true]) credentialControl(
        `credential-config-${key.toLowerCase()}-${mode}-${evaluator}-${label}-${require_keys ? 'require' : 'projection'}`,
        { env, require_keys }, 'three-credential-context-matrix');
    }
  }
}
const credentialOrderCases = [
  ['both-absent', {}],
  ['jev-null-anthropic-false', { TYPESAFE_API_KEY: null, ANTHROPIC_API_KEY: false }],
  ['jev-false-anthropic-valid', { TYPESAFE_API_KEY: false, ANTHROPIC_API_KEY: 'synthetic-anthropic-valid' }],
  ['jev-valid-anthropic-false', { TYPESAFE_API_KEY: 'synthetic-jev-valid', ANTHROPIC_API_KEY: false }],
  ['jev-empty-anthropic-false', { TYPESAFE_API_KEY: '', ANTHROPIC_API_KEY: false }],
  ['jev-false-anthropic-empty', { TYPESAFE_API_KEY: false, ANTHROPIC_API_KEY: '' }],
  ['both-valid', { TYPESAFE_API_KEY: 'synthetic-jev-valid', ANTHROPIC_API_KEY: 'synthetic-anthropic-valid' }],
  ['inactive-jev-false', { TYPESAFE_API_KEY: false, ANTHROPIC_API_KEY: 'synthetic-anthropic-valid' }, 'ollama'],
  ['subscription-jev-false-anthropic-false', { TYPESAFE_API_KEY: false, ANTHROPIC_API_KEY: false }, 'jev', 'subscription'],
  ['subscription-ollama-both-false', { TYPESAFE_API_KEY: false, ANTHROPIC_API_KEY: false }, 'ollama', 'subscription'],
];
for (const [label, raw, evaluator = 'jev', mode = 'api-key'] of credentialOrderCases) {
  const env = { ...raw, AUTOROUTER_EVALUATOR: evaluator, AUTOROUTER_AUTH_MODE: mode };
  for (const require_keys of [false, true]) credentialControl(
    `credential-order-${label}-${require_keys ? 'require' : 'projection'}`,
    { env, require_keys }, 'active-requirement-order');
}
for (const mode of ['api-key', 'subscription']) for (const validate_all of [false, true]) {
  const env = { ...credentialBaseEnv, TYPESAFE_API_KEY: false,
    AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_AUTH_MODE: mode };
  credentialControl(`credential-validation-mask-${mode}-${validate_all}`,
    { env, validate_all, require_keys: true }, 'validate-all-is-not-require-inactive');
}

const destination = process.argv[2] ?? resolve(import.meta.dirname, 'cases/config-auth.jsonl');
writeFileSync(destination, cases.map(value => JSON.stringify(value)).join('\n') + '\n');
console.log(`Wrote ${cases.length} config/auth/policy cases`);
