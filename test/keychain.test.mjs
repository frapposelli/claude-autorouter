import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { Readable } from 'node:stream';
import { join } from 'node:path';
import { createKeychain } from '../src/keychain.mjs';
import { keychainRemovals, loadUserConfig, saveUserConfig } from '../src/user-config.mjs';
import { configCommand } from '../src/config-command.mjs';
import { setup, doctor } from '../src/onboarding.mjs';
import { sessionsCommand } from '../src/session-history.mjs';

function fixture(t) {
  const dir = mkdtempSync(join(tmpdir(), 'autorouter-keychain-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return { AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_CONFIG: join(dir, 'config.json') };
}
const saved = env => JSON.parse(readFileSync(env.AUTOROUTER_CONFIG, 'utf8'));

// In-memory keychain with the same interface as createKeychain().
function memoryKeychain({ locked = false } = {}) {
  const items = new Map();
  return {
    items, available: true, locked,
    read(account) { if (this.locked) throw Object.assign(new Error('The macOS Keychain is locked.'), { code: 'AUTOROUTER_CONFIG_ERROR' }); return items.get(account); },
    write(account, value) { if (this.locked) throw Object.assign(new Error('The macOS Keychain is locked.'), { code: 'AUTOROUTER_CONFIG_ERROR' }); items.set(account, value); },
    remove(account) { items.delete(account); },
  };
}
const valuesIn = keychain => [...keychain.items.values()];

test('the security tool receives secrets on stdin only and reads them back to confirm the save', () => {
  const calls = [];
  const stored = new Map();
  const run = (command, args, options) => {
    calls.push({ command, args, input: options.input });
    if (args[0] === '-i') {
      const match = /-a "([^"]+)" -l "[^"]*" -w "((?:[^"\\]|\\.)*)"/.exec(options.input);
      stored.set(match[1], match[2].replace(/\\(.)/g, '$1'));
      return { status: 0, stdout: '' };
    }
    if (args[0] === 'find-generic-password') {
      const account = args[args.indexOf('-a') + 1];
      return stored.has(account) ? { status: 0, stdout: `${stored.get(account)}\n` } : { status: 44, stdout: '' };
    }
    stored.delete(args[args.indexOf('-a') + 1]);
    return { status: 0, stdout: 'attributes' };
  };
  const keychain = createKeychain({ run, platform: 'darwin' });
  const secret = `quote" back\\slash $HOME 'single'`;
  keychain.write('KEY:abc', secret, 'AutoRouter KEY');
  assert.equal(keychain.read('KEY:abc'), secret);
  assert.equal(keychain.read('MISSING'), undefined);
  keychain.remove('KEY:abc');
  keychain.remove('KEY:abc');
  assert.ok(calls.every(call => call.command === '/usr/bin/security'));
  assert.ok(calls.every(call => !call.args.join(' ').includes('back\\slash')), 'a secret must never be a process argument');
  assert.match(calls[0].input, /-w "quote\\" back\\\\slash \$HOME 'single'"\n$/);
});

test('keychain failures are generic, and invalid or unsupported use is rejected', () => {
  const run = () => ({ status: 51, stdout: 'private-tool-output', stderr: 'private-tool-error' });
  const keychain = createKeychain({ run, platform: 'darwin' });
  for (const action of [() => keychain.read('KEY'), () => keychain.remove('KEY'), () => keychain.write('KEY', 'value', 'label')]) {
    assert.throws(action, error => /macOS Keychain/.test(error.message) && !/private-tool/.test(error.message));
  }
  // Interactive mode exits successfully even when its command fails.
  const silent = createKeychain({ run: (_command, args) => ({ status: args[0] === '-i' ? 0 : 44, stdout: '' }), platform: 'darwin' });
  assert.throws(() => silent.write('KEY', 'value', 'label'), /Could not save/);
  for (const value of ['', 'two\nlines', 'non-ascii-é']) {
    assert.throws(() => keychain.write('KEY', value, 'label'), /printable single-line ASCII/);
  }
  assert.throws(() => createKeychain({ run, platform: 'linux' }).read('KEY'), /only on macOS/);
  assert.equal(createKeychain({ run, platform: 'linux' }).available, false);
});

