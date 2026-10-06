import test from 'node:test';
import assert from 'node:assert/strict';
import { CLIENT_PROFILES, readConfig, requireKeys } from '../src/config.mjs';
import { DEFAULT_OLLAMA_MODEL } from '../src/ollama-models.mjs';
import { resolve } from 'node:path';

test('session decision logging is opt-in and directory settings are validated without exposing their value', () => {
  assert.equal(readConfig({}).sessionLogDir, undefined);
  assert.equal(readConfig({ AUTOROUTER_SESSION_LOG_DIR: '' }).sessionLogDir, undefined);
  assert.equal(readConfig({ AUTOROUTER_SESSION_LOG_DIR: './local session logs' }).sessionLogDir, resolve('local session logs'));
  for (const value of [' ', 'private\npath', 'private\0path', false, null, 0, {}]) {
    assert.throws(() => readConfig({ AUTOROUTER_SESSION_LOG_DIR: value }), error => {
      assert.match(error.message, /AUTOROUTER_SESSION_LOG_DIR/);
      assert.ok(!error.message.includes('private'));
      return true;
    });
  }
});

test('session log mode controls excerpts without enabling logging', () => {
  assert.equal(readConfig({}).sessionLogMode, 'prompts');
  for (const mode of ['prompts', 'metadata']) {
    const config = readConfig({ AUTOROUTER_SESSION_LOG_MODE: mode });
    assert.equal(config.sessionLogDir, undefined);
    assert.equal(config.sessionLogMode, mode);
  }
  for (const value of ['', ' ', 'private-value', false, null]) {
    assert.throws(() => readConfig({ AUTOROUTER_SESSION_LOG_MODE: value }), error =>
      error.message.includes('AUTOROUTER_SESSION_LOG_MODE') && !error.message.includes('private-value'));
  }
});

test('Auto is an explicit client profile and accepts only documented supported routing targets', () => {
  assert.equal(readConfig({}).clientProfile, 'compatible');
  assert.deepEqual(CLIENT_PROFILES, ['compatible', 'native', 'auto']);
  assert.equal(readConfig({ AUTOROUTER_CLIENT_PROFILE: 'auto' }).models.sonnet, 'claude-sonnet-5-5');
  assert.equal(readConfig({ AUTOROUTER_CLIENT_PROFILE: 'auto' }).models.opus, 'claude-opus-5-5');
  assert.equal(readConfig({}).models.sonnet, 'claude-sonnet-5');
  for (const clientProfile of CLIENT_PROFILES) {
    assert.equal(readConfig({ AUTOROUTER_CLIENT_PROFILE: clientProfile }).clientProfile, clientProfile);
  }
  for (const [sonnet, opus] of [
    ['claude-sonnet-4-6', 'claude-opus-4-6'],
    ['claude-sonnet-5', 'claude-opus-4-7'],
    ['claude-sonnet-5-5', 'claude-opus-5-5'],
  ]) {
    const config = readConfig({ AUTOROUTER_CLIENT_PROFILE: 'auto', AUTOROUTER_SONNET_MODEL: sonnet, AUTOROUTER_OPUS_MODEL: opus });
    assert.equal(config.models.sonnet, sonnet);
    assert.equal(config.models.opus, opus);
  }
  for (const invalid of ['', 'Auto', 'automatic', 'unknown']) {
    assert.throws(() => readConfig({ AUTOROUTER_CLIENT_PROFILE: invalid }), /AUTOROUTER_CLIENT_PROFILE/);
  }
});

test('Auto rejects unsupported versions and unverified custom aliases without restricting other profiles', () => {
  for (const key of ['AUTOROUTER_SONNET_MODEL', 'AUTOROUTER_OPUS_MODEL']) {
    for (const model of ['claude-haiku-4-5-20251001', 'claude-sonnet-4-5', 'claude-opus-4-5',
      'sonnet', 'opus', 'custom/claude-sonnet-5', 'claude-opus-5-5-custom', 'claude-sonnet-99']) {
      assert.throws(() => readConfig({ AUTOROUTER_CLIENT_PROFILE: 'auto', [key]: model }),
        error => error.message.includes(key) && error.message.includes('Auto-mode-capable'));
      for (const profile of ['compatible', 'native']) {
        assert.doesNotThrow(() => readConfig({ AUTOROUTER_CLIENT_PROFILE: profile, [key]: model }));
      }
    }
  }
});

