import test from 'node:test';
import assert from 'node:assert/strict';
import { buildState } from '../src/prompt-state.mjs';

const text = value => ({ type: 'text', text: value });
const request = messages => ({ model: 'claude-haiku-4-5-20251001', messages });

test('the actual task survives large Claude Code reminder and deferred-tool prefixes', () => {
  const task = 'Design a distributed lease with fencing tokens that remains safe under process pauses, duplicate delivery, and split brain. Explain the invariants and race conditions.';
  const body = request([{ role: 'user', content: [
    text(`<system-reminder>${'Repository setup instructions '.repeat(1400)}</system-reminder>`),
    text(`<available-deferred-tools>${'mcp__example__lookup\n'.repeat(2000)}</available-deferred-tools>`),
    text(task),
  ] }]);
  const state = buildState(body);
  assert.equal(state.current_task, task);
  assert.equal(state.original_task, task);
  assert.ok(!JSON.stringify(state).includes('Repository setup instructions'));
  assert.ok(JSON.stringify(state).length <= 12000);
});

test('a new human task takes priority over the original task and trailing tool results', () => {
  const body = request([
    { role: 'user', content: 'Implement a distributed lease.' },
    { role: 'assistant', content: 'Done.' },
    { role: 'user', content: [text('<system-reminder>New date</system-reminder>'), text('What does [].length return in JavaScript?')] },
    { role: 'assistant', content: [{ type: 'tool_use', id: 'read', name: 'Read', input: { private: 'PRIVATE_TOOL_INPUT' } }] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'read', is_error: true, content: [text('File lookup failed.')] }] },
    { role: 'user', content: [text('<system-reminder>Background task completed</system-reminder>')] },
  ]);
  const state = buildState(body);
  assert.equal(state.original_task, 'Implement a distributed lease.');
  assert.equal(state.current_task, 'What does [].length return in JavaScript?');
  assert.ok(state.recent_messages.some(message => message.content.includes('[tool result ERROR] File lookup failed.')));
  assert.ok(!JSON.stringify(state).includes('PRIVATE_TOOL_INPUT'));
});

test('tag mentions, mixed blocks, and malformed wrappers are preserved as human instructions', () => {
  for (const task of [
    'Explain what <system-reminder> means in this XML format.',
    '<system-reminder>Context</system-reminder> Now implement a parser.',
    '<available-deferred-tools>Missing closing tag: explain this syntax error.',
    '<system-reminder>Keep this exact literal string</system-reminder>',
  ]) {
    // Plain message strings have no separately attached client reminder block.
    assert.equal(buildState(request([{ role: 'user', content: task }])).current_task, task);
    if (!task.endsWith('</system-reminder>')) {
      assert.equal(buildState(request([{ role: 'user', content: [text(task)] }])).current_task, task);
    }
  }
});

test('long tasks retain both ends and receive most of the budget before background context', () => {
  const body = request([
    { role: 'user', content: 'Old task '.repeat(5000) },
    { role: 'assistant', content: 'Old answer '.repeat(5000) },
    { role: 'user', content: 'FIRST_INSTRUCTION ' + 'Detailed task constraints '.repeat(3000) + ' FINAL_QUESTION' },
  ]);
  body.system = 'Background system instructions '.repeat(1000);
  const state = buildState(body);
  assert.ok(state.current_task.startsWith('FIRST_INSTRUCTION'));
  assert.ok(state.current_task.endsWith('FINAL_QUESTION'));
  assert.ok(state.current_task.length >= 6000);
  assert.ok(state.original_task.length > 0);
  assert.ok(state.system.length > 0);
  assert.ok(JSON.stringify(state).length <= 12000);
});

test('escaping stays within both normal and small budgets without exposing non-text payloads', () => {
  const body = request([
    { role: 'user', content: 'original\u0000'.repeat(1000) },
    { role: 'assistant', content: [
      { type: 'thinking', thinking: 'PRIVATE_THINKING', signature: 'PRIVATE_SIGNATURE' },
      { type: 'redacted_thinking', data: 'PRIVATE_REDACTED' },
      { type: 'tool_use', name: 'Read', input: { value: 'PRIVATE_INPUT' } },
    ] },
    { role: 'user', content: [{ type: 'tool_result', content: [
      text('Result starts ' + '\u0000'.repeat(8000) + ' result ends'),
      { type: 'image', source: { data: 'PRIVATE_IMAGE_BYTES' } },
    ] }] },
    { role: 'user', content: 'CURRENT_START ' + '\u0000"\\'.repeat(1000) + ' CURRENT_END' },
  ]);
  body.system = '\u0000'.repeat(1000);
  for (const limit of [12000, 2000]) {
    const state = buildState(body, limit);
    const serialized = JSON.stringify(state);
    assert.ok(serialized.length <= limit);
    assert.ok(state.current_task.startsWith('CURRENT_START'));
    assert.ok(state.current_task.endsWith('CURRENT_END'));
    assert.ok(state.original_task.length > 0);
    assert.ok(state.system.length > 0);
    assert.ok(!serialized.includes('PRIVATE_'));
  }
});

test('balanced history excerpts retain tool errors at the end of long results', () => {
  const body = request([
    { role: 'user', content: 'Fix the failed test.' },
    { role: 'assistant', content: [{ type: 'tool_use', id: 'test', name: 'Bash', input: { command: 'secret command' } }] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'test', is_error: true,
      content: 'START_OF_OUTPUT ' + 'ordinary test output\n'.repeat(2000) + ' FAILURE_AT_END: race detected' }] },
  ]);
  const state = buildState(body);
  const result = state.recent_messages.find(message => message.content.startsWith('[tool result ERROR]'));
  assert.ok(result.content.includes('START_OF_OUTPUT'));
  assert.ok(result.content.endsWith('FAILURE_AT_END: race detected'));
  assert.ok(!JSON.stringify(state).includes('secret command'));
});