test('the keychain store keeps secrets out of the file and loads them back', t => {
  const env = fixture(t), keychain = memoryKeychain();
  saveUserConfig({ AUTOROUTER_SECRET_STORE: 'keychain', AUTOROUTER_AUTH_MODE: 'api-key',
    ANTHROPIC_API_KEY: 'private-anthropic', TYPESAFE_API_KEY: 'private-jev' }, { env, keychain });
  assert.deepEqual(saved(env), { AUTOROUTER_SECRET_STORE: 'keychain', AUTOROUTER_AUTH_MODE: 'api-key' });
  assert.ok(!readFileSync(env.AUTOROUTER_CONFIG, 'utf8').includes('private-'));
  assert.equal(statSync(env.AUTOROUTER_CONFIG).mode & 0o777, 0o600);
  assert.deepEqual(valuesIn(keychain).sort(), ['private-anthropic', 'private-jev']);
  const loaded = loadUserConfig(env, { keychain });
  assert.equal(loaded.env.ANTHROPIC_API_KEY, 'private-anthropic');
  assert.equal(loaded.values.TYPESAFE_API_KEY, 'private-jev');
  assert.deepEqual(loaded.keychainSecrets.sort(), ['ANTHROPIC_API_KEY', 'TYPESAFE_API_KEY']);
  assert.equal(loaded.env.AUTOROUTER_TOKEN, undefined);
  // Separate configuration files never share keychain items.
  const other = fixture(t);
  saveUserConfig({ AUTOROUTER_SECRET_STORE: 'keychain' }, { env: other, keychain });
  assert.equal(loadUserConfig(other, { keychain }).env.ANTHROPIC_API_KEY, undefined);
  assert.equal(loadUserConfig({ ...env, ANTHROPIC_API_KEY: 'environment-key' }, { keychain }).env.ANTHROPIC_API_KEY, 'environment-key');
});

test('changing the store moves saved secrets in both directions without loss', async t => {
  const env = fixture(t), keychain = memoryKeychain(), lines = [];
  const options = { env, keychain, write: line => lines.push(line) };
  saveUserConfig({ AUTOROUTER_AUTH_MODE: 'api-key', ANTHROPIC_API_KEY: 'private-anthropic', AUTOROUTER_PORT: '8123' }, { env });
  assert.equal(await configCommand(['set', 'AUTOROUTER_SECRET_STORE', 'keychain'], options), true);
  assert.deepEqual(saved(env), { AUTOROUTER_AUTH_MODE: 'api-key', AUTOROUTER_PORT: '8123', AUTOROUTER_SECRET_STORE: 'keychain' });
  assert.deepEqual(valuesIn(keychain), ['private-anthropic']);
  assert.match(lines.at(-1), /Moved 1 saved secret to the macOS Keychain/);

  assert.equal(await configCommand(['set', 'AUTOROUTER_SECRET_STORE', 'file'], options), true);
  assert.equal(saved(env).ANTHROPIC_API_KEY, 'private-anthropic');
  assert.equal(keychain.items.size, 0);

  await configCommand(['set', 'AUTOROUTER_SECRET_STORE', 'keychain'], options);
  assert.equal(await configCommand(['unset', 'AUTOROUTER_SECRET_STORE'], options), true);
  assert.equal(saved(env).ANTHROPIC_API_KEY, 'private-anthropic');
  assert.equal(keychain.items.size, 0);
  assert.ok(!lines.join('\n').includes('private-'));
});

test('secret edits under the keychain store update and delete only keychain items', async t => {
  const env = fixture(t), keychain = memoryKeychain(), lines = [];
  saveUserConfig({ AUTOROUTER_SECRET_STORE: 'keychain', TYPESAFE_API_KEY: 'private-old' }, { env, keychain });
  const input = Object.assign(Readable.from(['private-new\n']), { isTTY: false });
  assert.equal(await configCommand(['set', 'TYPESAFE_API_KEY', '--stdin'], { env, keychain, input, write: line => lines.push(line) }), true);
  assert.deepEqual(valuesIn(keychain), ['private-new']);
  assert.ok(!readFileSync(env.AUTOROUTER_CONFIG, 'utf8').includes('private-'));
  assert.match(lines.at(-1), /Saved TYPESAFE_API_KEY in the macOS Keychain/);
  await configCommand(['set', 'AUTOROUTER_PORT', '8124'], { env, keychain, write: () => {} });
  assert.deepEqual(valuesIn(keychain), ['private-new'], 'unrelated edits keep keychain secrets');
  await configCommand(['unset', 'TYPESAFE_API_KEY'], { env, keychain, write: () => {} });
  assert.equal(keychain.items.size, 0);
});

test('a locked keychain blocks only operations that need the unreadable secret', async t => {
  const env = fixture(t), keychain = memoryKeychain();
  saveUserConfig({ AUTOROUTER_SECRET_STORE: 'keychain', ANTHROPIC_API_KEY: 'private-anthropic' }, { env, keychain });
  keychain.locked = true;
  assert.throws(() => loadUserConfig(env, { keychain }), /macOS Keychain|locked/);
  const overridden = { ...env, ANTHROPIC_API_KEY: 'env-a', TYPESAFE_API_KEY: 'env-b', AUTOROUTER_TOKEN: 'env-token-0123456789' };
  const loaded = loadUserConfig(overridden, { keychain });
  assert.equal(loaded.env.ANTHROPIC_API_KEY, 'env-a');
  assert.deepEqual(loaded.unavailableSecrets, ['ANTHROPIC_API_KEY', 'TYPESAFE_API_KEY', 'AUTOROUTER_TOKEN']);
  assert.throws(() => keychainRemovals(loaded, 'file'), /every saved secret/);
  await assert.rejects(configCommand(['set', 'AUTOROUTER_SECRET_STORE', 'file'], { env: overridden, keychain, write: () => {} }), /every saved secret/);
  keychain.locked = false;
  assert.deepEqual(valuesIn(keychain), ['private-anthropic']);
  assert.equal(saved(env).AUTOROUTER_SECRET_STORE, 'keychain');
});

