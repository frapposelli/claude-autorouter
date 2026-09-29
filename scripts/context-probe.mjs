#!/usr/bin/env node
// Capture startup metadata only. By default a local stub answers without tools
// or inference calls. --classify calls only Jev; --live calls Anthropic/Jev
// but forbids tool execution. This report never persists request bodies.
import http from 'node:http';
import { spawn } from 'node:child_process';
import { randomBytes, timingSafeEqual } from 'node:crypto';
import { readConfig, requireKeys } from '../src/config.mjs';
import { buildClaudeEnv, LOCAL_AUTH_HEADER } from '../src/auth.mjs';
import { createRouterServer, listen } from '../src/server.mjs';
import { Router, buildState, contextSizeBytes } from '../src/router.mjs';
import { createStatusState } from '../src/status-state.mjs';
import { renderStatusLine } from '../src/statusline.mjs';
import { readFileSync } from 'node:fs';

const args = process.argv.slice(2);
const options = { cwd: process.cwd(), toolSearch: 'unset' };
const EXAMPLES = {
  haiku: 'What does the JavaScript expression `[].length` evaluate to? Reply with only the integer.',
  sonnet: 'Add pagination to this REST endpoint using `page` and `pageSize`. Validate the parameters, preserve existing filtering, and add tests for empty results and out-of-range pages.',
  opus: 'Review this distributed locking design: worker A’s lease expires while paused. Worker B acquires a newer fencing token and writes successfully. A resumes and writes using its old token. The database checks only whether a token was ever issued. Explain the failure sequence and design the minimum atomic database check that prevents stale writes, including duplicate retries.',
};
for (let i = 0; i < args.length; i++) {
  if (args[i] === '--cwd' && args[i + 1]) options.cwd = args[++i];
  else if (args[i] === '--live') options.live = true;
  else if (args[i] === '--classify') options.classify = true;
  else if (args[i] === '--interactive') options.interactive = true;
  else if (args[i] === '--example' && Object.hasOwn(EXAMPLES, args[i + 1])) options.example = args[++i];
  else if (args[i] === '--tool-search' && ['unset', 'true', 'false'].includes(args[i + 1])) options.toolSearch = args[++i];
  else throw new Error('Usage: node scripts/context-probe.mjs [--cwd PATH] [--tool-search unset|true|false] [--example haiku|sonnet|opus] [--classify] [--live] [--interactive]');
}
if (options.interactive && process.platform !== 'darwin') throw new Error('--interactive currently requires macOS /usr/bin/script');
if (options.interactive && !process.stdin.isTTY) throw new Error('--interactive requires a terminal on stdin; /usr/bin/script cannot allocate its terminal from a pipe');
const prompt = EXAMPLES[options.example] ?? 'Reply only OK.';
const bytes = value => Buffer.byteLength(JSON.stringify(value ?? null));
const config = readConfig({ ...process.env, AUTOROUTER_AUTH_MODE: 'subscription', AUTOROUTER_CLIENT_PROFILE: 'compatible', AUTOROUTER_TOKEN: randomBytes(32).toString('hex') });
const requests = [];
function summarize(body, className, length = bytes(body)) {
  const groups = Object.create(null);
  for (const tool of body.tools ?? []) {
    const raw = /^mcp__([^]+?)__/.exec(tool.name ?? '')?.[1] ?? 'built-in';
    const group = raw.replace(/[^\w.-]/g, '_').slice(0, 80);
    groups[group] ??= { tools: 0, deferred: 0, schema_bytes: 0, active_schema_bytes: 0 };
    const entry = groups[group];
    entry.tools++; entry.schema_bytes += bytes(tool);
    if (tool.defer_loading === true) entry.deferred++;
    else entry.active_schema_bytes += bytes(tool);
  }
  const evaluatorState = buildState(body, config.stateChars);
  const includesPrompt = value => JSON.stringify(value ?? null).includes(JSON.stringify(prompt).slice(1, -1));
  const textBlocks = message => typeof message?.content === 'string' ? [message.content]
    : Array.isArray(message?.content) ? message.content.filter(block => block?.type === 'text' && typeof block.text === 'string').map(block => block.text) : [];
  const promptMessageIndex = (body.messages ?? []).findIndex(message => message.role === 'user' && textBlocks(message).join('\n').includes(prompt));
  const promptBlocks = textBlocks(body.messages?.[promptMessageIndex]);
  const promptBlockIndex = promptBlocks.findIndex(text => text.includes(prompt));
  const roleCounts = { user: 0, assistant: 0, system: 0, other: 0 };
  for (const message of body.messages ?? []) roleCounts[Object.hasOwn(roleCounts, message.role) ? message.role : 'other']++;
  const typedTools = [...new Set((body.tools ?? []).filter(tool => tool.type !== undefined).map(tool =>
    typeof tool.type === 'string' && /^[a-z][a-z0-9_]{0,79}$/.test(tool.type) ? tool.type : 'unknown'))];
  const entry = { request_class: ['main', 'compaction', 'auxiliary'].includes(className) ? className : 'unspecified',
    model: /^[\w.-]{1,100}$/.test(body.model) ? body.model : 'custom',
    body_bytes: length, context_guard_bytes: contextSizeBytes(body), system_bytes: bytes(body.system), messages_bytes: bytes(body.messages),
    tools_bytes: bytes(body.tools), tools: body.tools?.length ?? 0,
    deferred_tools: body.tools?.filter(tool => tool.defer_loading === true).length ?? 0,
    tool_search_present: body.tools?.some(tool => tool.name === 'ToolSearch') ?? false,
    message_roles: roleCounts,
    thinking_type: ['enabled', 'adaptive', 'disabled'].includes(body.thinking?.type) ? body.thinking.type : 'unspecified',
    model_specific_fields: ['context_management', 'speed', 'container', 'mcp_servers', 'output_config'].filter(field => body[field] !== undefined),
    typed_tool_types: typedTools.slice(0, 20), typed_tool_types_truncated: typedTools.length > 20,
    evaluator_state_chars: JSON.stringify(evaluatorState).length,
    evaluator_contains_entered_prompt: includesPrompt(evaluatorState), request_contains_entered_prompt: includesPrompt(body.messages),
    evaluator_current_task_chars: typeof evaluatorState.current_task === 'string' ? evaluatorState.current_task.length : null,
    entered_prompt_message_index: promptMessageIndex, entered_prompt_text_block_index: promptBlockIndex,
    entered_prompt_text_offset: promptBlocks.join('\n').indexOf(prompt), first_user_text_block_chars: textBlocks(body.messages?.find(message => message.role === 'user'))[0]?.length ?? 0,
    groups };
  requests.push(entry);
  return entry;
}
let statusState;
const usages = [], upstreamStatuses = [];
const actualRouter = options.live || options.classify ? new Router(config) : undefined;
if (actualRouter) requireKeys(config);
if (options.live) statusState = createStatusState({ baselineModel: config.models.opus });
const decisionMetadata = decision => ({ selected_model: decision.model, tier: decision.tier,
  classified_tier: decision.classified_tier, confidence: decision.confidence, reason: decision.reason, source: decision.source,
  latency_ms: decision.latency_ms });
