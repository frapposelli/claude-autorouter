// Additional exact-tarball checks. Every executable, setting, credential and
// provider response is synthetic; no user Claude installation is invoked.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import http from 'node:http';
import { mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { delimiter, dirname, join } from 'node:path';
import { pathToFileURL } from 'node:url';

const API_KEY = 'synthetic-lifecycle-api-key';
const JEV_KEY = 'synthetic-lifecycle-jev-key';
const OAUTH_TOKEN = 'synthetic-lifecycle-oauth-token';
const HAIKU = 'claude-haiku-4-5-20251001';
const SONNET = 'claude-sonnet-5';
const SONNET_55 = 'claude-sonnet-5-5';
const OPUS = 'claude-opus-5-5';
const isFailedStream = mode => ['sse-error', 'truncated-stream'].includes(mode);

function failedStreamBytes(mode) {
  const events = [
    { type: 'message_start', message: { id: 'synthetic-failed-stream', type: 'message', role: 'assistant', model: OPUS,
      content: [], stop_reason: null, usage: { input_tokens: 1000, output_tokens: 0,
        cache_creation_input_tokens: 0, cache_read_input_tokens: 0 } } },
    { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
    { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'Partial synthetic output 😀' } },
    { type: 'content_block_stop', index: 0 },
    { type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 4 } },
  ];
  if (mode === 'sse-error') events.push({ type: 'error', error: { type: 'overloaded_error', message: 'PRIVATE_SYNTHETIC_STREAM_ERROR' } });
  // Clean EOF after a complete final delta is supported. Cut the next frame
  // short to make this a genuinely incomplete response despite usable counts.
  const partialFrame = mode === 'truncated-stream' ? 'event: message_stop\r\ndata: {"type":"message_st' : '';
  return Buffer.from(': synthetic provider comment\r\n\r\n' + events.map(event =>
    `event: ${event.type}\r\ndata: ${JSON.stringify(event)}\r\n\r\n`).join('') + partialFrame);
}

