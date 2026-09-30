import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { gzipSync } from 'node:zlib';
import { readConfig } from '../src/config.mjs';
import { createRouterServer, listen } from '../src/server.mjs';
import { Router } from '../src/router.mjs';

const token = 'local-test-token-123456789';
const body = { model: 'claude-sonnet-5', stream: true, max_tokens: 1024, system: [{ type: 'text', text: 'system', cache_control: { type: 'ephemeral' } }], messages: [{ role: 'user', content: 'Fix a typo' }], tools: [{ name: 'Read', input_schema: { type: 'object' } }] };
async function fixture(t, handler, overrides = {}, routeImpl, onStatus, onDecision) {
  const upstream = http.createServer(handler);
  const address = await listen(upstream, 0);
  let evaluations = 0;
  const logs = [];
  const statuses = [];
  const config = { ...readConfig({ ANTHROPIC_API_KEY: 'upstream-secret', TYPESAFE_API_KEY: 'classifier-secret' }), localToken: token, upstream: `http://127.0.0.1:${address.port}`, ...overrides };
  const server = createRouterServer(config, {
    router: { route: async (...args) => { evaluations++; return routeImpl ? routeImpl(...args) : { model: config.models.haiku, source: 'test' }; } },
    log: entry => logs.push(entry), onStatus: entry => { statuses.push(entry); return onStatus?.(entry); },
    onDecision,
  });
  const local = await listen(server, 0);
  t.after(() => { server.closeAllConnections(); server.close(); upstream.closeAllConnections(); upstream.close(); });
  const localAuth = config.authMode === 'subscription' ? { 'x-autorouter-token': token } : { 'x-api-key': token };
  const baseUrl = `http://127.0.0.1:${local.port}`;
  const call = (path, options = {}) => fetch(`${baseUrl}${path}`, { ...options, headers: { ...localAuth, ...options.headers } });
  return { call, baseUrl, logs, statuses, evaluations: () => evaluations };
}

async function waitForStatus(f, event) {
  for (let i = 0; i < 100; i++) {
    if (f.statuses.some(entry => entry.event === event)) return;
    await new Promise(resolve => setTimeout(resolve, 5));
  }
  assert.fail(`Missing status event: ${event}`);
}

test('streams before completion, preserves SSE, payloads, headers and query; isolates credentials', async t => {
  let finish;
  const first = 'event: message_start\ndata: {"type":"message_start"}\n\n';
  const last = 'event: message_stop\ndata: {"type":"message_stop"}\n\n';
  const f = await fixture(t, async (req, res) => {
    let text = ''; for await (const chunk of req) text += chunk;
    assert.equal(req.url, '/v1/messages?beta=true');
    assert.equal(req.headers['x-api-key'], 'upstream-secret');
    assert.equal(req.headers.authorization, undefined);
    assert.equal(req.headers['anthropic-beta'], 'test-beta');
    assert.equal(req.headers['anthropic-version'], '2023-06-01');
    assert.deepEqual(JSON.parse(text), { ...body, model: 'claude-haiku-4-5-20251001' });
    res.writeHead(200, { 'content-type': 'text/event-stream', 'request-id': 'test-id' });
    res.write(first);
    finish = () => res.end(last);
  });
  const response = await f.call('/v1/messages?beta=true', { method: 'POST', headers: { authorization: `Bearer ${token}`, 'anthropic-beta': 'test-beta', 'anthropic-version': '2023-06-01' }, body: JSON.stringify(body) });
  assert.equal(response.headers.get('request-id'), 'test-id');
  const reader = response.body.getReader();
  const chunk = await reader.read();
  assert.equal(new TextDecoder().decode(chunk.value), first);
  finish();
  let rest = ''; for (;;) { const chunk = await reader.read(); if (chunk.done) break; rest += new TextDecoder().decode(chunk.value); }
  assert.equal(rest, last);
  assert.equal(f.evaluations(), 1);
  assert.ok(!JSON.stringify(f.logs).includes('Fix a typo'));
  assert.ok(!JSON.stringify(f.logs).includes('secret'));
});

test('token counting bypasses Jev and upstream errors pass through unchanged', async t => {
  const decisions = [];
  const error = JSON.stringify({ type: 'error', error: { type: 'rate_limit_error', message: 'Wait' } });
  const f = await fixture(t, async (req, res) => {
    let text = ''; for await (const chunk of req) text += chunk;
    assert.deepEqual(JSON.parse(text), body);
    res.writeHead(429, { 'retry-after': '7', 'content-type': 'application/json' }); res.end(error);
  }, {}, undefined, undefined, entry => decisions.push(entry));
  const response = await f.call('/v1/messages/count_tokens', { method: 'POST', body: JSON.stringify(body) });
  assert.equal(response.status, 429);
  assert.equal(response.headers.get('retry-after'), '7');
  assert.equal(await response.text(), error);
  assert.equal(f.evaluations(), 0);
  assert.deepEqual(f.statuses, []);
  assert.deepEqual(decisions, []);
});

test('rejects missing auth, browser origins, malformed JSON, unsupported paths and oversized input locally', async t => {
  const decisions = [];
  const f = await fixture(t, () => assert.fail('Must not reach upstream'), { maxBodyBytes: 1000 }, undefined, undefined, entry => decisions.push(entry));
  assert.equal((await f.call('/v1/messages', { method: 'POST', headers: { 'x-api-key': 'wrong' } })).status, 401);
  assert.equal((await f.call('/health', { headers: { origin: 'https://example.com' } })).status, 403);
  assert.equal((await f.call('/v1/messages', { method: 'POST', body: '{bad' })).status, 400);
  assert.equal((await f.call('/v1/messages', { method: 'POST', body: JSON.stringify({ ...body, messages: [null] }) })).status, 400);
  assert.equal((await f.call('/unexpected')).status, 404);
  assert.equal((await f.call('/v1/messages', { method: 'POST', body: 'x'.repeat(1001) })).status, 413);
  assert.equal((await f.call('/health')).status, 200);
  assert.equal(f.evaluations(), 0);
  assert.deepEqual(decisions, []);
});

test('opt-in decision callbacks receive bounded current task text without leaking it into status or ordinary diagnostics', async t => {
  const decisions = [];
  const prompt = 'DECISION_ONLY_TASK: inspect this fixture.\nKeep the supplied emoji 😀.';
  const privateValues = ['PRIVATE_SYSTEM', 'PRIVATE_SCHEMA', 'PRIVATE_THINKING', 'PRIVATE_IMAGE', 'PRIVATE_TOOL_RESULT', 'PRIVATE_REMINDER'];
  const payload = { ...body, system: privateValues[0], tools: [{ name: 'Read', description: privateValues[1], input_schema: { type: 'object' } }], messages: [
    { role: 'user', content: 'Earlier task' },
    { role: 'assistant', content: 'Earlier answer' },
    { role: 'user', content: [
      { type: 'text', text: '<system-reminder>PRIVATE_REMINDER</system-reminder>' },
      { type: 'text', text: prompt },
      { type: 'thinking', thinking: privateValues[2], signature: 'PRIVATE_SIGNATURE' },
      { type: 'image', source: { type: 'base64', media_type: 'image/png', data: privateValues[3] } },
    ] },
    { role: 'assistant', content: [{ type: 'tool_use', id: 'read-fixture', name: 'Read', input: { path: 'PRIVATE_PATH' } }] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'read-fixture', content: privateValues[4] }] },
  ] };
  const original = structuredClone(payload);
  let route = { model: 'claude-haiku-4-5-20251001', latency_ms: 12.25, source: 'jev', evaluator: 'jev', reason: 'classified', classified_tier: 'haiku' };
  const reply = 'event: message_start\ndata: {"type":"message_start","message":{"model":"provider-confirmed-model"}}\n\n'
    + 'event: message_stop\ndata: {"type":"message_stop"}\n\n';
  const f = await fixture(t, (_req, res) => {
    res.writeHead(200, { 'content-type': 'text/event-stream' }); res.end(reply);
  }, {}, () => route, undefined, entry => decisions.push(entry));
  const send = async (value, requestClass) => {
    const response = await f.call('/v1/messages', { method: 'POST', headers: {
      'x-claude-code-session-id': 'decision-session', 'x-claude-code-agent-id': 'decision-agent', 'x-claude-code-prompt-id': 'decision-prompt',
      ...(requestClass === undefined ? {} : { 'x-claude-code-request-class': requestClass }),
    }, body: JSON.stringify(value) });
    assert.equal(response.status, 200);
    assert.equal(await response.text(), reply);
    return decisions.at(-1);
  };
  const first = await send(payload, 'main');
  assert.equal(first.schema_version, 1);
  assert.equal(first.event, 'decision');
  assert.ok(Number.isFinite(Date.parse(first.timestamp)));
  assert.match(first.request_id, /^[a-f0-9-]{36}$/);
  assert.equal(first.session_id, 'decision-session');
  assert.equal(first.agent_id, 'decision-agent');
  assert.equal(first.prompt_id, 'decision-prompt');
  assert.equal(first.request_class, 'main');
  assert.equal(first.prompt_excerpt, prompt);
  assert.equal(first.prompt_truncated, false);
  assert.equal(first.requested_model, payload.model);
  assert.equal(first.selected_model, route.model);
  assert.equal(first.decision_latency_ms, 12.25);
  assert.equal(first.source, 'jev');
  assert.equal(first.evaluator, 'jev');
  assert.equal(first.classified_tier, 'haiku');
  assert.equal(first.reason, 'classified');
  for (const value of [...privateValues, 'PRIVATE_SIGNATURE', 'PRIVATE_PATH']) assert.ok(!JSON.stringify(first).includes(value));
  assert.deepEqual(payload, original);

  const multibyte = { ...body, messages: [{ role: 'user', content: '😀'.repeat(501) + 'OMITTED_TAIL' }] };
  route = { ...route, source: 'cache' };
  const bounded = await send(multibyte);
  assert.equal(bounded.prompt_excerpt, '😀'.repeat(500));
  assert.equal([...bounded.prompt_excerpt].length, 500);
  assert.equal(bounded.prompt_truncated, true);
  assert.equal(bounded.source, 'cache');
  const exact = await send({ ...body, messages: [{ role: 'user', content: '😀'.repeat(500) }] }, 'main');
  assert.equal(exact.prompt_truncated, false);

  route = { model: payload.model, source: 'passthrough', reason: 'internal_request', latency_ms: 0.5 };
  for (const requestClass of ['auxiliary', 'compaction', 'subagent', 'workflow']) {
    const internal = await send(payload, requestClass);
    assert.equal(internal.prompt_excerpt, '');
    assert.equal(internal.prompt_truncated, false);
    assert.equal(internal.source, 'passthrough');
    assert.equal(internal.request_class, requestClass);
  }
  assert.equal(decisions.length, 7);
  assert.equal(new Set(decisions.map(entry => entry.request_id)).size, 7);
  for (const channel of [f.logs, f.statuses]) {
    const serialized = JSON.stringify(channel);
    for (const value of [prompt, ...privateValues, '😀', 'OMITTED_TAIL']) assert.ok(!serialized.includes(value));
    assert.ok(!serialized.includes('prompt_excerpt'));
  }
});

