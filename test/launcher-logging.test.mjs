import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, readFile, writeFile, readdir, stat, rm, access } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { delimiter, dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { listen } from '../src/server.mjs';

const execute = promisify(execFile);
const cli = fileURLToPath(new URL('../bin/autorouter.mjs', import.meta.url));
const payload = text => ({ model: 'claude-sonnet-5', max_tokens: 64, stream: true,
  system: 'PRIVATE_EXECUTOR_SYSTEM', tools: [{ name: 'Read', description: 'PRIVATE_TOOL_SCHEMA', input_schema: { type: 'object' } }],
  messages: [{ role: 'user', content: text }] });
const requests = [
  { session: 'session-alpha', agent: 'shared-agent', prompt: 'alpha-prompt', requestClass: 'main', body: payload('LOG_ONLY_ALPHA_TASK') },
  { session: 'session-beta', agent: 'shared-agent', prompt: 'beta-prompt', requestClass: 'main', body: payload('😀'.repeat(501)) },
  { session: 'session-alpha', agent: 'worker-agent', prompt: 'worker-prompt', requestClass: 'subagent', body: payload('PRIVATE_AGENT_TASK') },
  { session: 'session-alpha', agent: 'shared-agent', prompt: 'review-prompt', requestClass: 'auxiliary', body: payload('PRIVATE_AUXILIARY_REVIEW') },
  { session: 'session-alpha', agent: 'shared-agent', prompt: 'final-prompt', requestClass: 'main', body: payload('LOG_ONLY_FINAL_ROW') },
];

async function launcherFixture(t) {
  const directory = await mkdtemp(join(tmpdir(), 'autorouter logging '));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const configPath = join(directory, 'config.json');
  await writeFile(configPath, JSON.stringify({ AUTOROUTER_EVALUATOR: 'jev' }), { mode: 0o600 });
  let generations = 0;
  const jev = http.createServer(async (req, res) => {
    for await (const _chunk of req) { /* Consume the synthetic evaluator body. */ }
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ answers: { tier: { choice: 'haiku', confidence: 0.99 } } }));
  });
  const upstream = http.createServer(async (req, res) => {
    let text = ''; for await (const chunk of req) text += chunk;
    const request = JSON.parse(text);
    generations++;
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.end('event: message_start\ndata: ' + JSON.stringify({ type: 'message_start', message: { model: request.model } })
      + '\n\nevent: message_stop\ndata: {"type":"message_stop"}\n\n');
  });
  t.after(() => {
    for (const server of [jev, upstream]) { server.closeAllConnections(); server.close(); }
  });
  const jevAddress = await listen(jev, 0);
  const upstreamAddress = await listen(upstream, 0);
  await writeFile(join(directory, 'claude'), `#!/usr/bin/env node
const assert = require('node:assert/strict');
const fs = require('node:fs');
(async () => {
  const requests = JSON.parse(process.env.SYNTHETIC_REQUESTS);
  const send = async request => {
    const response = await fetch(process.env.ANTHROPIC_BASE_URL + '/v1/messages', {
      method: 'POST', headers: { 'content-type': 'application/json', 'x-api-key': process.env.ANTHROPIC_API_KEY,
        'x-claude-code-session-id': request.session, 'x-claude-code-agent-id': request.agent,
        'x-claude-code-prompt-id': request.prompt, 'x-claude-code-request-class': request.requestClass },
      body: JSON.stringify(request.body),
    });
    assert.equal(response.status, 200);
    assert.match(await response.text(), /message_stop/);
  };
  await Promise.all(requests.slice(0, -1).map(send));
  await send(requests.at(-1));
  const snapshot = fs.readFileSync(process.env.AUTOROUTER_STATUS_FILE, 'utf8');
  for (const value of ['LOG_ONLY_', 'PRIVATE_', '😀', 'prompt_excerpt']) assert.ok(!snapshot.includes(value));
  console.log('FAKE_CLAUDE_DONE');
  process.exit(0);
})().catch(() => { console.error('Synthetic launcher check failed'); process.exitCode = 1; });
`, { mode: 0o700 });
  const run = async (logDirectory, mode) => execute(process.execPath, [cli, 'claude'], {
    cwd: directory,
    env: {
      PATH: directory + delimiter + dirname(process.execPath), AUTOROUTER_CONFIG: configPath,
      AUTOROUTER_AUTH_MODE: 'api-key', AUTOROUTER_CLIENT_PROFILE: 'compatible',
      ANTHROPIC_API_KEY: 'synthetic-upstream-key', TYPESAFE_API_KEY: 'synthetic-jev-key',
      AUTOROUTER_UPSTREAM_URL: `http://127.0.0.1:${upstreamAddress.port}`,
      AUTOROUTER_JEV_URL: `http://127.0.0.1:${jevAddress.port}/v1/systemone`,
      SYNTHETIC_REQUESTS: JSON.stringify(requests),
      ...(logDirectory === undefined ? {} : { AUTOROUTER_SESSION_LOG_DIR: logDirectory }),
      ...(mode === undefined ? {} : { AUTOROUTER_SESSION_LOG_MODE: mode }),
    }, timeout: 10000,
  });
  return { directory, configPath, run, generations: () => generations };
}

