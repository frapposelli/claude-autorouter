// Deterministic policy/continuity scenarios. No evaluator or provider calls.
let sequence = 0;
const emit = (op, input) => process.stdout.write(`${JSON.stringify({ id: `router-${sequence++}`, op, input })}\n`);
const user = content => ({ role: 'user', content });
const assistant = { role: 'assistant', content: 'Completed step.' };
const request = (model = 'claude-sonnet-5-5', content = 'Mechanical task') => ({ model, max_tokens: 4096, messages: [user(content)] });
const choice = tier => ({ tier, classified_tier: tier, source: 'jev', evaluator: 'jev', reason: 'classified', confidence: 0.98 });
const decisions = ['haiku', 'sonnet', 'opus'].map(choice).concat({ tier: 'opus', source: 'fallback', evaluator: 'jev', reason: 'classifier_unavailable', classifier_error: 'network_error' });
const review = [{ type: 'dangerous_tool_use', classifier_context: { v: 1, permission_mode: 'auto', opaque: 'SYNTHETIC_REVIEW_CANARY' } }];
const variants = [{}, { thinking: { type: 'disabled' } }, { thinking: { type: 'adaptive' } },
  { thinking: { type: 'enabled', budget_tokens: 1024 } }, { thinking: { type: 'between_tools' } }, { thinking: { type: 'future' } },
  { safeguards: review }, { safeguards: [] }, { safeguards: null }, { safeguards: [{ type: 'unknown' }] },
  { output_config: { effort: 'high' } }, { output_config: { effort: 'xhigh' } }, { max_tokens: 64001 },
  { context_management: { edits: [{ type: 'clear_thinking_20251015', keep: 'all' }] } },
  { context_management: { edits: [{ type: 'future' }] } }, { speed: 'fast' }, { container: 'synthetic' }, { mcp_servers: [] },
  { tools: [{ name: 'Synthetic', input_schema: { type: 'object' }, type: 'custom' }] },
  { tools: [{ name: 'Synthetic', type: 'computer_20250124' }] }, { future_extension: true },
  { temperature: 0.5 }, { messages: [user('Task'), { role: 'system', content: 'Turn instructions', clear_at: 'next_user_message' }] },
  { messages: [user('Earlier task'), { role: 'assistant', content: [{ type: 'thinking', thinking: 'SYNTHETIC_THOUGHT', signature: 'SYNTHETIC_SIGNATURE' }] }, user('New task')] },
  { messages: [user('Earlier task'), { role: 'assistant', content: [{ type: 'redacted_thinking', data: 'SYNTHETIC' }] }, user('New task')] },
  { messages: [user([{ type: 'image', source: { type: 'url', url: 'https://example.test/synthetic' } }])] }];