test('decision sink failures and pending writes never delay or change the upstream stream', { timeout: 5000 }, async t => {
  let calls = 0;
  const reply = 'event: message_delta\ndata: {"delta":{"stop_reason":"end_turn"}}\n\n';
  const f = await fixture(t, (_req, res) => {
    res.writeHead(200, { 'content-type': 'text/event-stream', 'request-id': 'decision-sink-fixture' }); res.end(reply);
  }, {}, () => ({ model: body.model, latency_ms: 1, source: 'jev', reason: 'classified' }), undefined, () => {
    calls++;
    if (calls === 1) throw new Error('PRIVATE_SYNC_LOG_FAILURE');
    if (calls === 2) return Promise.reject(new Error('PRIVATE_ASYNC_LOG_FAILURE'));
    return new Promise(() => {});
  });
  for (let index = 0; index < 3; index++) {
    const response = await f.call('/v1/messages', { method: 'POST', body: JSON.stringify(body), signal: AbortSignal.timeout(1000) });
    assert.equal(response.status, 200);
    assert.equal(response.headers.get('request-id'), 'decision-sink-fixture');
    assert.equal(await response.text(), reply);
  }
  assert.equal(calls, 3);
  assert.ok(!JSON.stringify({ logs: f.logs, statuses: f.statuses }).includes('PRIVATE_'));
});

test('a fallback decision is logged even when its upstream request fails', async t => {
  const decisions = [];
  const errorBody = JSON.stringify({ type: 'error', error: { type: 'rate_limit_error', message: 'Synthetic rate limit' } });
  const f = await fixture(t, (_req, res) => {
    res.writeHead(429, { 'content-type': 'application/json' }); res.end(errorBody);
  }, {}, () => ({ model: body.model, latency_ms: 1501, source: 'fallback', evaluator: 'jev', reason: 'classifier_unavailable', classifier_error: 'timeout' }),
  undefined, entry => decisions.push(entry));
  const response = await f.call('/v1/messages', { method: 'POST', body: JSON.stringify(body) });
  assert.equal(response.status, 429);
  assert.equal(await response.text(), errorBody);
  assert.equal(decisions.length, 1);
  assert.equal(decisions[0].source, 'fallback');
  assert.equal(decisions[0].classifier_error, 'timeout');
  assert.equal(decisions[0].selected_model, body.model);
  assert.equal(decisions[0].decision_latency_ms, 1501);
  assert.equal(decisions[0].prompt_excerpt, 'Fix a typo');
});

test('client cancellation closes the upstream stream', async t => {
  let upstreamClosed;
  const closed = new Promise(resolve => { upstreamClosed = resolve; });
  const f = await fixture(t, (req, res) => {
    res.on('close', upstreamClosed);
    res.writeHead(200, { 'content-type': 'text/event-stream' }); res.write(': ping\n\n');
  });
  const response = await f.call('/v1/messages', { method: 'POST', body: JSON.stringify(body) });
  await response.body.cancel();
  await Promise.race([closed, new Promise((_, reject) => { const timer = setTimeout(() => reject(new Error('Upstream not cancelled')), 2000); timer.unref(); })]);
  await waitForStatus(f, 'request_cancelled');
  assert.equal(f.statuses.some(entry => entry.event === 'request_error'), false);
});

test('complete proxy flow calls the real classifier client against a local Jev API mock', async t => {
  let classifications = 0;
  const logs = [];
  const statuses = [];
  const smallRequest = { ...body, model: 'claude-haiku-4-5-20251001', stream: false };
  const fitsHaikuRequest = { ...smallRequest, tools: [{ name: 'Read', description: 'Small-token-count catalog. '.repeat(7000), input_schema: { type: 'object' } }] };
  const largeRequest = { ...smallRequest, tools: [{ name: 'Read', description: 'Synthetic tool schema context. '.repeat(30000), input_schema: { type: 'object' } }] };
  let tokenCounts = 0;
  const jev = http.createServer(async (req, res) => {
    let text = ''; for await (const chunk of req) text += chunk;
    assert.equal(req.url, '/v1/systemone');
    assert.equal(req.headers.authorization, 'Bearer classifier-secret');
    const payload = JSON.parse(text);
    assert.ok(JSON.stringify(payload.state).length <= 12000);
    assert.equal(payload.state.original_task, 'Fix a typo');
    assert.deepEqual(Object.keys(payload.questions.tier.criteria), ['haiku', 'sonnet', 'opus']);
    classifications++;
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ answers: { tier: { choice: 'haiku', confidence: 0.99, probabilities: { haiku: 0.99, sonnet: 0.01, opus: 0 } } } }));
  });
  const jevAddress = await listen(jev, 0);
  const upstream = http.createServer(async (req, res) => {
    let text = ''; for await (const chunk of req) text += chunk;
    const parsed = JSON.parse(text);
    const large = parsed.tools[0].description?.startsWith('Synthetic');
    const fits = parsed.tools[0].description?.startsWith('Small-token-count');
    if (req.url === '/v1/messages/count_tokens') {
      tokenCounts++;
      assert.equal(req.headers['x-api-key'], 'upstream-secret');
      assert.equal(req.headers['x-autorouter-token'], undefined);
      assert.equal(parsed.model, 'claude-haiku-4-5-20251001');
      assert.deepEqual(parsed.tools, (large ? largeRequest : fitsHaikuRequest).tools);
      assert.equal(parsed.max_tokens, undefined);
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ input_tokens: large ? 227338 : 54481 }));
      return;
    }
    const model = large ? 'claude-sonnet-5' : 'claude-haiku-4-5-20251001';
    // A short user prompt with a large tool catalog must leave the proxy on
    // Sonnet, without dropping any of that catalog to make Haiku fit.
    assert.deepEqual(parsed, { ...(large ? largeRequest : fits ? fitsHaikuRequest : smallRequest), model });
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ id: 'msg_mock', model, content: [{ type: 'text', text: 'Done' }] }));
  });
  const upstreamAddress = await listen(upstream, 0);
  const server = createRouterServer({ ...readConfig({ ANTHROPIC_API_KEY: 'upstream-secret', TYPESAFE_API_KEY: 'classifier-secret' }), localToken: token, upstream: `http://127.0.0.1:${upstreamAddress.port}`, jevEndpoint: `http://127.0.0.1:${jevAddress.port}/v1/systemone` }, { log: entry => logs.push(entry), onStatus: entry => statuses.push(entry) });
  const address = await listen(server, 0);
  t.after(() => { for (const service of [server, upstream, jev]) { service.closeAllConnections(); service.close(); } });
  for (const [request, expectedModel] of [[smallRequest, 'claude-haiku-4-5-20251001'], [fitsHaikuRequest, 'claude-haiku-4-5-20251001'], [largeRequest, 'claude-sonnet-5']]) {
    const response = await fetch(`http://127.0.0.1:${address.port}/v1/messages`, { method: 'POST', headers: { 'x-api-key': token, 'content-type': 'application/json' }, body: JSON.stringify(request) });
    assert.equal(response.status, 200);
    assert.equal((await response.json()).model, expectedModel);
  }
  assert.equal(classifications, 3);
  assert.equal(tokenCounts, 2);
  const fittingRoute = statuses.filter(entry => entry.event === 'route')[1];
  assert.equal(fittingRoute.model, 'claude-haiku-4-5-20251001');
  assert.equal(fittingRoute.context_check, 'within_budget');
  assert.equal(fittingRoute.counted_input_tokens, 54481);
  assert.equal(statuses.findLast(entry => entry.event === 'route').reason, 'context_capacity');
  assert.equal(statuses.findLast(entry => entry.event === 'upstream_model').model, 'claude-sonnet-5');
  assert.ok(!JSON.stringify({ logs, statuses }).includes('Synthetic tool schema context'));
});

