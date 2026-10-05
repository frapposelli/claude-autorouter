import test from 'node:test';
import assert from 'node:assert/strict';
import { canRouteAutoRequest, hasRoutableSafeguards, targetCompatibility } from '../src/auto-routing.mjs';

const models = ['claude-sonnet-5', 'claude-sonnet-5-5', 'claude-opus-5', 'claude-opus-5-5'];
const request = overrides => ({
  model: 'claude-sonnet-5-5',
  max_tokens: 32000,
  messages: [{ role: 'user', content: 'Review the locking implementation.' }],
  ...overrides,
});

function freeze(value) {
  if (value && typeof value === 'object') {
    Object.values(value).forEach(freeze);
    Object.freeze(value);
  }
  return value;
}

test('modern Auto requests retain the complete safeguards, context edits and signed history', () => {
  const body = freeze(request({
    system: [{ type: 'text', text: 'Synthetic system instructions.' }],
    safeguards: [{ type: 'dangerous_tool_use', classifier_context: {
      v: 1, permission_mode: 'auto', policy: { denied_commands: ['rm -rf /'] },
    } }],
    thinking: { type: 'adaptive', display: 'summarized', block_binding: { prefix_mismatch_behavior: 'error' } },
    tools: [
      { type: 'tool_search_tool_regex_20251119', name: 'tool_search_tool_regex' },
      { name: 'Read', input_schema: { type: 'object', properties: {} }, defer_loading: true },
    ],
    context_management: { edits: [
      { type: 'clear_thinking_20251015', keep: 'all' },
      { type: 'clear_tool_uses_20250919', trigger: { type: 'input_tokens', value: 900000 },
        keep: { type: 'tool_uses', value: 5 }, exclude_tools: ['Read'] },
    ] },
    messages: [
      { role: 'user', content: 'Review this synthetic lock.' },
      { role: 'assistant', content: [
        { type: 'thinking', thinking: '', signature: 'synthetic-opaque-signature' },
        { type: 'redacted_thinking', data: 'synthetic-opaque-data' },
        { type: 'text', text: 'The lease must be checked atomically.' },
      ] },
      { role: 'user', content: 'Now consider duplicate retries.' },
      { role: 'system', content: 'Use the current repository policy.', output_config: { effort: 'high' } },
    ],
  }));
  const before = structuredClone(body);
  assert.equal(hasRoutableSafeguards(body), true);
  for (const target of ['claude-sonnet-5-5', 'claude-opus-5', 'claude-opus-5-5']) {
    assert.equal(canRouteAutoRequest(body, target), true, target);
  }
  assert.deepEqual(body, before);
});

test('ordinary requests route among exact supported model IDs without requiring Auto safeguards', () => {
  for (const source of models) {
    for (const target of models) {
      assert.equal(canRouteAutoRequest(request({ model: source }), target), true, `${source} -> ${target}`);
    }
  }
  for (const model of ['sonnet', 'opus', 'claude-sonnet-4-6', 'claude-opus-4-8',
    'claude-haiku-4-5-20251001', 'team/claude-opus-5-5', 'claude-opus-5-5-future']) {
    assert.equal(canRouteAutoRequest(request(), model), false, `target ${model}`);
    assert.equal(canRouteAutoRequest(request({ model }), 'claude-opus-5-5'), false, `source ${model}`);
  }
});

test('an unrecognized safeguards version or mixed contract is retained on the requested model', () => {
  const known = { type: 'dangerous_tool_use', classifier_context: { v: 1 } };
  for (const safeguards of [null, {}, [], [null], [known, { type: 'future_safeguard', classifier_context: { v: 1 } }],
    [{ ...known, classifier_context: { v: 2 } }], [{ ...known, classifier_context: { v: '1' } }],
    [{ type: 'dangerous_tool_use' }], [{ ...known, classifier_context: [] }]]) {
    const body = request({ safeguards });
    assert.equal(hasRoutableSafeguards(body), false, JSON.stringify(safeguards));
    assert.equal(canRouteAutoRequest(body, 'claude-opus-5-5'), false, JSON.stringify(safeguards));
  }
  assert.equal(hasRoutableSafeguards(request()), false);
  assert.equal(hasRoutableSafeguards(request({ model: 'claude-haiku-4-5-20251001', safeguards: [known] })), false);
});