let finishInteractive = () => {};
const server = options.live ? createRouterServer(config, {
  router: { async route(body, context) {
    // Keep the actual startup context, but prohibit tool calls at the API so
    // this diagnostic cannot run repository commands or connected-app actions.
    body.tool_choice = { type: 'none' };
    const entry = summarize(body, context.requestClass);
    const decision = await actualRouter.route(body, context);
    Object.assign(entry, decisionMetadata(decision));
    return decision;
  } },
  log: () => {},
  onStatus: event => {
    statusState.update(event);
    if (event.event === 'upstream_response') upstreamStatuses.push(event.status);
    if (event.event === 'upstream_usage') usages.push(event.usage);
    if (event.event === 'request_complete' && (!event.request_class || event.request_class === 'main')) finishInteractive();
  },
}) : http.createServer(async (req, res) => {
  try {
    const credential = Buffer.from(typeof req.headers[LOCAL_AUTH_HEADER] === 'string' ? req.headers[LOCAL_AUTH_HEADER] : '');
    const expected = Buffer.from(config.localToken);
    if (credential.length !== expected.length || !timingSafeEqual(credential, expected)) {
      res.writeHead(401); res.end(); return;
    }
    const url = new URL(req.url, 'http://localhost');
    if (url.pathname !== '/v1/messages' || req.method !== 'POST') { res.writeHead(404); res.end(); return; }
    const chunks = []; let length = 0;
    for await (const chunk of req) {
      length += chunk.length;
      if (length > config.maxBodyBytes) { res.writeHead(413); res.end(); return; }
      chunks.push(chunk);
    }
    const body = JSON.parse(Buffer.concat(chunks));
    const requestClass = req.headers['x-claude-code-request-class'];
    const entry = summarize(body, requestClass, length);
    const decision = actualRouter ? await actualRouter.route(body, {
      requestClass,
      scope: JSON.stringify([req.headers['x-claude-code-session-id'], req.headers['x-claude-code-agent-id']]),
      promptId: req.headers['x-claude-code-prompt-id'],
    }) : undefined;
    if (decision) Object.assign(entry, decisionMetadata(decision));
    if (!requestClass || requestClass === 'main') res.once('finish', () => finishInteractive());
    const message = { id: 'msg_local_context_probe', type: 'message', role: 'assistant', model: decision?.model ?? body.model,
      content: [], stop_reason: null, stop_sequence: null,
      usage: { input_tokens: 0, output_tokens: 0, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 } };
    if (!body.stream) {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ ...message, content: [{ type: 'text', text: 'OK' }], stop_reason: 'end_turn' })); return;
    }
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    const event = value => `event: ${value.type}\ndata: ${JSON.stringify(value)}\n\n`;
    res.end([
      { type: 'message_start', message },
      { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
      { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'OK' } },
      { type: 'content_block_stop', index: 0 },
      { type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 1 } },
      { type: 'message_stop' },
    ].map(event).join(''));
  } catch { if (!res.headersSent) res.writeHead(400); res.end(); }
});