const oauthHeaders = { authorization: 'Bearer fake-subscription-token', 'anthropic-beta': 'oauth-2025-04-20,future-capability', 'anthropic-version': '2023-06-01' };

test('large subscription requests count with the current OAuth headers and keep Haiku when they fit', async t => {
  const paths = [];
  const router = new Router(readConfig(), { fetchImpl: async () => Response.json({ answers: { tier: { choice: 'haiku', confidence: 1 } } }) });
  const requested = { ...body, model: 'claude-haiku-4-5-20251001', stream: false, system: 'Synthetic context. '.repeat(10000) };
  const f = await fixture(t, async (req, res) => {
    paths.push(req.url);
    assert.equal(req.headers.authorization, oauthHeaders.authorization);
    assert.equal(req.headers['anthropic-beta'], oauthHeaders['anthropic-beta']);
    assert.equal(req.headers['x-api-key'], undefined);
    assert.equal(req.headers['x-autorouter-token'], undefined);
    assert.equal(req.headers.cookie, undefined);
    let raw = ''; for await (const chunk of req) raw += chunk;
    const payload = JSON.parse(raw);
    assert.equal(payload.model, requested.model);
    assert.equal(payload.system, requested.system);
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify(req.url.includes('count_tokens') ? { input_tokens: 54000 }
      : { id: 'msg_counted', model: payload.model, content: [{ type: 'text', text: 'Done' }] }));
  }, { authMode: 'subscription' }, (...args) => router.route(...args));
  const response = await f.call('/v1/messages?beta=true', { method: 'POST', headers: { ...oauthHeaders, cookie: 'PRIVATE_COOKIE' }, body: JSON.stringify(requested) });
  assert.equal(response.status, 200);
  assert.equal((await response.json()).model, requested.model);
  assert.deepEqual(paths, ['/v1/messages/count_tokens?beta=true', '/v1/messages?beta=true']);
  assert.equal(f.statuses.find(entry => entry.event === 'route').counted_input_tokens, 54000);
  for (const secret of ['fake-subscription-token', 'PRIVATE_COOKIE', token]) assert.ok(!JSON.stringify({ logs: f.logs, statuses: f.statuses }).includes(secret));
});

test('subscription preserves OAuth headers, token refresh, SSE and usage limits, while stripping the local credential', async t => {
  const credentials = [];
  const sse = 'event: message_start\ndata: {"type":"message_start"}\n\nevent: message_stop\ndata: {"type":"message_stop"}\n\n';
  const f = await fixture(t, async (req, res) => {
    credentials.push(req.headers.authorization);
    assert.equal(req.headers['anthropic-beta'], oauthHeaders['anthropic-beta']);
    assert.equal(req.headers['anthropic-version'], oauthHeaders['anthropic-version']);
    assert.equal(req.headers['x-api-key'], undefined);
    assert.equal(req.headers['x-autorouter-token'], undefined);
    assert.equal(req.headers.cookie, undefined);
    let text = ''; for await (const chunk of req) text += chunk;
    assert.deepEqual(JSON.parse(text), { ...body, model: 'claude-haiku-4-5-20251001' });
    res.writeHead(200, { 'content-type': 'text/event-stream', 'anthropic-ratelimit-unified-5h-utilization': '0.3' });
    res.end(sse);
  }, { authMode: 'subscription' });
  for (const authorization of [oauthHeaders.authorization, 'Bearer fake-refreshed-token']) {
    const response = await f.call('/v1/messages?beta=true', { method: 'POST', headers: { ...oauthHeaders, authorization, cookie: 'should-not-be-forwarded' }, body: JSON.stringify(body) });
    assert.equal(response.status, 200);
    assert.equal(response.headers.get('anthropic-ratelimit-unified-5h-utilization'), '0.3');
    assert.equal(await response.text(), sse);
  }
  assert.deepEqual(credentials, [oauthHeaders.authorization, 'Bearer fake-refreshed-token']);
  assert.equal(f.evaluations(), 2);
  for (const secret of ['fake-subscription-token', 'fake-refreshed-token', token, 'upstream-secret']) assert.ok(!JSON.stringify(f.logs).includes(secret));
});

test('subscription rejects missing local or OAuth credentials and API-key conflicts before evaluation', async t => {
  const f = await fixture(t, () => assert.fail('Must not reach upstream'), { authMode: 'subscription' });
  for (const headers of [
    {},
    { ...oauthHeaders, 'x-autorouter-token': '' },
    { ...oauthHeaders, 'x-autorouter-token': 'incorrect' },
    { ...oauthHeaders, authorization: `Bearer ${token}`, 'x-autorouter-token': '' },
    { ...oauthHeaders, 'x-api-key': 'billable-api-key' },
    { ...oauthHeaders, 'anthropic-beta': 'unrelated-beta' },
  ]) {
    const response = await f.call('/v1/messages', { method: 'POST', headers, body: JSON.stringify(body) });
    assert.equal(response.status, 401);
    assert.ok(!(await response.text()).includes('billable-api-key'));
  }
  assert.equal((await f.call('/health')).status, 200);
  assert.equal((await f.call('/api/hello', { method: 'HEAD' })).status, 200);
  assert.equal(f.evaluations(), 0);
});

test('subscription forwards authentication failures and quota errors without retrying with an API key', async t => {
  const statuses = [401, 429];
  let calls = 0;
  const f = await fixture(t, (req, res) => {
    assert.equal(req.headers.authorization, oauthHeaders.authorization);
    assert.equal(req.headers['x-api-key'], undefined);
    const status = statuses[calls++];
    res.writeHead(status, { 'content-type': 'application/json', 'retry-after': '12', 'anthropic-ratelimit-unified-status': 'rejected' });
    res.end(JSON.stringify({ type: 'error', error: { message: `Upstream ${status}` } }));
  }, { authMode: 'subscription' });
  for (const status of statuses) {
    const response = await f.call('/v1/messages', { method: 'POST', headers: oauthHeaders, body: JSON.stringify(body) });
    assert.equal(response.status, status);
    assert.equal(response.headers.get('retry-after'), '12');
    assert.equal(response.headers.get('anthropic-ratelimit-unified-status'), 'rejected');
    assert.deepEqual(await response.json(), { type: 'error', error: { message: `Upstream ${status}` } });
  }
  assert.equal(calls, 2);
});

test('subscription model discovery and token counting use the same OAuth forwarding without Jev', async t => {
  const f = await fixture(t, async (req, res) => {
    assert.equal(req.headers.authorization, oauthHeaders.authorization);
    assert.equal(req.headers['anthropic-beta'], oauthHeaders['anthropic-beta']);
    assert.equal(req.headers['x-autorouter-token'], undefined);
    if (req.method === 'POST') {
      let text = ''; for await (const chunk of req) text += chunk;
      assert.deepEqual(JSON.parse(text), body);
    }
    res.writeHead(200, { 'content-type': 'application/json' }); res.end('{}');
  }, { authMode: 'subscription' });
  assert.equal((await f.call('/v1/models', { headers: oauthHeaders })).status, 200);
  assert.equal((await f.call('/v1/messages/count_tokens', { method: 'POST', headers: oauthHeaders, body: JSON.stringify(body) })).status, 200);
  assert.equal(f.evaluations(), 0);
});

test('subscription accepts and preserves Claude Code turn-scoped system messages', async t => {
  const payload = { ...body, messages: [...body.messages, { role: 'system', clear_at: 'next_user_message', content: [{ type: 'text', text: 'Turn instructions' }] }] };
  const beta = `${oauthHeaders['anthropic-beta']},mid-conversation-system-clear-at-2026-08-21`;
  const f = await fixture(t, async (req, res) => {
    let text = ''; for await (const chunk of req) text += chunk;
    assert.deepEqual(JSON.parse(text).messages, payload.messages);
    assert.equal(req.headers['anthropic-beta'], beta);
    res.writeHead(200, { 'content-type': 'application/json' }); res.end('{}');
  }, { authMode: 'subscription' });
  const response = await f.call('/v1/messages', { method: 'POST', headers: { ...oauthHeaders, 'anthropic-beta': beta }, body: JSON.stringify(payload) });
  assert.equal(response.status, 200);
  assert.equal(f.evaluations(), 1);
});

