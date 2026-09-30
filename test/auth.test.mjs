import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { mkdtemp, readFile, writeFile, rm, access } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, delimiter, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { CLIENT_PROFILES, readConfig, requireKeys } from '../src/config.mjs';
import { buildClaudeEnv, clientProfileForLaunch, conflictingProviders, isSubscriptionRequest } from '../src/auth.mjs';
import { listen } from '../src/server.mjs';

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

test('explicit Auto permission arguments select the routing profile without changing Claude arguments', () => {
  const cases = [
    [[], false],
    [['--permission-mode', 'auto'], true],
    [['--permission-mode=auto'], true],
    [['--permission-mode', 'auto', '--permission-mode=plan'], false],
    [['--permission-mode=manual', '--permission-mode', 'auto'], true],
    [['--', '--permission-mode', 'auto'], false],
    [['--permission-mode', 'auto', '--', '--permission-mode=manual'], true],
    [['--settings', '{"permissions":{"defaultMode":"auto"}}'], false],
    [['--permission-mode', 'Auto'], false],
  ];
  for (const profile of CLIENT_PROFILES) {
    for (const [args, explicitAuto] of cases) {
      const before = [...args];
      assert.equal(clientProfileForLaunch(profile, Object.freeze(args)), explicitAuto ? 'auto' : profile);
      assert.deepEqual(args, before);
    }
  }
});

test('Auto profile defaults to Sonnet without changing explicit model, thinking, or permission controls', () => {
  for (const authMode of ['subscription', 'api-key']) {
    const config = { ...readConfig({ AUTOROUTER_AUTH_MODE: authMode, AUTOROUTER_CLIENT_PROFILE: 'auto' }), localToken };
    const defaults = buildClaudeEnv(config, 'http://127.0.0.1:1234', {});
    assert.equal(defaults.ANTHROPIC_MODEL, config.models.sonnet);
    assert.equal(defaults.MAX_THINKING_TOKENS, undefined);
    for (const key of ['CLAUDE_CODE_ENABLE_AUTO_MODE', 'CLAUDE_CODE_AUTO_MODE_SERVER', 'CLAUDE_CODE_AUTO_MODE_MODEL']) {
      assert.equal(Object.hasOwn(defaults, key), false, `${key} must remain Claude's decision`);
    }
    const parent = {
      ANTHROPIC_MODEL: config.models.opus, MAX_THINKING_TOKENS: '10000',
      CLAUDE_CODE_AUTO_MODE_SERVER: '0', CLAUDE_CONFIG_DIR: '/synthetic/managed-claude',
    };
    const before = structuredClone(parent);
    const child = buildClaudeEnv(config, 'http://127.0.0.1:1234', parent);
    for (const [key, value] of Object.entries(parent)) assert.equal(child[key], value);
    assert.deepEqual(parent, before);
  }
});

test('all client profiles enable deferred MCP tools through the proxy while preserving explicit choices', () => {
  for (const clientProfile of CLIENT_PROFILES) {
    const config = { ...readConfig({}), localToken, clientProfile };
    const parent = {};
    assert.equal(buildClaudeEnv(config, 'http://127.0.0.1:1234', parent).ENABLE_TOOL_SEARCH, 'true');
    assert.deepEqual(parent, {});
    for (const value of ['true', 'false', 'auto', 'auto:5', '']) {
      assert.equal(buildClaudeEnv(config, 'http://127.0.0.1:1234', { ENABLE_TOOL_SEARCH: value }).ENABLE_TOOL_SEARCH, value);
    }
  }
});

