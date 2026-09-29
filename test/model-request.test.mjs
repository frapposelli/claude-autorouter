import test from 'node:test';
import assert from 'node:assert/strict';
import { prepareRequest } from '../src/model-request.mjs';

test('upgrading a compatible request to Opus enables its required adaptive thinking', () => {
  const body = { model: 'claude-haiku-4-5-20251001', thinking: { type: 'disabled' }, messages: [{ role: 'user', content: 'Task' }], max_tokens: 32000 };
  const before = structuredClone(body);
  const { request, adjustments } = prepareRequest(body, 'claude-opus-5-5');
  assert.equal(request.model, 'claude-opus-5-5');
  assert.deepEqual(request.thinking, { type: 'adaptive' });
  assert.deepEqual(request.messages, before.messages);
  assert.deepEqual(adjustments, ['adaptive_thinking_required']);
  assert.deepEqual(body, before);
});

test('other models, existing adaptive settings and explicitly requested models pass through', () => {
  for (const [source, target, thinking] of [
    ['haiku', 'claude-sonnet-5', { type: 'disabled' }],
    ['haiku', 'claude-opus-4-6', { type: 'disabled' }],
    ['haiku', 'claude-opus-5-5', { type: 'adaptive' }],
    ['haiku', 'claude-opus-5-5', undefined],
    ['claude-opus-5-5', 'claude-opus-5-5', { type: 'disabled' }],
  ]) {
    const body = { model: source, thinking, messages: [] };
    const result = prepareRequest(body, target);
    assert.deepEqual(result.request, { ...body, model: target });
    assert.deepEqual(result.adjustments, []);
  }
});