test('shared client and tool-search tools do not pin a compatible request', () => {
  const tools = [
    { name: 'Read', input_schema: { type: 'object', properties: {} } },
    { type: 'custom', name: 'Write', input_schema: { type: 'object', properties: {} } },
    { type: 'tool_search_tool_regex_20251119', name: 'tool_search_tool_regex' },
    { type: 'tool_search_tool_bm25_20251119', name: 'tool_search_tool_bm25' },
    { type: 'bash_20250124', name: 'bash' },
    { type: 'text_editor_20250728', name: 'str_replace_based_edit_tool' },
  ];
  for (const target of models) {
    assert.equal(canRouteAutoRequest(request({ tools }), target), true, target);
  }
});

test('model-specific server tools and unknown tool types prevent a cross-model route', () => {
  for (const type of ['advisor_20260301', 'computer_20251124', 'computer_toolset_20260801',
    'web_search_20260318', 'code_execution_20260521', 'mcp_toolset', 'future_tool_20270101']) {
    const body = request({ tools: [{ type, name: 'synthetic_tool', model: 'claude-sonnet-5-5' }] });
    assert.equal(canRouteAutoRequest(body, 'claude-opus-5-5'), false, type);
  }
  for (const tools of [null, {}, [null], [[]]]) {
    assert.equal(canRouteAutoRequest(request({ tools }), 'claude-opus-5-5'), false);
  }
});

test('inline shared tool definitions and references keep their system-message semantics unchanged', () => {
  const body = freeze(request({
    tools: [{ name: 'Existing', input_schema: { type: 'object', properties: {} }, defer_loading: true }],
    messages: [
      { role: 'user', content: 'Use the synthetic local tools.' },
      { role: 'system', content: [
        { type: 'tool_addition', tool: { type: 'tool_definition', definition: {
          name: 'Read', input_schema: { type: 'object', properties: {} },
        } } },
        { type: 'tool_addition', tool: { type: 'tool_definition', definition: {
          type: 'bash_20250124', name: 'bash',
        } } },
        { type: 'tool_addition', tool: { type: 'tool_reference', name: 'Existing' } },
        { type: 'tool_removal', tool: { type: 'tool_reference', name: 'Read' } },
      ] },
    ],
  }));
  const before = structuredClone(body);
  for (const target of ['claude-sonnet-5-5', 'claude-opus-5', 'claude-opus-5-5']) {
    assert.equal(canRouteAutoRequest(body, target), true, target);
  }
  assert.equal(canRouteAutoRequest(body, 'claude-sonnet-5'), false);
  assert.deepEqual(body, before);
});

test('inline tool additions cannot bypass typed-tool and unknown-reference compatibility guards', () => {
  const blockedTools = [
    ...['computer_20251124', 'advisor_20260301', 'mcp_toolset', 'future_tool_20270101'].map(type => ({
      type: 'tool_definition', definition: { type, name: 'Synthetic', model: 'claude-sonnet-5-5' },
    })),
    { type: 'tool_definition', definition: null },
    { type: 'tool_definition' },
    { type: 'mcp_tool_reference', name: 'Synthetic', server_name: 'local' },
    { type: 'mcp_toolset_reference', server_name: 'local' },
    { type: 'future_tool_reference', name: 'Synthetic' },
    null,
  ];
  for (const tool of blockedTools) {
    const body = request({ messages: [
      { role: 'user', content: 'Task' },
      { role: 'system', content: [{ type: 'tool_addition', tool }] },
    ] });
    assert.equal(canRouteAutoRequest(body, 'claude-opus-5-5'), false, JSON.stringify(tool));
  }
  for (const tool of [
    { type: 'tool_definition', definition: { name: 'Read', input_schema: { type: 'object' } } },
    { type: 'mcp_tool_reference', name: 'Synthetic', server_name: 'local' },
    { type: 'mcp_toolset_reference', server_name: 'local' },
    { type: 'future_tool_reference', name: 'Synthetic' },
  ]) {
    const body = request({ messages: [
      { role: 'user', content: 'Task' },
      { role: 'system', content: [{ type: 'tool_removal', tool }] },
    ] });
    assert.equal(canRouteAutoRequest(body, 'claude-opus-5-5'), false, JSON.stringify(tool));
  }
});