test('native Stop-hook block cap is injected only when configured and preserves explicit child settings', () => {
  const key = 'CLAUDE_CODE_STOP_HOOK_BLOCK_CAP';
  for (const authMode of ['subscription', 'api-key']) {
    for (const clientProfile of CLIENT_PROFILES) {
      const config = { ...readConfig({ AUTOROUTER_AUTH_MODE: authMode, AUTOROUTER_CLIENT_PROFILE: clientProfile }), localToken };
      const parent = { CLAUDE_CODE_GOAL_CHECKIN_MINUTES: '0', CLAUDE_CONFIG_DIR: '/test/claude',
        UNRELATED_SETTING: 'keep', ANTHROPIC_CUSTOM_HEADERS: 'X-Team: coding' };
      const before = structuredClone(parent);
      assert.equal(Object.hasOwn(buildClaudeEnv(config, 'http://127.0.0.1:1234', parent), key), false);
      for (const cap of [0, 2]) {
        const child = buildClaudeEnv({ ...config, stopHookBlockCap: cap }, 'http://127.0.0.1:1234', parent);
        assert.equal(child[key], String(cap));
        for (const setting of ['CLAUDE_CODE_GOAL_CHECKIN_MINUTES', 'CLAUDE_CONFIG_DIR', 'UNRELATED_SETTING']) {
          assert.equal(child[setting], parent[setting]);
        }
        assert.match(child.ANTHROPIC_CUSTOM_HEADERS, /X-Team: coding/);
        for (const explicit of ['0', '7']) {
          const explicitParent = { ...parent, [key]: explicit };
          assert.equal(buildClaudeEnv({ ...config, stopHookBlockCap: cap }, 'http://127.0.0.1:1234', explicitParent)[key], explicit);
          assert.equal(explicitParent[key], explicit);
        }
      }
      assert.deepEqual(parent, before);
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

test('explicit Auto launch configures real routing and preserves permission arguments and settings', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'autorouter-auto-launch-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const configPath = join(dir, 'autorouter-config.json');
  await writeFile(configPath, JSON.stringify({ AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_CLIENT_PROFILE: 'compatible' }), { mode: 0o600 });
  const suppliedSettings = JSON.stringify({ permissions: { disableAutoMode: 'disable', deny: ['Bash(rm *)'] } });
  const settingsPath = join(dir, 'settings.json');
  await writeFile(settingsPath, suppliedSettings, { mode: 0o600 });
  const classifications = [];
  const generations = [];
  const jev = http.createServer(async (req, res) => {
    let body = ''; for await (const chunk of req) body += chunk;
    classifications.push(JSON.parse(body));
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ answers: { tier: { choice: 'haiku', confidence: 0.99 } } }));
  });
  const upstream = http.createServer(async (req, res) => {
    let body = ''; for await (const chunk of req) body += chunk;
    generations.push(JSON.parse(body));
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ model: generations.at(-1).model, content: [{ type: 'text', text: 'Synthetic reply' }], stop_reason: 'end_turn' }));
  });
  t.after(() => {
    for (const service of [jev, upstream]) { service.closeAllConnections(); service.close(); }
  });
  const jevAddress = await listen(jev, 0);
  const upstreamAddress = await listen(upstream, 0);
  await writeFile(join(dir, 'claude'), `#!/usr/bin/env node
const assert = require('node:assert/strict');
const fs = require('node:fs');
(async () => {
  assert.deepEqual(process.argv.slice(2), JSON.parse(process.env.EXPECTED_ARGS));
  assert.equal(process.env.ANTHROPIC_MODEL, process.env.EXPECTED_CLIENT_MODEL);
  assert.equal(process.env.MAX_THINKING_TOKENS, process.env.EXPECTED_THINKING);
  assert.equal(process.env.CLAUDE_CODE_GATEWAY_HINT_HEADERS, '1');
  for (const key of ['CLAUDE_CODE_ENABLE_AUTO_MODE', 'CLAUDE_CODE_AUTO_MODE_SERVER', 'CLAUDE_CODE_AUTO_MODE_MODEL']) {
    assert.equal(process.env[key], undefined);
  }
  assert.equal(fs.readFileSync(process.env.CLAUDE_CONFIG_DIR + '/settings.json', 'utf8'), process.env.EXPECTED_SETTINGS);
  const response = await fetch(process.env.ANTHROPIC_BASE_URL + '/v1/messages', {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'x-api-key': process.env.ANTHROPIC_API_KEY,
      'x-claude-code-session-id': 'synthetic-auto-launch', 'x-claude-code-request-class': 'main' },
    body: JSON.stringify({ model: process.env.ANTHROPIC_MODEL, max_tokens: 64,
      messages: [{ role: 'user', content: 'Return the length of an empty array.' }] }),
  });
  assert.equal(response.status, 200);
  console.log(JSON.stringify(await response.json()));
})().catch(error => { console.error(error.message); process.exitCode = 1; });
`, { mode: 0o700 });
  const defaults = readConfig({});
  const autoDefaults = readConfig({ AUTOROUTER_CLIENT_PROFILE: 'auto' });
  const cases = [
    { profile: 'compatible', flags: ['--permission-mode', 'auto'], model: autoDefaults.models.sonnet },
    { profile: 'native', flags: ['--permission-mode=auto'], model: autoDefaults.models.sonnet },
    { profile: 'compatible', flags: ['--permission-mode', 'auto', '--permission-mode=manual'], model: defaults.models.haiku, thinking: '0' },
    { profile: 'compatible', flags: ['--', '--permission-mode=auto'], model: defaults.models.haiku, thinking: '0' },
  ];
  for (const { profile, flags, model, thinking } of cases) {
    const args = ['--settings', suppliedSettings, ...flags];
    const { stdout, stderr } = await promisify(execFile)(process.execPath,
      [fileURLToPath(new URL('../bin/autorouter.mjs', import.meta.url)), 'claude', ...args], {
        env: {
          PATH: dir + delimiter + dirname(process.execPath), AUTOROUTER_CONFIG: configPath,
          AUTOROUTER_AUTH_MODE: 'api-key', AUTOROUTER_CLIENT_PROFILE: profile, AUTOROUTER_STATUSLINE: '0',
          ANTHROPIC_API_KEY: 'synthetic-upstream-key', TYPESAFE_API_KEY: 'synthetic-jev-key',
          AUTOROUTER_UPSTREAM_URL: `http://127.0.0.1:${upstreamAddress.port}`,
          AUTOROUTER_JEV_URL: `http://127.0.0.1:${jevAddress.port}/v1/systemone`,
          CLAUDE_CONFIG_DIR: dir, EXPECTED_SETTINGS: suppliedSettings,
          EXPECTED_ARGS: JSON.stringify(args), EXPECTED_CLIENT_MODEL: model,
          ...(thinking === undefined ? {} : { EXPECTED_THINKING: thinking }),
        },
        timeout: 10000,
      });
    assert.equal(stderr, '');
    assert.equal(JSON.parse(stdout).model, model);
    // A plain request has no native feature guard to accidentally mask a
    // missing Auto router profile. Jev actually chose Haiku in every launch.
    assert.deepEqual(generations.at(-1), { model, max_tokens: 64,
      messages: [{ role: 'user', content: 'Return the length of an empty array.' }] });
    assert.equal(await readFile(settingsPath, 'utf8'), suppliedSettings);
  }
  assert.equal(classifications.length, cases.length);
  assert.equal(generations.length, cases.length);
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
