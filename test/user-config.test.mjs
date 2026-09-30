import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import {
  chmodSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync,
  rmSync, statSync, symlinkSync, writeFileSync,
} from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { getConfigPath, loadUserConfig, saveUserConfig } from '../src/user-config.mjs';
import { readConfig } from '../src/config.mjs';

function directory(t) {
  const path = mkdtempSync(join(tmpdir(), 'autorouter-user-config-'));
  t.after(() => rmSync(path, { recursive: true, force: true }));
  return path;
}

test('configuration paths use the user directory and support explicit path overrides', t => {
  const base = directory(t);
  assert.equal(getConfigPath({}), join(homedir(), '.config', 'claude-autorouter', 'config.json'));
  assert.equal(getConfigPath({ XDG_CONFIG_HOME: base }), join(base, 'claude-autorouter', 'config.json'));
  assert.equal(getConfigPath({ AUTOROUTER_CONFIG: './chosen-config.json' }), resolve('chosen-config.json'));
  assert.equal(getConfigPath({ AUTOROUTER_CONFIG: join(base, 'custom.json'), XDG_CONFIG_HOME: '/unused' }), join(base, 'custom.json'));
  assert.throws(() => getConfigPath({ AUTOROUTER_CONFIG: '' }), /non-empty path/);
  assert.throws(() => getConfigPath({ XDG_CONFIG_HOME: './relative' }), /absolute path/);
});

test('a missing default file is optional but an explicitly selected file must exist', t => {
  const base = directory(t);
  const env = { XDG_CONFIG_HOME: base, TYPESAFE_API_KEY: 'environment-key', PATH: '/test-path' };
  const loaded = loadUserConfig(env);
  assert.equal(loaded.exists, false);
  assert.equal(loaded.path, getConfigPath(env));
  assert.deepEqual(loaded.env, env);
  assert.notEqual(loaded.env, env);
  assert.throws(() => loadUserConfig({ AUTOROUTER_CONFIG: join(base, 'missing.json') }), /missing configuration file/);
});

test('environment settings take precedence without mutating the saved file or caller environment', t => {
  const base = directory(t);
  const env = { XDG_CONFIG_HOME: base, TYPESAFE_API_KEY: 'environment-key', AUTOROUTER_PORT: '9000', PATH: '/test-path' };
  const before = { ...env };
  const saved = { TYPESAFE_API_KEY: 'saved-key', AUTOROUTER_AUTH_MODE: 'subscription', AUTOROUTER_PORT: '8787', ENABLE_TOOL_SEARCH: 'auto:5' };
  const path = saveUserConfig(saved, { env });
  const loaded = loadUserConfig(env);
  assert.equal(loaded.exists, true);
  assert.equal(loaded.path, path);
  assert.deepEqual(loaded.env, { ...saved, ...env });
  assert.deepEqual(env, before);
  assert.deepEqual(JSON.parse(readFileSync(path, 'utf8')), saved);
});

test('native Stop-hook block cap persists as a string and runtime environment overrides the saved opt-in', t => {
  const env = { XDG_CONFIG_HOME: directory(t) };
  const key = 'CLAUDE_CODE_STOP_HOOK_BLOCK_CAP';
  const saved = { AUTOROUTER_AUTH_MODE: 'subscription', [key]: '2' };
  const path = saveUserConfig(saved, { env });
  assert.equal(loadUserConfig(env).env[key], '2');
  assert.equal(readConfig(loadUserConfig(env).env).stopHookBlockCap, 2);
  const overridden = { ...env, [key]: '0' };
  const before = structuredClone(overridden);
  assert.equal(readConfig(loadUserConfig(overridden).env).stopHookBlockCap, 0);
  assert.deepEqual(overridden, before);
  assert.deepEqual(JSON.parse(readFileSync(path, 'utf8')), saved);
  assert.throws(() => saveUserConfig({ [key]: 2 }, { env, overwrite: true }), /values must be strings/);
  assert.deepEqual(JSON.parse(readFileSync(path, 'utf8')), saved);
});

test('loading is independent of the current repository and never reads its .env file', t => {
  const base = directory(t);
  const env = { XDG_CONFIG_HOME: join(base, 'user-settings') };
  saveUserConfig({ TYPESAFE_API_KEY: 'user-key' }, { env });
  const moduleUrl = new URL('../src/user-config.mjs', import.meta.url).href;
  for (const name of ['first-repository', 'second-repository']) {
    const cwd = join(base, name);
    mkdirSync(cwd);
    writeFileSync(join(cwd, '.env'), 'TYPESAFE_API_KEY=repository-key\nAUTOROUTER_AUTH_MODE=subscription\n');
    writeFileSync(join(cwd, 'config.json'), '{"TYPESAFE_API_KEY":"repository-key"}');
    const script = `import { loadUserConfig } from ${JSON.stringify(moduleUrl)}; console.log(JSON.stringify(loadUserConfig(${JSON.stringify(env)})));`;
    const loaded = JSON.parse(execFileSync(process.execPath, ['--input-type=module', '-e', script], { cwd, encoding: 'utf8' }));
    assert.equal(loaded.env.TYPESAFE_API_KEY, 'user-key');
    assert.equal(loaded.env.AUTOROUTER_AUTH_MODE, undefined);
    assert.equal(loaded.path, getConfigPath(env));
  }
});