test('a failed keychain write leaves the existing plaintext configuration untouched', async t => {
  const env = fixture(t), keychain = memoryKeychain();
  saveUserConfig({ ANTHROPIC_API_KEY: 'private-anthropic' }, { env });
  const before = readFileSync(env.AUTOROUTER_CONFIG, 'utf8');
  keychain.write = () => { throw Object.assign(new Error('Could not save an AutoRouter secret to the macOS Keychain.'), { code: 'AUTOROUTER_CONFIG_ERROR' }); };
  await assert.rejects(configCommand(['set', 'AUTOROUTER_SECRET_STORE', 'keychain'], { env, keychain, write: () => {} }), /Could not save/);
  assert.equal(readFileSync(env.AUTOROUTER_CONFIG, 'utf8'), before);
});

test('setup can store keys in the keychain and refuses that store off macOS before prompting', async t => {
  const env = { ...fixture(t), TYPESAFE_API_KEY: 'private-jev' }, keychain = memoryKeychain(), lines = [];
  await setup(['--secret-store', 'keychain'], { env, keychain, platform: 'darwin', write: line => lines.push(line), prompt: () => assert.fail('Unexpected prompt') });
  assert.equal(saved(env).AUTOROUTER_SECRET_STORE, 'keychain');
  assert.ok(!readFileSync(env.AUTOROUTER_CONFIG, 'utf8').includes('private-'));
  assert.deepEqual(valuesIn(keychain), ['private-jev']);
  assert.ok(lines.some(line => line.startsWith('Keys are stored in the macOS Keychain')));

  const linux = fixture(t);
  await assert.rejects(setup(['--secret-store', 'keychain'], { env: linux, keychain, platform: 'linux', write: () => {},
    prompt: () => assert.fail('Must not prompt for a secret that cannot be stored') }), /only on macOS/);
  await assert.rejects(setup(['--secret-store', 'vault'], { env: linux, write: () => {} }), /file or keychain/);

  // A forced update that changes the store moves the saved key back to the file.
  await setup(['--force', '--secret-store', 'file'], { env, keychain, platform: 'darwin', write: () => {}, prompt: () => assert.fail('Unexpected prompt') });
  assert.equal(saved(env).TYPESAFE_API_KEY, 'private-jev');
  assert.equal(keychain.items.size, 0);
});

test('invalid store values are rejected and history never reads the keychain', async t => {
  const env = fixture(t);
  assert.throws(() => saveUserConfig({ AUTOROUTER_SECRET_STORE: 'vault' }, { env }), /file or keychain/);
  await assert.rejects(configCommand(['set', 'AUTOROUTER_SECRET_STORE', 'vault'], { env, write: () => {} }), /file or keychain/);
  writeFileSync(env.AUTOROUTER_CONFIG, JSON.stringify({ AUTOROUTER_SECRET_STORE: 'keychain' }), { mode: 0o600 });
  // The default keychain is never consulted: a read would fail on Linux or touch the real keychain.
  assert.equal(loadUserConfig(env, { readSecrets: false }).values.ANTHROPIC_API_KEY, undefined);
  const lines = [];
  assert.equal(await sessionsCommand(['list'], { env, write: line => lines.push(line) }), true);
});

test('config show reports keychain provenance without values', async t => {
  const env = fixture(t), keychain = memoryKeychain(), lines = [];
  saveUserConfig({ AUTOROUTER_SECRET_STORE: 'keychain', TYPESAFE_API_KEY: 'private-jev' }, { env, keychain });
  assert.equal(await configCommand(['show', '--json'], { env, keychain, write: line => lines.push(line) }), true);
  const report = JSON.parse(lines.join('\n'));
  assert.deepEqual(report.settings.TYPESAFE_API_KEY, { source: 'keychain', active: true, secret: true, present: true });
  assert.deepEqual(report.settings.AUTOROUTER_SECRET_STORE, { source: 'file', active: true, value: 'keychain' });
  assert.ok(!lines.join('\n').includes('private-'));
});

