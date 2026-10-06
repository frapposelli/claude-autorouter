import test from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { applyPolicy, defaultPolicyPath, loadPolicy } from '../src/policy.mjs';
import { loadUserConfig, saveUserConfig } from '../src/user-config.mjs';
import { configCommand } from '../src/config-command.mjs';
import { setup } from '../src/onboarding.mjs';
import { readConfig } from '../src/config.mjs';

const uid = process.getuid();

// The production path requires root ownership. Tests treat their own user as
// the trusted owner so no privileges are needed.
function fixture(t, policy, { mode = 0o644, directoryMode = 0o755 } = {}) {
  const root = mkdtempSync(join(tmpdir(), 'autorouter-policy-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const directory = join(root, 'etc');
  mkdirSync(directory, { mode: directoryMode });
  chmodSync(directory, directoryMode);
  const path = join(directory, 'policy.json');
  if (policy !== undefined) {
    writeFileSync(path, typeof policy === 'string' ? policy : JSON.stringify(policy), { mode });
    chmodSync(path, mode);
  }
  return { root, directory, path, options: { path, trustedUid: uid } };
}

test('the policy path is fixed per platform and a missing file means no policy', t => {
  assert.equal(defaultPolicyPath('darwin'), '/Library/Application Support/claude-autorouter/policy.json');
  assert.equal(defaultPolicyPath('linux'), '/etc/claude-autorouter/policy.json');
  assert.equal(loadPolicy(fixture(t).options), undefined);
});

test('a trusted, valid policy is loaded', t => {
  const f = fixture(t, { allowed_evaluators: ['ollama', 'ollama'], session_log_mode: 'metadata', upstream_url: 'https://api.anthropic.com' });
  assert.deepEqual(loadPolicy(f.options).values, { allowed_evaluators: ['ollama'], session_log_mode: 'metadata', upstream_url: 'https://api.anthropic.com' });
});

test('policy fails closed on untrusted, malformed or unknown content without echoing it', t => {
  const bad = [
    ['group-writable file', fixture(t, {}, { mode: 0o664 })],
    ['world-writable directory', fixture(t, {}, { directoryMode: 0o777 })],
    ['not valid json', fixture(t, 'PRIVATE_POLICY_TEXT')],
    ['not an object', fixture(t, '[]')],
    ['unknown key', fixture(t, { PRIVATE_KEY_NAME: 1 })],
    ['empty allowlist', fixture(t, { allowed_evaluators: [] })],
    ['unknown allowlist value', fixture(t, { allowed_evaluators: ['PRIVATE_VALUE'] })],
    ['bad log mode', fixture(t, { session_log_mode: 'PRIVATE_VALUE' })],
    ['bad url', fixture(t, { upstream_url: 'PRIVATE_VALUE' })],
  ];
  for (const [name, f] of bad) {
    assert.throws(() => loadPolicy(f.options), error => error.code === 'AUTOROUTER_CONFIG_ERROR' && !/PRIVATE_/.test(error.message), name);
  }
  const other = fixture(t, {});
  assert.throws(() => loadPolicy({ ...other.options, trustedUid: uid + 1 }), /owned by root/);
  const link = fixture(t);
  writeFileSync(join(link.root, 'target.json'), '{}');
  symlinkSync(join(link.root, 'target.json'), link.path);
  assert.throws(() => loadPolicy(link.options), /regular file/);
});

test('allowlists reject unlisted choices, including defaults, and locks replace supplied values', () => {
  const policy = { allowed_evaluators: ['ollama'], allowed_auth_modes: ['subscription'], session_log_mode: 'metadata', upstream_url: 'https://api.anthropic.com' };
  assert.throws(() => applyPolicy({ AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_AUTH_MODE: 'subscription' }, policy), /AUTOROUTER_EVALUATOR is not permitted/);
  assert.throws(() => applyPolicy({}, policy), /AUTOROUTER_AUTH_MODE is not permitted/, 'The api-key default is not allowed');
  assert.throws(() => applyPolicy({ AUTOROUTER_AUTH_MODE: 'subscription' }, { allowed_evaluators: ['jev'] }), /AUTOROUTER_EVALUATOR/, 'The ollama default is not allowed');
  const env = { AUTOROUTER_AUTH_MODE: 'subscription', AUTOROUTER_SESSION_LOG_MODE: 'prompts', AUTOROUTER_UPSTREAM_URL: 'https://evil.example', KEEP: '1' };
  const applied = applyPolicy(env, policy);
  assert.equal(applied.env.AUTOROUTER_SESSION_LOG_MODE, 'metadata');
  assert.equal(applied.env.AUTOROUTER_UPSTREAM_URL, 'https://api.anthropic.com');
  assert.equal(applied.env.KEEP, '1');
  assert.deepEqual(applied.locked, ['AUTOROUTER_SESSION_LOG_MODE', 'AUTOROUTER_UPSTREAM_URL']);
  assert.equal(env.AUTOROUTER_SESSION_LOG_MODE, 'prompts', 'The caller environment is not mutated');
  assert.doesNotThrow(() => applyPolicy({ AUTOROUTER_EVALUATOR: 'jev' }, policy, { allowlists: false }));
});

test('neither the saved file nor the environment can override a policy lock', t => {
  const f = fixture(t, { allowed_evaluators: ['ollama'], session_log_mode: 'metadata', upstream_url: 'https://api.anthropic.com' });
  const env = { AUTOROUTER_CONFIG: join(f.root, 'config.json') };
  saveUserConfig({ AUTOROUTER_SESSION_LOG_MODE: 'prompts', AUTOROUTER_SESSION_LOG_DIR: join(f.root, 'logs') }, { env });
  const loaded = loadUserConfig({ ...env, AUTOROUTER_UPSTREAM_URL: 'https://proxy.example', AUTOROUTER_SESSION_LOG_MODE: 'prompts' }, { policy: f.options });
  const config = readConfig(loaded.env);
  assert.equal(config.sessionLogMode, 'metadata');
  assert.equal(config.upstream, 'https://api.anthropic.com');
  assert.deepEqual(loaded.policyLocked, ['AUTOROUTER_SESSION_LOG_MODE', 'AUTOROUTER_UPSTREAM_URL']);
  assert.throws(() => loadUserConfig({ ...env, AUTOROUTER_EVALUATOR: 'jev' }, { policy: f.options }), /AUTOROUTER_EVALUATOR is not permitted/);
  assert.equal(loadUserConfig({ ...env, AUTOROUTER_EVALUATOR: 'jev' }, { policy: f.options, enforcePolicy: false }).env.AUTOROUTER_EVALUATOR, 'jev');
});

test('config show reports policy-locked settings and set refuses disallowed values', async t => {
  const f = fixture(t, { allowed_evaluators: ['ollama'], session_log_mode: 'metadata' });
  const env = { AUTOROUTER_CONFIG: join(f.root, 'config.json') };
  saveUserConfig({ AUTOROUTER_EVALUATOR: 'ollama' }, { env });
  const lines = [];
  assert.equal(await configCommand(['show', '--json'], { env, write: line => lines.push(line), policy: f.options }), true);
  const report = JSON.parse(lines.join('\n'));
  assert.equal(report.settings.AUTOROUTER_SESSION_LOG_MODE.source, 'policy');
  assert.equal(report.settings.AUTOROUTER_SESSION_LOG_MODE.value, 'metadata');
  assert.equal(report.policy_path, f.path);
  const before = readFileSync(env.AUTOROUTER_CONFIG, 'utf8');
  await assert.rejects(configCommand(['set', 'AUTOROUTER_EVALUATOR', 'jev'], { env, write() {}, policy: f.options }), /not permitted/);
  assert.equal(readFileSync(env.AUTOROUTER_CONFIG, 'utf8'), before);
  assert.equal(await configCommand(['set', 'AUTOROUTER_PORT', '9001'], { env, write() {}, policy: f.options }), true);
});

test('a disallowed saved evaluator can still be repaired, and setup refuses to save one', async t => {
  const f = fixture(t, { allowed_evaluators: ['ollama'] });
  const env = { AUTOROUTER_CONFIG: join(f.root, 'config.json') };
  saveUserConfig({ AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: 'synthetic-jev-key' }, { env });
  assert.throws(() => loadUserConfig(env, { policy: f.options }), /not permitted/, 'Launch refuses the disallowed setting');
  assert.equal(await configCommand(['set', 'AUTOROUTER_EVALUATOR', 'ollama'], { env, write() {}, policy: f.options }), true);
  assert.equal(loadUserConfig(env, { policy: f.options }).env.AUTOROUTER_EVALUATOR, 'ollama');
  await assert.rejects(setup(['--evaluator', 'jev', '--force'], {
    env: { ...env, TYPESAFE_API_KEY: 'synthetic-jev-key' }, write() {}, prompt: () => assert.fail('No prompt'), policy: f.options,
  }), /not permitted/);
});