test('native Stop-hook block cap is opt-in and accepts explicit zero or safe decimal counts', () => {
  assert.equal(readConfig({}).stopHookBlockCap, undefined);
  for (const [value, expected] of [[0, 0], ['0', 0], [2, 2], ['2', 2], ['002', 2],
    [Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER], [String(Number.MAX_SAFE_INTEGER), Number.MAX_SAFE_INTEGER]]) {
    for (const evaluator of ['jev', 'ollama']) {
      const config = readConfig({ AUTOROUTER_EVALUATOR: evaluator, CLAUDE_CODE_STOP_HOOK_BLOCK_CAP: value });
      assert.equal(config.stopHookBlockCap, expected);
      assert.equal(config.jevTimeoutMs, 1500);
      assert.equal(config.ollamaTimeoutMs, 30000);
    }
  }
});

test('native Stop-hook block cap rejects coercion, nondecimal notation, fractions and unsafe integers', () => {
  for (const value of [-1, '-1', 1.5, '1.5', '2.0', '1e2', '1e-999', '-1e-999', '0x2', '+2', '', ' ',
    '2\n3', null, false, true, [], {}, NaN, Infinity, 'Infinity', 'invalid', Number.MAX_SAFE_INTEGER + 1, '9007199254740992']) {
    assert.throws(() => readConfig({ CLAUDE_CODE_STOP_HOOK_BLOCK_CAP: value }), /CLAUDE_CODE_STOP_HOOK_BLOCK_CAP/);
  }
});

test('local Ollama is the default evaluator, Jev is selectable, and local decisions default to the explicit Nimble quantization', () => {
  const defaults = readConfig({});
  assert.equal(defaults.evaluator, 'ollama');
  assert.equal(readConfig({ AUTOROUTER_EVALUATOR: 'jev' }).evaluator, 'jev');
  assert.equal(defaults.jevEndpoint, 'https://api.typesafe.ai/v1/systemone');
  assert.equal(defaults.jevTimeoutMs, 1500);
  assert.equal(DEFAULT_OLLAMA_MODEL, 'nimble:9b-q4_K_M');
  assert.equal(defaults.ollamaModel, DEFAULT_OLLAMA_MODEL);
  assert.equal(defaults.ollamaTimeoutMs, 30000);
  const custom = readConfig({ AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_OLLAMA_MODEL: 'team/decision-model:v1' });
  assert.equal(custom.ollamaModel, 'team/decision-model:v1');
  assert.equal(custom.ollamaEndpoint, 'http://127.0.0.1:11434');
  assert.equal(custom.ollamaTimeoutMs, 1500);
});

test('local deadlines recognize official model aliases and quantizations without changing model identity', () => {
  for (const prefix of ['', 'library/', 'registry.ollama.ai/', 'registry.ollama.ai/library/']) {
    for (const [tag, expected] of [
      ['tev1', 15000], ['tev1:latest', 15000], ['tev1:4b', 15000],
      ['tev1:4b-q4_K_M', 15000], ['tev1:4b-q8_0', 15000], ['tev1:4b-f16', 15000],
      ['tev1:0.8b', 1500], ['tev1:0.8b-q8_0', 1500],
      ['nimble', 30000], ['nimble:latest', 30000], ['nimble:9b', 30000],
      ['nimble:9b-q4_K_M', 30000], ['nimble:9b-q8_0', 30000], ['nimble:9b-f16', 30000],
    ]) {
      const model = prefix + tag;
      const config = readConfig({ AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_OLLAMA_MODEL: model });
      assert.equal(config.ollamaTimeoutMs, expected, model);
      assert.equal(config.ollamaModel, model, 'The request must retain the chosen alias');
      assert.equal(config.jevTimeoutMs, 1500, 'Local defaults do not change the Jev deadline');
    }
  }
});

test('custom namespaces and unknown local model tags keep the short deadline', () => {
  for (const model of ['team/nimble', 'team/tev1:4b', 'registry.ollama.ai/team/nimble:9b-q4_K_M',
    'registry.example/library/nimble:9b', 'library/team/tev1:4b', 'tev1:40b', 'tev1:4b-experimental',
    'nimble:small', 'nimble:9b-custom', 'nimble:9b-q4_K_M-extra', 'nimble-other:9b', 'custom:v1']) {
    assert.equal(readConfig({ AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_OLLAMA_MODEL: model }).ollamaTimeoutMs, 1500, model);
  }
});

test('an explicit local deadline overrides every model default and retains bounds', () => {
  for (const model of ['tev1:0.8b', 'tev1:4b', DEFAULT_OLLAMA_MODEL, 'team/nimble']) {
    for (const timeout of [0, 1, 1500, 18000, 30000]) {
      const config = readConfig({ AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_OLLAMA_MODEL: model, AUTOROUTER_OLLAMA_TIMEOUT_MS: String(timeout) });
      assert.equal(config.ollamaTimeoutMs, timeout, `${model}: ${timeout}`);
    }
  }
  for (const value of ['-1', '30001', '1.5', '1e-999', '-1e-999', 'Infinity', 'unknown', '', ' ', null, false, true, [], {}]) {
    assert.throws(() => readConfig({ AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_OLLAMA_TIMEOUT_MS: value }), /AUTOROUTER_OLLAMA_TIMEOUT_MS/);
  }
});