test('opt-in launcher logs each observed session privately and drains the final row before exit', { timeout: 15000 }, async t => {
  const f = await launcherFixture(t);
  const logDirectory = join(f.directory, 'private logs');
  const first = await f.run(logDirectory, 'prompts');
  assert.equal(first.stdout, 'FAKE_CLAUDE_DONE\n');
  assert.equal(first.stderr, '');
  const names = await readdir(logDirectory);
  assert.equal(names.length, 2);
  assert.equal((await stat(logDirectory)).mode & 0o777, 0o700);
  const originalFiles = new Map();
  for (const name of names) {
    assert.match(name, /^autorouter-session-[A-Za-z0-9-]+\.jsonl$/);
    assert.ok(!name.includes('session-alpha') && !name.includes('session-beta'));
    const path = join(logDirectory, name);
    assert.equal((await stat(path)).mode & 0o777, 0o600);
    const text = await readFile(path, 'utf8');
    originalFiles.set(name, text);
    assert.ok(text.endsWith('\n'));
    const records = text.trimEnd().split('\n').map(line => JSON.parse(line));
    const rows = records.filter(row => row.event === 'decision');
    const outcomes = records.filter(row => row.event === 'outcome');
    assert.equal(outcomes.length, rows.length);
    assert.equal(records.length, rows.length * 2);
    for (const decision of rows) {
      const outcome = outcomes.find(row => row.request_id === decision.request_id);
      assert.ok(outcome);
      assert.equal(outcome.status, 'completed');
      assert.equal(outcome.confirmed_model, decision.selected_model);
      assert.equal(outcome.session_id, decision.session_id);
      assert.equal(outcome.prompt_excerpt, undefined);
      assert.equal(outcome.schema_version, 2);
    }
    const session = rows[0].session_id;
    assert.ok(['session-alpha', 'session-beta'].includes(session));
    assert.ok(rows.every(row => row.session_id === session));
    for (const row of rows) {
      assert.equal(row.schema_version, 2);
      assert.equal(row.event, 'decision');
      assert.ok(Number.isFinite(Date.parse(row.timestamp)));
      assert.match(row.request_id, /^[a-f0-9-]{36}$/);
      assert.equal(row.requested_model, 'claude-sonnet-5');
      assert.equal(typeof row.decision_latency_ms, 'number');
      assert.ok(Number.isFinite(row.decision_latency_ms) && row.decision_latency_ms >= 0);
      assert.equal(row.selected_model, row.request_class === 'auxiliary' ? 'claude-sonnet-5' : 'claude-haiku-4-5-20251001');
      if (['subagent', 'auxiliary'].includes(row.request_class)) assert.equal(row.prompt_excerpt, '');
    }
    if (session === 'session-alpha') {
      assert.equal(rows.length, 4);
      assert.equal(rows.find(row => row.prompt_id === 'alpha-prompt').prompt_excerpt, 'LOG_ONLY_ALPHA_TASK');
      assert.equal(rows.at(-1).prompt_id, 'final-prompt');
      assert.equal(rows.at(-1).prompt_excerpt, 'LOG_ONLY_FINAL_ROW');
      assert.equal(rows.find(row => row.request_class === 'auxiliary').source, 'passthrough');
      assert.deepEqual(new Set(rows.map(row => row.agent_id)), new Set(['shared-agent', 'worker-agent']));
    } else {
      assert.equal(rows.length, 1);
      assert.equal(rows[0].prompt_excerpt, '😀'.repeat(500));
      assert.equal(rows[0].prompt_truncated, true);
    }
    for (const privateValue of ['PRIVATE_', 'synthetic-upstream-key', 'synthetic-jev-key']) assert.ok(!text.includes(privateValue));
  }
  // Resuming the same Claude session in a later router launch must create new
  // files, not append or truncate the earlier launch's private log.
  const second = await f.run(logDirectory, 'prompts');
  assert.equal(second.stdout, 'FAKE_CLAUDE_DONE\n');
  assert.equal(second.stderr, '');
  assert.equal((await readdir(logDirectory)).length, 4);
  for (const [name, text] of originalFiles) assert.equal(await readFile(join(logDirectory, name), 'utf8'), text);
  assert.equal(f.generations(), requests.length * 2);
});