export function runLifecycleChild(command, args, { cwd, env, signalOnReady, timeoutMs = 10000 } = {}) {
  return new Promise((resolve, reject) => {
    // A private process group lets a failed check clean up its fake Claude too.
    const child = spawn(command, args, { cwd, env, detached: true, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '', stderr = '', ready, signalled = false, overflow = false;
    const kill = () => { try { process.kill(-child.pid, 'SIGKILL'); } catch { child.kill('SIGKILL'); } };
    const timeout = setTimeout(kill, timeoutMs);
    child.stdout.on('data', chunk => {
      stdout += chunk;
      if (stdout.length + stderr.length > 1024 * 1024) { overflow = true; kill(); return; }
      for (const line of stdout.split('\n')) {
        try { const value = JSON.parse(line); if (value.marker === 'lifecycle_ready') ready = value; } catch {}
      }
      if (ready && signalOnReady && !signalled) { signalled = true; child.kill(signalOnReady); }
    });
    child.stderr.on('data', chunk => {
      stderr += chunk;
      if (stdout.length + stderr.length > 1024 * 1024) { overflow = true; kill(); }
    });
    child.once('error', error => { clearTimeout(timeout); reject(error); });
    child.once('close', (code, signal) => {
      clearTimeout(timeout);
      if (overflow || signal === 'SIGKILL') reject(new Error('Packaged launcher exceeded its output or shutdown deadline'));
      else resolve({ code, signal, stdout, stderr, ready });
    });
  });
}

const FAKE_CLAUDE = `#!/usr/bin/env node
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
const expected = JSON.parse(process.env.LIFECYCLE_EXPECTED);
const failedStream = ['sse-error', 'truncated-stream'].includes(expected.mode);
assert.equal(process.env.ANTHROPIC_MODEL, expected.clientModel);
assert.equal(process.env.TYPESAFE_API_KEY, undefined);
assert.notEqual(process.env.ANTHROPIC_API_KEY, ${JSON.stringify(API_KEY)});
const args = process.argv.slice(2);
const settingsFile = args[args.lastIndexOf('--settings') + 1];
assert.ok(settingsFile);
const settings = JSON.parse(await readFile(settingsFile, 'utf8'));
assert.equal(settings.statusLine.type, 'command');
if (expected.profile === 'native') assert.equal(process.env.MAX_THINKING_TOKENS, '1234');
if (expected.profile === 'auto') assert.ok(args.includes('auto'));
const headers = { 'content-type': 'application/json', 'x-api-key': process.env.ANTHROPIC_API_KEY,
  'x-claude-code-session-id': expected.id, 'x-claude-code-prompt-id': 'synthetic-prompt', 'x-claude-code-request-class': 'main' };
const base = process.env.ANTHROPIC_BASE_URL;
assert.equal((await fetch(base + '/health', { headers })).status, 200);
const controller = new AbortController();
const response = await fetch(base + '/v1/messages', { method: 'POST', headers, signal: controller.signal,
  body: JSON.stringify({ model: process.env.ANTHROPIC_MODEL, max_tokens: 16, stream: expected.mode === 'cancel' || failedStream,
    messages: [{ role: 'user', content: 'Return zero.' }] }) });
assert.equal(response.status, 200);
if (expected.mode === 'cancel') {
  const reader = response.body.getReader();
  assert.equal((await reader.read()).done, false);
  controller.abort();
  await reader.cancel().catch(() => {});
} else if (failedStream) {
  assert.equal(response.headers.get('request-id'), 'synthetic-failed-stream');
  assert.deepEqual(Buffer.from(await response.arrayBuffer()), Buffer.from(expected.failedWireBase64, 'base64'));
  assert.equal(controller.signal.aborted, false, 'Upstream failure must be independent of client cancellation');
} else assert.equal((await response.json()).model, expected.selectedModel);
let state;
const phase = expected.mode === 'cancel' ? 'cancelled' : expected.mode === 'sse-error' ? 'error' : 'ready';
for (let attempt = 0; attempt < 100; attempt++) {
  state = JSON.parse(await readFile(process.env.AUTOROUTER_STATUS_FILE, 'utf8')).sessions[expected.id];
  if (state?.phase === phase && (!failedStream || state.completion_confirmed === false)) break;
  await new Promise(resolve => setTimeout(resolve, 10));
}
assert.equal(state?.phase, phase);
if (expected.mode !== 'cancel') assert.equal(state.actual_model, failedStream ? ${JSON.stringify(OPUS)} : expected.selectedModel);
if (failedStream) {
  assert.equal(state.completion_confirmed, false);
  if (expected.mode === 'sse-error') assert.equal(state.error_type, 'overloaded_error');
  const line = execFileSync('/bin/sh', ['-c', settings.statusLine.command], {
    input: JSON.stringify({ session_id: expected.id }), encoding: 'utf8',
    env: { ...process.env, NO_COLOR: '1', COLUMNS: '200' }, timeout: 3000,
  });
  assert.match(line, expected.mode === 'sse-error' ? /error.*overloaded_error/ : /completion unknown/);
  assert.ok(!line.includes('PRIVATE_SYNTHETIC_STREAM_ERROR'));
  const failedRequestId = state.request_id;
  // Retry the same human prompt/identity. Observing Opus in the failed stream
  // must not create a confirmed Opus pin that overrides the Sonnet selection.
  const retry = await fetch(base + '/v1/messages', { method: 'POST', headers,
    body: JSON.stringify({ model: process.env.ANTHROPIC_MODEL, max_tokens: 16, stream: false,
      messages: [{ role: 'user', content: 'Return zero.' }] }) });
  assert.equal(retry.status, 200);
  assert.equal((await retry.json()).model, expected.selectedModel);
  for (let attempt = 0; attempt < 100; attempt++) {
    state = JSON.parse(await readFile(process.env.AUTOROUTER_STATUS_FILE, 'utf8')).sessions[expected.id];
    if (state?.request_id !== failedRequestId && state?.phase === 'ready' && state.completion_confirmed === true) break;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  assert.notEqual(state.request_id, failedRequestId);
  assert.equal(state.phase, 'ready');
  assert.equal(state.reason, 'classified');
  assert.equal(state.completion_confirmed, true);
  assert.equal(state.actual_model, expected.selectedModel);
}
if (expected.mode === 'signal') {
  for (const name of ['SIGINT', 'SIGTERM']) process.once(name, () => {
    process.stdout.write(JSON.stringify({ signal_received: name }) + '\\n', () => process.exit(name === 'SIGINT' ? 130 : 143));
  });
  setInterval(() => {}, 1000);
}
console.log(JSON.stringify({ marker: 'lifecycle_ready', gateway: base, statusFile: process.env.AUTOROUTER_STATUS_FILE, settingsFile }));
if (expected.mode === 'nonzero') process.exitCode = 17;
`;

export async function verifyPackagedLifecycle({ installedCommand, installedRoot, env, directory }) {
  await mkdir(directory, { recursive: true });
  const fakeBin = join(directory, 'fake Claude');
  const noBin = join(directory, 'no executable');
  const runtimeTmp = join(directory, 'runtime temporary files');
  const logDir = join(directory, 'decision logs');
  const configFile = join(directory, 'synthetic user config.json');
  await Promise.all([mkdir(fakeBin), mkdir(noBin), mkdir(runtimeTmp)]);
  await writeFile(join(fakeBin, 'claude'), FAKE_CLAUDE, { mode: 0o700 });
  await writeFile(configFile, '{}\n', { mode: 0o600 });
  let active, providerError, closedStream = false, calls = 0, diagnosticCalls = 0;
  let diagnosticCases = [];
  const provider = http.createServer(async (req, res) => {
    try {
      let raw = ''; for await (const chunk of req) raw += chunk;
      const body = raw ? JSON.parse(raw) : undefined;
      assert.equal(req.headers['x-autorouter-token'], undefined);
      if (active?.diagnostic) {
        assert.equal(req.headers.authorization, undefined);
        assert.equal(req.headers['x-api-key'], undefined);
        let result;
        if (req.url === '/api/version') result = { version: '0.35.0' };
        else if (req.url === '/api/tags') result = { models: [{ name: 'nimble:9b-q4_K_M' }] };
        else if (req.url === '/api/ps') result = { models: [] };
        else if (req.url === '/api/show') {
          assert.equal(body.model, 'nimble:9b-q4_K_M');
          result = { details: { parameter_size: '9B' } };
        } else if (req.url === '/v1/systemone') {
          diagnosticCalls++;
          const startup = body.state.current_task === 'Return the literal word ready.';
          const fixture = diagnosticCases.find(item => item.prompt === body.state.current_task);
          assert.ok(startup || fixture, 'Diagnostic must use only its bundled synthetic fixtures');
          const choice = startup ? 'haiku' : active.collapse ? 'sonnet' : fixture.expected;
          result = { model: body.model, answers: { tier: { type: 'choice', choice, confidence: 1,
            probabilities: Object.fromEntries(['haiku', 'sonnet', 'opus'].map(tier => [tier, tier === choice ? 1 : 0])) } },
            usage: { input_tokens: 900, output_tokens: 1 } };
        } else assert.fail('Diagnostic reached a provider generation, download, or mutation endpoint');
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(JSON.stringify(result));
        return;
      }
      if (req.url === '/v1/systemone') {
        assert.equal(req.headers.authorization, `Bearer ${JEV_KEY}`);
        assert.equal(req.headers['x-api-key'], undefined);
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ answers: { tier: { choice: active.profile === 'auto' ? 'haiku' : 'sonnet', confidence: 0.99 } } }));
        return;
      }
      assert.equal(req.url, '/v1/messages');
      calls++;
      assert.equal(body.model, active.selectedModel);
      assert.deepEqual(body.messages, [{ role: 'user', content: 'Return zero.' }]);
      if (active.oauth) {
        assert.equal(req.headers.authorization, `Bearer ${OAUTH_TOKEN}`);
        assert.equal(req.headers['x-api-key'], undefined);
        assert.equal(req.headers['anthropic-beta'], 'oauth-2025-04-20');
      } else {
        assert.equal(req.headers['x-api-key'], API_KEY);
        assert.equal(req.headers.authorization, undefined);
      }
      const message = { id: 'synthetic-lifecycle-message', type: 'message', role: 'assistant', model: body.model,
        content: [{ type: 'text', text: '0' }], stop_reason: 'end_turn',
        usage: { input_tokens: 1000, output_tokens: 1, cache_creation_input_tokens: 0, cache_read_input_tokens: 0 } };
      if (body.stream) {
        res.writeHead(200, { 'content-type': 'text/event-stream', 'request-id': 'synthetic-failed-stream' });
        if (isFailedStream(active.mode)) {
          const bytes = failedStreamBytes(active.mode);
          const multibyte = bytes.indexOf(Buffer.from('😀'));
          res.write(bytes.subarray(0, 17));
          res.write(bytes.subarray(17, multibyte + 1));
          setImmediate(() => res.end(bytes.subarray(multibyte + 1)));
        } else {
          res.write(`event: message_start\ndata: ${JSON.stringify({ type: 'message_start', message: { ...message, stop_reason: null, content: [] } })}\n\n`);
          res.once('close', () => { closedStream = true; });
        }
      } else {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(JSON.stringify(message));
      }
    } catch (error) {
      providerError = error;
      if (!res.headersSent) res.writeHead(500, { 'content-type': 'application/json' });
      res.end('{"error":{"type":"api_error","message":"Synthetic fixture failure"}}');
    }
  });
  await new Promise((resolve, reject) => { provider.once('error', reject); provider.listen(0, '127.0.0.1', resolve); });
  const endpoint = `http://127.0.0.1:${provider.address().port}`;
  const baseEnv = { ...env, PATH: [fakeBin, dirname(process.execPath)].join(delimiter), TMPDIR: runtimeTmp,
    AUTOROUTER_CONFIG: configFile, AUTOROUTER_AUTH_MODE: 'api-key',
    AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: JEV_KEY, ANTHROPIC_API_KEY: API_KEY,
    AUTOROUTER_JEV_URL: `${endpoint}/v1/systemone`, AUTOROUTER_UPSTREAM_URL: endpoint, AUTOROUTER_SESSION_LOG_DIR: logDir };
  const completed = [];
  try {
    const launchCases = [
      { id: 'compatible', profile: 'compatible', clientModel: HAIKU, selectedModel: SONNET },
      { id: 'native', profile: 'native', clientModel: SONNET_55, selectedModel: SONNET },
      { id: 'auto', profile: 'auto', clientModel: SONNET_55, selectedModel: SONNET_55 },
      { id: 'nonzero', profile: 'compatible', clientModel: HAIKU, selectedModel: SONNET, mode: 'nonzero' },
      { id: 'sigint', profile: 'compatible', clientModel: HAIKU, selectedModel: SONNET, mode: 'signal', signal: 'SIGINT' },
      { id: 'sigterm', profile: 'compatible', clientModel: HAIKU, selectedModel: SONNET, mode: 'signal', signal: 'SIGTERM' },
      { id: 'cancel', profile: 'compatible', clientModel: HAIKU, selectedModel: SONNET, mode: 'cancel' },
      { id: 'sse-error', profile: 'compatible', clientModel: HAIKU, selectedModel: SONNET, mode: 'sse-error' },
      { id: 'truncated-stream', profile: 'compatible', clientModel: HAIKU, selectedModel: SONNET, mode: 'truncated-stream' },
      { id: 'metadata', profile: 'compatible', clientModel: HAIKU, selectedModel: SONNET, logMode: 'metadata' },
    ];
    const expectedCalls = launchCases.reduce((count, item) => count + (isFailedStream(item.mode) ? 2 : 1), 0);
    for (const item of launchCases) {
      active = item;
      const childEnv = { ...baseEnv, AUTOROUTER_CLIENT_PROFILE: item.profile, LIFECYCLE_EXPECTED: JSON.stringify({ ...item,
        ...(isFailedStream(item.mode) ? { failedWireBase64: failedStreamBytes(item.mode).toString('base64') } : {}) }),
        ...(item.logMode ? { AUTOROUTER_SESSION_LOG_MODE: item.logMode } : {}),
        ...(item.profile === 'native' ? { ANTHROPIC_MODEL: SONNET_55, MAX_THINKING_TOKENS: '1234' } : {}) };
      const args = [installedCommand, 'claude', ...(item.profile === 'auto' ? ['--permission-mode', 'auto'] : [])];
      const child = await runLifecycleChild(process.execPath, args, { cwd: directory, env: childEnv, signalOnReady: item.signal });
      if (providerError) throw providerError;
      assert.equal(child.stderr, '', `${item.id}: ${child.stderr}`);
      assert.equal(child.code, item.mode === 'nonzero' ? 17 : item.signal === 'SIGINT' ? 130 : item.signal === 'SIGTERM' ? 143 : 0, item.id);
      assert.ok(child.ready, item.id);
      if (item.signal) assert.ok(child.stdout.includes(`"signal_received":"${item.signal}"`));
      for (const path of [child.ready.statusFile, child.ready.settingsFile]) await assert.rejects(readFile(path), { code: 'ENOENT' });
      await assert.rejects(fetch(`${child.ready.gateway}/health`, { signal: AbortSignal.timeout(500) }));
      assert.deepEqual(await readdir(runtimeTmp), []);
      if (item.mode === 'cancel') {
        for (let retry = 0; retry < 50 && !closedStream; retry++) await new Promise(resolve => setTimeout(resolve, 10));
        assert.equal(closedStream, true);
      }
      completed.push(item.id);
    }
    const missing = await runLifecycleChild(process.execPath, [installedCommand, 'claude'], {
      cwd: directory, env: { ...baseEnv, PATH: noBin, AUTOROUTER_CLIENT_PROFILE: 'compatible' },
    });
    assert.equal(missing.code, 1);
    assert.match(missing.stderr, /Could not launch Claude Code/);
    assert.deepEqual(await readdir(runtimeTmp), []);
    completed.push('missing-executable');
    const { PRICING_VERSION, estimateOutcomeSavings } = await import(pathToFileURL(join(installedRoot, 'src/savings.mjs')));
    const files = await readdir(logDir);
    assert.equal(files.length, launchCases.length);
    for (const file of files) {
      const text = await readFile(join(logDir, file), 'utf8');
      const records = text.trim().split('\n').map(line => JSON.parse(line));
      const decisions = records.filter(row => row.event === 'decision');
      const outcomes = records.filter(row => row.event === 'outcome');
      assert.ok(records.every(row => row.schema_version === 2));
      const decision = decisions[0], outcome = outcomes[0];
      const item = launchCases.find(row => row.id === decision.session_id);
      assert.ok(item);
      const failedStream = isFailedStream(item.mode);
      const completed = item.mode !== 'cancel' && !failedStream;
      assert.equal(records.length, failedStream ? 4 : 2);
      assert.equal(decisions.length, failedStream ? 2 : 1);
      assert.equal(outcomes.length, failedStream ? 2 : 1);
      assert.equal(outcome.request_id, decision.request_id);
      assert.equal(outcome.session_id, decision.session_id);
      assert.equal(outcome.selected_model, item.selectedModel);
      assert.equal(outcome.confirmed_model, failedStream ? OPUS : item.selectedModel);
      assert.deepEqual(outcome.model_transitions, [failedStream ? OPUS : item.selectedModel]);
      assert.equal(outcome.status, item.mode === 'cancel' ? 'cancelled' : item.mode === 'sse-error' ? 'error' : 'completed');
      assert.equal(outcome.completion_confirmed, completed);
      assert.equal(outcome.pricing_version, PRICING_VERSION);
      assert.equal(outcome.baseline_model, 'claude-opus-5-5');
      for (const key of ['first_response_ms', 'total_latency_ms', 'routing_latency_ms']) assert.ok(Number.isFinite(outcome[key]) && outcome[key] >= 0, key);
      assert.equal(estimateOutcomeSavings(outcome).priced, completed);
      if (failedStream) {
        assert.equal(outcome.usage_complete, false);
        assert.equal(outcome.pricing_eligible, false);
        assert.equal(outcome.unpriced_reason, item.mode === 'sse-error' ? 'request_failed' : 'unconfirmed_completion');
        assert.equal(outcome.error_type, item.mode === 'sse-error' ? 'overloaded_error' : undefined);
        assert.ok(!text.includes('PRIVATE_SYNTHETIC_STREAM_ERROR'));
        const retryDecision = decisions[1], retryOutcome = outcomes[1];
        assert.notEqual(retryDecision.request_id, decision.request_id);
        assert.equal(retryDecision.prompt_id, decision.prompt_id);
        assert.equal(retryDecision.reason, 'classified');
        assert.equal(retryDecision.selected_model, item.selectedModel);
        assert.equal(retryOutcome.request_id, retryDecision.request_id);
        assert.equal(retryOutcome.status, 'completed');
        assert.equal(retryOutcome.completion_confirmed, true);
        assert.equal(retryOutcome.confirmed_model, item.selectedModel);
        assert.equal(estimateOutcomeSavings(retryOutcome).priced, true);
      }
      if (item.logMode === 'metadata') {
        assert.ok(records.every(row => !Object.hasOwn(row, 'prompt_excerpt') && !Object.hasOwn(row, 'prompt_truncated')));
        assert.ok(!text.includes('Return zero.'));
      } else assert.equal(decision.prompt_excerpt, 'Return zero.');
      for (const credential of [API_KEY, JEV_KEY, OAUTH_TOKEN]) assert.ok(!text.includes(credential));
    }

    // History inspects only the installed private log files. It needs neither
    // evaluator credentials nor access to a Claude login or running gateway.
    const historyEnv = { ...baseEnv };
    for (const key of ['TYPESAFE_API_KEY', 'ANTHROPIC_API_KEY', 'AUTOROUTER_JEV_URL', 'AUTOROUTER_UPSTREAM_URL']) delete historyEnv[key];
    const listed = await runLifecycleChild(process.execPath, [installedCommand, 'sessions', 'list', '--json'], { cwd: directory, env: historyEnv });
    assert.equal(listed.code, 0, listed.stderr);
    assert.equal(listed.stderr, '');
    const history = JSON.parse(listed.stdout);
    assert.equal(history.type, 'session_history');
    assert.equal(history.logging_enabled, true);
    assert.equal(history.sessions.length, launchCases.length);
    assert.equal(history.coverage.partial, false);
    for (const summary of history.sessions) {
      const cancelled = summary.session_id === 'cancel';
      const streamError = summary.session_id === 'sse-error';
      const truncated = summary.session_id === 'truncated-stream';
      assert.equal(summary.decisions, streamError || truncated ? 2 : 1);
      assert.equal(summary.outcomes, streamError || truncated ? 2 : 1);
      assert.equal(summary.completed, cancelled ? 0 : 1);
      assert.equal(summary.failed, streamError ? 1 : 0);
      assert.equal(summary.unconfirmed, truncated ? 1 : 0);
      assert.equal(summary.cancelled, cancelled ? 1 : 0);
      assert.equal(summary.savings.priced_requests, cancelled ? 0 : 1);
      assert.equal(summary.savings.unpriced_requests, cancelled || streamError || truncated ? 1 : 0);
    }
    const metadata = history.sessions.find(row => row.session_id === 'metadata');
    for (const json of [false, true]) {
      const shown = await runLifecycleChild(process.execPath, [installedCommand, 'sessions', 'show', metadata.id, ...(json ? ['--json'] : [])],
        { cwd: directory, env: historyEnv });
      assert.equal(shown.code, 0, shown.stderr);
      assert.equal(shown.stderr, '');
      assert.ok(!shown.stdout.includes('Return zero.'));
      if (json) {
        const detail = JSON.parse(shown.stdout);
        assert.equal(detail.summary.id, metadata.id);
        assert.equal(detail.records.length, 2);
        assert.equal(detail.summary.savings.priced_requests, 1);
      } else assert.match(shown.stdout, /confirmed completed/);
    }
    const disabledEnv = { ...historyEnv }; delete disabledEnv.AUTOROUTER_SESSION_LOG_DIR;
    const disabled = await runLifecycleChild(process.execPath, [installedCommand, 'sessions', 'list', '--json'], { cwd: directory, env: disabledEnv });
    assert.equal(disabled.code, 0, disabled.stderr);
    assert.equal(JSON.parse(disabled.stdout).logging_enabled, false);
    assert.equal(calls, expectedCalls, 'History commands must not call providers');
    assert.deepEqual(await readdir(runtimeTmp), []);
    completed.push('sessions-list-show-metadata');

    // Subscription mode deliberately fixes its production upstream. Exercise
    // the installed gateway and auth modules with an explicit loopback fixture
    // instead of changing that production restriction or touching a login.
    const { readConfig } = await import(pathToFileURL(join(installedRoot, 'src/config.mjs')));
    const { buildClaudeEnv } = await import(pathToFileURL(join(installedRoot, 'src/auth.mjs')));
    const { createRouterServer, listen } = await import(pathToFileURL(join(installedRoot, 'src/server.mjs')));
    for (const profile of ['compatible', 'native', 'auto']) {
      active = { profile, oauth: true, selectedModel: profile === 'auto' ? SONNET_55 : SONNET };
      const config = readConfig({ AUTOROUTER_AUTH_MODE: 'subscription', AUTOROUTER_CLIENT_PROFILE: profile,
        AUTOROUTER_TOKEN: 'synthetic-loopback-local-token-123', TYPESAFE_API_KEY: JEV_KEY, AUTOROUTER_JEV_URL: `${endpoint}/v1/systemone` });
      config.upstream = endpoint;
      const gateway = createRouterServer(config, { log: () => {} });
      try {
        const address = await listen(gateway, 0);
        const childEnv = buildClaudeEnv(config, `http://127.0.0.1:${address.port}`, { ANTHROPIC_MODEL: SONNET_55 });
        const localHeaders = Object.fromEntries(childEnv.ANTHROPIC_CUSTOM_HEADERS.split('\n').map(line => {
          const colon = line.indexOf(':'); return [line.slice(0, colon), line.slice(colon + 1).trim()];
        }));
        const response = await fetch(`${childEnv.ANTHROPIC_BASE_URL}/v1/messages`, { method: 'POST',
          headers: { ...localHeaders, authorization: `Bearer ${OAUTH_TOKEN}`, 'anthropic-beta': 'oauth-2025-04-20', 'content-type': 'application/json' },
          body: JSON.stringify({ model: childEnv.ANTHROPIC_MODEL, max_tokens: 16, messages: [{ role: 'user', content: 'Return zero.' }] }),
          signal: AbortSignal.timeout(3000) });
        assert.equal(response.status, 200);
        assert.equal((await response.json()).model, active.selectedModel);
        if (providerError) throw providerError;
        completed.push(`oauth-${profile}`);
      } finally { gateway.closeAllConnections(); await new Promise(resolve => gateway.close(resolve)); }
    }
    assert.equal(calls, expectedCalls + 3);
    const diagnostic = await import(pathToFileURL(join(installedRoot, 'src/local-diagnostic.mjs')));
    diagnosticCases = diagnostic.LOCAL_DIAGNOSTIC_CASES;
    for (const collapse of [false, true]) {
      active = { diagnostic: true, collapse };
      const before = await readFile(configFile, 'utf8');
      const result = await runLifecycleChild(process.execPath, [installedCommand, 'doctor', '--evaluate-local', '--json'], {
        cwd: directory, env: { ...baseEnv, AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_OLLAMA_MODEL: 'nimble:9b-q4_K_M', AUTOROUTER_OLLAMA_URL: endpoint },
      });
      if (providerError) throw providerError;
      assert.equal(result.stderr, '');
      assert.equal(result.code, collapse ? 1 : 0, result.stdout);
      const report = JSON.parse(result.stdout);
      assert.equal(report.type, 'local_evaluator_diagnostic');
      assert.equal(report.passed, !collapse);
      assert.equal(report.rows.length, diagnosticCases.length);
      assert.equal(report.paid_provider_calls, 0);
      assert.equal(report.downloads, 0);
      assert.equal(report.configuration_changed, false);
      if (collapse) assert.equal(report.gates.coverage.passed, false);
      assert.equal(await readFile(configFile, 'utf8'), before);
      assert.deepEqual(await readdir(runtimeTmp), []);
      completed.push(collapse ? 'local-diagnostic-rejects-collapsed-tiers' : 'local-diagnostic');
    }
    assert.equal(calls, expectedCalls + 3, 'Local diagnostics must not generate Claude responses');
    assert.equal(diagnosticCalls, 2 * (diagnosticCases.length + 1));
    return completed;
  } finally { provider.closeAllConnections(); await new Promise(resolve => provider.close(resolve)); }
}