const models = ['claude-haiku-4-5-20251001', 'claude-haiku-4-5', 'claude-sonnet-5', 'claude-sonnet-5-5', 'claude-opus-5', 'claude-opus-5-5', 'claude-opus-4-7', 'future-model', 'custom-haiku-model'];
for (const profile of ['compatible', 'native', 'auto']) {
  const env = { AUTOROUTER_CLIENT_PROFILE: profile, AUTOROUTER_SONNET_MODEL: 'claude-sonnet-5-5' };
  for (const model of models) for (const patch of variants) for (const decision of decisions) {
    for (const requestClass of ['', 'main', 'auxiliary', 'compaction']) {
      emit('router', { env, steps: [{ op: 'route', body: { ...request(model), ...patch }, decision, count: 1200, options: { requestClass } }] });
    }
  }
}
const toolCall = { role: 'assistant', content: [{ type: 'tool_use', id: 't1', name: 'Read', input: { path: 'synthetic' } }] };
const toolResult = user([{ type: 'tool_result', tool_use_id: 't1', content: 'Synthetic result' }]);
for (const model of ['claude-haiku-4-5-20251001', 'claude-sonnet-5-5', 'claude-opus-5-5']) {
  for (const initial of decisions) for (const following of decisions) {
    for (const promptId of ['', 'same-prompt']) for (const confirmed of [null, model, 'claude-opus-5-5']) {
      const body = request(model); const options = { scope: 's/agent', promptId, requestId: 'r1' };
      const steps = [{ op: 'route', body, options, decision: initial },
        { op: 'complete', request_id: 'r1', evidence: confirmed ? { continuation_model: confirmed, tool_uses: [{ id: 't1', model: confirmed }] } : {} },
        { op: 'advance', ms: 7200000 },
        { op: 'route', body: { ...body, messages: [...body.messages, toolCall, toolResult] }, decision: following, options: { ...options, requestId: 'r2' } },
        { op: 'complete', request_id: 'r2', evidence: {} },
        { op: 'route', body: { ...body, messages: [...body.messages, assistant, user('New task')] }, decision: following, options: { scope: 's/agent', promptId: 'new-prompt' } }];
      emit('router', { steps });
    }
  }
}
const condition = 'Implement synthetic task and verify acceptance tests';
const goal = request('claude-haiku-4-5-20251001', `<command-name>/goal</command-name>\n<command-message>goal</command-message>\n<command-args>${condition}</command-args>`);
for (const promptId of ['', 'goal-prompt']) for (const scope of ['same', 'different']) {
  for (const patch of [{}, { thinking: { type: 'adaptive' } }, { model: 'claude-sonnet-5-5' }, { thinking: { type: 'between_tools' } }]) {
    const feedback = { ...goal, ...patch, messages: [...goal.messages, assistant, user(`Stop hook feedback:\n[${condition}]: Verification missing.`)] };
    emit('router', { steps: [{ op: 'route', body: goal, decision: choice('opus'), options: { scope: 'same', promptId } },
      { op: 'route', body: feedback, decision: choice('haiku'), options: { scope, promptId } },
      { op: 'route', body: { ...feedback, messages: [...feedback.messages, toolCall, toolResult] }, decision: choice('haiku'), options: { scope, promptId } }] });
  }
}
for (const field of ['system', 'messages', 'tools']) {
  const body = request('claude-haiku-4-5-20251001');
  const large = 'synthetic '.repeat(15001);
  if (field === 'system') body.system = large;
  if (field === 'messages') body.messages[0].content = large;
  if (field === 'tools') body.tools = [{ name: 'Read', description: large, input_schema: { type: 'object' } }];
  for (const count of [undefined, null, '1200', -1, 0, 190000, 190001, Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER + 1]) {
    for (const tier of ['haiku', 'opus']) emit('router', { steps: [{ op: 'route', body, decision: choice(tier), count }] });
  }
}
const deferred = { ...request('claude-haiku-4-5'), tools: [{ name: 'Search', input_schema: { type: 'object' } },
  { name: 'Deferred', input_schema: { type: 'object', properties: { query: { type: 'string', description: 'schema '.repeat(1000) } } }, defer_loading: true }] };
for (const patch of [{}, { defer_loading: 'true' }, { type: 'custom' }, { type: null }, { strict: 'true' }, { cache_control: {} },
  { allowed_callers: [null] }, { input_schema: { type: 'array' } }, { future_extension: true }, { description: null }]) {
  for (const content of ['Task', [{ type: 'tool_reference', tool_name: 'Deferred' }],
    [{ type: 'tool_reference', tool_name: 'Deferred' }, { type: 'tool_reference', tool_name: 'Deferred' }],
    [{ type: 'tool_result', content: [{ type: 'tool_reference', tool_name: 'Deferred' }] }],
    [{ type: 'future', payload: [{ type: 'tool_reference', tool_name: 'Deferred' }] }],
    [{ type: 'tool_reference', tool_name: null }], [{ type: 'tool_use', name: 'Deferred', input: {} }]]) {
    const body = { ...deferred, tools: [deferred.tools[0], { ...deferred.tools[1], ...patch }], messages: [user(content)] };
    for (const model of ['claude-haiku-4-5', 'unknown']) emit('context_size', { bytes: [...Buffer.from(JSON.stringify(body))], model });
  }
}
for (const limit of [0, 1, 2]) {
  const body = request();
  emit('router', { turn_entries: limit, steps: [
    { op: 'route', body, options: { scope: 'same', promptId: 'a', requestId: 'a' }, decision: choice('opus') },
    { op: 'route', body, options: { scope: 'same', promptId: 'b', requestId: 'b' }, decision: choice('haiku') },
    { op: 'complete', request_id: 'a', evidence: { continuation_model: 'claude-opus-5-5', tool_uses: [{ id: 't1', model: 'claude-opus-5-5' }] } },
    { op: 'complete', request_id: 'b', evidence: { continuation_model: 'claude-haiku-4-5-20251001', tool_uses: [{ id: 't1', model: 'claude-haiku-4-5-20251001' }] } },
    { op: 'route', body: { ...body, messages: [...body.messages, toolCall, toolResult] }, options: { scope: 'same' }, decision: choice('sonnet') },
  ] });
}
