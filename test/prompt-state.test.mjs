import test from 'node:test';
import assert from 'node:assert/strict';
import { buildState, goalFeedbackIndexes, promptExcerpt } from '../src/prompt-state.mjs';

const text = value => ({ type: 'text', text: value });
const request = messages => ({ model: 'claude-haiku-4-5-20251001', messages });
const goalCommand = condition => ({ role: 'user', content: [text(`<command-name>/goal</command-name>\n<command-message>goal</command-message>\n<command-args>${condition}</command-args>`)] });
const goalFeedback = (condition, reason = 'Verification is still missing.') => ({ role: 'user', content: [text(`Stop hook feedback:\n[${condition}]: ${reason}`)] });
const assistant = { role: 'assistant', content: 'Work is partly complete.' };

test('prompt excerpts contain only the latest direct human text, never tool or multimodal payloads', () => {
  const unreadable = (type, properties) => Object.defineProperties({ type }, Object.fromEntries(
    properties.map(key => [key, { get() { assert.fail(`Excerpt read ${type}.${key}`); } }])));
  const body = request([
    { role: 'user', content: 'Original task that should not be logged again.' },
    { role: 'assistant', get content() { assert.fail('Excerpt read assistant output'); } },
    { role: 'user', content: [
      text(`<system-reminder>${'Synthetic setup instructions '.repeat(1500)}</system-reminder>`),
      text('<available-deferred-tools>mcp__synthetic__lookup</available-deferred-tools>'),
      text('Describe the attached diagram.'),
      unreadable('image', ['source']), unreadable('document', ['source', 'title', 'context']),
      unreadable('thinking', ['thinking', 'signature']), unreadable('redacted_thinking', ['data']),
      unreadable('tool_use', ['name', 'input']), unreadable('future_block', ['text', 'content']),
      text('Keep the answer brief.'),
    ] },
    { role: 'assistant', get content() { assert.fail('Excerpt read assistant tool calls'); } },
    { role: 'user', content: [text('Tool-result metadata is not a human task.'), unreadable('tool_result', ['content'])] },
    { role: 'user', content: '<system-reminder>Background work finished.</system-reminder>' },
  ]);
  Object.defineProperties(body, {
    system: { get() { assert.fail('Excerpt read system instructions'); } },
    tools: { get() { assert.fail('Excerpt read tool definitions'); } },
  });
  assert.equal(promptExcerpt(body), 'Describe the attached diagram.\nKeep the answer brief.');
});

test('prompt excerpts follow the established goal and human steering instead of Stop feedback', () => {
  const condition = 'Implement a fixture and verify its acceptance test passes.';
  const messages = [goalCommand(condition), assistant, goalFeedback(condition), assistant,
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'test', content: 'Synthetic test output.' }] },
    assistant, goalFeedback(condition, 'A final verification is missing.')];
  const body = request(messages);
  const before = structuredClone(body);
  assert.equal(promptExcerpt(body), buildState(body).current_task);
  assert.ok(promptExcerpt(body).includes(condition));
  assert.ok(!promptExcerpt(body).includes('Stop hook feedback:'));
  assert.deepEqual(body, before);
  messages.push({ role: 'user', content: 'Use the alternate fixture directory.' }, assistant, goalFeedback(condition));
  assert.equal(promptExcerpt(body), 'Use the alternate fixture directory.');
  messages.push(assistant, goalFeedback('An unmatched condition'));
  assert.equal(promptExcerpt(body), messages.at(-1).content[0].text, 'Unrecognized feedback may be an actual human message');
});

test('new non-text human tasks have empty excerpts rather than stale earlier task text', () => {
  for (const block of [
    { type: 'image', source: { data: 'synthetic-image' } },
    { type: 'document', source: { data: 'synthetic-document' } },
    { type: 'thinking', thinking: 'synthetic-reasoning' },
    { type: 'tool_use', name: 'Synthetic', input: { text: 'synthetic-input' } },
    { type: 'future_block', text: 'Unknown blocks are not direct human text.' },
    { type: 'text', text: { nested: 'Malformed text must not be coerced.' } },
  ]) {
    assert.equal(promptExcerpt(request([
      { role: 'user', content: 'An earlier unrelated task.' }, assistant,
      { role: 'user', content: [text('<system-reminder>New context</system-reminder>'), block] },
    ])), '');
  }
  for (const body of [{}, { messages: null }, request([]), request([assistant]),
    request([{ role: 'user', content: '   ' }]),
    request([{ role: 'user', content: [text('<available-deferred-tools>Names only</available-deferred-tools>')] }]),
  ]) assert.equal(promptExcerpt(body), '');
});

test('prompt excerpt limits count Unicode code points and never leave invalid surrogate pairs', () => {
  const body = request([{ role: 'user', content: '😀'.repeat(600) }]);
  assert.equal(promptExcerpt(body), '😀'.repeat(500));
  for (const maxChars of [0, 1, 2, 3, 500, 501]) {
    const result = promptExcerpt(body, maxChars);
    assert.equal(Array.from(result).length, maxChars);
    assert.ok(result.isWellFormed());
  }
  const mixed = request([{ role: 'user', content: [text('A😀'), text('𐐷B')] }]);
  assert.equal(promptExcerpt(mixed, 4), 'A😀\n𐐷');
  assert.equal(promptExcerpt(request([{ role: 'user', content: '\ud800A\udc00' }])), '\ufffdA\ufffd');
  for (const invalid of [-1, 1.5, Infinity, NaN, '500', Number.MAX_SAFE_INTEGER + 1]) {
    assert.throws(() => promptExcerpt(body, invalid), /maxChars/);
  }
});