let child;
try {
  const address = await listen(server, 0);
  const env = buildClaudeEnv(config, `http://127.0.0.1:${address.port}`);
  if (options.toolSearch === 'unset') delete env.ENABLE_TOOL_SEARCH;
  else env.ENABLE_TOOL_SEARCH = options.toolSearch;
  // Wait for normal MCP discovery so the measurement includes connected tools.
  env.MCP_CONNECTION_NONBLOCKING = '0';
  const result = await new Promise(resolve => {
    // Interactive stub mode preserves the user's normal tool/capability set.
    // A stub never emits tool_use blocks, so no model-directed action can run.
    const claudeArgs = options.interactive && !options.live ? [] : ['--permission-mode', 'dontAsk'];
    if (!options.interactive) claudeArgs.push('--print', '--no-session-persistence', '--output-format', 'stream-json', '--verbose');
    claudeArgs.push(prompt);
    child = spawn(options.interactive ? '/usr/bin/script' : 'claude', options.interactive ? ['-q', '/dev/null', 'claude', ...claudeArgs] : claudeArgs,
      { cwd: options.cwd, env: { ...env, ...(options.interactive ? { TERM: 'xterm-256color', COLUMNS: '160', LINES: '40' } : {}) },
        detached: true, stdio: [options.interactive ? 'inherit' : 'ignore', 'pipe', 'pipe'] });
    let stdoutBytes = 0, stderrBytes = 0, timedOut = false;
    let killTimer, exitTimer, stopTimer, responseReceived = false;
    const uiSignals = { trust_prompt: false, api_error: false, unknown_option: false };
    finishInteractive = () => {
      if (!options.interactive || responseReceived) return;
      responseReceived = true;
      exitTimer = setTimeout(() => { try { process.kill(-child.pid, 'SIGTERM'); } catch {} }, 1000);
      stopTimer = setTimeout(() => { try { process.kill(-child.pid, 'SIGKILL'); } catch {} }, 5000);
    };
    const timer = setTimeout(() => {
      timedOut = true; try { process.kill(-child.pid, 'SIGTERM'); } catch {}
      killTimer = setTimeout(() => { try { process.kill(-child.pid, 'SIGKILL'); } catch {} }, 3000);
    }, 60000);
    child.stdout.on('data', chunk => {
      stdoutBytes += chunk.length;
      if (options.interactive) {
        const text = chunk.toString('utf8');
        uiSignals.trust_prompt ||= /trust this (?:folder|directory)|trust the files/i.test(text);
        uiSignals.api_error ||= /API Error|Invalid API key/i.test(text);
        uiSignals.unknown_option ||= /unknown option/i.test(text);
      }
    });
    child.stderr.on('data', chunk => { stderrBytes += chunk.length; });
    child.stdin?.on('error', () => {});
    child.once('error', () => { clearTimeout(timer); resolve({ spawn_error: true }); });
    child.once('close', (code, signal) => {
      clearTimeout(timer); clearTimeout(killTimer); clearTimeout(exitTimer); clearTimeout(stopTimer);
      resolve({ code, signal, timed_out: timedOut, stdout_bytes: stdoutBytes, stderr_bytes: stderrBytes,
        ...(options.interactive ? { response_received: responseReceived, controlled_stop: responseReceived && !timedOut, ui_signals: uiSignals } : {}) });
    });
  });
  let statusLines;
  if (statusState?.path) {
    statusState.flush();
    const snapshot = JSON.parse(readFileSync(statusState.path, 'utf8'));
    statusLines = Object.keys(snapshot.sessions).map(session_id => renderStatusLine({ session_id }, snapshot, { color: false, columns: 240 }));
  }
  console.log(JSON.stringify({ probe: options.live ? 'live_inference_tools_prohibited' : options.classify ? 'local_stub_live_jev' : 'local_stub_no_inference',
    mode: options.interactive ? 'interactive' : 'print', example: options.example ?? 'ok', tool_search: options.toolSearch,
    ...result, requests, ...(options.live ? { upstream_statuses: upstreamStatuses, provider_usage: usages, status_lines: statusLines } : {}) }));
  if ((result.code !== 0 && !result.controlled_stop) || !requests.length || (options.live && (!usages.length || upstreamStatuses.some(status => status !== 200)))) process.exitCode = 1;
} finally {
  if (child?.pid) { try { process.kill(-child.pid, 'SIGTERM'); } catch {} }
  server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
  statusState?.close();
}
