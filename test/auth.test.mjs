import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm, access } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, delimiter, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { readConfig, requireKeys } from '../src/config.mjs';
import { buildClaudeEnv, conflictingProviders, isSubscriptionRequest } from '../src/auth.mjs';

const localToken = 'test-local-token-123456789';

test('alternate Claude backends cannot silently bypass the local router', () => {
  for (const key of ['CLAUDE_CODE_USE_BEDROCK', 'CLAUDE_CODE_USE_VERTEX', 'CLAUDE_CODE_USE_FOUNDRY', 'CLAUDE_CODE_USE_MANTLE', 'CLAUDE_CODE_USE_ANTHROPIC_AWS']) {
    for (const enabled of ['1', 'true', 'TRUE']) assert.deepEqual(conflictingProviders({ [key]: enabled }), [key]);
    for (const disabled of ['0', 'false', '', undefined]) assert.deepEqual(conflictingProviders({ [key]: disabled }), []);
  }
});

test('subscription needs only Jev; API-key mode remains the backward-compatible default', () => {
  const subscription = readConfig({ AUTOROUTER_AUTH_MODE: 'subscription', TYPESAFE_API_KEY: 'test-jev', ANTHROPIC_API_KEY: 'ignored' });
  assert.equal(subscription.anthropicKey, undefined);
  assert.doesNotThrow(() => requireKeys(subscription));
  assert.throws(() => requireKeys(readConfig({ AUTOROUTER_AUTH_MODE: 'subscription' })), /TYPESAFE_API_KEY/);
  const api = readConfig({ TYPESAFE_API_KEY: 'test-jev' });
  assert.equal(api.authMode, 'api-key');
  assert.throws(() => requireKeys(api), /ANTHROPIC_API_KEY/);
  assert.doesNotThrow(() => requireKeys(readConfig({ TYPESAFE_API_KEY: 'test-jev', ANTHROPIC_API_KEY: 'test-api' })));
  assert.throws(() => readConfig({ AUTOROUTER_AUTH_MODE: 'automatic' }), /AUTOROUTER_AUTH_MODE/);
});

test('subscription configuration will not forward login credentials to a custom upstream', () => {
  for (const upstream of ['https://example.com', 'https://api.anthropic.com.example.com', 'https://api.anthropic.com/other', 'http://127.0.0.1:1234']) {
    assert.throws(() => readConfig({ AUTOROUTER_AUTH_MODE: 'subscription', AUTOROUTER_UPSTREAM_URL: upstream }), /Subscription mode requires/);
  }
});

test('subscription launcher uses independent local authentication without mutating parent credentials', () => {
  const parent = {
    ANTHROPIC_API_KEY: 'old-api-key', ANTHROPIC_AUTH_TOKEN: 'old-bearer', CLAUDE_CODE_OAUTH_TOKEN: 'old-setup-token',
    TYPESAFE_API_KEY: 'jev-secret', AUTOROUTER_TOKEN: 'old-router-token',
    ANTHROPIC_CUSTOM_HEADERS: 'X-Team: coding\r\nAuthorization: Bearer stale\r\nx-API-key: stale\r\nX-Autorouter-Token: stale',
    CLAUDE_CONFIG_DIR: '/custom/claude', PATH: '/usr/bin',
  };
  const before = structuredClone(parent);
  const config = { ...readConfig({ AUTOROUTER_AUTH_MODE: 'subscription' }), localToken };
  const env = buildClaudeEnv(config, 'http://127.0.0.1:1234', parent);
  assert.deepEqual(parent, before);
  for (const key of ['ANTHROPIC_API_KEY', 'ANTHROPIC_AUTH_TOKEN', 'CLAUDE_CODE_OAUTH_TOKEN', 'TYPESAFE_API_KEY', 'AUTOROUTER_TOKEN']) assert.equal(env[key], undefined);
  assert.equal(env.ANTHROPIC_CUSTOM_HEADERS, `X-Team: coding\nX-Autorouter-Token: ${localToken}`);
  assert.equal(env.ANTHROPIC_BASE_URL, 'http://127.0.0.1:1234');
  assert.equal(env.CLAUDE_CONFIG_DIR, parent.CLAUDE_CONFIG_DIR);
  assert.equal(env.CLAUDE_CODE_GATEWAY_HINT_HEADERS, '1');
});