test('context management accepts only the two proven shared edit types', () => {
  for (const context_management of [[], {}, { edits: null }, { edits: [null] },
    { edits: [{ type: 'compact_20260112' }] }, { edits: [{ type: 'future_edit' }] },
    { edits: [{ type: 'clear_thinking_20251015', keep: 'all' }], future_setting: true }]) {
    assert.equal(canRouteAutoRequest(request({ context_management }), 'claude-opus-5-5'), false,
      JSON.stringify(context_management));
  }
});

test('finite tool and execution envelopes reject unfamiliar fields without changing native requests', () => {
  const variants = [
    ...['auto', 'none', 'any', 'tool'].map(type => [{ tool_choice: { type, ...(type === 'tool' ? { name: 'Read' } : {}),
      future_execution_contract: { version: 1 } } }, 'tool_choice']),
    ...['bash_20250124', 'text_editor_20250728', 'tool_search_tool_regex_20251119', 'tool_search_tool_bm25_20251119']
      .map(type => [{ tools: [{ type, name: 'Synthetic', future_execution_contract: true }] }, 'tool_type']),
    [{ tools: [{ name: 'Read', input_schema: { type: 'object' },
      cache_control: { type: 'ephemeral', future_binding: true } }] }, 'tool_type'],
    [{ cache_control: { type: 'ephemeral', future_binding: true } }, 'request_extension'],
    [{ thinking: { type: 'adaptive', block_binding: { prefix_mismatch_behavior: 'error', future_binding: true } } }, 'thinking_extension'],
    [{ output_config: { format: { type: 'json_schema', schema: {}, future_output_contract: true } } }, 'output_extension'],
    [{ output_config: { task_budget: { type: 'tokens', total: 1000, future_budget_contract: true } } }, 'task_budget'],
    [{ messages: [{ role: 'user', content: [{ type: 'tool_use', id: 'call-1', name: 'Read', input: {},
      caller: { type: 'direct', future_execution_contract: true } }] }] }, 'content_extension'],
    [{ safeguards: [{ type: 'dangerous_tool_use', classifier_context: { v: 1 }, future_safety_contract: true }] }, 'safeguards'],
    [{ messages: [{ role: 'system', content: [{ type: 'tool_addition', tool: { type: 'tool_definition', definition: {
      type: 'bash_20250124', name: 'bash', future_execution_contract: true,
    } } }] }] }, 'inline_tool'],
  ];
  for (const [extra, reason] of variants) {
    const body = freeze(request(extra));
    const before = structuredClone(body);
    for (const autoMode of [false, true]) {
      assert.deepEqual(targetCompatibility(body, 'claude-opus-5-5', { autoMode }), { compatible: false, reason });
      assert.deepEqual(targetCompatibility(body, body.model, { autoMode }), { compatible: true });
    }
    assert.deepEqual(body, before);
  }
});

test('documented built-in tool, output and thinking envelopes retain all opaque data while routing', () => {
  const opaque = { future_contract: { arbitrary: 'untouched' } };
  const cache_control = { type: 'ephemeral', ttl: '1h' };
  const shared = { cache_control, defer_loading: true, strict: true, allowed_callers: ['direct'] };
  const body = freeze(request({
    cache_control,
    tool_choice: { type: 'auto', disable_parallel_tool_use: true },
    tools: [
      { type: 'custom', name: 'Read', input_schema: opaque, input_examples: [opaque], eager_input_streaming: null, ...shared },
      { type: 'bash_20250124', name: 'bash', input_examples: [opaque], ...shared },
      { type: 'text_editor_20250728', name: 'str_replace_based_edit_tool', max_characters: 10000, input_examples: [opaque], ...shared },
      { type: 'tool_search_tool_regex_20251119', name: 'tool_search_tool_regex', ...shared },
      { type: 'tool_search_tool_bm25_20251119', name: 'tool_search_tool_bm25', ...shared },
    ],
    thinking: { type: 'adaptive', display: 'updates', block_binding: { prefix_mismatch_behavior: 'error' } },
    output_config: { effort: 'high', format: { type: 'json_schema', schema: opaque },
      task_budget: { type: 'tokens', total: 10000, remaining: 7000 } },
    safeguards: [{ type: 'dangerous_tool_use', classifier_context: { v: 1, opaque } }],
  }));
  const before = structuredClone(body);
  for (const autoMode of [false, true]) assert.deepEqual(targetCompatibility(body, 'claude-opus-5-5', { autoMode }), { compatible: true });
  assert.deepEqual(body, before);
  for (const tool_choice of [{ type: 'any', disable_parallel_tool_use: true },
    { type: 'tool', name: 'Read', disable_parallel_tool_use: true }, { type: 'none' }]) {
    const forced = freeze(request({ model: 'claude-haiku-4-5', tool_choice }));
    assert.equal(targetCompatibility(forced, 'claude-sonnet-5').compatible, true);
  }
  const enabled = freeze(request({ model: 'claude-sonnet-4-5', thinking: {
    type: 'enabled', budget_tokens: 2048, display: 'summarized', block_binding: { prefix_mismatch_behavior: 'error' },
  } }));
  assert.equal(targetCompatibility(enabled, 'claude-opus-4-5').compatible, true);
});

