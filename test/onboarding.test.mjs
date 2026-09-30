import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { PassThrough } from 'node:stream';
import { setup, doctor, askSecret } from '../src/onboarding.mjs';
import { DEFAULT_OLLAMA_MODEL } from '../src/ollama-models.mjs';
import { readConfig } from '../src/config.mjs';
import { loadUserConfig } from '../src/user-config.mjs';

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

test('setup persists the chosen client profile with CLI precedence and leaves permission controls to Claude', async t => {
  for (const [inherited, args, expected] of [
    ['native', ['--client-profile', 'auto'], 'auto'],
    ['auto', [], 'auto'],
    ['auto', ['--client-profile', 'native'], 'native'],
    ['auto', ['--client-profile', 'compatible'], 'compatible'],
  ]) {
    const paths = fixture(t);
    const env = { ...paths, AUTOROUTER_CLIENT_PROFILE: inherited, TYPESAFE_API_KEY: 'synthetic-jev-key',
      ANTHROPIC_MODEL: 'claude-opus-5-5', MAX_THINKING_TOKENS: '10000', CLAUDE_CODE_AUTO_MODE_SERVER: '0' };
    const before = structuredClone(env);
    const lines = [];
    await setup(args, { env, write: line => lines.push(line),
      prompt: () => assert.fail('Key is supplied'), fetchImpl: () => assert.fail('Jev setup must not make provider calls') });
    assert.deepEqual(JSON.parse(readFileSync(paths.AUTOROUTER_CONFIG, 'utf8')), {
      AUTOROUTER_AUTH_MODE: 'subscription', AUTOROUTER_CLIENT_PROFILE: expected,
      AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: 'synthetic-jev-key',
    });
    assert.equal(readConfig(loadUserConfig(paths).env).clientProfile, expected);
    assert.deepEqual(env, before);
    if (expected === 'auto') {
      assert.match(lines.join('\n'), /Sonnet\/Opus task routing/);
      assert.match(lines.join('\n'), /Claude controls permission-mode availability and safety checks/);
    }
  }
});

test('setup rejects missing or invalid client profiles before keys, provider calls, and persistence', async t => {
  const paths = fixture(t);
  const unexpected = () => assert.fail('Invalid profiles must not prompt or contact providers');
  for (const value of [undefined, '', ' ', 'automatic', 'Auto', '--force']) {
    await assert.rejects(setup(['--client-profile', ...(value === undefined ? [] : [value])], {
      env: paths, write: () => {}, prompt: unexpected, fetchImpl: unexpected,
    }), /--client-profile must be compatible, native or auto/);
    assert.equal(existsSync(paths.AUTOROUTER_CONFIG), false);
  }
  await assert.rejects(setup([], {
    env: { ...paths, AUTOROUTER_CLIENT_PROFILE: 'unknown' }, write: () => {}, prompt: unexpected, fetchImpl: unexpected,
  }), /client-profile/);
  assert.equal(existsSync(paths.AUTOROUTER_CONFIG), false);
});

test('doctor reports the saved Auto profile without checking or claiming permission-mode eligibility', async t => {
  const paths = fixture(t);
  await setup(['--client-profile', 'auto'], { env: { ...paths, TYPESAFE_API_KEY: 'synthetic-jev-key' }, write: () => {} });
  const saved = readFileSync(paths.AUTOROUTER_CONFIG, 'utf8');
  const lines = [];
  const calls = [];
  assert.equal(await doctor({ env: paths, write: line => lines.push(line),
    fetchImpl: () => assert.fail('Doctor must not make provider calls'),
    run: async (command, args) => {
      calls.push([command, args]);
      return { stdout: args[0] === '--version' ? '2.1.285' : '{"loggedIn":true,"authMethod":"claude.ai"}' };
    },
  }), true);
  assert.match(lines.join('\n'), /Auto-compatible profile: Sonnet\/Opus task routing/);
  assert.match(lines.join('\n'), /Claude controls permission-mode availability and safety checks/);
  assert.doesNotMatch(lines.join('\n'), /Auto (?:permission )?mode (?:is )?(?:available|enabled)/i);
  assert.deepEqual(calls, [['claude', ['--version']], ['claude', ['auth', 'status', '--json']]]);
  assert.equal(readFileSync(paths.AUTOROUTER_CONFIG, 'utf8'), saved);
});

