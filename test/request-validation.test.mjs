import test from 'node:test';
import assert from 'node:assert/strict';
import { validateRequestShape } from '../src/request-validation.mjs';

const request = overrides => ({ model: 'claude-sonnet-5-5', messages: [{ role: 'user', content: 'Task' }], ...overrides });

test('consumed request containers reject malformed shapes with bounded, content-free errors', () => {
  const variants = [null, [], request({ model: '' }), request({ messages: {} }),
    request({ messages: [null] }), request({ messages: [{ role: 'private-secret', content: 'secret' }] }),
    request({ messages: [{ role: 'user', content: {} }] }),
    request({ messages: [{ role: 'user', content: [null] }] }),
    request({ messages: [{ role: 'user', content: [{ type: 'text', text: { secret: true } }] }] }),
    ...[{}, [null], [[]], [{ type: {} }], [{ name: 42 }]].map(tools => request({ tools })),
    ...['thinking', 'tool_choice', 'output_config'].flatMap(field => [null, [], 42].map(value => request({ [field]: value }))),
    ...[[], 42].map(value => request({ context_management: value })),
    request({ thinking: {} }), request({ tool_choice: { type: [] } }),
    request({ system: {} }), request({ max_tokens: 1.2 }), request({ max_tokens: -1 }), request({ stream: 'true' }),
    request({ messages: [{ role: 'system', content: 'Task', output_config: [] }] }),
  ];
  for (const body of variants) {
    const result = validateRequestShape(body);
    assert.equal(result.valid, false, JSON.stringify(body));
    assert.ok(result.error.length < 100);
    assert.ok(!result.error.includes('secret'));
  }
});

test('cache-population requests permit max_tokens zero without altering the request', () => {
  const body = request({ max_tokens: 0 });
  const before = structuredClone(body);
  assert.deepEqual(validateRequestShape(body), { valid: true });
  assert.deepEqual(body, before);
});

test('documented nullable context, message output and custom tool type stay intact', () => {
  const body = request({ context_management: null,
    messages: [{ role: 'system', content: 'A turn-scoped instruction', output_config: null },
      { role: 'user', content: 'Task' }],
    tools: [{ type: null, name: 'Read', input_schema: { type: 'object' } }],
  });
  const before = structuredClone(body);
  assert.deepEqual(validateRequestShape(body), { valid: true });
  assert.deepEqual(body, before);
});

test('unknown provider extensions, tool contracts and signed content are preserved', () => {
  const body = request({
    system: [{ type: 'future_system', opaque: { text: ['not a parsed text field'] } }],
    tools: [{ type: 'future_tool', name: 'Tool', private_settings: { nested: true } }],
    safeguards: { future_contract: true },
    thinking: { type: 'future_thinking', budget_policy: { custom: true } },
    context_management: { future_strategy: [null, 42] },
    tool_choice: { type: 'future_choice' },
    future_feature: { opaque: [null, 'data'] },
    messages: [
      { role: 'assistant', content: [{ type: 'thinking', thinking: '', signature: 'opaque' }] },
      { role: 'user', content: [{ type: 'future_block', content: { opaque: true } },
        { type: 'tool_result', content: [{ type: 'image', source: { type: 'future_source', data: [] } }] }] },
    ],
  });
  const before = structuredClone(body);
  assert.deepEqual(validateRequestShape(body), { valid: true });
  assert.deepEqual(body, before);
});

test('nested tool results are checked without interpreting tool input or schemas', () => {
  const malformed = request({ messages: [{ role: 'user', content: [{ type: 'tool_result', content: [null] }] }] });
  assert.equal(validateRequestShape(malformed).valid, false);
  assert.equal(validateRequestShape(request({ messages: [{ role: 'user', content: [{ type: 'tool_result', content: {} }] }] })).valid, false);
  const body = request({ tools: [{ name: 'Read', input_schema: { type: 'object', properties: { content: { type: 'array' } } } }],
    messages: [{ role: 'assistant', content: [{ type: 'tool_use', name: 'Read', input: { type: 'tool_result', content: null } }] },
      { role: 'user', content: [{ type: 'tool_result' }, { type: 'tool_result', content: 'done' }] }] });
  assert.equal(validateRequestShape(body).valid, true);
  let content = [{ type: 'text', text: 'leaf' }];
  for (let i = 0; i < 100; i++) content = [{ type: 'tool_result', content }];
  assert.deepEqual(validateRequestShape(request({ messages: [{ role: 'user', content }] })),
    { valid: false, error: 'Invalid Messages API request shape: content nesting' });
});
