// Synthetic UTF-16 budgets, wrapper recognition, privacy, and goal continuity.
let sequence = 0;
const emit = (op, input) => process.stdout.write(`${JSON.stringify({ id: `prompt-${sequence++}`, op, input })}\n`);
const text = text => ({ type: 'text', text });
const user = content => ({ role: 'user', content });
const assistant = { role: 'assistant', content: 'Work remains.' };
const goal = condition => user([text(`<command-name>/goal</command-name>\n<command-message>goal</command-message>\n<command-args>${condition}</command-args>`)]);
const feedback = (condition, reason = 'Verification remains.') => user([text(`Stop hook feedback:\n[${condition}]: ${reason}`)]);
const bytes = body => [...Buffer.from(JSON.stringify(body))];
const states = body => {
  for (const limit of [0, 1, 50, 150, 200, 499, 2000, 12000]) emit('build_state_json', { bytes: bytes(body), limit });
  emit('build_ollama_state_json', { bytes: bytes(body), limit: 3000 });
  for (const max_chars of [0, 1, 2, 3, 10, 50, 500]) emit('prompt_excerpt_json', { bytes: bytes(body), max_chars });
};
const tasks = ['', '  ', '\ufeff', '\u0085', 'task', 'A😀𐐷B', '\ud800A\udc00', '😀'.repeat(800),
  '<system-reminder>context</system-reminder>', '<available-deferred-tools>names</available-deferred-tools>',
  ' <system-reminder>a</system-reminder>\n<available-deferred-tools>b</available-deferred-tools> ',
  '<system-reminder>Context</system-reminder> Implement parser.', '<system-reminder>unfinished',
  'Explain <system-reminder> syntax.', 'FIRST ' + '\0"\\'.repeat(1500) + ' LAST',
  'FIRST ' + 'task context '.repeat(3000) + ' LAST', 'api_key=synthetic-long-secret-123456',
  'Cookie: synthetic-user=value; password=synthetic-secret-token', 'person@example.com',
  'password=\ud800secret\udc00token', 'curl --user ' + '\ud800'.repeat(64) + ':secret',
  'curl --user ' + '😀'.repeat(33) + ':secret'];
for (const task of tasks) {
  for (const content of [task, [text(task)], [text('<system-reminder>ignored</system-reminder>'), text(task)]]) {
    states({ system: [text('<system-reminder>setup</system-reminder>'), text('Review carefully')],
      tools: [{ name: 'Synthetic', input_schema: { type: 'object' } }],
      messages: [user('Original task'), assistant, user(content), assistant,
        user([{ type: 'tool_result', is_error: true, content: [text('START ' + 'output '.repeat(1000) + ' FAILURE')] }])] });
  }
}
for (const type of ['image', 'document', 'thinking', 'redacted_thinking', 'tool_use', 'tool_result', 'future', 'text']) {
  for (const value of [null, {}, [], [1, null, true], 0, false, 'secret canary']) {
    states({ messages: [user('Earlier task'), assistant, user([{ type, text: value, name: value,
      input: { password: 'PRIVATE_CANARY' }, thinking: 'PRIVATE_CANARY', source: { data: 'PRIVATE_CANARY' }, content: value }])] });
  }
}
for (const condition of ['Verify fixture', 'x'.repeat(510), 'x'.repeat(499) + '😀tail', 'x'.repeat(4001)]) {
  const prefix = condition.slice(0, 500).replace(/[\ud800-\udbff]$/, '');
  const short = `${prefix}… [+${condition.length - prefix.length} chars]`;
  for (const replacement of ['', 'clear', 'STOP', 'off', 'reset', 'none', 'cancel', 'New goal', 'x'.repeat(4001)]) {
    const messages = [goal(condition), assistant, feedback(short), assistant, feedback(condition),
      user('Human steering'), assistant, feedback(short), goal(replacement), assistant, feedback(condition)];
    emit('goal_feedback_indexes', messages); states({ messages });
  }
  for (const content of [`Stop hook feedback:\n[${condition}]: `, `Stop hook feedback:\n[${condition}]:\ufeff`,
    `Stop hook feedback:\n[${condition}]:\u0085`, `Stop hook feedback:\n[${condition}]: Remaining`,
    [text(`Stop hook feedback:\n[${condition}]: Remaining`), text('New task')],
    [text(`Stop hook feedback:\n[${condition}]: Remaining`), { type: 'tool_result', content: 'Result' }]]) {
    const messages = [goal(condition), assistant, user(content)];
    emit('goal_feedback_indexes', messages); states({ messages });
  }
}
for (const max_chars of [-1, 1.5, null, '500', false, {}, [], Number.MAX_SAFE_INTEGER + 1]) {
  emit('prompt_excerpt', { body: { messages: [user('Current task')] }, max_chars });
}
for (const body of [{}, { messages: null }, { messages: [] }, { messages: [assistant] }, { messages: [user('')] }]) states(body);
for (const content of ['😀'.repeat(1200), '\0'.repeat(3000), '\ud800'.repeat(3000), 'é'.repeat(3000)]) {
  for (const limit of [0, 1, 199, 200, 201, 500, 1000, 3000]) {
    emit('build_ollama_state_json', { bytes: bytes({ system: 'Background '.repeat(1000), messages: [user(content)] }), limit });
  }
}
