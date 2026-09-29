import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { gzipSync } from 'node:zlib';
import { readConfig } from '../src/config.mjs';
import { createRouterServer, listen } from '../src/server.mjs';
import { Router } from '../src/router.mjs';

const token = 'local-test-token-123456789';
const body = { model: 'claude-sonnet-5', stream: true, max_tokens: 1024, system: [{ type: 'text', text: 'system', cache_control: { type: 'ephemeral' } }], messages: [{ role: 'user', content: 'Fix a typo' }], tools: [{ name: 'Read', input_schema: { type: 'object' } }] };
async function fixture(t, handler, overrides = {}, routeImpl, onStatus) {
  const upstream = http.createServer(handler);
  const address = await listen(upstream, 0);
  let evaluations = 0;
  const logs = [];
  const statuses = [];
  const config = { ...readConfig({ ANTHROPIC_API_KEY: 'upstream-secret', TYPESAFE_API_KEY: 'classifier-secret' }), localToken: token, upstream: `http://127.0.0.1:${address.port}`, ...overrides };
  const server = createRouterServer(config, {
    router: { route: async (...args) => { evaluations++; return routeImpl ? routeImpl(...args) : { model: config.models.haiku, source: 'test' }; } },
    log: entry => logs.push(entry), onStatus: entry => { statuses.push(entry); return onStatus?.(entry); },
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
  const error = JSON.stringify({ type: 'error', error: { type: 'rate_limit_error', message: 'Wait' } });
  const f = await fixture(t, async (req, res) => {
    let text = ''; for await (const chunk of req) text += chunk;
    assert.deepEqual(JSON.parse(text), body);
    res.writeHead(429, { 'retry-after': '7', 'content-type': 'application/json' }); res.end(error);
  });
  const response = await f.call('/v1/messages/count_tokens', { method: 'POST', body: JSON.stringify(body) });
  assert.equal(response.status, 429);
  assert.equal(response.headers.get('retry-after'), '7');
  assert.equal(await response.text(), error);
  assert.equal(f.evaluations(), 0);
  assert.deepEqual(f.statuses, []);
});

test('rejects missing auth, browser origins, malformed JSON, unsupported paths and oversized input locally', async t => {
  const f = await fixture(t, () => assert.fail('Must not reach upstream'), { maxBodyBytes: 1000 });
  assert.equal((await f.call('/v1/messages', { method: 'POST', headers: { 'x-api-key': 'wrong' } })).status, 401);
  assert.equal((await f.call('/health', { headers: { origin: 'https://example.com' } })).status, 403);
  assert.equal((await f.call('/v1/messages', { method: 'POST', body: '{bad' })).status, 400);
  assert.equal((await f.call('/v1/messages', { method: 'POST', body: JSON.stringify({ ...body, messages: [null] }) })).status, 400);
  assert.equal((await f.call('/unexpected')).status, 404);
  assert.equal((await f.call('/v1/messages', { method: 'POST', body: 'x'.repeat(1001) })).status, 413);
  assert.equal((await f.call('/health')).status, 200);
  assert.equal(f.evaluations(), 0);
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
  let evaluating;
  const started = new Promise(resolve => { evaluating = resolve; });
  const f = await fixture(t, () => assert.fail('Cancelled classification must not reach upstream'), {}, (request, { signal }) => {
    evaluating();
    return new Promise((resolve, reject) => signal.addEventListener('abort', () => reject(signal.reason), { once: true }));
  });
  const controller = new AbortController();
  const pending = f.call('/v1/messages', { method: 'POST', signal: controller.signal, body: JSON.stringify(body) });
  const rejected = assert.rejects(pending);
  await started;
  controller.abort();
  await rejected;
  await waitForStatus(f, 'request_cancelled');
  assert.deepEqual(f.statuses.map(entry => entry.event), ['request_start', 'request_cancelled']);
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
