import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, delimiter, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { listen } from '../src/server.mjs';

test('launcher keeps inference logs out of Claude by default while status and explicit debug logs work', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'autorouter-logging-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  let evaluations = 0;
  let generations = 0;
  const jev = http.createServer(async (req, res) => {
    let body = ''; for await (const chunk of req) body += chunk;
    assert.equal(req.headers.authorization, 'Bearer private-classifier-key');
    assert.equal(JSON.parse(body).state.original_task, 'private-prompt-content');
    evaluations++;
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ answers: { tier: { choice: 'sonnet', confidence: 0.99 } } }));
  });
  const upstream = http.createServer(async (req, res) => {
    let body = ''; for await (const chunk of req) body += chunk;
    assert.equal(req.headers['x-api-key'], 'private-upstream-key');
    assert.equal(JSON.parse(body).model, 'claude-sonnet-5');
    generations++;
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.end('event: message_start\ndata: {"type":"message_start","message":{"model":"claude-sonnet-5","usage":{"input_tokens":1000,"output_tokens":1}}}\n\n'
      + 'event: content_block_delta\ndata: {"type":"content_block_delta","delta":{"text":"private-response-content"}}\n\n'
      + 'event: message_delta\ndata: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":100}}\n\n'
      + 'event: message_stop\ndata: {"type":"message_stop"}\n\n');
  });
  t.after(() => {
    for (const service of [jev, upstream]) { service.closeAllConnections(); service.close(); }
  });
  const jevAddress = await listen(jev, 0);
  const upstreamAddress = await listen(upstream, 0);
  await writeFile(join(dir, 'claude'), `#!/usr/bin/env node
const assert = require('node:assert/strict');
const fs = require('node:fs');
(async () => {
  assert.equal(process.env.TYPESAFE_API_KEY, undefined);
  const credential = process.env.ANTHROPIC_API_KEY;
  assert.equal(credential.length, 64);
  assert.notEqual(credential, 'private-upstream-key');
  const response = await fetch(process.env.ANTHROPIC_BASE_URL + '/v1/messages', {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'x-api-key': credential,
      'x-claude-code-session-id': 'logging-session', 'x-claude-code-request-class': 'main' },
    body: JSON.stringify({ model: process.env.ANTHROPIC_MODEL, max_tokens: 64, stream: true,
      messages: [{ role: 'user', content: 'private-prompt-content' }] }),
  });
  assert.equal(response.status, 200);
  assert.match(await response.text(), /private-response-content/);
  let status, savings;
  for (let attempt = 0; attempt < 100; attempt++) {
    const snapshot = JSON.parse(fs.readFileSync(process.env.AUTOROUTER_STATUS_FILE, 'utf8'));
    status = snapshot.sessions['logging-session'];
    savings = snapshot.savings['logging-session'];
    if (status?.phase === 'ready' && status.actual_model === 'claude-sonnet-5' && savings?.requests === 1) break;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  assert.equal(status?.phase, 'ready');
  assert.equal(status.actual_model, 'claude-sonnet-5');
  assert.equal(status.source, 'jev');
  assert.equal(savings.requests, 1);
  assert.equal(savings.unpriced_requests, 0);
  assert.equal(savings.actual_usd, 0.003);
  assert.equal(savings.baseline_usd, 0.006);
  assert.equal(savings.saved_usd, 0.003);
  assert.equal(savings.percent, 50);
  console.log('Claude reply: private-response-content');
})().catch(() => { console.error('Fake Claude validation failed'); process.exitCode = 1; });
`, { mode: 0o700 });
  for (const debug of [false, true]) {
    const { stdout, stderr } = await promisify(execFile)(process.execPath,
      [fileURLToPath(new URL('../bin/autorouter.mjs', import.meta.url)), 'claude'], {
        env: {
          PATH: dir + delimiter + dirname(process.execPath),
          AUTOROUTER_AUTH_MODE: 'api-key', ANTHROPIC_API_KEY: 'private-upstream-key', TYPESAFE_API_KEY: 'private-classifier-key',
          AUTOROUTER_UPSTREAM_URL: `http://127.0.0.1:${upstreamAddress.port}`,
          AUTOROUTER_JEV_URL: `http://127.0.0.1:${jevAddress.port}/v1/systemone`,
          ...(debug ? { AUTOROUTER_DEBUG: '1' } : {}),
        },
        timeout: 10000,
      });
    assert.equal(stdout, 'Claude reply: private-response-content\n');
    if (debug) {
      assert.match(stderr, /AutoRouter listening/);
      for (const event of ['route', 'upstream_response', 'upstream_model']) {
        assert.match(stderr, new RegExp(`"event":"${event}"`));
      }
      assert.match(stderr, /"model":"claude-sonnet-5"/);
    } else assert.equal(stderr, '');
    for (const sensitive of ['private-upstream-key', 'private-classifier-key', 'private-prompt-content', 'private-response-content']) {
      assert.ok(!stderr.includes(sensitive));
    }
  }
  assert.equal(evaluations, 2);
  assert.equal(generations, 2);
});