test('documented null optional envelopes preserve their default meaning and wire representation', () => {
  const body = freeze(request({
    cache_control: null, context_management: null,
    thinking: { type: 'adaptive', block_binding: null },
    output_config: { effort: null, format: null, task_budget: null },
    tools: [{ type: null, name: 'Read', input_schema: { type: 'object' }, cache_control: null }],
    messages: [
      { role: 'user', content: 'Task' },
      { role: 'system', content: 'Reminder', output_config: null },
      { role: 'system', content: 'Default effort', output_config: { effort: null } },
    ],
  }));
  const before = structuredClone(body);
  for (const autoMode of [false, true]) assert.deepEqual(targetCompatibility(body, 'claude-opus-5-5', { autoMode }), { compatible: true });
  assert.deepEqual(body, before);
});

test('unknown semantic message, custom-tool and edit fields retain the source model without rejecting opaque task data', () => {
  for (const [extra, reason] of [
    [{ messages: [{ role: 'user', content: 'Task', future_request_contract: { version: 1 } }] }, 'content_extension'],
    [{ tools: [{ name: 'Read', input_schema: { type: 'object' }, future_execution_contract: { version: 1 } }] }, 'tool_type'],
    [{ context_management: { edits: [{ type: 'clear_thinking_20251015', keep: 'all', future_edit_contract: { version: 1 } }] } }, 'context_management'],
    [{ context_management: { edits: [{ type: 'clear_tool_uses_20250919', trigger: { type: 'input_tokens', value: 100, future_limit: true } }] } }, 'context_management'],
    [{ messages: [{ role: 'system', content: [{ type: 'tool_addition', tool: { type: 'tool_definition', definition: {
      name: 'Read', input_schema: { type: 'object' }, future_execution_contract: true,
    } } }] }] }, 'inline_tool'],
  ]) {
    const body = freeze(request(extra));
    const before = structuredClone(body);
    for (const autoMode of [false, true]) {
      assert.deepEqual(targetCompatibility(body, 'claude-opus-5-5', { autoMode }), { compatible: false, reason });
      assert.deepEqual(targetCompatibility(body, body.model, { autoMode }), { compatible: true });
    }
    assert.deepEqual(body, before);
  }
  const opaque = { future_contract: { version: 1, nested: { anything: true } } };
  const body = freeze(request({
    tools: [{ type: 'custom', name: 'Read', description: 'Read synthetic data', input_schema: opaque, input_examples: [opaque],
      cache_control: { type: 'ephemeral' }, defer_loading: true, strict: true, allowed_callers: ['direct'], eager_input_streaming: true }],
    context_management: { edits: [
      { type: 'clear_thinking_20251015', keep: { type: 'thinking_turns', value: 1 } },
      { type: 'clear_tool_uses_20250919', clear_at_least: { type: 'input_tokens', value: 100 }, clear_tool_inputs: ['Read'],
        exclude_tools: ['Keep'], keep: { type: 'tool_uses', value: 2 }, trigger: { type: 'input_tokens', value: 500 } },
    ] },
    messages: [
      { role: 'assistant', content: [{ type: 'thinking', signature: 'opaque-signed-history', thinking: 'Reasoning' },
        { type: 'tool_use', id: 'tool-1', name: 'Read', input: opaque }] },
      { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'tool-1', content: [{ type: 'document', source: opaque }] }] },
      { role: 'system', clear_at: 'next_user_message', content: 'Synthetic turn reminder.' },
    ],
  }));
  const before = structuredClone(body);
  assert.equal(canRouteAutoRequest(body, 'claude-opus-5-5'), true);
  assert.deepEqual(body, before);
});

