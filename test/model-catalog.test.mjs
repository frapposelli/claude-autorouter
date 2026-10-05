import test from 'node:test';
import assert from 'node:assert/strict';
import { MODEL_CATALOG, modelCapabilities, modelContextWindow, hasNativeMillionContext,
  canUpgradeContext, supportsToolReferences, supportsAutoMode } from '../src/model-catalog.mjs';

test('catalog uses exact native identities and exposes immutable, dated capability facts', () => {
  const haiku = modelCapabilities('claude-haiku-4-5');
  assert.equal(haiku, modelCapabilities('claude-haiku-4-5-20251001'));
  assert.equal(haiku.family, 'haiku');
  assert.equal(haiku.maxOutputTokens, 64000);
  assert.equal(Object.isFrozen(MODEL_CATALOG), true);
  for (const model of Object.keys(MODEL_CATALOG)) {
    const entry = modelCapabilities(model);
    assert.equal(Object.isFrozen(entry), true);
    assert.equal(Object.isFrozen(entry.thinkingTypes), true);
    assert.match(entry.reviewedAt, /^\d{4}-\d{2}-\d{2}$/);
    assert.match(entry.source, /^https:\/\/platform\.claude\.com\/docs\//);
  }
  for (const model of ['opus', 'team/claude-opus-5-5', 'claude-opus-5-5-future', '__proto__', 'toString', null]) {
    assert.equal(modelCapabilities(model), undefined, String(model));
    assert.equal(modelContextWindow(model), undefined);
    assert.equal(supportsToolReferences(model), false);
    assert.equal(supportsAutoMode(model), false);
  }
});

test('subscription-sensitive context does not become an unconditional 1M promise', () => {
  assert.equal(modelContextWindow('claude-haiku-4-5'), 200000);
  assert.equal(canUpgradeContext('claude-haiku-4-5'), true);
  for (const model of ['claude-sonnet-4-6', 'claude-opus-4-6']) {
    assert.equal(modelContextWindow(model), undefined);
    assert.equal(hasNativeMillionContext(model), false);
    assert.equal(canUpgradeContext(model), true);
    assert.equal(supportsAutoMode(model), true);
  }
  for (const model of ['claude-sonnet-5', 'claude-sonnet-5-5', 'claude-opus-4-7', 'claude-opus-4-8', 'claude-opus-5', 'claude-opus-5-5']) {
    assert.equal(hasNativeMillionContext(model), true);
    assert.equal(canUpgradeContext(model), false);
    assert.equal(supportsToolReferences(model), true);
  }
});