test('API-key launcher continues using its temporary credential', () => {
  const env = buildClaudeEnv({ ...readConfig({}), localToken }, 'http://127.0.0.1:1234', { ANTHROPIC_API_KEY: 'upstream-secret', TYPESAFE_API_KEY: 'jev-secret' });
  assert.equal(env.ANTHROPIC_API_KEY, localToken);
  assert.equal(env.ANTHROPIC_AUTH_TOKEN, localToken);
  assert.equal(env.TYPESAFE_API_KEY, undefined);
});

test('compatible profile selects shared client capabilities; native preserves user model and thinking', () => {
  const config = { ...readConfig({}), localToken };
  const parent = { ANTHROPIC_MODEL: 'claude-opus-5-5', MAX_THINKING_TOKENS: '10000' };
  const compatible = buildClaudeEnv(config, 'http://127.0.0.1:1234', parent);
  assert.equal(compatible.ANTHROPIC_MODEL, config.models.haiku);
  assert.equal(compatible.MAX_THINKING_TOKENS, '0');
  const native = buildClaudeEnv({ ...config, clientProfile: 'native' }, 'http://127.0.0.1:1234', parent);
  assert.equal(native.ANTHROPIC_MODEL, parent.ANTHROPIC_MODEL);
  assert.equal(native.MAX_THINKING_TOKENS, parent.MAX_THINKING_TOKENS);
  assert.throws(() => readConfig({ AUTOROUTER_CLIENT_PROFILE: 'unknown' }), /AUTOROUTER_CLIENT_PROFILE/);
});

test('both client profiles enable deferred MCP tools through the proxy while preserving explicit choices', () => {
  for (const clientProfile of ['compatible', 'native']) {
    const config = { ...readConfig({}), localToken, clientProfile };
    const parent = {};
    assert.equal(buildClaudeEnv(config, 'http://127.0.0.1:1234', parent).ENABLE_TOOL_SEARCH, 'true');
    assert.deepEqual(parent, {});
    for (const value of ['true', 'false', 'auto', 'auto:5', '']) {
      assert.equal(buildClaudeEnv(config, 'http://127.0.0.1:1234', { ENABLE_TOOL_SEARCH: value }).ENABLE_TOOL_SEARCH, value);
    }
  }
});

test('subscription request recognition requires bearer and OAuth capability, and excludes API keys', () => {
  const valid = { authorization: 'Bearer fake-login-token', 'anthropic-beta': 'some-beta, oauth-2025-04-20, future-beta' };
  assert.ok(isSubscriptionRequest(valid));
  for (const headers of [{}, { authorization: valid.authorization }, { ...valid, 'x-api-key': 'api-key' }, { ...valid, authorization: 'Basic abc' }, { ...valid, authorization: 'Bearer ' }, { ...valid, 'anthropic-beta': 'not-oauth-2025-04-20' }]) {
    assert.equal(isSubscriptionRequest(headers), false);
  }
});