test('known content types with unfamiliar semantic fields cannot cross a model boundary', () => {
  const blocks = [
    { type: 'text', text: 'Task', future_contract: true },
    { type: 'thinking', thinking: 'Reasoning', signature: 'opaque-signature', future_binding: true },
    { type: 'redacted_thinking', data: 'opaque-encrypted-data', future_binding: true },
    { type: 'tool_use', id: 'call-1', name: 'Read', input: {}, future_execution_contract: true },
    { type: 'tool_result', tool_use_id: 'call-1', content: 'Result', future_execution_contract: true },
    { type: 'image', source: { type: 'url', url: 'https://example.test/image' }, future_contract: true },
    { type: 'document', source: { type: 'text', media_type: 'text/plain', data: 'Data' }, future_contract: true },
    { type: 'tool_reference', tool_name: 'Read', future_execution_contract: true },
    { type: 'text', text: 'Task', cache_control: { type: 'ephemeral', future_binding: true } },
    { type: 'tool_search_tool_result', tool_use_id: 'search-1', content: {
      type: 'tool_search_tool_search_result', tool_references: [], future_execution_contract: true,
    } },
    { type: 'tool_search_tool_result', tool_use_id: 'search-1', content: {
      type: 'tool_search_tool_search_result', tool_references: [{ type: 'tool_reference', tool_name: 'Read', future_execution_contract: true }],
    } },
  ];
  for (const block of blocks) for (const wrapped of [false, true]) {
    const body = freeze(request({ messages: [{ role: 'user', content: wrapped
      ? [{ type: 'tool_result', tool_use_id: 'outer-call', content: [block] }] : [block] }] }));
    const before = structuredClone(body);
    for (const autoMode of [false, true]) {
      assert.deepEqual(targetCompatibility(body, 'claude-opus-5-5', { autoMode }), { compatible: false, reason: 'content_extension' });
      assert.deepEqual(targetCompatibility(body, body.model, { autoMode }), { compatible: true });
    }
    assert.deepEqual(body, before);
  }
  const system = request({ system: [{ type: 'text', text: 'Instructions', future_contract: true }] });
  assert.equal(targetCompatibility(system, 'claude-opus-5-5').reason, 'content_extension');
  const inline = request({ messages: [{ role: 'system', content: [{ type: 'tool_addition',
    tool: { type: 'tool_reference', name: 'Read', future_binding: true } }] }] });
  assert.equal(targetCompatibility(inline, 'claude-opus-5-5').reason, 'inline_tool');
});

test('documented block attributes remain routable and opaque payloads round-trip without inspection', () => {
  const opaque = { future_contract: { type: 'future_block', nested: { arbitrary: true } } };
  const cache_control = { type: 'ephemeral', ttl: '1h' };
  const body = freeze(request({
    messages: [
      { role: 'assistant', content: [
        { type: 'thinking', signature: 'signed+opaque/==', thinking: 'Original reasoning remains byte-for-byte.' },
        { type: 'redacted_thinking', data: 'encrypted+opaque/==' },
        { type: 'tool_use', id: 'call-1', name: 'Read', input: opaque, caller: { type: 'direct' }, toolset_name: null, cache_control },
      ] },
      { role: 'user', content: [
        { type: 'text', text: 'Current task', citations: [opaque], cache_control },
        { type: 'image', source: opaque, transformations: { oversized_image: 'downsize' }, cache_control },
        { type: 'document', source: opaque, citations: opaque, title: 'Synthetic document', context: 'Context', cache_control },
        { type: 'tool_result', tool_use_id: 'call-1', is_error: false, toolset_name: null, cache_control,
          content: [{ type: 'text', text: 'Read result', citations: [opaque] }, { type: 'tool_reference', tool_name: 'Read', cache_control }] },
        { type: 'tool_search_tool_result', tool_use_id: 'search-1', cache_control, content: {
          type: 'tool_search_tool_search_result', tool_references: [{ type: 'tool_reference', tool_name: 'Read', cache_control }],
        } },
        { type: 'tool_search_tool_result', tool_use_id: 'search-2', content: {
          type: 'tool_search_tool_result_error', error_code: 'unavailable', error_message: 'Synthetic failure',
        } },
      ] },
      { role: 'system', content: [{ type: 'tool_removal', tool: { type: 'tool_reference', name: 'Old' }, cache_control }] },
    ],
  }));
  const before = structuredClone(body);
  assert.equal(canRouteAutoRequest(body, 'claude-opus-5-5'), true);
  assert.deepEqual(targetCompatibility(body, 'claude-opus-5-5'), { compatible: true });
  assert.deepEqual(body, before);
});

