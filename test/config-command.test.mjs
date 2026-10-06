import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, readFileSync, existsSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Readable } from 'node:stream';
import { configCommand } from '../src/config-command.mjs';
import { saveUserConfig, loadUserConfig } from '../src/user-config.mjs';

function fixture(t) {
  const dir = mkdtempSync(join(tmpdir(), 'autorouter-config-command-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return { AUTOROUTER_CONFIG: join(dir, 'config.json') };
}
const quiet = () => {};

test('show reports default, saved and environment provenance and hides every secret', async t => {
  const env = fixture(t), lines = [];
  saveUserConfig({ AUTOROUTER_AUTH_MODE: 'subscription', AUTOROUTER_PORT: '8123', AUTOROUTER_EVALUATOR: 'jev',
    TYPESAFE_API_KEY: 'private-saved-key', AUTOROUTER_TOKEN: 'private-token-value', ANTHROPIC_API_KEY: 'private-unused-key' }, { env });
  const before = readFileSync(env.AUTOROUTER_CONFIG, 'utf8');
  const options = { env: { ...env, AUTOROUTER_PORT: '9123', TYPESAFE_API_KEY: 'private-environment-key' }, write: line => lines.push(line) };
  assert.equal(await configCommand(['show', '--json'], options), true);
  const report = JSON.parse(lines.join('\n'));
  assert.deepEqual(report.settings.AUTOROUTER_PORT, { source: 'environment', active: true, overrides_file: true, value: 9123 });
  assert.equal(report.settings.AUTOROUTER_AUTH_MODE.source, 'file');
  assert.equal(report.settings.AUTOROUTER_JEV_MODEL.source, 'default');
  assert.deepEqual(report.settings.TYPESAFE_API_KEY, { source: 'environment', active: true, overrides_file: true, secret: true, present: true });
  assert.equal(report.settings.ANTHROPIC_API_KEY.active, false);
  assert.ok(!lines.join('\n').includes('private-'));
  lines.length = 0;
  assert.equal(await configCommand(['show'], options), true);
  assert.match(lines.join('\n'), /TYPESAFE_API_KEY=\[set; hidden\]/);
  assert.ok(!lines.join('\n').includes('private-'));
  assert.equal(readFileSync(env.AUTOROUTER_CONFIG, 'utf8'), before);
});

test('show is read-only without a saved file and full checks report inactive errors safely', async t => {
  const env = fixture(t), lines = [];
  assert.equal(await configCommand(['show', '--json'], { env, write: line => lines.push(line) }), true);
  assert.equal(JSON.parse(lines.pop()).config_exists, false);
  assert.equal(existsSync(env.AUTOROUTER_CONFIG), false);
  saveUserConfig({ AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_JEV_URL: 'https://private-user:private-secret@example.test' }, { env });
  const before = readFileSync(env.AUTOROUTER_CONFIG, 'utf8');
  assert.equal(await configCommand(['show', '--json'], { env, write: line => lines.push(line) }), true);
  const active = JSON.parse(lines.pop());
  assert.deepEqual(active.settings.AUTOROUTER_JEV_URL, { source: 'file', active: false, value: null });
  assert.equal(await configCommand(['show', '--check-all', '--json'], { env, write: line => lines.push(line) }), false);
  const text = lines.pop();
  assert.match(JSON.parse(text).error, /AUTOROUTER_JEV_URL/);
  assert.ok(!text.includes('private-'));
  assert.equal(readFileSync(env.AUTOROUTER_CONFIG, 'utf8'), before);
});

test('set and unset merge only the named setting and environment overrides stay ephemeral', async t => {
  const env = fixture(t), lines = [];
  const saved = { AUTOROUTER_AUTH_MODE: 'subscription', TYPESAFE_API_KEY: 'test-secret', AUTOROUTER_PORT: '8123', ENABLE_TOOL_SEARCH: 'auto:5' };
  saveUserConfig(saved, { env });
  assert.equal(await configCommand(['set', 'AUTOROUTER_PORT', '0'], {
    env: { ...env, AUTOROUTER_PORT: '9123', AUTOROUTER_DEBUG: '1' }, write: line => lines.push(line),
  }), true);
  assert.deepEqual(loadUserConfig(env).values, { ...saved, AUTOROUTER_PORT: '0' });
  assert.match(lines.join('\n'), /environment still overrides AUTOROUTER_PORT/);
  assert.equal(statSync(env.AUTOROUTER_CONFIG).mode & 0o777, 0o600);
  await configCommand(['unset', 'AUTOROUTER_PORT'], { env, write: quiet });
  const { AUTOROUTER_PORT, ...expected } = saved;
  assert.deepEqual(loadUserConfig(env).values, expected);
});

test('secret arguments are rejected before persistence and stdin or hidden entry never echoes them', async t => {
  const env = fixture(t), lines = [], write = line => lines.push(line);
  await assert.rejects(configCommand(['set', 'TYPESAFE_API_KEY', 'private-argv-secret'], {
    env, write, promptSecret: () => assert.fail('Must reject argv first'),
  }), error => !error.message.includes('private-argv-secret') && /not accepted/.test(error.message));
  assert.equal(existsSync(env.AUTOROUTER_CONFIG), false);
  await configCommand(['set', 'TYPESAFE_API_KEY', '--stdin'], { env, write, input: Readable.from(['private-stdin-secret\n']) });
  await configCommand(['set', 'ANTHROPIC_API_KEY'], { env, write, promptSecret: async () => 'private-hidden-secret' });
  assert.deepEqual(loadUserConfig(env).values, { TYPESAFE_API_KEY: 'private-stdin-secret', ANTHROPIC_API_KEY: 'private-hidden-secret' });
  assert.ok(!lines.join('\n').includes('private-'));
  const before = readFileSync(env.AUTOROUTER_CONFIG, 'utf8');
  for (const input of ['a\nb', '', 'x'.repeat(16385)]) {
    await assert.rejects(configCommand(['set', 'TYPESAFE_API_KEY', '--stdin'], { env, write, input: Readable.from([input]) }));
    assert.equal(readFileSync(env.AUTOROUTER_CONFIG, 'utf8'), before);
  }
});

test('invalid edits fail without clobbering saved settings or leaking URL credentials', async t => {
  const env = fixture(t);
  saveUserConfig({ AUTOROUTER_PORT: '8123' }, { env });
  const before = readFileSync(env.AUTOROUTER_CONFIG, 'utf8');
  for (const [key, value] of [['AUTOROUTER_PORT', ''], ['AUTOROUTER_MIN_CONFIDENCE', ' '], ['AUTOROUTER_DEBUG', 'true'],
    ['AUTOROUTER_JEV_URL', 'https://private-user:private-secret@example.test'], ['AUTOROUTER_UPSTREAM_URL', 'private-invalid-url'],
    ['AUTOROUTER_SONNET_MODEL', ''], ['AUTOROUTER_OLLAMA_TIMEOUT_MS', '']]) {
    await assert.rejects(configCommand(['set', key, value], { env, write: quiet }), error => !error.message.includes('private-'));
    assert.equal(readFileSync(env.AUTOROUTER_CONFIG, 'utf8'), before);
  }
  await assert.rejects(configCommand(['set', 'private-unknown-key', 'value'], { env, write: quiet }), error => !error.message.includes('private-unknown-key'));
});

test('cancelled or stale interactive secret edits preserve the current file', async t => {
  const env = fixture(t);
  saveUserConfig({ AUTOROUTER_PORT: '8123' }, { env });
  await assert.rejects(configCommand(['set', 'TYPESAFE_API_KEY'], { env, write: quiet, promptSecret: async () => { throw new Error('Cancelled'); } }), /Cancelled/);
  assert.deepEqual(loadUserConfig(env).values, { AUTOROUTER_PORT: '8123' });
  await assert.rejects(configCommand(['set', 'TYPESAFE_API_KEY'], { env, write: quiet, promptSecret: async () => {
    saveUserConfig({ AUTOROUTER_PORT: '9123' }, { env, overwrite: true });
    return 'private-new-secret';
  } }), /changed while this operation was running/);
  assert.deepEqual(loadUserConfig(env).values, { AUTOROUTER_PORT: '9123' });
});

test('JSON inspection of a malformed file remains machine readable and never includes parser input', async t => {
  const env = fixture(t), lines = [];
  writeFileSync(env.AUTOROUTER_CONFIG, '{"private-malformed-secret":');
  assert.equal(await configCommand(['show', '--json'], { env, write: line => lines.push(line) }), false);
  assert.equal(lines.length, 1);
  assert.equal(JSON.parse(lines[0]).valid, false);
  assert.match(JSON.parse(lines[0]).error, /valid JSON/);
  assert.ok(!lines[0].includes('private-malformed-secret'));
});
