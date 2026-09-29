import test from 'node:test';
import assert from 'node:assert/strict';
import { readConfig, requireKeys } from '../src/config.mjs';
import { DEFAULT_OLLAMA_MODEL } from '../src/ollama-models.mjs';

test('Jev remains the default and local decisions default to the explicit Nimble quantization', () => {
  const defaults = readConfig({});
  assert.equal(defaults.evaluator, 'jev');
  assert.equal(defaults.jevEndpoint, 'https://api.typesafe.ai/v1/systemone');
  assert.equal(DEFAULT_OLLAMA_MODEL, 'nimble:9b-q4_K_M');
  assert.equal(defaults.ollamaModel, DEFAULT_OLLAMA_MODEL);
  const custom = readConfig({ AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_OLLAMA_MODEL: 'team/decision-model:v1' });
  assert.equal(custom.ollamaModel, 'team/decision-model:v1');
  assert.equal(custom.ollamaEndpoint, 'http://127.0.0.1:11434');
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