test('new macOS setups default to the keychain; existing plaintext configurations are left alone and flagged', async t => {
  const env = { ...fixture(t), TYPESAFE_API_KEY: 'private-jev' }, keychain = memoryKeychain(), lines = [];
  const options = { env, keychain, platform: 'darwin', write: line => lines.push(line), prompt: () => assert.fail('Unexpected prompt') };
  await setup([], options);
  assert.equal(saved(env).AUTOROUTER_SECRET_STORE, 'keychain');
  assert.ok(!readFileSync(env.AUTOROUTER_CONFIG, 'utf8').includes('private-'));
  assert.deepEqual(valuesIn(keychain), ['private-jev']);

  const legacy = { ...fixture(t), TYPESAFE_API_KEY: 'private-legacy' };
  saveUserConfig({ AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: 'private-legacy' }, { env: legacy });
  const legacyLines = [];
  await setup(['--force', '--auth-mode', 'subscription'], { ...options, env: legacy, write: line => legacyLines.push(line) });
  assert.equal(saved(legacy).TYPESAFE_API_KEY, 'private-legacy', 'an update must not move keys implicitly');
  assert.ok(legacyLines.some(line => /plaintext.*Keychain/.test(line)));
  const checks = [];
  await doctor({ env: legacy, keychain, platform: 'darwin', write: line => checks.push(line),
    run: async () => { throw new Error('no claude'); } });
  assert.ok(checks.some(line => line.startsWith('WARN  Saved keys are plaintext')));
  assert.ok(!checks.join('\n').includes('private-'));

  // --replace rebuilds with the default; a key-free configuration is unaffected on Linux.
  const linux = { ...fixture(t), TYPESAFE_API_KEY: 'private-jev' };
  await setup([], { env: linux, keychain, platform: 'linux', write: () => {} });
  assert.equal(saved(linux).AUTOROUTER_SECRET_STORE, undefined);
});

test('an unavailable keychain falls back to the file only when it was the implicit default', async t => {
  const broken = memoryKeychain({ locked: true });
  const env = { ...fixture(t), TYPESAFE_API_KEY: 'private-jev' }, lines = [];
  await setup([], { env, keychain: broken, platform: 'darwin', write: line => lines.push(line), prompt: () => assert.fail('Unexpected prompt') });
  assert.equal(saved(env).TYPESAFE_API_KEY, 'private-jev');
  assert.equal(saved(env).AUTOROUTER_SECRET_STORE, undefined);
  assert.ok(lines.some(line => /Keychain is unavailable/.test(line)));
  const explicit = fixture(t);
  await assert.rejects(setup(['--secret-store', 'keychain'], { env: { ...explicit, TYPESAFE_API_KEY: 'private-jev' }, keychain: broken,
    platform: 'darwin', write: () => {}, prompt: () => assert.fail('Unexpected prompt') }), /macOS Keychain|locked/);
});

test('a newline in a Keychain item name cannot start another security command', t => {
  const calls = [];
  const run = (_command, args, options) => { calls.push({ args, input: options.input }); return { status: 0, stdout: '' }; };
  const keychain = createKeychain({ run, platform: 'darwin' });
  const hostile = ['label\ndelete-keychain /tmp/synthetic.keychain', 'label\r-w', 'label x', 'label\u0000x', 'label\u001bx'];
  for (const value of hostile) {
    assert.throws(() => keychain.write('ACCOUNT:0123456789abcdef', 'synthetic-secret', value), /control characters/);
    assert.throws(() => keychain.write(value, 'synthetic-secret', 'AutoRouter label'), /control characters/);
  }
  assert.equal(calls.length, 0, 'The security tool is never invoked with a hostile name');

  // A legitimate label, even with quotes, backslashes and non-ASCII text, is exactly one command.
  const stored = new Map();
  const single = createKeychain({ platform: 'darwin', run: (_command, args, options) => {
    if (args[0] === '-i') {
      assert.equal(options.input.split('\n').filter(Boolean).length, 1);
      stored.set('value', 'synthetic-secret');
      return { status: 0, stdout: '' };
    }
    return { status: 0, stdout: `${stored.get('value')}\n` };
  } });
  single.write('ACCOUNT:0123456789abcdef', 'synthetic-secret', 'AutoRouter KEY (/Users/Müller "a" \\ b/config.json)');
});

test('saving with the keychain store and a hostile config path fails before any Keychain call', t => {
  const base = mkdtempSync(join(tmpdir(), 'autorouter-keychain-path-'));
  t.after(() => rmSync(base, { recursive: true, force: true }));
  const keychain = memoryKeychain();
  const env = { AUTOROUTER_CONFIG: join(base, 'x\ndelete-keychain /tmp/synthetic.keychain\nconfig.json') };
  assert.throws(() => saveUserConfig({ AUTOROUTER_SECRET_STORE: 'keychain', TYPESAFE_API_KEY: 'synthetic-secret' }, { env, keychain }), /control characters/);
  assert.equal(keychain.items.size, 0);
});