test('subscription forwards gateway identity and adapted Opus thinking, and observes the actual streamed model', async t => {
  const payload = { ...body, model: 'claude-haiku-4-5-20251001', thinking: { type: 'disabled' } };
  const before = structuredClone(payload);
  const selected = 'claude-opus-5-5';
  const reported = 'claude-opus-5-5-provider-revision';
  const first = 'event: message_start\r\ndata: {"type":"message_start",';
  const next = `"message":{"model":"${reported}","content":[]}}\r\n\r\n`;
  const last = 'event: message_stop\r\ndata: {"type":"message_stop"}\r\n\r\n';
  let continueStream;
  const f = await fixture(t, async (req, res) => {
    let text = ''; for await (const chunk of req) text += chunk;
    assert.deepEqual(JSON.parse(text), { ...payload, model: selected, thinking: { type: 'adaptive' } });
    assert.equal(req.headers.authorization, oauthHeaders.authorization);
    assert.equal(req.headers['x-autorouter-token'], undefined);
    assert.equal(req.headers['accept-encoding'], 'identity');
    res.writeHead(200, { 'content-type': 'text/event-stream', 'request-id': 'provider-request' });
    continueStream = () => { res.write(next); res.end(last); };
    res.write(first);
  }, { authMode: 'subscription' }, (original, context) => {
    assert.deepEqual(original, payload);
    assert.equal(context.promptId, 'human-prompt-1');
    assert.equal(context.scope, JSON.stringify(['session-1', 'agent-1']));
    assert.equal(context.requestClass, 'main');
    assert.ok(context.signal instanceof AbortSignal);
    return { model: selected, source: 'test' };
  });
  const response = await f.call('/v1/messages', { method: 'POST', headers: {
    ...oauthHeaders, 'accept-encoding': 'gzip', 'x-claude-code-prompt-id': 'human-prompt-1',
    'x-claude-code-session-id': 'session-1', 'x-claude-code-agent-id': 'agent-1', 'x-claude-code-request-class': 'main',
  }, body: JSON.stringify(payload) });
  assert.equal(response.status, 200);
  const reader = response.body.getReader();
  const initial = await reader.read();
  assert.equal(Buffer.from(initial.value).toString(), first);
  assert.equal(f.logs.some(entry => entry.event === 'upstream_model'), false);
  continueStream();
  const chunks = [initial.value];
  for (;;) { const chunk = await reader.read(); if (chunk.done) break; chunks.push(chunk.value); }
  assert.equal(Buffer.concat(chunks).toString(), first + next + last);
  assert.deepEqual(payload, before);
  assert.deepEqual(f.logs.find(entry => entry.event === 'route'), {
    event: 'route', requested_model: payload.model, model: selected, source: 'test', request_adjustments: ['adaptive_thinking_required'],
  });
  assert.deepEqual(f.logs.filter(entry => entry.event === 'upstream_model'), [{ event: 'upstream_model', model: reported }]);
  assert.ok(f.logs.some(entry => entry.event === 'upstream_response' && entry.status === 200));
  for (const privateValue of ['Fix a typo', 'fake-subscription-token', token]) assert.ok(!JSON.stringify(f.logs).includes(privateValue));
});

test('real routing adapts low-confidence Sonnet 5.5 and its signed tool continuation without altering SSE', { timeout: 5000 }, async t => {
  const selected = 'claude-sonnet-5-5';
  const config = readConfig({ AUTOROUTER_SONNET_MODEL: selected, TYPESAFE_API_KEY: 'classifier-secret' });
  let classifications = 0;
  const router = new Router(config, { fetchImpl: async (url, options) => {
    assert.equal(url, config.jevEndpoint);
    assert.equal(options.headers.authorization, 'Bearer classifier-secret');
    assert.equal(JSON.parse(options.body).state.current_task, 'Fix a typo');
    classifications++;
    return Response.json({ answers: { tier: { choice: 'haiku', confidence: classifications === 1 ? 0.6 : 0.99 } } });
  } });
  const thinking = { type: 'thinking', thinking: 'Synthetic private reasoning: inspect → edit.', signature: 'synthetic-signature+/==' };
  const toolUse = { type: 'tool_use', id: 'tool_sonnet_55', name: 'Read', input: { path: 'README.md' } };
  const initial = { ...body, model: config.models.haiku, thinking: { type: 'disabled' } };
  const continuation = { ...initial, messages: [
    ...initial.messages,
    { role: 'assistant', content: [thinking, toolUse] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: toolUse.id, content: 'Synthetic file text.' }] },
  ] };
  const requests = [initial, continuation];
  const originals = structuredClone(requests);
  const event = payload => `event: ${payload.type}\r\ndata: ${JSON.stringify(payload)}\r\n\r\n`;
  const streamed = Buffer.from([
    { type: 'message_start', message: { id: 'msg_sonnet_55', model: selected, content: [], usage: { input_tokens: 10, output_tokens: 1 } } },
    { type: 'content_block_start', index: 0, content_block: { type: 'thinking', thinking: '' } },
    { type: 'content_block_delta', index: 0, delta: { type: 'thinking_delta', thinking: thinking.thinking } },
    { type: 'content_block_delta', index: 0, delta: { type: 'signature_delta', signature: thinking.signature } },
    { type: 'content_block_stop', index: 0 },
    { type: 'content_block_start', index: 1, content_block: { ...toolUse, input: {} } },
    { type: 'content_block_delta', index: 1, delta: { type: 'input_json_delta', partial_json: JSON.stringify(toolUse.input) } },
    { type: 'content_block_stop', index: 1 },
    { type: 'message_delta', delta: { stop_reason: 'tool_use' }, usage: { output_tokens: 20 } },
    { type: 'message_stop' },
  ].map(event).join(''));
  const finished = Buffer.from([
    { type: 'message_start', message: { id: 'msg_sonnet_55_done', model: selected, content: [] } },
    { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
    { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'Done.' } },
    { type: 'content_block_stop', index: 0 },
    { type: 'message_delta', delta: { stop_reason: 'end_turn' }, usage: { output_tokens: 2 } },
    { type: 'message_stop' },
  ].map(event).join(''));
  const replies = [streamed, finished];
  let upstreamCalls = 0;
  const received = [];
  const f = await fixture(t, async (req, res) => {
    let text = ''; for await (const chunk of req) text += chunk;
    const parsed = JSON.parse(text);
    const index = upstreamCalls++;
    received.push(parsed);
    // Reproduce the provider rejection instead of accepting an invalid body.
    if (parsed.model !== selected || parsed.thinking?.type !== 'between_tools') {
      res.writeHead(400, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ type: 'error', error: { type: 'invalid_request_error', message: 'Sonnet 5.5 requires between_tools thinking for this request' } }));
      return;
    }
    assert.equal(req.headers.authorization, oauthHeaders.authorization);
    assert.equal(req.headers['x-autorouter-token'], undefined);
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    const reply = replies[index];
    // Split inside a UTF-8 character to catch accidental decode/re-encode.
    const split = index === 0 ? reply.indexOf(Buffer.from('→')) + 1 : 31;
    res.write(reply.subarray(0, split));
    res.end(reply.subarray(split));
  }, { authMode: 'subscription', models: config.models }, (...args) => router.route(...args));
  for (const [index, request] of requests.entries()) {
    const response = await f.call('/v1/messages', { method: 'POST', headers: {
      ...oauthHeaders, 'x-claude-code-session-id': 'sonnet-55-session',
      'x-claude-code-prompt-id': 'sonnet-55-turn', 'x-claude-code-request-class': 'main',
    }, body: JSON.stringify(request) });
    assert.equal(response.status, 200);
    assert.deepEqual(Buffer.from(await response.arrayBuffer()), replies[index]);
    assert.deepEqual(received[index], { ...request, model: selected, thinking: { type: 'between_tools' } });
  }
  assert.equal(upstreamCalls, 2);
  assert.equal(classifications, 2);
  assert.deepEqual(requests, originals);
  const routes = f.statuses.filter(entry => entry.event === 'route');
  assert.deepEqual(routes.map(({ model, source, reason, classified_tier }) => ({ model, source, reason, classified_tier })), [
    { model: selected, source: 'jev', reason: 'low_confidence', classified_tier: 'haiku' },
    { model: selected, source: 'jev', reason: 'tool_turn_pinned', classified_tier: 'haiku' },
  ]);
  assert.deepEqual(f.logs.filter(entry => entry.event === 'upstream_model').map(entry => entry.model), [selected, selected]);
  assert.deepEqual(f.logs.filter(entry => entry.event === 'upstream_response').map(entry => entry.status), [200, 200]);
  for (const secret of [thinking.thinking, thinking.signature, 'classifier-secret', 'fake-subscription-token']) {
    assert.ok(!JSON.stringify({ logs: f.logs, statuses: f.statuses }).includes(secret));
  }
});

