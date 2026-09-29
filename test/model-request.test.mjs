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

test('Sonnet 5.5 maps disabled thinking to its lowest supported setting without changing the source request', () => {
  for (const effort of [undefined, 'low', 'medium', 'high']) {
    const body = { model: 'claude-haiku-4-5-20251001', thinking: { type: 'disabled' },
      messages: [{ role: 'user', content: 'Investigate the dashboard discrepancy.' }], max_tokens: 32000,
      ...(effort ? { output_config: { effort } } : {}) };
    const before = structuredClone(body);
    const result = prepareRequest(body, 'claude-sonnet-5-5');
    assert.deepEqual(result.request, { ...before, model: 'claude-sonnet-5-5', thinking: { type: 'between_tools' } });
    assert.deepEqual(result.adjustments, ['between_tools_thinking_required']);
    assert.deepEqual(body, before);
  }
});

test('Sonnet 5.5 retains high effort and per-message effort changes by using adaptive thinking', () => {
  for (const overrides of [
    { output_config: { effort: 'xhigh' } }, { output_config: { effort: 'max' } },
    { messages: [{ role: 'user', content: 'Task' }, { role: 'system', content: 'Next turn', output_config: { effort: 'medium' } }] },
    { output_config: { effort: 'low' }, messages: [{ role: 'user', content: 'Task' }, { role: 'system', content: 'Next turn', output_config: { effort: 'high' } }] },
  ]) {
    const body = { model: 'claude-haiku-4-5-20251001', thinking: { type: 'disabled' },
      messages: [{ role: 'user', content: 'Task' }], ...overrides };
    const before = structuredClone(body);
    const result = prepareRequest(body, 'claude-sonnet-5-5');
    assert.deepEqual(result.request, { ...before, model: 'claude-sonnet-5-5', thinking: { type: 'adaptive' } });
    assert.deepEqual(result.adjustments, ['adaptive_thinking_required']);
    assert.deepEqual(body, before);
  }
  const consistent = { model: 'claude-haiku-4-5-20251001', thinking: { type: 'disabled' },
    output_config: { effort: 'low' }, messages: [{ role: 'user', content: 'Task' }, { role: 'system', content: 'Next turn', output_config: { effort: 'low' } }] };
  assert.deepEqual(prepareRequest(consistent, 'claude-sonnet-5-5').request.thinking, { type: 'between_tools' });
});

test('other models, existing adaptive settings and explicitly requested models pass through', () => {
  for (const [source, target, thinking] of [
    ['haiku', 'claude-sonnet-5', { type: 'disabled' }],
    ['haiku', 'claude-opus-4-6', { type: 'disabled' }],
    ['haiku', 'claude-opus-5-5', { type: 'adaptive' }],
    ['haiku', 'claude-opus-5-5', undefined],
    ['claude-opus-5-5', 'claude-opus-5-5', { type: 'disabled' }],
    ['haiku', 'team/claude-sonnet-5-5', { type: 'disabled' }],
    ['haiku', 'claude-sonnet-5-5-future', { type: 'disabled' }],
    ['haiku', 'claude-sonnet-5-5', { type: 'adaptive', display: 'summarized' }],
    ['haiku', 'claude-sonnet-5-5', undefined],
    ['claude-sonnet-5-5', 'claude-sonnet-5-5', { type: 'between_tools' }],
    ['claude-sonnet-5-5', 'claude-sonnet-5-5', { type: 'disabled' }],
  ]) {
    const body = { model: source, thinking, messages: [] };
    const result = prepareRequest(body, target);
    assert.deepEqual(result.request, { ...body, model: target });
    assert.deepEqual(result.adjustments, []);
  }
});
