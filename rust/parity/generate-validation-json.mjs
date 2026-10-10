// Source-only frozen-reference fixtures. No provider calls or user data.
let index = 0;
const emit = raw => console.log(JSON.stringify({ id: `validation-json-${index++}`, op: 'validate_request_json', input: { bytes: [...Buffer.from(raw)] }, source_tests: ['test/request-validation.test.mjs'] }));
const request = extra => `{"model":"claude-sonnet-5-5","messages":[{"role":"user","content":"Task"}],${extra}}`;
for (const field of ['thinking', 'tool_choice', 'output_config', 'context_management', 'max_tokens', 'stream', 'system', 'tools']) {
  for (const raw of ['1e400', '-1e400', '-0', 'null', 'false', '[]', '{}', '"\\ud800"']) emit(request(`"${field}":${raw}`));
}
for (const raw of ['1e400', 'null', '{}', '"\\ud800"']) {
  emit(request(`"messages":[{"role":"user","content":"Task","output_config":${raw}}]`));
  emit(request(`"tools":[{"type":${raw},"name":"synthetic"}]`));
}
for (const model of ['\\ud800', '\\udfff', '\\ufeff', '\\u0085', '\\ud800\\ufeff']) emit(`{"model":"${model}","messages":[]}`);
for (const depth of [0, 1, 30, 31, 32, 33, 64, 4096]) {
  const content = '[{"type":"tool_result","content":'.repeat(depth) + '[{"type":"text","text":"\\ud800"}]' + '}]'.repeat(depth);
  emit(request(`"messages":[{"role":"user","content":${content}}]`));
  const opaque = '{"opaque":'.repeat(depth) + '"\\udfff"' + '}'.repeat(depth);
  emit(request(`"tools":[{"name":"synthetic","input_schema":${opaque}}]`));
}