test('metadata-only launcher history contains correlated decisions and outcomes without excerpts', { timeout: 15000 }, async t => {
  const f = await launcherFixture(t);
  const directory = join(f.directory, 'metadata logs');
  await f.run(directory, 'metadata');
  for (const name of await readdir(directory)) {
    const text = await readFile(join(directory, name), 'utf8');
    const records = text.trim().split('\n').map(line => JSON.parse(line));
    assert.ok(records.some(row => row.event === 'decision'));
    assert.ok(records.some(row => row.event === 'outcome'));
    for (const value of ['prompt_excerpt', 'prompt_truncated', 'LOG_ONLY_', 'PRIVATE_', '😀']) assert.ok(!text.includes(value));
  }
});

test('unset logging creates no files and an empty environment override disables a saved directory', { timeout: 15000 }, async t => {
  const f = await launcherFixture(t);
  const before = (await readdir(f.directory)).sort();
  const absent = await f.run();
  assert.equal(absent.stdout, 'FAKE_CLAUDE_DONE\n');
  assert.equal(absent.stderr, '');
  assert.deepEqual((await readdir(f.directory)).sort(), before);
  const logDirectory = join(f.directory, 'must not exist');
  await writeFile(f.configPath, JSON.stringify({ AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_SESSION_LOG_DIR: logDirectory }), { mode: 0o600 });
  const disabled = await f.run('');
  assert.equal(disabled.stdout, 'FAKE_CLAUDE_DONE\n');
  assert.equal(disabled.stderr, '');
  await assert.rejects(access(logDirectory), { code: 'ENOENT' });
  assert.deepEqual((await readdir(f.directory)).sort(), before);
});

test('unavailable log storage warns once without exposing prompts or preventing requests', { timeout: 15000 }, async t => {
  const f = await launcherFixture(t);
  const invalidDirectory = join(f.directory, 'PRIVATE_LOG_LOCATION');
  await writeFile(invalidDirectory, 'existing unrelated file', { mode: 0o600 });
  const result = await f.run(invalidDirectory);
  assert.equal(result.stdout, 'FAKE_CLAUDE_DONE\n');
  assert.equal(result.stderr, 'AutoRouter session logging disabled.\n');
  assert.equal(await readFile(invalidDirectory, 'utf8'), 'existing unrelated file');
  assert.equal(f.generations(), requests.length);
  for (const value of ['LOG_ONLY_', 'PRIVATE_', '😀', f.directory]) assert.ok(!result.stderr.includes(value));
});

test('launcher keeps inference logs out of Claude by default while status and explicit debug logs work', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'autorouter-logging-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  // Explicit fixture config isolates this subprocess from saved user settings.
  const configPath = join(dir, 'autorouter-config.json');
  await writeFile(configPath, JSON.stringify({ AUTOROUTER_EVALUATOR: 'jev' }), { mode: 0o600 });
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
  assert.equal(process.env.AUTOROUTER_EVALUATOR, 'jev');
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
          AUTOROUTER_CONFIG: configPath,
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