test('auto execution routes independently while auxiliary permission checks and denied safeguard verdicts remain unchanged', async t => {
  const config = readConfig({ TYPESAFE_API_KEY: 'classifier-secret', AUTOROUTER_CLIENT_PROFILE: 'auto', AUTOROUTER_SONNET_MODEL: 'claude-sonnet-5-5' });
  let classifications = 0;
  const router = new Router(config, { fetchImpl: async (url, options) => {
    assert.equal(url, config.jevEndpoint);
    assert.equal(JSON.parse(options.body).state.current_task, 'Design a secure cross-process transaction protocol.');
    classifications++;
    return Response.json({ answers: { tier: { choice: 'opus', confidence: 0.99 } } });
  } });
  const explanation = 'Synthetic denied action';
  const auxiliary = { ...body, model: config.models.haiku,
    system: [{ type: 'text', text: 'Synthetic classifier attribution. ' + 'review context '.repeat(11000) }],
    messages: [{ role: 'user', content: 'Evaluate the synthetic tool permission request.' }],
    tools: [], stop_sequences: ['</block>'], thinking: { type: 'disabled' },
  };
  const signedHistory = { type: 'thinking', thinking: 'Synthetic prior reasoning → retained.', signature: 'synthetic-safety-signature+/==' };
  const safeguarded = { ...body, model: config.models.sonnet, thinking: { type: 'adaptive' },
    messages: [...body.messages, { role: 'assistant', content: [signedHistory, { type: 'text', text: 'The typo is fixed.' }] },
      { role: 'user', content: 'Design a secure cross-process transaction protocol.' }],
    safeguards: [{ type: 'dangerous_tool_use', classifier_context: { v: 1, synthetic_context: 'Preserve this review context.' } }],
  };
  const requests = [auxiliary, safeguarded];
  const expectedModels = [auxiliary.model, config.models.opus];
  const originals = structuredClone(requests);
  const classes = ['auxiliary', 'main'];
  const beta = oauthHeaders['anthropic-beta'] + ',dangerous-tool-use-2026-09-03';
  const event = payload => `event: ${payload.type}\r\ndata: ${JSON.stringify(payload)}\r\n\r\n`;
  const replies = requests.map((_request, index) => Buffer.from([
    { type: 'message_start', message: { id: `msg_safety_${index}`, model: expectedModels[index], content: [] } },
    { type: 'content_block_start', index: 0, content_block: index === 0 ? { type: 'text', text: '' }
      : { type: 'tool_use', id: 'tool_test', name: 'Bash', input: {} } },
    { type: 'content_block_delta', index: 0, delta: index === 0
      ? { type: 'text_delta', text: `<block>${explanation}</block>` }
      : { type: 'input_json_delta', partial_json: JSON.stringify({ command: 'printf synthetic' }) } },
    { type: 'content_block_stop', index: 0 },
    { type: 'message_delta', delta: index === 0 ? { stop_reason: 'stop_sequence', stop_sequence: '</block>' }
      : { stop_reason: 'tool_use', safeguard_results: [{ type: 'dangerous_tool_use', status: { type: 'available',
        tool_uses: { tool_test: { type: 'evaluated', outcome: 'flagged', explanation } } } }] }, usage: { output_tokens: 20 } },
    { type: 'message_stop' },
  ].map(event).join('')));
  let upstreamCalls = 0;
  const f = await fixture(t, async (req, res) => {
    let text = ''; for await (const chunk of req) text += chunk;
    const index = upstreamCalls++;
    assert.equal(req.url, '/v1/messages?beta=true');
    assert.equal(req.headers.authorization, oauthHeaders.authorization);
    assert.equal(req.headers['anthropic-beta'], beta);
    assert.equal(req.headers['anthropic-version'], oauthHeaders['anthropic-version']);
    assert.equal(req.headers['x-claude-code-request-class'], classes[index]);
    assert.equal(req.headers['x-claude-code-session-id'], 'safety-session');
    assert.equal(req.headers['x-autorouter-token'], undefined);
    assert.equal(text, JSON.stringify({ ...requests[index], model: expectedModels[index] }));
    res.writeHead(200, { 'content-type': 'text/event-stream', 'request-id': `safety-provider-${index}`, 'x-safety-fixture': 'retained' });
    res.write(replies[index].subarray(0, 37));
    res.end(replies[index].subarray(37));
  }, { authMode: 'subscription', models: config.models, clientProfile: 'auto' }, (...args) => router.route(...args));
  for (const [index, request] of requests.entries()) {
    const response = await f.call('/v1/messages?beta=true', { method: 'POST', headers: {
      ...oauthHeaders, 'anthropic-beta': beta, 'x-claude-code-request-class': classes[index], 'x-claude-code-session-id': 'safety-session',
    }, body: JSON.stringify(request) });
    assert.equal(response.status, 200);
    assert.equal(response.headers.get('request-id'), `safety-provider-${index}`);
    assert.equal(response.headers.get('x-safety-fixture'), 'retained');
    assert.deepEqual(Buffer.from(await response.arrayBuffer()), replies[index]);
  }
  assert.equal(classifications, 1);
  assert.equal(upstreamCalls, 2);
  assert.deepEqual(requests, originals);
  assert.deepEqual(f.statuses.filter(entry => entry.event === 'route').map(({ model, source, reason, classified_tier }) =>
    ({ model, source, reason, classified_tier })), [
    { model: auxiliary.model, source: 'passthrough', reason: 'internal_request', classified_tier: undefined },
    { model: config.models.opus, source: 'jev', reason: 'classified', classified_tier: 'opus' },
  ]);
  for (const value of [explanation, signedHistory.thinking, signedHistory.signature, safeguarded.safeguards[0].classifier_context.synthetic_context, 'classifier-secret', 'fake-subscription-token']) {
    assert.ok(!JSON.stringify({ logs: f.logs, statuses: f.statuses }).includes(value));
  }
});

for (const usePromptIds of [true, false]) test(`auto execution switches Sonnet to Opus to Sonnet across human turns, preserving native context and tool ownership (${usePromptIds ? 'gateway prompt IDs' : 'without prompt IDs'})`, { timeout: 5000 }, async t => {
  const config = readConfig({ TYPESAFE_API_KEY: 'classifier-secret', AUTOROUTER_CLIENT_PROFILE: 'auto', AUTOROUTER_SONNET_MODEL: 'claude-sonnet-5-5' });
  const choices = ['haiku', 'opus', 'haiku', 'haiku'];
  let classifications = 0;
  const router = new Router(config, { fetchImpl: async (url, options) => {
    assert.equal(url, config.jevEndpoint);
    assert.equal(options.headers.authorization, 'Bearer classifier-secret');
    assert.ok(classifications < choices.length, 'Unexpected classifier request');
    return Response.json({ answers: { tier: { choice: choices[classifications++], confidence: 0.99 } } });
  } });
  const instructions = { role: 'system', clear_at: 'next_user_message', content: [{ type: 'text', text: 'Synthetic turn-specific instructions.' }] };
  const initial = { ...body, model: config.models.sonnet, thinking: { type: 'adaptive' },
    // Both selected models have the same native 1M window. The byte trigger
    // must not independently pin Opus after a demanding task is complete.
    system: [{ type: 'text', text: 'Synthetic shared context. '.repeat(7000), cache_control: { type: 'ephemeral' } }],
    messages: [...body.messages, instructions],
    context_management: { edits: [
      { type: 'clear_thinking_20251015', keep: { type: 'thinking_turns', value: 1 } },
      { type: 'clear_tool_uses_20250919', trigger: { type: 'input_tokens', value: 100000 }, keep: { type: 'tool_uses', value: 3 } },
    ] },
    safeguards: [{ type: 'dangerous_tool_use', classifier_context: { v: 1, synthetic_context: 'Keep the native tool review context.' } }],
  };
  const sonnetThinking = { type: 'thinking', thinking: 'Synthetic Sonnet reasoning.', signature: 'signed-sonnet-history+/==' };
  const opusThinking = { type: 'thinking', thinking: 'Synthetic Opus reasoning.', signature: 'signed-opus-history+/==' };
  const toolUse = { type: 'tool_use', id: 'tool_auto_fixture', name: 'Read', input: { path: 'fixture.txt' } };
  const demanding = { ...initial, messages: [...initial.messages,
    { role: 'assistant', content: [sonnetThinking, { type: 'text', text: 'The typo is fixed.' }] },
    { role: 'user', content: 'Investigate an intermittent cross-process race with no known cause.' }, instructions,
  ] };
  const toolContinuation = { ...demanding, messages: [...demanding.messages,
    { role: 'assistant', content: [opusThinking, toolUse] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: toolUse.id, content: 'Synthetic fixture result.' }] },
  ] };
  const nextHuman = { ...toolContinuation, messages: [...toolContinuation.messages,
    { role: 'assistant', content: [{ type: 'text', text: 'The concurrency issue is resolved.' }] },
    { role: 'user', content: 'Now change the exact typo teh to the in fixture.txt.' }, instructions,
  ] };
  const requests = [initial, demanding, toolContinuation, nextHuman];
  const originals = structuredClone(requests);
  const expectedModels = [config.models.sonnet, config.models.opus, config.models.opus, config.models.sonnet];
  const promptIds = ['auto-first', 'auto-demanding', 'auto-demanding', 'auto-next'];
  const beta = `${oauthHeaders['anthropic-beta']},dangerous-tool-use-2026-09-03,context-management-2025-06-27,mid-conversation-system-clear-at-2026-08-21`;
  const event = payload => `event: ${payload.type}\r\ndata: ${JSON.stringify(payload)}\r\n\r\n`;
  const replies = requests.map((_request, index) => Buffer.from([
    { type: 'message_start', message: { id: `msg_auto_${index}`, model: expectedModels[index], content: [],
      ...(index > 0 ? { input_transformations: [{ type: 'thinking_dropped', reason: 'model_binding_mismatch' }] } : {}) } },
    { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
    { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'Synthetic reply → unchanged.' } },
    { type: 'content_block_stop', index: 0 },
    { type: 'message_delta', delta: { stop_reason: 'end_turn', safeguard_results: [{ type: 'dangerous_tool_use', status: { type: 'available', tool_uses: {} } }] }, usage: { output_tokens: 10 } },
    { type: 'message_stop' },
  ].map(event).join('')));
  const received = [];
  const f = await fixture(t, async (req, res) => {
    let text = ''; for await (const chunk of req) text += chunk;
    const index = received.length;
    received.push(JSON.parse(text));
    // A count endpoint would consume an extra call and violate this assertion.
    assert.equal(req.url, '/v1/messages?beta=true');
    assert.equal(req.headers.authorization, oauthHeaders.authorization);
    assert.equal(req.headers['anthropic-beta'], beta);
    assert.equal(req.headers['x-claude-code-session-id'], 'auto-switch-session');
    assert.equal(req.headers['x-claude-code-prompt-id'], usePromptIds ? promptIds[index] : undefined);
    assert.equal(req.headers['x-autorouter-token'], undefined);
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    const split = replies[index].indexOf(Buffer.from('→')) + 1;
    res.write(replies[index].subarray(0, split));
    res.end(replies[index].subarray(split));
  }, { authMode: 'subscription', models: config.models, clientProfile: 'auto' }, (...args) => router.route(...args));
  for (const [index, request] of requests.entries()) {
    const response = await f.call('/v1/messages?beta=true', { method: 'POST', headers: {
      ...oauthHeaders, 'anthropic-beta': beta, 'x-claude-code-session-id': 'auto-switch-session', 'x-claude-code-agent-id': 'auto-main-agent',
      'x-claude-code-request-class': 'main', ...(usePromptIds ? { 'x-claude-code-prompt-id': promptIds[index] } : {}),
    }, body: JSON.stringify(request) });
    assert.equal(response.status, 200);
    assert.deepEqual(Buffer.from(await response.arrayBuffer()), replies[index]);
    assert.deepEqual(received[index], { ...request, model: expectedModels[index] });
  }
  assert.equal(classifications, 4);
  assert.equal(received.length, 4);
  assert.deepEqual(requests, originals);
  assert.deepEqual(f.statuses.filter(entry => entry.event === 'route').map(({ model, source, reason, classified_tier }) =>
    ({ model, source, reason, classified_tier })), [
    { model: config.models.sonnet, source: 'jev', reason: 'auto_mode_floor', classified_tier: 'haiku' },
    { model: config.models.opus, source: 'jev', reason: 'classified', classified_tier: 'opus' },
    { model: config.models.opus, source: 'jev', reason: 'tool_turn_pinned', classified_tier: 'haiku' },
    { model: config.models.sonnet, source: 'jev', reason: 'auto_mode_floor', classified_tier: 'haiku' },
  ]);
  for (const value of [sonnetThinking.thinking, sonnetThinking.signature, opusThinking.thinking, opusThinking.signature,
    initial.safeguards[0].classifier_context.synthetic_context, 'classifier-secret', 'fake-subscription-token']) {
    assert.ok(!JSON.stringify({ logs: f.logs, statuses: f.statuses }).includes(value));
  }
});