test('forced tool choice, fast mode and retained execution facilities prevent routing', () => {
  for (const tool_choice of [{ type: 'any' }, { type: 'tool', name: 'Read' }, { type: 'future_choice' }, null, []]) {
    assert.equal(canRouteAutoRequest(request({ tool_choice }), 'claude-opus-5-5'), false);
  }
  for (const tool_choice of [{ type: 'auto', disable_parallel_tool_use: true }, { type: 'none' }]) {
    assert.equal(canRouteAutoRequest(request({ tool_choice }), 'claude-opus-5-5'), true);
  }
  for (const override of [{ speed: 'fast' }, { speed: 'future_speed' }, { container: {} },
    { mcp_servers: [] }, { compaction: {} }]) {
    assert.equal(canRouteAutoRequest(request(override), 'claude-opus-5-5'), false, JSON.stringify(override));
  }
  assert.equal(canRouteAutoRequest(request({ speed: 'standard' }), 'claude-opus-5-5'), true);
});

test('output caps must fit the target standard Messages API window', () => {
  for (const max_tokens of [0, 1, 128000]) {
    assert.equal(canRouteAutoRequest(request({ max_tokens }), 'claude-opus-5-5'), true);
  }
  for (const max_tokens of [-1, 1.5, 128001, 131072, 300000, '128000', null, NaN, Infinity]) {
    assert.equal(canRouteAutoRequest(request({ max_tokens }), 'claude-opus-5-5'), false, String(max_tokens));
  }
});

test('Sonnet 5 cannot receive mid-conversation instructions, per-message effort or task budgets', () => {
  for (const override of [
    { messages: [{ role: 'user', content: 'Task' }, { role: 'system', content: 'New instructions' }] },
    { messages: [{ role: 'user', content: 'Task', output_config: { effort: 'low' } }] },
    { output_config: { task_budget: { type: 'tokens', total: 10000 } } },
  ]) {
    const body = request(override);
    const before = structuredClone(body);
    assert.equal(canRouteAutoRequest(body, 'claude-sonnet-5'), false);
    assert.equal(canRouteAutoRequest(body, 'claude-opus-5-5'), true);
    assert.deepEqual(body, before);
  }
  assert.equal(canRouteAutoRequest(request({ output_config: { effort: 'high' } }), 'claude-sonnet-5'), true);
});

test('between_tools routing is restricted to Sonnet 5.5 and its exact native shape', () => {
  const body = request({ thinking: { type: 'between_tools' } });
  for (const target of ['claude-sonnet-5-5', 'claude-opus-5', 'claude-opus-5-5']) {
    assert.equal(canRouteAutoRequest(body, target), true, target);
  }
  assert.equal(canRouteAutoRequest(body, 'claude-sonnet-5'), false);
  for (const source of ['claude-sonnet-5', 'claude-opus-5', 'claude-opus-5-5']) {
    assert.equal(canRouteAutoRequest({ ...body, model: source }, 'claude-opus-5-5'), false, source);
  }
  for (const thinking of [null, [], { type: 'enabled', budget_tokens: 1000 }, { type: 'future_mode' },
    { type: 'between_tools', display: 'summarized' }, { type: 'between_tools', budget_tokens: 1000 },
    { type: 'between_tools', block_binding: { prefix_mismatch_behavior: 'drop_block' } },
    { type: 'between_tools', future_setting: true }]) {
    assert.equal(canRouteAutoRequest(request({ thinking }), 'claude-opus-5-5'), false, JSON.stringify(thinking));
  }
});