test('subscription launcher starts and stops its proxy with a fake Claude process, without reading a login', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'autorouter-launcher-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  // CLI startup must never load the developer's saved evaluator or keys.
  const configPath = join(dir, 'autorouter-config.json');
  await writeFile(configPath, JSON.stringify({ AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_JEV_URL: 'http://127.0.0.1:1/v1/systemone' }), { mode: 0o600 });
  await writeFile(join(dir, 'claude'), `#!/usr/bin/env node
const assert = require('node:assert/strict');
const fs = require('node:fs');
(async () => {
  assert.equal(process.env.AUTOROUTER_EVALUATOR, 'jev');
  assert.equal(process.argv[2], '--settings');
  assert.deepEqual(process.argv.slice(4), ['--model', 'sonnet']);
  const settings = JSON.parse(fs.readFileSync(process.argv[3], 'utf8'));
  assert.equal(settings.statusLine.type, 'command');
  assert.equal(settings.statusLine.refreshInterval, 1);
  assert.match(settings.statusLine.command, /statusline.mjs/);
  const snapshot = JSON.parse(fs.readFileSync(process.env.AUTOROUTER_STATUS_FILE, 'utf8'));
  assert.equal(snapshot.version, 1);
  assert.deepEqual(snapshot.sessions, {});
  for (const key of ['ANTHROPIC_API_KEY', 'ANTHROPIC_AUTH_TOKEN', 'CLAUDE_CODE_OAUTH_TOKEN', 'TYPESAFE_API_KEY']) assert.equal(process.env[key], undefined);
  const [name, token] = process.env.ANTHROPIC_CUSTOM_HEADERS.split(': ');
  assert.equal(name, 'X-Autorouter-Token');
  assert.equal(token.length, 64);
  const response = await fetch(process.env.ANTHROPIC_BASE_URL + '/health', { headers: { [name]: token } });
  assert.equal(response.status, 200);
  console.log(JSON.stringify({ baseUrl: process.env.ANTHROPIC_BASE_URL, statusPath: process.env.AUTOROUTER_STATUS_FILE, settingsPath: process.argv[3] }));
})().catch(() => { console.error('Fake Claude validation failed'); process.exitCode = 1; });
`, { mode: 0o700 });
  const { stdout, stderr } = await promisify(execFile)(process.execPath, [fileURLToPath(new URL('../bin/autorouter.mjs', import.meta.url)), 'claude', '--model', 'sonnet'], {
    env: { PATH: dir + delimiter + dirname(process.execPath), AUTOROUTER_CONFIG: configPath,
      AUTOROUTER_AUTH_MODE: 'subscription', TYPESAFE_API_KEY: 'fake-jev-key', ANTHROPIC_API_KEY: 'must-be-removed' },
    timeout: 10000,
  });
  assert.equal(stderr, '');
  assert.ok(!stderr.includes('must-be-removed'));
  const { baseUrl, statusPath, settingsPath } = JSON.parse(stdout);
  await assert.rejects(access(statusPath));
  await assert.rejects(access(settingsPath));
  await assert.rejects(fetch(baseUrl + '/health', { signal: AbortSignal.timeout(500) }));
});

test('statusline opt-out or setup failure passes original settings and clears inherited router status', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'autorouter-launcher-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const configPath = join(dir, 'autorouter-config.json');
  await writeFile(configPath, JSON.stringify({ AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_JEV_URL: 'http://127.0.0.1:1/v1/systemone' }), { mode: 0o600 });
  await writeFile(join(dir, 'claude'), `#!/usr/bin/env node
require('node:assert/strict').equal(process.env.AUTOROUTER_EVALUATOR, 'jev');
console.log(JSON.stringify({ args: process.argv.slice(2), statusPath: process.env.AUTOROUTER_STATUS_FILE }));
`, { mode: 0o700 });
  for (const [enabled, supplied] of [['0', '{"statusLine":{"type":"command","command":"my-status"}}'], ['1', '{invalid-settings']]) {
    const args = ['--settings', supplied];
    const { stdout, stderr } = await promisify(execFile)(process.execPath, [fileURLToPath(new URL('../bin/autorouter.mjs', import.meta.url)), 'claude', ...args], {
      env: { PATH: dir + delimiter + dirname(process.execPath), AUTOROUTER_CONFIG: configPath,
        AUTOROUTER_AUTH_MODE: 'subscription', TYPESAFE_API_KEY: 'fake', AUTOROUTER_STATUSLINE: enabled, AUTOROUTER_STATUS_FILE: '/stale/other-session.json' },
      timeout: 10000,
    });
    const result = JSON.parse(stdout);
    assert.deepEqual(result.args, args);
    assert.equal(result.statusPath, undefined);
    if (enabled === '1') assert.match(stderr, /status line unavailable/);
  }
});