for (const usePromptIds of [true, false]) test(`goal feedback and tool results retain the main model, preserving auxiliary verdicts and new human turns (${usePromptIds ? 'gateway prompt IDs' : 'expanded command without prompt IDs'})`, { timeout: 5000 }, async t => {
  const config = readConfig({ AUTOROUTER_SONNET_MODEL: 'claude-sonnet-5-5', TYPESAFE_API_KEY: 'classifier-secret' });
  const classifierChoices = ['sonnet', 'haiku', 'haiku', 'haiku'];
  let classifications = 0;
  const router = new Router(config, { fetchImpl: async (url, options) => {
    assert.equal(url, config.jevEndpoint);
    assert.equal(options.headers.authorization, 'Bearer classifier-secret');
    assert.ok(classifications < classifierChoices.length, 'Unexpected classifier request');
    return Response.json({ answers: { tier: { choice: classifierChoices[classifications++], confidence: 0.99 } } });
  } });
  const condition = 'Create the fixture file once the synthetic resource is available.';
  const answer = 'The synthetic resource is unavailable. Please enable it to continue.';
  const verdict = '{"ok":false,"reason":"The synthetic resource is not available yet."}';
  const initial = {
    ...body, model: config.models.haiku, thinking: { type: 'disabled' },
    messages: [{ role: 'user', content: [
      { type: 'text', text: `<command-name>/goal</command-name>\n<command-message>goal</command-message>\n<command-args>${condition}</command-args>` },
      { type: 'text', text: `A session-scoped Stop hook is now active with condition: "${condition}". Briefly acknowledge the goal, then immediately start working toward it.` },
    ] }],
  };
  const judgedMessages = [...initial.messages, { role: 'assistant', content: [{ type: 'text', text: answer }] }];
  const judge = {
    ...initial, system: 'Judge the synthetic goal using transcript evidence only.', tools: [],
    messages: [...judgedMessages, { role: 'user', content: `Has this stopping condition been satisfied? ${condition}` }],
    output_config: { format: { type: 'json_schema', schema: {
      type: 'object', properties: { ok: { type: 'boolean' }, reason: { type: 'string' }, impossible: { type: 'boolean' } },
      required: ['ok', 'reason'], additionalProperties: false,
    } } },
  };
  // Claude's internal isMeta flag is absent from the Messages API: the same
  // human-prompt identity must distinguish this feedback from a new task.
  const continuation = {
    ...initial, messages: [...judgedMessages, { role: 'user', content: [
      { type: 'text', text: `Stop hook feedback:\n[${condition}]: ${JSON.parse(verdict).reason}`, cache_control: { type: 'ephemeral' } },
    ] }],
  };
  const toolUse = { type: 'tool_use', id: 'tool_goal_fixture', name: 'Read', input: { path: 'fixture.txt' } };
  const toolContinuation = { ...initial, messages: [
    ...continuation.messages,
    { role: 'assistant', content: [{ type: 'text', text: 'Checking the fixture.' }, toolUse] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: toolUse.id, content: 'The synthetic resource is unavailable.', cache_control: { type: 'ephemeral' } }] },
  ] };
  const nextTurn = { ...initial, messages: [
    ...toolContinuation.messages, { role: 'assistant', content: [{ type: 'text', text: answer }] },
    { role: 'user', content: 'Instead, what is the length of []? Reply with the number.' },
  ] };
  const requests = [initial, judge, continuation, toolContinuation, nextTurn];
  const originals = structuredClone(requests);
  const expectedModels = [config.models.sonnet, config.models.haiku, config.models.sonnet, config.models.sonnet, config.models.haiku];
  const classes = ['main', 'auxiliary', 'main', 'main', 'main'];
  const promptIds = usePromptIds ? ['goal-prompt', 'goal-prompt', 'goal-prompt', 'goal-prompt', 'new-human-prompt'] : [];
  const event = payload => `event: ${payload.type}\r\ndata: ${JSON.stringify(payload)}\r\n\r\n`;
  const replies = [answer, verdict, 'Checking the fixture.', answer, '0'].map((text, index) => Buffer.from([
    { type: 'message_start', message: { id: `msg_goal_${index}`, model: expectedModels[index], content: [] } },
    { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
    { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text } },
    { type: 'content_block_stop', index: 0 },
    ...(index === 2 ? [
      { type: 'content_block_start', index: 1, content_block: { ...toolUse, input: {} } },
      { type: 'content_block_delta', index: 1, delta: { type: 'input_json_delta', partial_json: JSON.stringify(toolUse.input) } },
      { type: 'content_block_stop', index: 1 },
    ] : []),
    { type: 'message_delta', delta: { stop_reason: index === 2 ? 'tool_use' : 'end_turn' }, usage: { output_tokens: 20 } },
    { type: 'message_stop' },
  ].map(event).join('')));
  const received = [];
  const f = await fixture(t, async (req, res) => {
    let text = ''; for await (const chunk of req) text += chunk;
    const index = received.length;
    received.push(JSON.parse(text));
    assert.equal(req.url, '/v1/messages?beta=true');
    assert.equal(req.headers.authorization, oauthHeaders.authorization);
    assert.equal(req.headers['anthropic-beta'], oauthHeaders['anthropic-beta']);
    assert.equal(req.headers['anthropic-version'], oauthHeaders['anthropic-version']);
    assert.equal(req.headers['x-claude-code-session-id'], 'synthetic-goal-session');
    assert.equal(req.headers['x-claude-code-agent-id'], 'synthetic-main-agent');
    assert.equal(req.headers['x-claude-code-prompt-id'], promptIds[index]);
    assert.equal(req.headers['x-claude-code-request-class'], classes[index]);
    assert.equal(req.headers['x-autorouter-token'], undefined);
    res.writeHead(200, { 'content-type': 'text/event-stream', 'request-id': `goal-provider-${index}` });
    res.write(replies[index].subarray(0, 31));
    res.end(replies[index].subarray(31));
  }, { authMode: 'subscription', models: config.models }, (...args) => router.route(...args));
  for (const [index, request] of requests.entries()) {
    const response = await f.call('/v1/messages?beta=true', { method: 'POST', headers: {
      ...oauthHeaders, 'x-claude-code-session-id': 'synthetic-goal-session', 'x-claude-code-agent-id': 'synthetic-main-agent',
      ...(usePromptIds ? { 'x-claude-code-prompt-id': promptIds[index] } : {}), 'x-claude-code-request-class': classes[index],
    }, body: JSON.stringify(request) });
    assert.equal(response.status, 200);
    assert.equal(response.headers.get('request-id'), `goal-provider-${index}`);
    assert.deepEqual(Buffer.from(await response.arrayBuffer()), replies[index]);
    assert.deepEqual(received[index], {
      ...request, model: expectedModels[index],
      thinking: { type: expectedModels[index] === config.models.sonnet ? 'between_tools' : 'disabled' },
    });
  }
  assert.equal(classifications, 4);
  assert.deepEqual(requests, originals);
  const routes = f.statuses.filter(entry => entry.event === 'route');
  assert.deepEqual(routes.map(({ model, source, reason, classified_tier }) => ({ model, source, reason, classified_tier })), [
    { model: config.models.sonnet, source: 'jev', reason: 'classified', classified_tier: 'sonnet' },
    { model: config.models.haiku, source: 'passthrough', reason: 'internal_request', classified_tier: undefined },
    { model: config.models.sonnet, source: 'jev', reason: usePromptIds ? 'prompt_turn_pinned' : 'goal_turn_pinned', classified_tier: 'haiku' },
    { model: config.models.sonnet, source: 'jev', reason: 'tool_turn_pinned', classified_tier: 'haiku' },
    { model: config.models.haiku, source: 'jev', reason: 'classified', classified_tier: 'haiku' },
  ]);
  assert.deepEqual(f.logs.filter(entry => entry.event === 'upstream_model').map(entry => entry.model), expectedModels);
  for (const privateValue of [condition, answer, verdict, 'classifier-secret', 'fake-subscription-token']) {
    assert.ok(!JSON.stringify({ logs: f.logs, statuses: f.statuses }).includes(privateValue));
  }
});