test('session logging setup saves an absolute directory with CLI precedence and supports disabling at runtime', async t => {
  const paths = fixture(t);
  const logDirectory = join(paths.AUTOROUTER_CONFIG, '..', 'decision logs');
  const env = { ...paths, TYPESAFE_API_KEY: 'synthetic-jev-key', AUTOROUTER_SESSION_LOG_DIR: '/unused-log-directory' };
  const before = structuredClone(env);
  const lines = [];
  await setup(['--session-log-dir', logDirectory], { env, write: line => lines.push(line), prompt: () => assert.fail('Key supplied') });
  const saved = readFileSync(paths.AUTOROUTER_CONFIG, 'utf8');
  assert.equal(JSON.parse(saved).AUTOROUTER_SESSION_LOG_DIR, resolve(logDirectory));
  assert.equal(readConfig(loadUserConfig(paths).env).sessionLogDir, resolve(logDirectory));
  assert.equal(readConfig(loadUserConfig({ ...paths, AUTOROUTER_SESSION_LOG_DIR: '' }).env).sessionLogDir, undefined);
  assert.equal(existsSync(logDirectory), false, 'Setup does not start logging or create log files');
  assert.deepEqual(env, before);
  assert.match(lines.join('\n'), /Session decision logs enabled/);
  lines.length = 0;
  assert.equal(await doctor({ env: paths, write: line => lines.push(line),
    run: async (_command, args) => ({ stdout: args[0] === '--version' ? '2.1.285' : '{"loggedIn":true,"authMethod":"claude.ai"}' }),
    fetchImpl: () => assert.fail('Doctor must not perform inference'),
  }), true);
  assert.match(lines.join('\n'), /Session decision logs enabled/);
  assert.equal(existsSync(logDirectory), false);
  assert.equal(readFileSync(paths.AUTOROUTER_CONFIG, 'utf8'), saved);
  await setup(['--force', '--session-log-dir', ''], { env, write: () => {} });
  assert.equal(JSON.parse(readFileSync(paths.AUTOROUTER_CONFIG, 'utf8')).AUTOROUTER_SESSION_LOG_DIR, '');
});

