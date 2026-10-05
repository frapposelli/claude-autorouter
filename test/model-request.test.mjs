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

test('a Sonnet 5.5 between_tools request upgrades to adaptive Opus without altering signed history or safeguards', () => {
  const body = {
    model: 'claude-sonnet-5-5', thinking: { type: 'between_tools' },
    system: [{ type: 'text', text: 'Synthetic system instructions.' }],
    tools: [{ name: 'Read', input_schema: { type: 'object', properties: {} } }],
    safeguards: [{ type: 'dangerous_tool_use', classifier_context: { v: 1, permission_mode: 'auto' } }],
    context_management: { edits: [{ type: 'clear_thinking_20251015', keep: 'all' }] },
    output_config: { effort: 'high' },
    messages: [
      { role: 'user', content: 'Inspect the synthetic lock.' },
      { role: 'assistant', content: [
        { type: 'thinking', thinking: '', signature: 'opaque-synthetic-signature' },
        { type: 'redacted_thinking', data: 'opaque-synthetic-data' },
        { type: 'text', text: 'The fence must be monotonic.' },
      ] },
      { role: 'user', content: 'Now review duplicate retries.' },
      { role: 'system', content: 'The task now requires deeper review.', output_config: { effort: 'high' } },
    ],
  };
  const before = structuredClone(body);
  for (const model of ['claude-opus-5', 'claude-opus-5-5']) {
    const { request, adjustments } = prepareRequest(body, model);
    assert.deepEqual(request, { ...before, model, thinking: { type: 'adaptive' } });
    assert.deepEqual(adjustments, ['adaptive_thinking_required']);
    for (const key of ['system', 'tools', 'safeguards', 'context_management', 'output_config', 'messages']) {
      assert.equal(request[key], body[key], `${key} must stay unchanged`);
    }
  }
  assert.deepEqual(body, before);
});

test('between_tools adaptation does not guess aliases, source models or extended thinking contracts', () => {
  for (const [source, target, thinking] of [
    ['claude-sonnet-5', 'claude-opus-5-5', { type: 'between_tools' }],
    ['claude-opus-5', 'claude-opus-5-5', { type: 'between_tools' }],
    ['sonnet', 'claude-opus-5-5', { type: 'between_tools' }],
    ['team/claude-sonnet-5-5', 'claude-opus-5-5', { type: 'between_tools' }],
    ['claude-sonnet-5-5-future', 'claude-opus-5-5', { type: 'between_tools' }],
    ['claude-sonnet-5-5', 'opus', { type: 'between_tools' }],
    ['claude-sonnet-5-5', 'claude-opus-4-8', { type: 'between_tools' }],
    ['claude-sonnet-5-5', 'claude-sonnet-5', { type: 'between_tools' }],
    ['claude-sonnet-5-5', 'team/claude-opus-5-5', { type: 'between_tools' }],
    ['claude-sonnet-5-5', 'claude-opus-5-5-future', { type: 'between_tools' }],
    ...[
      { type: 'between_tools', display: 'summarized' },
      { type: 'between_tools', budget_tokens: 1000 },
      { type: 'between_tools', block_binding: { prefix_mismatch_behavior: 'drop_block' } },
      { type: 'between_tools', future_setting: true },
    ].map(thinking => ['claude-sonnet-5-5', 'claude-opus-5-5', thinking]),
  ]) {
    const body = { model: source, thinking, messages: [{ role: 'user', content: 'Task' }] };
    const before = structuredClone(body);
    const result = prepareRequest(body, target);
    assert.deepEqual(result.request, { ...before, model: target }, `${source} -> ${target}`);
    assert.deepEqual(result.adjustments, []);
    assert.deepEqual(body, before);
  }
});

test('disabled-thinking extensions are never discarded during adaptation', () => {
  for (const thinking of [{ type: 'disabled', future_setting: true },
    { type: 'disabled', display: 'summarized' }, { type: 'disabled', block_binding: { prefix_mismatch_behavior: 'error' } }]) {
    const body = { model: 'claude-haiku-4-5', thinking, messages: [{ role: 'user', content: 'Task' }] };
    for (const model of ['claude-sonnet-5-5', 'claude-opus-5-5']) {
      const result = prepareRequest(body, model);
      assert.deepEqual(result, { request: { ...body, model }, adjustments: [] });
      assert.equal(result.request.thinking, thinking);
    }
  }
});