test('compressed upstream responses preserve their exact bytes and skip model observation', async t => {
  const compressed = gzipSync('event: message_start\ndata: {"type":"message_start","message":{"model":"compressed-provider-model"}}\n\n');
  const f = await fixture(t, (req, res) => {
    assert.equal(req.headers['accept-encoding'], 'identity');
    res.writeHead(200, { 'content-type': 'text/event-stream', 'content-encoding': 'gzip' });
    res.end(compressed);
  });
  // Native HTTP deliberately avoids fetch's automatic response decompression.
  const received = await new Promise((resolve, reject) => {
    const request = http.request(`${f.baseUrl}/v1/messages`, {
      method: 'POST', headers: { 'x-api-key': token, 'content-type': 'application/json' },
    }, response => {
      assert.equal(response.headers['content-encoding'], 'gzip');
      const chunks = [];
      response.on('data', chunk => chunks.push(chunk));
      response.on('error', reject);
      response.on('end', () => resolve(Buffer.concat(chunks)));
    });
    request.on('error', reject);
    request.end(JSON.stringify(body));
  });
  assert.deepEqual(received, compressed);
  assert.equal(f.logs.some(entry => entry.event === 'upstream_model'), false);
});

test('status lifecycle correlates gateway identities and distinguishes requested, selected and provider models', async t => {
  const selected = 'claude-sonnet-5';
  const actual = 'claude-sonnet-5-provider-revision';
  const f = await fixture(t, (req, res) => {
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.end(`event: message_start\ndata: {"message":{"model":"${actual}"}}\n\nevent: message_stop\ndata: {}\n\n`);
  }, {}, () => ({ model: selected, source: 'jev', tier: 'haiku', reason: 'tool_turn_pinned', latency_ms: 42, private_debug: 'PRIVATE_DETAIL' }));
  const payload = { ...body, model: 'claude-haiku-4-5-20251001' };
  const response = await f.call('/v1/messages', { method: 'POST', headers: {
    'x-claude-code-session-id': 'session-1', 'x-claude-code-agent-id': 'agent-1',
    'x-claude-code-prompt-id': 'prompt-1', 'x-claude-code-request-class': 'main',
  }, body: JSON.stringify(payload) });
  await response.text();
  await waitForStatus(f, 'request_complete');
  assert.deepEqual(f.statuses.map(entry => entry.event), ['request_start', 'route', 'upstream_response', 'upstream_model', 'request_complete']);
  const requestId = f.statuses[0].request_id;
  assert.match(requestId, /^[a-f0-9-]{36}$/);
  for (const entry of f.statuses) {
    assert.equal(entry.request_id, requestId);
    assert.equal(entry.session_id, 'session-1');
    assert.equal(entry.agent_id, 'agent-1');
    assert.equal(entry.prompt_id, 'prompt-1');
    assert.equal(entry.request_class, 'main');
    assert.equal(entry.tier, undefined);
  }
  const route = f.statuses.find(entry => entry.event === 'route');
  assert.equal(route.requested_model, payload.model);
  assert.equal(route.model, selected);
  assert.equal(route.reason, 'tool_turn_pinned');
  assert.equal(route.latency_ms, 42);
  assert.equal(f.statuses.find(entry => entry.event === 'upstream_model').model, actual);
  for (const privateValue of ['PRIVATE_DETAIL', 'Fix a typo', token, 'upstream-secret', 'classifier-secret']) {
    assert.ok(!JSON.stringify(f.statuses).includes(privateValue));
  }
});

test('status distinguishes overlapping sessions and ignores discovery, counting and unauthenticated traffic', async t => {
  const f = await fixture(t, (req, res) => {
    res.writeHead(200, { 'content-type': 'application/json' }); res.end('{}');
  });
  for (const [path, options] of [
    ['/v1/models', {}], ['/health', {}],
    ['/v1/messages/count_tokens', { method: 'POST', body: JSON.stringify(body) }],
    ['/v1/messages', { method: 'POST', headers: { 'x-api-key': 'wrong' }, body: JSON.stringify(body) }],
  ]) await (await f.call(path, options)).text();
  assert.deepEqual(f.statuses, []);
  await Promise.all(['first', 'second'].map(async session => {
    const response = await f.call('/v1/messages', { method: 'POST', headers: { 'x-claude-code-session-id': session }, body: JSON.stringify(body) });
    await response.text();
  }));
  const starts = f.statuses.filter(entry => entry.event === 'request_start');
  assert.equal(starts.length, 2);
  assert.notEqual(starts[0].request_id, starts[1].request_id);
  for (const start of starts) {
    assert.ok(f.statuses.filter(entry => entry.request_id === start.request_id).every(entry => entry.session_id === start.session_id));
    assert.equal(start.agent_id, undefined);
  }
});

test('SSE errors remain observable after HTTP success and model confirmation with only a safe error category', async t => {
  const sse = 'event: message_start\ndata: {"message":{"model":"claude-haiku-4-5-20251001"}}\n\n'
    + 'event: error\ndata: {"type":"error","error":{"type":"overloaded_error","message":"PRIVATE_REQUEST"}}\n\n';
  const f = await fixture(t, (req, res) => {
    res.writeHead(200, { 'content-type': 'text/event-stream' }); res.end(sse);
  });
  const response = await f.call('/v1/messages', { method: 'POST', body: JSON.stringify(body) });
  assert.equal(response.status, 200);
  assert.equal(await response.text(), sse);
  await waitForStatus(f, 'request_complete');
  assert.deepEqual(f.statuses.map(entry => entry.event), ['request_start', 'route', 'upstream_response', 'upstream_model', 'upstream_error', 'request_complete']);
  assert.equal(f.statuses.find(entry => entry.event === 'upstream_error').error_type, 'overloaded_error');
  assert.ok(!JSON.stringify(f.statuses).includes('PRIVATE_REQUEST'));
  assert.ok(!JSON.stringify(f.logs).includes('PRIVATE_REQUEST'));
});

test('status surfaces rejected request shapes and HTTP errors, and preserves fallback metadata', async t => {
  const f = await fixture(t, (req, res) => {
    res.writeHead(429, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ type: 'error', error: { type: 'rate_limit_error', message: 'PRIVATE_REQUEST' } }));
  }, {}, () => ({ model: 'claude-sonnet-5', source: 'fallback', reason: 'classifier_unavailable', classifier_error: 'http_error', classifier_status: 503, latency_ms: 33 }));
  const bad = await f.call('/v1/messages', { method: 'POST', body: '{bad' });
  assert.equal(bad.status, 400);
  await bad.text();
  assert.deepEqual(f.statuses.map(entry => entry.event), ['request_start', 'request_error']);
  assert.equal(f.statuses[1].status, 400);
  f.statuses.length = 0;
  const response = await f.call('/v1/messages', { method: 'POST', body: JSON.stringify(body) });
  assert.equal(response.status, 429);
  await response.text();
  await waitForStatus(f, 'request_complete');
  const route = f.statuses.find(entry => entry.event === 'route');
  assert.equal(route.source, 'fallback');
  assert.equal(route.classifier_error, 'http_error');
  assert.equal(route.classifier_status, 503);
  assert.equal(f.statuses.find(entry => entry.event === 'upstream_response').status, 429);
  assert.equal(f.statuses.find(entry => entry.event === 'upstream_error').error_type, 'rate_limit_error');
});

