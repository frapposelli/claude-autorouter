import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { PassThrough } from 'node:stream';
import { setup, doctor, askSecret } from '../src/onboarding.mjs';
import { DEFAULT_OLLAMA_MODEL } from '../src/ollama-models.mjs';

function fixture(t) {
  const directory = mkdtempSync(join(tmpdir(), 'autorouter-onboarding-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  return { AUTOROUTER_CONFIG: join(directory, 'config.json') };
}

test('hidden key prompt disables terminal echo before inviting input and restores it afterward', async () => {
  const input = new PassThrough();
  input.isTTY = true;
  input.isRaw = false;
  input.setRawMode = value => { input.isRaw = value; };
  let visible = '';
  const output = { isTTY: true, write(text) {
    visible += text;
    if (text.includes('(hidden)')) {
      assert.equal(input.isRaw, true, 'No prompt may be shown before terminal echo is disabled');
      queueMicrotask(() => input.write('hidden-key-sentinel\r'));
    }
  } };
  assert.equal(await askSecret('TYPESAFE_API_KEY', { input, output }), 'hidden-key-sentinel');
  assert.equal(input.isRaw, false);
  assert.ok(!visible.includes('hidden-key-sentinel'));
  input.destroy();
});

test('setup stores only relevant keys, defaults to subscription, and never prints credentials', async t => {
  const env = { ...fixture(t), TYPESAFE_API_KEY: 'private-jev-sentinel', ANTHROPIC_API_KEY: 'unused-api-secret', UNRELATED: 'other-secret' };
  const lines = [];
  await setup([], { env, write: line => lines.push(line), prompt: () => assert.fail('Unexpected prompt') });
  assert.deepEqual(JSON.parse(readFileSync(env.AUTOROUTER_CONFIG, 'utf8')), {
    AUTOROUTER_AUTH_MODE: 'subscription', AUTOROUTER_CLIENT_PROFILE: 'compatible', AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: 'private-jev-sentinel',
  });
  for (const secret of ['private-jev-sentinel', 'unused-api-secret', 'other-secret']) assert.ok(!lines.join('\n').includes(secret));
  await assert.rejects(setup([], { env, write: () => {}, prompt: () => assert.fail('Must reject before prompting') }), /already exists/);
  await setup(['--force', '--auth-mode', 'api-key'], { env, write: () => {} });
  const saved = JSON.parse(readFileSync(env.AUTOROUTER_CONFIG, 'utf8'));
  assert.equal(saved.AUTOROUTER_AUTH_MODE, 'api-key');
  assert.equal(saved.ANTHROPIC_API_KEY, env.ANTHROPIC_API_KEY);
});

test('setup prompts only for missing keys and leaves no file on invalid or cancelled input', async t => {
  const env = fixture(t);
  const prompts = [];
  await assert.rejects(setup(['--auth-mode', 'api-key'], {
    env, write: () => {}, prompt: async key => { prompts.push(key); return key === 'TYPESAFE_API_KEY' ? 'jev-secret' : ''; },
  }), /ANTHROPIC_API_KEY/);
  assert.deepEqual(prompts, ['TYPESAFE_API_KEY', 'ANTHROPIC_API_KEY']);
  assert.equal(existsSync(env.AUTOROUTER_CONFIG), false);
  await assert.rejects(setup([], { env, write: () => {}, prompt: async () => { throw new Error('Setup cancelled'); } }), /cancelled/);
  assert.equal(existsSync(env.AUTOROUTER_CONFIG), false);
  await assert.rejects(setup(['--key', 'secret'], { env, write: () => {} }), error => !error.message.includes('secret'));
  await assert.rejects(askSecret('TYPESAFE_API_KEY', { input: { isTTY: false }, output: {} }), /environment/);
});

test('doctor uses saved subscription login with sanitized environment and does not print account data', async t => {
  const env = { ...fixture(t), TYPESAFE_API_KEY: 'jev-secret' };
  await setup([], { env, write: () => {} });
  delete env.TYPESAFE_API_KEY;
  env.ANTHROPIC_API_KEY = 'stale-api-secret';
  env.ANTHROPIC_AUTH_TOKEN = 'stale-token';
  env.CLAUDE_CODE_OAUTH_TOKEN = 'stale-oauth';
  const lines = [];
  const calls = [];
  const healthy = await doctor({ env, write: line => lines.push(line), run: async (command, args, options) => {
    calls.push([command, args]);
    for (const key of ['TYPESAFE_API_KEY', 'ANTHROPIC_API_KEY', 'ANTHROPIC_AUTH_TOKEN', 'CLAUDE_CODE_OAUTH_TOKEN', 'AUTOROUTER_CONFIG', 'ANTHROPIC_BASE_URL']) {
      assert.equal(options.env[key], undefined);
    }
    if (args[0] === '--version') return { stdout: '2.1.284 (Claude Code)' };
    return { stdout: JSON.stringify({ loggedIn: true, authMethod: 'claude.ai', email: 'private@example.test', token: 'auth-secret' }) };
  } });
  assert.equal(healthy, true);
  assert.deepEqual(calls, [['claude', ['--version']], ['claude', ['auth', 'status', '--json']]]);
  for (const value of ['private@example.test', 'auth-secret', 'jev-secret', 'stale-token']) assert.ok(!lines.join('\n').includes(value));
  assert.equal(env.ANTHROPIC_API_KEY, 'stale-api-secret');
});

test('doctor reports missing login, conflicting provider, and subprocess failures without dumping errors', async t => {
  const env = { ...fixture(t), TYPESAFE_API_KEY: 'jev-secret' };
  await setup([], { env, write: () => {} });
  for (const status of [{ loggedIn: false }, { loggedIn: true, authMethod: 'api_key' }]) {
    assert.equal(await doctor({ env, write: () => {}, run: async (_command, args) => ({ stdout: args[0] === '--version' ? '2.1.284' : JSON.stringify(status) }) }), false);
  }
  const lines = [];
  assert.equal(await doctor({ env, write: line => lines.push(line), run: async () => { throw new Error('private-error-secret'); } }), false);
  assert.ok(!lines.join('\n').includes('private-error-secret'));
  assert.equal(await doctor({ env: { ...env, CLAUDE_CODE_USE_VERTEX: '1' }, write: () => {}, run: async (_command, args) => ({ stdout: args[0] === '--version' ? '2.1.284' : '{"loggedIn":true,"authMethod":"claude.ai"}' }) }), false);
});

test('doctor accepts environment-only API configuration and skips subscription inspection', async t => {
  const env = { ...fixture(t), TYPESAFE_API_KEY: 'jev-secret', ANTHROPIC_API_KEY: 'api-secret' };
  // Explicit config paths are required to exist; use an empty XDG tree instead.
  env.XDG_CONFIG_HOME = join(env.AUTOROUTER_CONFIG, '..', 'xdg');
  delete env.AUTOROUTER_CONFIG;
  const calls = [];
  const lines = [];
  assert.equal(await doctor({ env, write: line => lines.push(line), run: async (_command, args) => { calls.push(args); return { stdout: '2.1.284' }; } }), true);
  assert.deepEqual(calls, [['--version']]);
  assert.match(lines.join('\n'), /absent; using environment/);
  assert.match(lines.join('\n'), /key validity.*not tested/);
});

function localOllama(model = DEFAULT_OLLAMA_MODEL, { installed = true, details = { details: { parameter_size: '1.7B' } } } = {}) {
  const calls = [];
  return { calls, fetchImpl: async (url, options) => {
    const path = new URL(url).pathname;
    calls.push({ path, method: options.method, ...(options.body ? { body: JSON.parse(options.body) } : {}) });
    if (path === '/api/version') return Response.json({ version: '0.35.0' });
    if (path === '/api/tags') return Response.json({ models: installed ? [{ name: model }] : [] });
    if (path === '/api/show') return Response.json(details);
    if (path === '/v1/systemone') return Response.json({ model, answers: { tier: { type: 'choice', choice: 'haiku', probabilities: { haiku: 1, sonnet: 0, opus: 0 }, confidence: 1 } }, usage: { input_tokens: 200, output_tokens: 1 } });
    assert.fail('Unexpected local Ollama operation');
  } };
}

test('Ollama subscription setup needs no keys, stays local, and preloads before saving', async t => {
  const env = { ...fixture(t), TYPESAFE_API_KEY: 'unused-jev-key', ANTHROPIC_API_KEY: 'unused-anthropic-key' };
  const local = localOllama();
  const lines = [];
  await setup(['--evaluator', 'ollama'], { env, write: line => lines.push(line), fetchImpl: local.fetchImpl,
    prompt: () => assert.fail('Ollama subscription mode requires no API keys') });
  const saved = JSON.parse(readFileSync(env.AUTOROUTER_CONFIG, 'utf8'));
  assert.deepEqual(saved, {
    AUTOROUTER_AUTH_MODE: 'subscription', AUTOROUTER_CLIENT_PROFILE: 'compatible',
    AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_OLLAMA_MODEL: DEFAULT_OLLAMA_MODEL,
  });
  assert.deepEqual(local.calls.map(call => call.path), ['/api/version', '/api/tags', '/api/show', '/api/show', '/v1/systemone']);
  const warm = local.calls.at(-1).body;
  assert.equal(warm.state.current_task, 'Return the literal word ready.');
  assert.equal(warm.questions.tier.type, 'choice');
  assert.equal(warm.options, undefined);
  assert.equal(warm.messages, undefined);
  assert.ok(!JSON.stringify(local.calls).includes('unused-'));
  assert.match(lines.join('\n'), /locally with Ollama/);
  assert.ok(!lines.join('\n').includes('unused-'));
});

test('Ollama setup preserves API-key authentication and selects the default or an explicit compatible model', async t => {
  const env = { ...fixture(t), AUTOROUTER_OLLAMA_URL: 'http://localhost:11435', AUTOROUTER_OLLAMA_KEEP_ALIVE: '10m' };
  const prompts = [];
  const local = localOllama(DEFAULT_OLLAMA_MODEL);
  await setup(['--evaluator', 'ollama', '--auth-mode', 'api-key'], {
    env, write: () => {}, fetchImpl: local.fetchImpl,
    prompt: async key => { prompts.push(key); return 'anthropic-key'; },
  });
  assert.deepEqual(prompts, ['ANTHROPIC_API_KEY']);
  const saved = JSON.parse(readFileSync(env.AUTOROUTER_CONFIG, 'utf8'));
  assert.equal(saved.AUTOROUTER_OLLAMA_MODEL, DEFAULT_OLLAMA_MODEL);
  assert.equal(saved.ANTHROPIC_API_KEY, 'anthropic-key');
  assert.equal(saved.TYPESAFE_API_KEY, undefined);
  assert.equal(saved.AUTOROUTER_OLLAMA_URL, env.AUTOROUTER_OLLAMA_URL);
  assert.equal(local.calls.at(-1).body.keep_alive, '10m');

  const custom = localOllama('custom-router:latest');
  await setup(['--force', '--evaluator', 'ollama', '--ollama-model', 'custom-router:latest'], {
    env, write: () => {}, fetchImpl: custom.fetchImpl,
  });
  assert.equal(JSON.parse(readFileSync(env.AUTOROUTER_CONFIG, 'utf8')).AUTOROUTER_OLLAMA_MODEL, 'custom-router:latest');
});

test('failed Ollama setup leaves the existing configuration intact and never downloads implicitly', async t => {
  const env = { ...fixture(t), TYPESAFE_API_KEY: 'original-key' };
  await setup([], { env, write: () => {} });
  const original = readFileSync(env.AUTOROUTER_CONFIG, 'utf8');
  const local = localOllama(DEFAULT_OLLAMA_MODEL, { installed: false });
  await assert.rejects(setup(['--force', '--evaluator', 'ollama'], { env, write: () => {}, fetchImpl: local.fetchImpl }), /--pull/);
  assert.deepEqual(local.calls.map(call => call.path), ['/api/version', '/api/tags']);
  assert.equal(readFileSync(env.AUTOROUTER_CONFIG, 'utf8'), original);
  for (const args of [['--pull'], ['--evaluator', 'unknown'], ['--evaluator', 'ollama', '--ollama-preset'], ['--evaluator', 'ollama', '--ollama-model']]) {
    await assert.rejects(setup(args, { env: fixture(t), write: () => {}, fetchImpl: () => assert.fail('Invalid arguments must not make requests') }));
  }
});

test('Ollama doctor verifies local model metadata without warming, downloading, or asking for Jev', async t => {
  const env = { ...fixture(t), AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_AUTH_MODE: 'subscription' };
  env.XDG_CONFIG_HOME = join(env.AUTOROUTER_CONFIG, '..', 'xdg');
  delete env.AUTOROUTER_CONFIG;
  const local = localOllama();
  const lines = [];
  const healthy = await doctor({ env, write: line => lines.push(line), fetchImpl: local.fetchImpl,
    run: async (_command, args) => ({ stdout: args[0] === '--version' ? '2.1.284' : '{"loggedIn":true,"authMethod":"claude.ai"}' }) });
  assert.equal(healthy, true);
  assert.deepEqual(local.calls.map(call => call.path), ['/api/version', '/api/tags', '/api/show']);
  assert.match(lines.join('\n'), /Local Ollama model available/);
  const missing = localOllama(DEFAULT_OLLAMA_MODEL, { installed: false });
  assert.equal(await doctor({ env, write: line => lines.push(line), fetchImpl: missing.fetchImpl,
    run: async (_command, args) => ({ stdout: args[0] === '--version' ? '2.1.284' : '{"loggedIn":true,"authMethod":"claude.ai"}' }) }), false);
  assert.match(lines.join('\n'), /--pull/);
  assert.deepEqual(missing.calls.map(call => call.path), ['/api/version', '/api/tags']);
});