test('local subscription evaluation needs no API key while Jev and API billing retain their keys', () => {
  assert.throws(() => requireKeys(readConfig({ AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_AUTH_MODE: 'subscription' })), /TYPESAFE_API_KEY/);
  assert.doesNotThrow(() => requireKeys(readConfig({ AUTOROUTER_AUTH_MODE: 'subscription' })), 'Ollama needs no Jev key by default');
  assert.doesNotThrow(() => requireKeys(readConfig({ AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_AUTH_MODE: 'subscription' })));
  assert.throws(() => requireKeys(readConfig({ AUTOROUTER_EVALUATOR: 'ollama' })), /ANTHROPIC_API_KEY/);
  assert.doesNotThrow(() => requireKeys(readConfig({ AUTOROUTER_EVALUATOR: 'ollama', ANTHROPIC_API_KEY: 'test-api' })));
});

test('local decision configuration rejects unsafe endpoints, cloud tags and invalid resource settings', () => {
  for (const env of [{ AUTOROUTER_EVALUATOR: 'auto' }, { AUTOROUTER_OLLAMA_URL: 'https://example.com' },
    { AUTOROUTER_OLLAMA_URL: 'http://127.0.0.1:11434/redirect' }, { AUTOROUTER_OLLAMA_MODEL: 'model:cloud' },
    { AUTOROUTER_OLLAMA_TIMEOUT_MS: '-1' }, { AUTOROUTER_OLLAMA_KEEP_ALIVE: '-1' }]) {
    assert.throws(() => readConfig({ AUTOROUTER_EVALUATOR: 'ollama', ...env }));
  }
});

test('runtime validates the selected evaluator and explicit full checks include inactive settings', () => {
  const jev = { AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_OLLAMA_URL: 'https://private-sentinel:secret@example.test', AUTOROUTER_OLLAMA_TIMEOUT_MS: '' };
  assert.equal(readConfig(jev).evaluator, 'jev');
  assert.throws(() => readConfig(jev, { validateAll: true }), /AUTOROUTER_OLLAMA_TIMEOUT_MS/);
  const local = { AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_JEV_URL: 'private-invalid-url', AUTOROUTER_MIN_CONFIDENCE: '' };
  assert.equal(readConfig(local).evaluator, 'ollama');
  assert.throws(() => readConfig(local, { validateAll: true }), error => {
    assert.match(error.message, /AUTOROUTER_JEV_URL/);
    assert.ok(!error.message.includes('private-invalid-url'));
    return true;
  });
});

test('numeric settings reject blank and coercible values while meaningful zero remains valid', () => {
  for (const key of ['AUTOROUTER_PORT', 'AUTOROUTER_JEV_TIMEOUT_MS', 'AUTOROUTER_TOKEN_COUNT_TIMEOUT_MS', 'AUTOROUTER_MIN_CONFIDENCE']) {
    for (const value of ['', ' ', null, false, '0x10', '1e2']) assert.throws(() => readConfig({ AUTOROUTER_EVALUATOR: 'jev', [key]: value }), new RegExp(key));
  }
  assert.equal(readConfig({ AUTOROUTER_PORT: '0' }).port, 0);
  assert.equal(readConfig({ AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_MIN_CONFIDENCE: '0' }).minConfidence, 0);
  assert.equal(readConfig({ AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_MIN_CONFIDENCE: '.5' }).minConfidence, .5);
});

test('owned boolean flags and model names reject invalid values without exposing them', () => {
  for (const key of ['AUTOROUTER_STATUSLINE', 'AUTOROUTER_DEBUG']) {
    for (const value of ['', 'false', 'true', '2', false]) assert.throws(() => readConfig({ [key]: value }), new RegExp(key));
    for (const value of ['0', '1']) assert.doesNotThrow(() => readConfig({ [key]: value }));
  }
  for (const key of ['AUTOROUTER_HAIKU_MODEL', 'AUTOROUTER_SONNET_MODEL', 'AUTOROUTER_OPUS_MODEL', 'AUTOROUTER_JEV_MODEL']) {
    for (const value of ['', ' ', 'private\nsentinel', null]) assert.throws(() => readConfig({ AUTOROUTER_EVALUATOR: 'jev', [key]: value }), error => {
      assert.match(error.message, new RegExp(key));
      assert.ok(!error.message.includes('private'));
      return true;
    });
    assert.doesNotThrow(() => readConfig({ AUTOROUTER_EVALUATOR: 'jev', [key]: 'custom/team-model:v1' }));
  }
  assert.doesNotThrow(() => readConfig({ ENABLE_TOOL_SEARCH: 'auto:5' }));
});