test('upstream stream disconnection is an error, not a client cancellation', async t => {
  let disconnect;
  const f = await fixture(t, (req, res) => {
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.write(': ping\n\n');
    disconnect = () => res.destroy();
  });
  const response = await f.call('/v1/messages', { method: 'POST', body: JSON.stringify(body) });
  const reader = response.body.getReader();
  await reader.read();
  disconnect();
  await assert.rejects(async () => { while (!(await reader.read()).done) {} });
  await waitForStatus(f, 'request_error');
  assert.equal(f.statuses.some(entry => entry.event === 'request_cancelled'), false);
  assert.equal(f.statuses.some(entry => entry.event === 'request_complete'), false);
});

test('synchronous and asynchronous telemetry failures never interfere with successful inference', async t => {
  let callbacks = 0;
  const f = await fixture(t, (req, res) => {
    res.writeHead(200, { 'content-type': 'application/json' }); res.end('{"model":"claude-haiku-4-5-20251001"}');
  }, {}, undefined, () => {
    callbacks++;
    if (callbacks % 2) throw new Error('Status storage unavailable');
    return Promise.reject(new Error('Asynchronous status failure'));
  });
  const response = await f.call('/v1/messages', { method: 'POST', body: JSON.stringify(body) });
  assert.equal(response.status, 200);
  assert.equal((await response.json()).model, 'claude-haiku-4-5-20251001');
  await waitForStatus(f, 'request_complete');
  assert.equal(callbacks, 5);
});

test('an upstream timeout after streaming begins reports an error rather than cancellation', async t => {
  const f = await fixture(t, (req, res) => {
    res.writeHead(200, { 'content-type': 'text/event-stream' }); res.write(': ping\n\n');
  }, { upstreamTimeoutMs: 40 });
  const response = await f.call('/v1/messages', { method: 'POST', body: JSON.stringify(body) });
  await assert.rejects(response.text());
  await waitForStatus(f, 'request_error');
  assert.equal(f.statuses.some(entry => entry.event === 'request_cancelled'), false);
  assert.equal(f.statuses.filter(entry => entry.event === 'request_error').length, 1);
});

test('cancellation during classification closes status without exposing a route or forwarding upstream', async t => {
  const decisions = [];
  let evaluating;
  const started = new Promise(resolve => { evaluating = resolve; });
  const f = await fixture(t, () => assert.fail('Cancelled classification must not reach upstream'), {}, (request, { signal }) => {
    evaluating();
    return new Promise((resolve, reject) => signal.addEventListener('abort', () => reject(signal.reason), { once: true }));
  }, undefined, entry => decisions.push(entry));
  const controller = new AbortController();
  const pending = f.call('/v1/messages', { method: 'POST', signal: controller.signal, body: JSON.stringify(body) });
  const rejected = assert.rejects(pending);
  await started;
  controller.abort();
  await rejected;
  await waitForStatus(f, 'request_cancelled');
  assert.deepEqual(f.statuses.map(entry => entry.event), ['request_start', 'request_cancelled']);
  assert.deepEqual(decisions, []);
});

test('disconnect cancels an unlimited Ollama decision through the real router before any upstream call', { timeout: 3000 }, async t => {
  let evaluating;
  const started = new Promise(resolve => { evaluating = resolve; });
  let decisionSignal;
  let bodyCancelled = false;
  let upstreamCalls = 0;
  const paths = [];
  const config = { ...readConfig({ AUTOROUTER_EVALUATOR: 'ollama' }), ollamaTimeoutMs: 0 };
  const router = new Router(config, { fetchImpl: async (url, options) => {
    const path = new URL(url).pathname;
    paths.push(path);
    assert.equal(new URL(url).origin, config.ollamaEndpoint);
    if (path === '/api/show') return Response.json({ details: { parameter_size: '9B' } });
    assert.equal(path, '/v1/systemone');
    assert.equal(JSON.parse(options.body).state.current_task, 'Fix a typo');
    decisionSignal = options.signal;
    return new Response(new ReadableStream({
      pull() { evaluating(); },
      cancel() { bodyCancelled = true; },
    }));
  } });
  const f = await fixture(t, (_req, res) => {
    upstreamCalls++;
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end('{}');
  }, { evaluator: 'ollama', ollamaTimeoutMs: 0 }, (...args) => router.route(...args));
  const controller = new AbortController();
  t.after(() => controller.abort());
  const pending = f.call('/v1/messages', { method: 'POST', signal: controller.signal, body: JSON.stringify(body) });
  const rejected = assert.rejects(pending, error => error.name === 'AbortError');
  await started;
  // A zero deadline must leave the decision pending until its caller cancels.
  await new Promise(resolve => setTimeout(resolve, 30));
  assert.equal(decisionSignal.aborted, false);
  assert.equal(bodyCancelled, false);
  assert.deepEqual(f.statuses.map(entry => entry.event), ['request_start']);
  controller.abort();
  await rejected;
  await waitForStatus(f, 'request_cancelled');
  assert.equal(decisionSignal.aborted, true);
  assert.equal(bodyCancelled, true);
  assert.equal(upstreamCalls, 0);
  assert.deepEqual(paths, ['/api/show', '/v1/systemone']);
  assert.deepEqual(f.statuses.map(entry => entry.event), ['request_start', 'request_cancelled']);
  assert.equal(f.logs.some(entry => entry.event === 'route'), false);
});

test('completed streamed usage is correlated with its request before completion and never added to debug logs', async t => {
  const usage = { input_tokens: 100, output_tokens: 20, cache_creation_input_tokens: 60, cache_read_input_tokens: 200,
    cache_creation: { ephemeral_5m_input_tokens: 40, ephemeral_1h_input_tokens: 20 }, inference_geo: 'us', speed: 'fast', service_tier: 'standard' };
  const sse = `event: message_start\ndata: ${JSON.stringify({ type: 'message_start', message: { model: 'claude-sonnet-5', usage: { ...usage, output_tokens: 1 } } })}\n\n`
    + 'event: message_delta\ndata: {"type":"message_delta","usage":{"output_tokens":20},"delta":{"stop_reason":"end_turn"}}\n\n'
    + 'event: message_stop\ndata: {"type":"message_stop"}\n\n';
  const f = await fixture(t, (req, res) => { res.writeHead(200, { 'content-type': 'text/event-stream' }); res.end(sse); });
  const payload = { ...body, speed: 'fast', inference_geo: 'us', service_tier: 'standard_only' };
  const response = await f.call('/v1/messages', { method: 'POST', headers: { 'x-claude-code-session-id': 'usage-session' }, body: JSON.stringify(payload) });
  assert.equal(await response.text(), sse);
  await waitForStatus(f, 'request_complete');
  assert.deepEqual(f.statuses.map(entry => entry.event), ['request_start', 'route', 'upstream_response', 'upstream_model', 'upstream_usage', 'request_complete']);
  const entry = f.statuses.find(entry => entry.event === 'upstream_usage');
  assert.equal(entry.session_id, 'usage-session');
  assert.equal(entry.request_id, f.statuses[0].request_id);
  assert.deepEqual(entry.usage, usage);
  assert.deepEqual(f.statuses.find(entry => entry.event === 'route').pricing_context, { speed: 'fast', inference_geo: 'us', service_tier: 'standard_only' });
  assert.equal(f.logs.some(entry => entry.event === 'upstream_usage'), false);
});

test('JSON usage cannot count HTTP errors and unknown request pricing fields remain private', async t => {
  const f = await fixture(t, (req, res) => {
    res.writeHead(429, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ model: 'claude-haiku-4-5-20251001', usage: { input_tokens: 10, output_tokens: 20 } }));
  });
  const response = await f.call('/v1/messages', { method: 'POST', body: JSON.stringify({ ...body, speed: 'PRIVATE_SPEED', inference_geo: 'PRIVATE_GEO', service_tier: 'PRIVATE_TIER' }) });
  await response.text();
  await waitForStatus(f, 'request_complete');
  assert.equal(f.statuses.some(entry => entry.event === 'upstream_usage'), false);
  assert.deepEqual(f.statuses.find(entry => entry.event === 'route').pricing_context, { speed: 'unknown', inference_geo: 'unknown', service_tier: 'unknown' });
  assert.ok(!JSON.stringify(f.statuses).includes('PRIVATE_'));
});

test('JSON usage is observed with default request pricing and unsupported advisor or fallback markers', async t => {
  const f = await fixture(t, (req, res) => {
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ model: 'claude-haiku-4-5-20251001', usage: { input_tokens: 10, output_tokens: 20 } }));
  });
  for (const fields of [{}, { fallbacks: [] }, { fallback_credit_token: 'PRIVATE_TOKEN' }, { tools: [{ type: 'advisor_20260630' }] }]) {
    f.statuses.length = 0;
    const response = await f.call('/v1/messages', { method: 'POST', body: JSON.stringify({ ...body, ...fields }) });
    await response.text();
    await waitForStatus(f, 'request_complete');
    const expected = { speed: 'standard', inference_geo: 'global', service_tier: 'auto' };
    if (Object.keys(fields).length) expected.pricing_unsupported = true;
    assert.deepEqual(f.statuses.find(entry => entry.event === 'route').pricing_context, expected);
    assert.deepEqual(f.statuses.find(entry => entry.event === 'upstream_usage').usage, { input_tokens: 10, output_tokens: 20 });
    assert.ok(!JSON.stringify(f.statuses).includes('PRIVATE_TOKEN'));
  }
});