test('session logging setup rejects invalid directory flags before prompting or saving', async t => {
  const env = fixture(t);
  const unexpected = () => assert.fail('Invalid paths cannot prompt or contact providers');
  for (const args of [['--session-log-dir'], ['--session-log-dir', '--force'], ['--session-log-dir', 'private\npath']]) {
    await assert.rejects(setup(args, { env, write: () => {}, prompt: unexpected, fetchImpl: unexpected }), /session-log-dir/);
    assert.equal(existsSync(env.AUTOROUTER_CONFIG), false);
  }
  await assert.rejects(setup([], { env: { ...env, AUTOROUTER_SESSION_LOG_DIR: 'private\0path' },
    write: () => {}, prompt: unexpected, fetchImpl: unexpected }), /AUTOROUTER_SESSION_LOG_DIR/);
  assert.equal(existsSync(env.AUTOROUTER_CONFIG), false);
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

test('native Stop-hook block cap setup works with either evaluator and CLI values override the environment', async t => {
  for (const evaluator of ['jev', 'ollama']) {
    const paths = fixture(t);
    const env = { ...paths, TYPESAFE_API_KEY: 'synthetic-jev-key', CLAUDE_CODE_STOP_HOOK_BLOCK_CAP: '9' };
    const before = structuredClone(env);
    const local = localOllama();
    const lines = [];
    const options = { env, write: line => lines.push(line), prompt: () => assert.fail('Keys are supplied'),
      fetchImpl: evaluator === 'ollama' ? local.fetchImpl : () => assert.fail('Jev setup must not make provider calls') };
    await setup(['--evaluator', evaluator, '--stop-hook-block-cap', '0002'], options);
    const saved = JSON.parse(readFileSync(paths.AUTOROUTER_CONFIG, 'utf8'));
    assert.equal(saved.CLAUDE_CODE_STOP_HOOK_BLOCK_CAP, '2');
    assert.equal(saved.AUTOROUTER_EVALUATOR, evaluator);
    assert.equal(readConfig(loadUserConfig(paths).env).stopHookBlockCap, 2);
    assert.equal(readConfig(loadUserConfig({ ...paths, CLAUDE_CODE_STOP_HOOK_BLOCK_CAP: '0' }).env).stopHookBlockCap, 0);
    assert.match(lines.join('\n'), /Claude Stop\/SubagentStop cap: 2 continuations without tool use/);

    lines.length = 0;
    await setup(['--force', '--evaluator', evaluator, '--stop-hook-block-cap', '0'], options);
    assert.equal(JSON.parse(readFileSync(paths.AUTOROUTER_CONFIG, 'utf8')).CLAUDE_CODE_STOP_HOOK_BLOCK_CAP, '0');
    assert.match(lines.join('\n'), /Claude Stop\/SubagentStop continuation cap disabled \(0\)/);
    assert.deepEqual(env, before);
    if (evaluator === 'ollama') assert.equal(local.calls.filter(call => call.path === '/v1/systemone').length, 2, 'The option does not add extra evaluator calls');
  }
});

test('native Stop-hook block cap setup saves a normalized environment opt-in without changing its parent', async t => {
  for (const [raw, expected] of [['0002', '2'], ['0000', '0']]) {
    const env = { ...fixture(t), TYPESAFE_API_KEY: 'synthetic-jev-key', CLAUDE_CODE_STOP_HOOK_BLOCK_CAP: raw };
    const before = structuredClone(env);
    await setup([], { env, write: () => {}, prompt: () => assert.fail('Key is supplied'),
      fetchImpl: () => assert.fail('Jev setup must not make provider calls') });
    assert.equal(JSON.parse(readFileSync(env.AUTOROUTER_CONFIG, 'utf8')).CLAUDE_CODE_STOP_HOOK_BLOCK_CAP, expected);
    assert.deepEqual(env, before);
  }
});

test('native Stop-hook block cap setup rejects invalid CLI and environment values before prompts, persistence or providers', async t => {
  const unexpected = () => assert.fail('Invalid caps must not prompt or contact a provider');
  for (const evaluator of ['jev', 'ollama']) {
    const paths = fixture(t);
    for (const value of [undefined, '', ' ', '--pull', '-1', '1.5', '2.0', '1e2', '1e-999', 'Infinity', '9007199254740992']) {
      await assert.rejects(setup(['--evaluator', evaluator, '--stop-hook-block-cap', ...(value === undefined ? [] : [value])], {
        env: paths, write: () => {}, prompt: unexpected, fetchImpl: unexpected,
      }), /stop-hook-block-cap/);
      assert.equal(existsSync(paths.AUTOROUTER_CONFIG), false);
    }
    for (const value of ['', ' ', '-1', '1.5', '1e2', '9007199254740992', null, false]) {
      await assert.rejects(setup(['--evaluator', evaluator], {
        env: { ...paths, CLAUDE_CODE_STOP_HOOK_BLOCK_CAP: value }, write: () => {}, prompt: unexpected, fetchImpl: unexpected,
      }), /CLAUDE_CODE_STOP_HOOK_BLOCK_CAP/);
      assert.equal(existsSync(paths.AUTOROUTER_CONFIG), false);
    }
  }
  const env = { ...fixture(t), TYPESAFE_API_KEY: 'saved-key' };
  await setup([], { env, write: () => {} });
  const original = readFileSync(env.AUTOROUTER_CONFIG, 'utf8');
  await assert.rejects(setup(['--force', '--evaluator', 'ollama'], {
    env: { ...env, CLAUDE_CODE_STOP_HOOK_BLOCK_CAP: '' }, write: () => {}, prompt: unexpected, fetchImpl: unexpected,
  }), /CLAUDE_CODE_STOP_HOOK_BLOCK_CAP/);
  assert.equal(readFileSync(env.AUTOROUTER_CONFIG, 'utf8'), original);
});

test('native Stop-hook block cap doctor reports only a configured value and respects runtime overrides without inference', async t => {
  const paths = fixture(t);
  const setupEnv = { ...paths, TYPESAFE_API_KEY: 'synthetic-jev-key' };
  await setup([], { env: setupEnv, write: () => {} });
  const lines = [];
  const calls = [];
  const options = { env: paths, write: line => lines.push(line), fetchImpl: () => assert.fail('Doctor must not perform inference'),
    run: async (_command, args) => {
      calls.push(args);
      return { stdout: args[0] === '--version' ? '2.1.285' : '{"loggedIn":true,"authMethod":"claude.ai"}' };
    } };
  assert.equal(await doctor(options), true);
  assert.ok(!lines.join('\n').includes('Stop/SubagentStop'));
  await setup(['--force', '--stop-hook-block-cap', '2'], { env: setupEnv, write: () => {} });
  const saved = readFileSync(paths.AUTOROUTER_CONFIG, 'utf8');
  lines.length = 0;
  assert.equal(await doctor(options), true);
  assert.match(lines.join('\n'), /Claude Stop\/SubagentStop cap: 2 continuations without tool use/);
  lines.length = 0;
  assert.equal(await doctor({ ...options, env: { ...paths, CLAUDE_CODE_STOP_HOOK_BLOCK_CAP: '0' } }), true);
  assert.match(lines.join('\n'), /Claude Stop\/SubagentStop continuation cap disabled \(0\)/);
  assert.equal(readFileSync(paths.AUTOROUTER_CONFIG, 'utf8'), saved);
  assert.deepEqual(calls, Array.from({ length: 3 }, () => [['--version'], ['auth', 'status', '--json']]).flat());
});

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
  assert.match(lines.join('\n'), /Local evaluator: nimble:9b-q4_K_M; routing deadline 30000 ms per request/);
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

test('forced model setup persists an explicit deadline and later environment overrides still win', async t => {
  const paths = fixture(t);
  const lines = [];
  await setup(['--evaluator', 'ollama', '--ollama-model', 'tev1:4b'], {
    env: { ...paths, AUTOROUTER_OLLAMA_TIMEOUT_MS: '2400' }, write: line => lines.push(line),
    fetchImpl: localOllama('tev1:4b').fetchImpl,
  });
  assert.equal(JSON.parse(readFileSync(paths.AUTOROUTER_CONFIG, 'utf8')).AUTOROUTER_OLLAMA_TIMEOUT_MS, '2400');
  assert.match(lines.join('\n'), /tev1:4b; routing deadline 2400 ms per request/);

  lines.length = 0;
  await setup(['--force', '--evaluator', 'ollama', '--ollama-model', DEFAULT_OLLAMA_MODEL], {
    env: { ...paths, AUTOROUTER_OLLAMA_TIMEOUT_MS: '8000' }, write: line => lines.push(line),
    fetchImpl: localOllama().fetchImpl,
  });
  const saved = JSON.parse(readFileSync(paths.AUTOROUTER_CONFIG, 'utf8'));
  assert.equal(saved.AUTOROUTER_OLLAMA_MODEL, DEFAULT_OLLAMA_MODEL);
  assert.equal(saved.AUTOROUTER_OLLAMA_TIMEOUT_MS, '8000');
  assert.match(lines.join('\n'), /nimble:9b-q4_K_M; routing deadline 8000 ms per request/);
  assert.equal(readConfig(loadUserConfig(paths).env).ollamaTimeoutMs, 8000);
  assert.equal(readConfig(loadUserConfig({ ...paths, AUTOROUTER_OLLAMA_TIMEOUT_MS: '600' }).env).ollamaTimeoutMs, 600);
});

test('setup can disable the runtime deadline explicitly, overriding the environment and reporting it in doctor', async t => {
  const paths = fixture(t);
  const lines = [];
  const local = localOllama('tev1:4b');
  await setup(['--evaluator', 'ollama', '--ollama-model', 'tev1:4b', '--ollama-timeout-ms', '0'], {
    env: { ...paths, AUTOROUTER_OLLAMA_TIMEOUT_MS: '1500' }, write: line => lines.push(line), fetchImpl: local.fetchImpl,
  });
  assert.equal(JSON.parse(readFileSync(paths.AUTOROUTER_CONFIG, 'utf8')).AUTOROUTER_OLLAMA_TIMEOUT_MS, '0');
  assert.equal(readConfig(loadUserConfig(paths).env).ollamaTimeoutMs, 0);
  assert.match(lines.join('\n'), /tev1:4b; routing deadline disabled\./);
  assert.ok(!lines.join('\n').includes('deadline 0 ms'));
  assert.equal(local.calls.filter(call => call.path === '/v1/systemone').length, 1, 'Setup still performs its separately bounded synthetic warmup');

  lines.length = 0;
  local.calls.length = 0;
  const healthy = await doctor({ env: paths, write: line => lines.push(line), fetchImpl: local.fetchImpl,
    run: async (_command, args) => ({ stdout: args[0] === '--version' ? '2.1.284' : '{"loggedIn":true,"authMethod":"claude.ai"}' }),
  });
  assert.equal(healthy, true);
  assert.match(lines.join('\n'), /tev1:4b; routing deadline disabled\./);
  assert.deepEqual(local.calls.map(call => call.path), ['/api/version', '/api/tags', '/api/show']);

  lines.length = 0;
  await setup(['--force', '--evaluator', 'ollama', '--ollama-timeout-ms', '2200'], {
    env: { ...paths, AUTOROUTER_OLLAMA_TIMEOUT_MS: '0' }, write: line => lines.push(line), fetchImpl: localOllama().fetchImpl,
  });
  assert.equal(JSON.parse(readFileSync(paths.AUTOROUTER_CONFIG, 'utf8')).AUTOROUTER_OLLAMA_TIMEOUT_MS, '2200');
  assert.match(lines.join('\n'), /routing deadline 2200 ms per request/);
});

test('setup rejects missing, blank, invalid and Jev deadline options before prompting or calling a provider', async t => {
  const env = fixture(t);
  const unexpected = () => assert.fail('Invalid setup options must not prompt or contact providers');
  for (const value of [undefined, '', ' ', '--pull', '-1', '30001', '1.5', '1e-999', '-1e-999', 'NaN', 'Infinity']) {
    const args = ['--evaluator', 'ollama', '--ollama-timeout-ms', ...(value === undefined ? [] : [value])];
    await assert.rejects(setup(args, { env, write: () => {}, prompt: unexpected, fetchImpl: unexpected }), /--ollama-timeout-ms requires an integer/);
    assert.equal(existsSync(env.AUTOROUTER_CONFIG), false);
  }
  await assert.rejects(setup(['--ollama-timeout-ms', '0'], { env, write: () => {}, prompt: unexpected, fetchImpl: unexpected }), /require --evaluator ollama/);
  assert.equal(existsSync(env.AUTOROUTER_CONFIG), false);
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
  assert.match(lines.join('\n'), /nimble:9b-q4_K_M; routing deadline 30000 ms per request/);
  assert.match(lines.join('\n'), /classification speed and accuracy are not tested/);
  const missing = localOllama(DEFAULT_OLLAMA_MODEL, { installed: false });
  assert.equal(await doctor({ env, write: line => lines.push(line), fetchImpl: missing.fetchImpl,
    run: async (_command, args) => ({ stdout: args[0] === '--version' ? '2.1.284' : '{"loggedIn":true,"authMethod":"claude.ai"}' }) }), false);
  assert.match(lines.join('\n'), /--pull/);
  assert.deepEqual(missing.calls.map(call => call.path), ['/api/version', '/api/tags']);
});

test('Ollama doctor reports the effective saved model and environment deadline without inference', async t => {
  const paths = fixture(t);
  await setup(['--evaluator', 'ollama', '--ollama-model', 'tev1:4b'], {
    env: { ...paths, AUTOROUTER_OLLAMA_TIMEOUT_MS: '7000' }, write: () => {},
    fetchImpl: localOllama('tev1:4b').fetchImpl,
  });
  const local = localOllama('tev1:4b');
  const lines = [];
  const healthy = await doctor({
    env: { ...paths, AUTOROUTER_OLLAMA_TIMEOUT_MS: '9000' }, write: line => lines.push(line), fetchImpl: local.fetchImpl,
    run: async (_command, args) => ({ stdout: args[0] === '--version' ? '2.1.284' : '{"loggedIn":true,"authMethod":"claude.ai"}' }),
  });
  assert.equal(healthy, true);
  assert.match(lines.join('\n'), /tev1:4b; routing deadline 9000 ms per request/);
  assert.deepEqual(local.calls.map(call => call.path), ['/api/version', '/api/tags', '/api/show']);
  assert.equal(JSON.parse(readFileSync(paths.AUTOROUTER_CONFIG, 'utf8')).AUTOROUTER_OLLAMA_TIMEOUT_MS, '7000');
});
