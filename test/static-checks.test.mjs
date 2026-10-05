import test from 'node:test';
import assert from 'node:assert/strict';
import { checkContractSources } from '../scripts/check-contracts.mjs';

test('static producer checks catch misspelled fields, invalid enum literals and timing types before execution', () => {
  const errors = checkContractSources(new Map([
    ['server.mjs', `status('answered', { selected_modle: 'opus', first_response_ms: '1ms' });
      onStatus({ event: 'route', request_id: 'id', source: 'random', body: {} });`],
    ['router.mjs', `class Router { route() { return { model:'opus', source:'jev', reason:'classified',
      latency_ms:1,evaluation_latency_ms:1,selected_modle:'opus' }; } } const x = config.timoutMs;`],
  ]));
  for (const phrase of ['Unknown lifecycle event', 'selected_modle', 'first_response_ms', 'source', 'body', 'timoutMs']) {
    assert.ok(errors.some(error => error.includes(phrase)), phrase);
  }
});

test('static producer checks accept the bounded documented contract with opaque runtime fields', () => {
  assert.deepEqual(checkContractSources(new Map([
    ['server.mjs', `status('route', { model: selected, source:'jev', latency_ms:1 });
      onStatus({ ...context, event:'request_complete', completion_confirmed:true });`],
    ['router.mjs', `class Router { route() { return { ...decision, model:chosen,reason:'classified',
      latency_ms:1,evaluation_latency_ms:1 }; } } const x = config.jevTimeoutMs;`],
  ])), []);
});