test('unknown keys, non-string values, and malformed JSON fail without exposing their contents', t => {
  const base = directory(t);
  const env = { AUTOROUTER_CONFIG: join(base, 'config.json') };
  const secret = 'TEST_SECRET_MUST_NOT_APPEAR';
  for (const content of [
    `{"${secret}":"unknown"}`,
    `{"TYPESAFE_API_KEY":{"private":"${secret}"}}`,
    `{"TYPESAFE_API_KEY":"${secret}",}`,
    '[]', 'null', '"a string"',
    '{"__proto__":{"polluted":"value"}}',
  ]) {
    writeFileSync(env.AUTOROUTER_CONFIG, content);
    assert.throws(() => loadUserConfig(env), error => {
      assert.equal(error.code, 'AUTOROUTER_CONFIG_ERROR');
      assert.ok(!String(error.stack).includes(secret));
      assert.equal(error.cause, undefined);
      return true;
    });
  }
  const newEnv = { AUTOROUTER_CONFIG: join(base, 'new', 'config.json') };
  for (const values of [{ [secret]: 'value' }, { TYPESAFE_API_KEY: { private: secret } }, [], null]) {
    assert.throws(() => saveUserConfig(values, { env: newEnv }), error => !String(error.stack).includes(secret));
  }
  assert.equal(readdirSync(base).includes('new'), false);
});

test('new files and directories are private while existing parent permissions remain unchanged', t => {
  const base = directory(t);
  chmodSync(base, 0o755);
  const env = { XDG_CONFIG_HOME: join(base, 'new-parent') };
  const path = saveUserConfig({ TYPESAFE_API_KEY: 'saved-key' }, { env });
  if (process.platform !== 'win32') {
    assert.equal(statSync(base).mode & 0o777, 0o755);
    assert.equal(statSync(env.XDG_CONFIG_HOME).mode & 0o777, 0o700);
    assert.equal(statSync(dirname(path)).mode & 0o777, 0o700);
    assert.equal(statSync(path).mode & 0o777, 0o600);
  }
  assert.deepEqual(readdirSync(dirname(path)), ['config.json']);
});

test('saving is exclusive by default and an explicit overwrite atomically replaces the old configuration', t => {
  const base = directory(t);
  const env = { XDG_CONFIG_HOME: base };
  const original = { TYPESAFE_API_KEY: 'original-key', AUTOROUTER_AUTH_MODE: 'subscription' };
  const path = saveUserConfig(original, { env });
  assert.throws(() => saveUserConfig({ TYPESAFE_API_KEY: 'replacement-key' }, { env }), /already exists/);
  assert.deepEqual(JSON.parse(readFileSync(path, 'utf8')), original);
  const oldInode = statSync(path).ino;
  chmodSync(path, 0o644);
  assert.equal(saveUserConfig({ TYPESAFE_API_KEY: 'replacement-key' }, { env, overwrite: true }), path);
  assert.deepEqual(JSON.parse(readFileSync(path, 'utf8')), { TYPESAFE_API_KEY: 'replacement-key' });
  if (process.platform !== 'win32') {
    assert.notEqual(statSync(path).ino, oldInode);
    assert.equal(statSync(path).mode & 0o777, 0o600);
  }
  assert.deepEqual(readdirSync(dirname(path)), ['config.json']);
});

test('writes reject existing and dangling symlinks without altering their targets', t => {
  const base = directory(t);
  const victim = join(base, 'victim.json');
  writeFileSync(victim, 'original contents');
  for (const [name, target] of [['existing', victim], ['dangling', join(base, 'missing-target')]]) {
    const path = join(base, `${name}.json`);
    symlinkSync(target, path);
    for (const overwrite of [false, true]) {
      assert.throws(() => saveUserConfig({ TYPESAFE_API_KEY: 'new-key' }, { env: { AUTOROUTER_CONFIG: path }, overwrite }), /symbolic-link/);
    }
    assert.ok(lstatSync(path).isSymbolicLink());
  }
  assert.equal(readFileSync(victim, 'utf8'), 'original contents');
  assert.equal(readdirSync(base).includes('missing-target'), false);
});

test('filesystem errors do not echo sensitive paths', t => {
  const base = directory(t);
  const secret = 'TEST_SECRET_PATH';
  const notDirectory = join(base, secret);
  writeFileSync(notDirectory, 'file');
  const env = { AUTOROUTER_CONFIG: join(notDirectory, 'config.json') };
  for (const operation of [() => loadUserConfig(env), () => saveUserConfig({ TYPESAFE_API_KEY: 'key' }, { env })]) {
    assert.throws(operation, error => {
      assert.ok(!String(error.stack).includes(secret));
      assert.equal(error.cause, undefined);
      return true;
    });
  }
});
