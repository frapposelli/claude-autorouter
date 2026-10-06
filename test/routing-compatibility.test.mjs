import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { readConfig } from '../src/config.mjs';
import { Router } from '../src/router.mjs';
import { createRouterServer, listen } from '../src/server.mjs';
import { createTokenCounter } from '../src/token-counter.mjs';

const token = 'compatibility-test-local-token';
const config = profile => readConfig({ AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_CLIENT_PROFILE: profile,
  AUTOROUTER_SONNET_MODEL: 'claude-sonnet-5-5', AUTOROUTER_OPUS_MODEL: 'claude-opus-5-5',
  ANTHROPIC_API_KEY: 'synthetic-upstream-key', TYPESAFE_API_KEY: 'synthetic-classifier-key' });
const answer = choice => Response.json({ answers: { tier: { choice, confidence: 0.99 } } });
const request = (profile, extra = {}) => ({
  model: profile === 'auto' ? 'claude-sonnet-5' : 'claude-haiku-4-5-20251001',
  max_tokens: 4096, thinking: { type: 'disabled' },
  messages: [{ role: 'user', content: 'Read the synthetic test file.' }],
  tools: [{ name: 'Read', input_schema: { type: 'object', properties: {} } }], ...extra,
});

test('every routing profile rejects incompatible forced-tool upgrades while preserving the original contract', async () => {
  for (const profile of ['compatible', 'native', 'auto']) {
    for (const tier of ['sonnet', 'opus']) for (const tool_choice of [{ type: 'any' }, { type: 'tool', name: 'Read' }]) {
      const body = request(profile, { tool_choice });
      const before = structuredClone(body);
      const router = new Router(config(profile), { fetchImpl: async () => answer(tier) });
      const decision = await router.route(body);
      assert.equal(decision.model, body.model, `${profile}/${tier}/${tool_choice.type}`);
      assert.equal(decision.classified_tier, tier);
      assert.equal(decision.reason, profile === 'auto' ? 'auto_mode_incompatible' : 'model_incompatible');
      assert.equal(decision.compatibility_reason, 'forced_tool_choice');
      assert.deepEqual(body, before);
    }
  }
});

async function gateway(t, profile = 'compatible') {
  const received = [], logs = [];
  let evaluations = 0, counts = 0;
  const upstream = http.createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const body = JSON.parse(raw);
    received.push({ body, headers: req.headers, path: req.url });
    res.writeHead(200, { 'content-type': 'application/json', 'request-id': 'synthetic-upstream-id' });
    res.end(JSON.stringify({ type: 'message', role: 'assistant', model: body.model,
      content: [{ type: 'text', text: 'Synthetic response.' }], stop_reason: 'end_turn',
      usage: { input_tokens: 10, output_tokens: 4 } }));
  });
  t.after(() => { upstream.closeAllConnections(); upstream.close(); });
  const upstreamAddress = await listen(upstream, 0);
  const c = { ...config(profile), localToken: token, upstream: `http://127.0.0.1:${upstreamAddress.port}` };
  const router = new Router(c, { fetchImpl: async () => { evaluations++; return answer('opus'); } });
  const server = createRouterServer(c, { router, log: entry => logs.push(entry),
    tokenCounter: async () => { counts++; return 100; } });
  t.after(() => { server.closeAllConnections(); server.close(); });
  const address = await listen(server, 0);
  return { received, logs, evaluations: () => evaluations, counts: () => counts,
    send: (body, path = '/v1/messages') => fetch(`http://127.0.0.1:${address.port}${path}`, {
      method: 'POST', headers: { 'x-api-key': token, 'content-type': 'application/json',
        'anthropic-beta': 'synthetic-future-beta', 'x-claude-code-session-id': 'synthetic-session' },
      body: JSON.stringify(body),
    }) };
}

test('gateway preserves forced tool bodies and headers in compatible, native and Auto profiles', async t => {
  for (const profile of ['compatible', 'native', 'auto']) {
    const f = await gateway(t, profile);
    const body = request(profile, { tool_choice: { type: 'tool', name: 'Read' } });
    const response = await f.send(body, '/v1/messages?beta=true');
    assert.equal(response.status, 200);
    assert.equal((await response.json()).model, body.model);
    assert.equal(response.headers.get('request-id'), 'synthetic-upstream-id');
    assert.deepEqual(f.received[0].body, body);
    assert.equal(f.received[0].path, '/v1/messages?beta=true');
    assert.equal(f.received[0].headers['anthropic-beta'], 'synthetic-future-beta');
    assert.equal(f.evaluations(), 1);
    assert.equal(f.counts(), 0);
  }
});

test('gateway rejects malformed consumed shapes before evaluation, token counting or upstream forwarding', async t => {
  const f = await gateway(t);
  const invalid = [
    { tools: {} }, { tools: [null] }, { thinking: [] }, { output_config: 'PRIVATE_TEST_VALUE' },
    { messages: [{ role: 'user', content: [{ type: 'tool_result', content: [null] }] }] },
  ];
  for (const fields of invalid) for (const path of ['/v1/messages', '/v1/messages/count_tokens']) {
    const response = await f.send(request('compatible', fields), path);
    assert.equal(response.status, 400);
    const text = await response.text();
    assert.match(text, /Invalid Messages API request shape/);
    assert.ok(!text.includes('PRIVATE_TEST_VALUE'));
  }
  assert.equal(f.evaluations(), 0);
  assert.equal(f.counts(), 0);
  assert.deepEqual(f.received, []);
  assert.ok(!JSON.stringify(f.logs).includes('PRIVATE_TEST_VALUE'));
});

test('unfamiliar provider extensions and native same-model contracts pass through unmodified', async t => {
  const f = await gateway(t);
  const variants = [
    request('compatible', { future_contract: { version: 7, opaque: 'retained' } }),
    request('compatible', { system: [{ type: 'future_system_block', opaque: 'retained' }] }),
    request('compatible', { messages: [{ role: 'user', content: [{ type: 'future_message_block', opaque: 'retained' }] }] }),
    request('compatible', { model: 'team/claude-sonnet-5-5' }),
    request('compatible', { model: 'claude-opus-5-5', thinking: { type: 'disabled' }, tool_choice: { type: 'any' } }),
  ];
  for (const body of variants) {
    const response = await f.send(body);
    assert.equal(response.status, 200);
    await response.arrayBuffer();
    assert.deepEqual(f.received.at(-1).body, body);
  }
});

test('internal token counting avoids incompatible targets and retains native provider-owned requests', async () => {
  let calls = 0;
  const counter = createTokenCounter(config('compatible'), { fetchImpl: async () => {
    calls++; return Response.json({ input_tokens: 100 });
  } });
  const forced = request('compatible', { tool_choice: { type: 'any' } });
  assert.equal(await counter(forced, 'claude-opus-5-5'), undefined);
  const adaptive = request('auto', { thinking: { type: 'adaptive' } });
  assert.equal(await counter(adaptive, 'claude-haiku-4-5-20251001'), undefined);
  assert.equal(calls, 0);
  assert.equal(await counter(forced, forced.model), 100);
  assert.equal(calls, 1);
});
