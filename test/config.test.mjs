import test from 'node:test';
import assert from 'node:assert/strict';
import { readConfig, requireKeys } from '../src/config.mjs';
import { DEFAULT_OLLAMA_MODEL } from '../src/ollama-models.mjs';

test('Jev remains the default and local decisions default to the explicit Nimble quantization', () => {
  const defaults = readConfig({});
  assert.equal(defaults.evaluator, 'jev');
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
    assert.equal(readConfig({ AUTOROUTER_OLLAMA_MODEL: model }).ollamaTimeoutMs, 1500, model);
  }
});

test('an explicit local deadline overrides every model default and retains bounds', () => {
  for (const model of ['tev1:0.8b', 'tev1:4b', DEFAULT_OLLAMA_MODEL, 'team/nimble']) {
    for (const timeout of [1, 1500, 18000, 30000]) {
      const config = readConfig({ AUTOROUTER_OLLAMA_MODEL: model, AUTOROUTER_OLLAMA_TIMEOUT_MS: String(timeout) });
      assert.equal(config.ollamaTimeoutMs, timeout, `${model}: ${timeout}`);
    }
  }
  for (const value of ['0', '30001', '1.5', 'Infinity', 'unknown']) {
    assert.throws(() => readConfig({ AUTOROUTER_OLLAMA_TIMEOUT_MS: value }), /AUTOROUTER_OLLAMA_TIMEOUT_MS/);
  }
});

test('local subscription evaluation needs no API key while Jev and API billing retain their keys', () => {
  assert.throws(() => requireKeys(readConfig({ AUTOROUTER_AUTH_MODE: 'subscription' })), /TYPESAFE_API_KEY/);
  assert.doesNotThrow(() => requireKeys(readConfig({ AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_AUTH_MODE: 'subscription' })));
  assert.throws(() => requireKeys(readConfig({ AUTOROUTER_EVALUATOR: 'ollama' })), /ANTHROPIC_API_KEY/);
  assert.doesNotThrow(() => requireKeys(readConfig({ AUTOROUTER_EVALUATOR: 'ollama', ANTHROPIC_API_KEY: 'test-api' })));
});

test('local decision configuration rejects unsafe endpoints, cloud tags and invalid resource settings', () => {
  for (const env of [{ AUTOROUTER_EVALUATOR: 'auto' }, { AUTOROUTER_OLLAMA_URL: 'https://example.com' },
    { AUTOROUTER_OLLAMA_URL: 'http://127.0.0.1:11434/redirect' }, { AUTOROUTER_OLLAMA_MODEL: 'model:cloud' },
    { AUTOROUTER_OLLAMA_TIMEOUT_MS: '0' }, { AUTOROUTER_OLLAMA_KEEP_ALIVE: '-1' }]) {
    assert.throws(() => readConfig(env));
  }
});