test('all-profile target checks preserve forced tool intent instead of producing invalid 5.5 requests', () => {
  for (const tool_choice of [{ type: 'any' }, { type: 'tool', name: 'Read' }]) {
    const body = freeze(request({ model: 'claude-haiku-4-5-20251001', thinking: { type: 'disabled' },
      tool_choice, tools: [{ name: 'Read', input_schema: { type: 'object' } }] }));
    const before = structuredClone(body);
    for (const target of ['claude-sonnet-5-5', 'claude-opus-5-5']) {
      assert.deepEqual(targetCompatibility(body, target), { compatible: false, reason: 'forced_tool_choice' });
    }
    // Opus/Sonnet 5 accept this setting. The check must not unnecessarily
    // pin ordinary compatible-profile requests merely because a tool is forced.
    assert.deepEqual(targetCompatibility(body, 'claude-sonnet-5'), { compatible: true });
    assert.deepEqual(targetCompatibility(body, 'claude-opus-5'), { compatible: true });
    assert.deepEqual(body, before);
  }
});

test('ordinary compatible requests still support all three configured tiers', () => {
  for (const source of ['claude-haiku-4-5-20251001', 'claude-sonnet-5', 'claude-opus-5-5']) {
    const body = request({ model: source, thinking: { type: 'disabled' } });
    for (const target of ['claude-haiku-4-5-20251001', 'claude-sonnet-5', 'claude-opus-5-5']) {
      assert.equal(targetCompatibility(body, target).compatible, true, `${source} -> ${target}`);
    }
  }
});

test('zero-output cache population remains compatible across profiles and preserves the request', () => {
  for (const autoMode of [false, true]) {
    const body = freeze(request({ max_tokens: 0, system: [
      { type: 'text', text: 'Synthetic cache prefix.', cache_control: { type: 'ephemeral' } },
    ] }));
    const before = structuredClone(body);
    for (const target of autoMode ? models : ['claude-haiku-4-5-20251001', ...models]) {
      assert.deepEqual(targetCompatibility(body, target, { autoMode }), { compatible: true }, `${autoMode}: ${target}`);
    }
    assert.deepEqual(body, before);
  }
});

test('target checks apply output, thinking, effort, sampling and prefill restrictions across profiles', () => {
  const haiku = request({ model: 'claude-haiku-4-5-20251001' });
  const variants = [
    [{ ...haiku, thinking: { type: 'enabled', budget_tokens: 1000 } }, 'claude-opus-5-5', 'thinking_mode'],
    [{ ...haiku, thinking: { type: 'adaptive' } }, 'claude-sonnet-4-5', 'thinking_mode'],
    [request({ max_tokens: 64001 }), 'claude-haiku-4-5', 'output_limit'],
    [request({ max_tokens: 128001 }), 'claude-opus-5-5', 'output_limit'],
    [request({ output_config: { effort: 'max' } }), 'claude-haiku-4-5', 'effort'],
    [request({ output_config: { effort: 'xhigh' } }), 'claude-sonnet-4-6', 'effort'],
    [{ ...haiku, temperature: 0.4 }, 'claude-sonnet-5', 'sampling'],
    [{ ...haiku, top_p: 0.8 }, 'claude-opus-5-5', 'sampling'],
    [{ ...haiku, top_k: 42 }, 'claude-opus-4-8', 'sampling'],
    [{ ...haiku, messages: [{ role: 'assistant', content: 'Prefill: ' }] }, 'claude-opus-5-5', 'assistant_prefill'],
  ];
  for (const [body, target, reason] of variants) {
    assert.deepEqual(targetCompatibility(body, target), { compatible: false, reason });
  }
  assert.equal(targetCompatibility({ ...haiku, temperature: 1, top_p: 1 }, 'claude-sonnet-5').compatible, true);
});

test('unfamiliar native identities and provider extensions remain on their source model unmodified', () => {
  const variants = [
    request({ model: 'team/claude-sonnet-5-5' }),
    request({ future_parameter: { opaque: 'keep me' } }),
    request({ output_config: { future_format: { opaque: true } } }),
    request({ messages: [{ role: 'user', content: [{ type: 'future_block', opaque: 'keep me' }] }] }),
    request({ thinking: { type: 'adaptive', future_option: true } }),
  ];
  for (const body of variants) {
    freeze(body);
    const before = structuredClone(body);
    assert.equal(targetCompatibility(body, 'claude-opus-5-5').compatible, false);
    assert.deepEqual(targetCompatibility(body, body.model), { compatible: true });
    assert.deepEqual(body, before);
  }
  assert.deepEqual(targetCompatibility(request(), 'custom-opus'), { compatible: false, reason: 'unknown_model' });
});