test('bounded excerpts avoid building evaluator history or reading text after the limit', () => {
  const body = request([
    { role: 'user', get content() { assert.fail('An ordinary excerpt does not need earlier task text'); } },
    { role: 'user', content: [text('Current task '.repeat(10000)),
      { type: 'text', get text() { assert.fail('Text after the excerpt limit must not be collected'); } }] },
  ]);
  assert.equal(promptExcerpt(body, 12), 'Current task');
  assert.equal(promptExcerpt(body, 0), '');
  for (const task of ['Explain <system-reminder> in this syntax.',
    '<system-reminder>Context</system-reminder> Implement the parser.',
    '<available-deferred-tools>Explain the missing closing tag.']) {
    assert.equal(promptExcerpt(request([{ role: 'user', content: [text(task)] }])), task);
  }
});

test('exact goal feedback retains the human task and remains visible as history without changing messages', () => {
  const condition = 'Implement the feature and verify all acceptance tests pass.';
  const messages = [goalCommand(condition), assistant, goalFeedback(condition), assistant,
    { role: 'user', content: `Stop hook feedback:\n[${condition}]: One test still fails.` }];
  messages[2].content[0].cache_control = { type: 'ephemeral' };
  const before = structuredClone(messages);
  assert.deepEqual([...goalFeedbackIndexes(messages)], [2, 4]);
  const state = buildState(request(messages));
  assert.equal(state.current_task, messages[0].content[0].text);
  assert.equal(state.original_task, state.current_task);
  assert.ok(state.recent_messages.some(message => message.content.includes('Verification is still missing.')));
  assert.ok(state.recent_messages.some(message => message.content.includes('One test still fails.')));
  assert.deepEqual(messages, before);
});

test('ordinary, unmatched, quoted, malformed, and mixed hook text remains a human task', () => {
  const condition = 'Verify the fixture.';
  const valid = `Stop hook feedback:\n[${condition}]: Work remains.`;
  for (const content of [
    'Stop hook feedback: Work remains.',
    `Stop hook feedback:\n[Another goal]: Work remains.`,
    `Explain this quote: ${valid}`,
    `\n${valid}`,
    `Stop hook feedback:\n[${condition}]:   `,
    `Stop hook feedback:\n[${condition}] Work remains.`,
    [text(valid), text('This is a new human request.')],
    [text(valid), { type: 'tool_result', tool_use_id: 'a', content: 'Result' }],
    [text(valid), text('<system-reminder>Extra context</system-reminder>')],
  ]) {
    const messages = [goalCommand(condition), assistant, { role: 'user', content }];
    assert.deepEqual([...goalFeedbackIndexes(messages)], []);
    if (typeof content === 'string') assert.equal(buildState(request(messages)).current_task, content);
  }
  for (const messages of [
    [assistant, goalFeedback(condition)],
    [goalCommand(condition), goalFeedback(condition)],
    [{ role: 'user', content: `Explain <command-name>/goal</command-name> and <command-args>${condition}</command-args>.` }, assistant, goalFeedback(condition)],
  ]) assert.deepEqual([...goalFeedbackIndexes(messages)], []);
});

test('human steering remains the current task while later feedback still refers to the active goal', () => {
  const condition = 'Verify the fixture.';
  const messages = [goalCommand(condition), assistant, goalFeedback(condition), assistant,
    { role: 'user', content: 'Use the alternate test directory instead.' }, assistant, goalFeedback(condition)];
  assert.deepEqual([...goalFeedbackIndexes(messages)], [2, 6]);
  assert.equal(buildState(request(messages)).current_task, 'Use the alternate test directory instead.');
});

test('replacing or clearing a goal resets feedback recognition while a status query retains it', () => {
  const condition = 'Verify the original fixture.';
  for (const replacement of ['clear', 'stop', 'off', 'reset', 'none', 'cancel', 'Verify the new fixture.']) {
    const messages = [goalCommand(condition), assistant, goalFeedback(condition), goalCommand(replacement), assistant, goalFeedback(condition)];
    assert.deepEqual([...goalFeedbackIndexes(messages)], [2]);
    if (replacement.startsWith('Verify')) {
      messages.push(assistant, goalFeedback(replacement));
      assert.deepEqual([...goalFeedbackIndexes(messages)], [2, 7]);
    }
  }
  assert.deepEqual([...goalFeedbackIndexes([goalCommand(condition), goalCommand(''), assistant, goalFeedback(condition)])], [3]);
});

test('short repeated condition labels require earlier full feedback and the exact UTF-16 truncation', () => {
  for (const [condition, short] of [
    ['x'.repeat(510), `${'x'.repeat(500)}… [+10 chars]`],
    [`${'x'.repeat(499)}😀tail`, `${'x'.repeat(499)}… [+6 chars]`],
  ]) {
    const firstShort = [goalCommand(condition), assistant, goalFeedback(short)];
    assert.deepEqual([...goalFeedbackIndexes(firstShort)], []);
    const messages = [goalCommand(condition), assistant, goalFeedback(condition), assistant, goalFeedback(short)];
    assert.deepEqual([...goalFeedbackIndexes(messages)], [2, 4]);
    messages.push(assistant, goalFeedback(short.replace(/\[\+\d+ chars\]/, '[+999 chars]')));
    assert.deepEqual([...goalFeedbackIndexes(messages)], [2, 4]);
    messages.push(goalCommand(condition), assistant, goalFeedback(short));
    assert.deepEqual([...goalFeedbackIndexes(messages)], [2, 4]);
  }
});

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
