#!/usr/bin/env node
// Live subscription validation: prompts and credentials stay out of the report.
import { spawn } from 'node:child_process';
import { randomBytes, createHash } from 'node:crypto';
import { mkdtemp, writeFile, readFile, realpath, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { readConfig, requireKeys } from '../src/config.mjs';
import { buildClaudeEnv } from '../src/auth.mjs';
import { Router, buildState, contextSizeBytes } from '../src/router.mjs';
import { createRouterServer, listen } from '../src/server.mjs';
import { createStatusState } from '../src/status-state.mjs';
import { renderStatusLine } from '../src/statusline.mjs';

const TEST_SOURCE = `import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mergeIntervals } from './merge-intervals.mjs';
test('sorts, merges nested and touching intervals, and preserves the input', () => {
  const input = [[8, 10], [1, 5], [2, 3], [5, 7], [12, 15], [9, 11]];
  const original = structuredClone(input);
  assert.deepEqual(mergeIntervals(input), [[1, 7], [8, 11], [12, 15]]);
  assert.deepEqual(input, original);
});
test('empty, negative, nested and duplicate intervals', () => {
  assert.deepEqual(mergeIntervals([]), []);
  assert.deepEqual(mergeIntervals([[1, 9], [2, 3], [4, 5], [1, 9]]), [[1, 9]]);
  assert.deepEqual(mergeIntervals([[-2, 1], [-5, -3], [-3, -2]]), [[-5, 1]]);
});
`;
const IMPLEMENTATION = `export function mergeIntervals(intervals) {
  const merged = [];
  for (const [start, end] of intervals) {
    const previous = merged.at(-1);
    if (previous && start <= previous[1]) previous[1] = end;
    else merged.push([start, end]);
  }
  return merged;
}
`;
// Keep this opt-in: it deliberately sends over 200K input tokens to reproduce
// a simple task whose system context exceeds Haiku's window. A file avoids the
// platform's process-argument limit, and the fixture is removed after the run.
const LARGE_CONTEXT_SYSTEM = 'This is a synthetic context-capacity validation fixture. The repeated words between the padding markers are irrelevant data. Answer the user normally and do not repeat the padding.\n<synthetic-padding>\n'
  + 'x '.repeat(220000) + '\n</synthetic-padding>\n';
const SYNTHETIC_REMINDER = '<system-reminder>\nSynthetic tool catalog metadata, unrelated to the user task.\n'
  + 'Synthetic catalog entry for a tool that is not enabled. '.repeat(4000) + '\n</system-reminder>';
const CASES = {
  simple: {
    prompts: ['What does the JavaScript expression [].length evaluate to? Reply with only the integer.'],
    check: results => results.length === 1 && results[0].trim() === '0',
  },
  medium: {
    prompts: ['Review this JavaScript event-loop sequence and determine the output order: console.log("A"); Promise.resolve().then(() => { console.log("B"); queueMicrotask(() => console.log("C")); }); console.log("D"); Explain the ordering briefly, then finish with ORDER=A,D,B,C if that is correct, or ORDER= followed by the actual comma-separated order.'],
    check: results => results.length === 1 && /ORDER=A,D,B,C/.test(results[0]),
  },
  difficult: {
    prompts: ['Analyze a distributed locking design rigorously. A and B use a lease service with monotonically increasing fencing tokens. A obtains token 10 and starts a write, then pauses longer than its lease. B obtains token 11, writes the resource, and returns success. A resumes with token 10. The database validates only that the token was once issued by the lease service; it does not remember the largest token observed. Can the design guarantee that A cannot overwrite B? Give a concrete legal schedule, distinguish fencing from mere lease validity, and explain the minimum atomic resource-side check and state needed to repair it, including duplicate retries. Finish with exactly one verdict line: VERDICT=SAFE or VERDICT=UNSAFE.'],
    check: results => results.length === 1 && /VERDICT=UNSAFE/.test(results[0]) && /atomic/i.test(results[0]) && /10/.test(results[0]) && /11/.test(results[0]),
  },
  coding: {
    tools: true,
    prompts: ['In this synthetic fixture, repair mergeIntervals in merge-intervals.mjs so that it sorts and merges overlapping or touching numeric intervals, handles nested intervals, and never changes the input. Use Read to inspect the source and its test, Edit to fix only merge-intervals.mjs, and Bash to execute exactly: node --test merge-intervals.test.mjs. Do not change tests or any other files. After tests pass, summarize the fix briefly.'],
    check: results => results.length === 1 && results[0].length > 0,
  },
  continuation: {
    prompts: [
      'Remember this synthetic test marker for our next message: ROUTER_CONTINUATION_47. Reply exactly ACK.',
      'What was the synthetic test marker I asked you to remember? Reply with only that marker.',
    ],
    check: results => results.length === 2 && results[0].trim() === 'ACK' && results[1].trim() === 'ROUTER_CONTINUATION_47',
  },
  large_context: {
    optIn: true,
    appendSystemPrompt: LARGE_CONTEXT_SYSTEM,
    prompts: ['What does the JavaScript expression [].length evaluate to? Reply with only the integer.'],
    check: results => results.length === 1 && results[0].trim() === '0',
  },
  example_haiku: {
    optIn: true, syntheticReminder: true, expectedTier: 'haiku',
    prompts: ['What does the JavaScript expression `[].length` evaluate to? Reply with only the integer.'],
    check: results => results.length === 1 && results[0].trim() === '0',
  },
  example_sonnet: {
    optIn: true, syntheticReminder: true, expectedTier: 'sonnet',
    prompts: ['Add pagination to this REST endpoint using `page` and `pageSize`. Validate the parameters, preserve existing filtering, and add tests for empty results and out-of-range pages.'],
    check: results => results.length === 1 && results[0].length > 0,
  },
  example_opus: {
    optIn: true, syntheticReminder: true, expectedTier: 'opus',
    prompts: ['Review this distributed locking design: worker A’s lease expires while paused. Worker B acquires a newer fencing token and writes successfully. A resumes and writes using its old token. The database checks only whether a token was ever issued. Explain the failure sequence and design the minimum atomic database check that prevents stale writes, including duplicate retries.'],
    check: results => results.length === 1 && /atomic/i.test(results[0]),
  },
};
CASES.thinking_continuation = {
  prompts: [
    `${CASES.difficult.prompts[0]} Also remember the synthetic marker ROUTER_THINKING_83 for my next message. Keep your analysis concise.`,
    'What was the synthetic marker in my previous message? Reply with only that marker.',
  ],
  check: results => results.length === 2 && /VERDICT=UNSAFE/.test(results[0]) && results[1].trim() === 'ROUTER_THINKING_83',
};

function parseArgs(args) {
  const options = { cases: Object.keys(CASES).filter(name => !CASES[name].optIn), timeoutMs: 120000, model: undefined };
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === '--help') { options.help = true; continue; }
    if (arg === '--no-thinking') { options.noThinking = true; continue; }
    if (['--simulate-evaluator-outage', '--simulate-jev-outage'].includes(arg)) { options.simulateJevOutage = true; continue; }
    if (!['--case', '--timeout-ms', '--model', '--client-model'].includes(arg) || !args[i + 1]) throw new Error('Use --help for supported options');
    const value = args[++i];
    if (arg === '--case') options.cases = value.split(',');
    if (arg === '--model' || arg === '--client-model') options.model = value;
    if (arg === '--timeout-ms') options.timeoutMs = Number(value);
  }
  if (options.cases.some(name => !CASES[name])) throw new Error('Unknown live-test case');
  if (!Number.isInteger(options.timeoutMs) || options.timeoutMs < 1000 || options.timeoutMs > 120000) throw new Error('--timeout-ms must be an integer between 1000 and 120000');
  return options;
}

const family = model => /haiku/i.test(model) ? 'haiku' : /sonnet/i.test(model) ? 'sonnet' : /opus/i.test(model) ? 'opus' : model;
const digest = text => createHash('sha256').update(text).digest('hex');

function metadata(body, context) {
  const blocks = body.messages.flatMap(message => Array.isArray(message.content) ? message.content : []);
  return {
    requested_model: body.model,
    request_class: ['main', 'compaction', 'auxiliary'].includes(context.requestClass) ? context.requestClass : context.requestClass ? 'other' : 'unspecified',
    message_count: body.messages.length,
    message_roles: body.messages.map(message => message.role),
    request_bytes: Buffer.byteLength(JSON.stringify(body)),
    system_bytes: Buffer.byteLength(JSON.stringify(body.system ?? null)),
    tools_bytes: Buffer.byteLength(JSON.stringify(body.tools ?? [])),
    messages_bytes: Buffer.byteLength(JSON.stringify(body.messages)),
    has_system_messages: body.messages.some(message => message.role === 'system'),
    thinking_type: ['enabled', 'adaptive', 'disabled'].includes(body.thinking?.type) ? body.thinking.type : 'unspecified',
    thinking_history_count: blocks.filter(block => ['thinking', 'redacted_thinking'].includes(block.type)).length,
    tool_result_count: blocks.filter(block => block.type === 'tool_result').length,
    tool_count: body.tools?.length ?? 0,
    typed_tools: [...new Set((body.tools ?? []).map(tool => tool.type).filter(Boolean))],
    max_tokens: body.max_tokens,
    has_output_effort: Boolean(body.output_config?.effort),
    has_context_management: Boolean(body.context_management),
  };
}

function runClaude({ cwd, env, prompts, timeoutMs, model, tools, maxTurnsSupported, appendSystemPromptFile }) {
  const args = [
    '--print', '--safe-mode', '--restricted', '--setting-sources', '',
    '--strict-mcp-config', '--mcp-config', '{"mcpServers":{}}',
    '--no-session-persistence', '--input-format', 'stream-json',
    '--output-format', 'stream-json', '--verbose', '--permission-mode', 'dontAsk',
    '--no-chrome', '--tools', tools ? 'Read,Edit,Bash' : '',
  ];
  if (maxTurnsSupported) args.push('--max-turns', '12');
  if (model) args.push('--model', model);
  if (appendSystemPromptFile) args.push('--append-system-prompt-file', appendSystemPromptFile);
  // A single leading slash is relative to the permission settings source,
  // not the filesystem root. Anchor the edit to this isolated working dir.
  if (tools) args.push('--allowedTools', 'Read', 'Edit(./merge-intervals.mjs)', 'Bash(node --test merge-intervals.test.mjs)');
  return new Promise(resolve => {
    const started = performance.now();
    const child = spawn('claude', args, { cwd, env, stdio: ['pipe', 'pipe', 'pipe'] });
    const results = [], assistantModels = new Set(), toolNames = new Set(), modelUsage = {}, failures = [], usageShapes = [];
    const calls = new Map(), toolResults = new Map(), denials = [];
    let buffer = '', resultCount = 0, bytes = 0, stderrBytes = 0, timedOut = false, killedForOutput = false;
    let deniedTools = 0, parseErrors = 0, sentPrompts = 0, killTimer;
    const stop = () => { child.kill('SIGTERM'); killTimer = setTimeout(() => child.kill('SIGKILL'), 2000); killTimer.unref(); };
    const timeout = setTimeout(() => { timedOut = true; stop(); }, timeoutMs);
    function sendNext() {
      if (sentPrompts >= prompts.length) { child.stdin.end(); return; }
      child.stdin.write(`${JSON.stringify({ type: 'user', message: { role: 'user', content: prompts[sentPrompts++] } })}\n`);
    }
    child.stdin.on('error', () => {});
    function callMetadata(name, input = {}) {
      const tool = ['Read', 'Edit', 'Bash'].includes(name) ? name : 'other';
      if (tool === 'Bash') {
        const command = String(input.command ?? '').trim();
        return { tool, command_category: command === 'pwd' ? 'working_directory_lookup' : command === 'node --test merge-intervals.test.mjs' ? 'exact_fixture_test'
          : command.includes('node --test merge-intervals.test.mjs') ? 'fixture_test_with_other_shell_text' : 'other' };
      }
      const path = String(input.file_path ?? '');
      return { tool, target_category: path === 'merge-intervals.mjs' || path === './merge-intervals.mjs' || path === join(cwd, 'merge-intervals.mjs') ? 'fixture_source'
        : path === 'merge-intervals.test.mjs' || path === './merge-intervals.test.mjs' || path === join(cwd, 'merge-intervals.test.mjs') ? 'fixture_test' : 'other' };
    }
    function event(line) {
      if (!line.trim()) return;
      let value;
      try { value = JSON.parse(line); } catch { parseErrors++; return; }
      if (value.type === 'assistant') {
        if (typeof value.message?.model === 'string') assistantModels.add(value.message.model);
        const usage = value.message?.usage;
        if (usage && typeof usage === 'object') {
          const safeEnum = value => value === null ? null : value === undefined ? 'absent'
            : ['global', 'us', 'not_available', 'standard', 'message', 'compaction', 'advisor_message', 'fallback_message'].includes(value) ? value : 'other';
          usageShapes.push({ inference_geo: safeEnum(usage.inference_geo),
            iterations: Array.isArray(usage.iterations) ? usage.iterations.slice(0, 10).map(entry => ({
              type: safeEnum(entry?.type),
              ...Object.fromEntries(['input_tokens', 'output_tokens', 'cache_creation_input_tokens', 'cache_read_input_tokens']
                .filter(key => Number.isSafeInteger(entry?.[key])).map(key => [key, entry[key]])),
            })) : usage.iterations === null ? null : 'absent' });
        }
        for (const block of value.message?.content ?? []) if (block.type === 'tool_use') {
          if (['Read', 'Edit', 'Bash'].includes(block.name)) toolNames.add(block.name);
          calls.set(block.id, callMetadata(block.name, block.input));
        }
      }
      if (value.type === 'user') {
        for (const block of value.message?.content ?? []) if (block.type === 'tool_result') {
          toolResults.set(block.tool_use_id, { is_error: Boolean(block.is_error) });
        }
      }
      if (value.type === 'result') {
        resultCount++;
        results.push(typeof value.result === 'string' ? value.result : '');
        if (value.is_error) failures.push(['error_max_turns', 'error_during_execution', 'error_max_budget_usd'].includes(value.subtype) ? value.subtype : 'claude_result_error');
        deniedTools += value.permission_denials?.length ?? 0;
        for (const denial of value.permission_denials ?? []) {
          denials.push({ id: denial.tool_use_id, ...callMetadata(denial.tool_name, denial.tool_input) });
        }
        for (const [name, usage] of Object.entries(value.modelUsage ?? {})) {
          modelUsage[name] = Object.fromEntries(Object.entries(usage).filter(([key, val]) => /tokens|cost|requests/i.test(key) && typeof val === 'number'));
        }
        if (sentPrompts < prompts.length && !value.is_error) sendNext();
        else child.stdin.end();
      }
    }
    child.stdout.on('data', chunk => {
      bytes += chunk.length;
      if (bytes > 8 * 1024 * 1024) { killedForOutput = true; stop(); return; }
      buffer += chunk.toString();
      let end;
      while ((end = buffer.indexOf('\n')) >= 0) { event(buffer.slice(0, end)); buffer = buffer.slice(end + 1); }
    });
    child.stderr.on('data', chunk => { stderrBytes += chunk.length; });
    child.on('error', () => failures.push('claude_spawn_error'));
    child.on('close', (code, signal) => {
      clearTimeout(timeout); clearTimeout(killTimer);
      if (buffer.trim()) event(buffer);
      const outcomes = [...calls].map(([id, call]) => ({ ...call,
        result_received: toolResults.has(id), is_error: toolResults.get(id)?.is_error ?? null,
        permission_denied: denials.some(denial => denial.id === id),
      }));
      resolve({ results, exit_code: code, exit_signal: signal, timed_out: timedOut, output_limit_exceeded: killedForOutput,
        duration_ms: Math.round(performance.now() - started), result_count: resultCount, assistant_models: [...assistantModels],
        tools_used: [...toolNames], permission_denials: deniedTools, parse_errors: parseErrors,
        stderr_bytes: stderrBytes, failures, tool_outcomes: outcomes, provider_usage_shapes: usageShapes,
        successful_tools: [...new Set(outcomes.filter(outcome => outcome.result_received && !outcome.is_error && !outcome.permission_denied).map(outcome => outcome.tool))],
        denied_tool_details: denials.map(({ id, ...detail }) => detail), client_model_usage_estimate: modelUsage });
    });
    sendNext();
  });
}

function runCommand(command, args, cwd, timeoutMs = 10000) {
  return new Promise(resolve => {
    const child = spawn(command, args, { cwd, stdio: ['ignore', 'pipe', 'ignore'] });
    let stdout = '', overflow = false;
    const timer = setTimeout(() => child.kill('SIGKILL'), timeoutMs);
    child.stdout.on('data', chunk => { if (stdout.length < 100000) stdout += chunk.toString(); else { overflow = true; child.kill('SIGKILL'); } });
    child.on('error', () => {});
    child.on('close', code => { clearTimeout(timer); resolve({ code, stdout, overflow }); });
  });
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help) {
    console.log('Usage: node --env-file=.env scripts/live-validation.mjs [--case simple,medium,difficult,coding,continuation,thinking_continuation,large_context,example_haiku,example_sonnet,example_opus] [--client-model MODEL] [--no-thinking] [--simulate-evaluator-outage] [--timeout-ms 120000]\nUses the existing Claude subscription login. Executes real Claude requests and the configured evaluator (except simulated evaluator outages). --simulate-jev-outage remains an alias. Reports metadata only; fixtures are removed. Model costs in CLI usage are list-price estimates, not subscription charges.\nlarge_context is opt-in and deliberately sends more than 200K input tokens in a synthetic system fixture. example_* cases reproduce reminder-prefixed prompts with synthetic context.');
    return;
  }
  const config = readConfig({ ...process.env, AUTOROUTER_AUTH_MODE: 'subscription', AUTOROUTER_TOKEN: randomBytes(32).toString('hex') });
  requireKeys(config);
  const help = await runCommand('claude', ['--help'], process.cwd());
  if (help.code !== 0 || !help.stdout.includes('--safe-mode') || !help.stdout.includes('--restricted')) throw new Error('A Claude CLI with --safe-mode and --restricted support is required');
  const maxTurnsSupported = help.stdout.includes('--max-turns');
  const version = await runCommand('claude', ['--version'], process.cwd());
  const report = { type: 'live_validation', cli_version: version.stdout.trim().slice(0, 100), auth_mode: 'subscription',
    evaluator: config.evaluator, classifier_timeout_ms: config.evaluator === 'ollama' ? config.ollamaTimeoutMs : config.jevTimeoutMs, client_profile: config.clientProfile,
    client_model: options.model ?? (config.clientProfile === 'compatible' ? config.models.haiku : process.env.ANTHROPIC_MODEL ?? 'default'),
    thinking_disabled: Boolean(options.noThinking || config.clientProfile === 'compatible' || process.env.MAX_THINKING_TOKENS === '0'),
    simulated_classifier_outage: options.simulateJevOutage ?? false,
    cost_basis: 'CLI list-price estimate, not subscription charges', max_turns_supported: maxTurnsSupported, cases: [] };
  const root = await mkdtemp(join(tmpdir(), 'autorouter-live-'));
  try {
    for (const name of options.cases) {
      const dir = await realpath(await mkdtemp(join(root, `${name}-`)));
      const scenario = CASES[name];
      let appendSystemPromptFile;
      if (scenario.appendSystemPrompt) {
        appendSystemPromptFile = join(dir, 'synthetic-system-context.txt');
        await writeFile(appendSystemPromptFile, scenario.appendSystemPrompt, { mode: 0o600 });
      }
      if (scenario.tools) {
        await writeFile(join(dir, 'merge-intervals.mjs'), IMPLEMENTATION);
        await writeFile(join(dir, 'merge-intervals.test.mjs'), TEST_SOURCE);
      }
      const routes = [], proxyErrors = [], upstreamModels = new Set(), upstreamStatuses = [], usageReports = [];
      const statusState = createStatusState({ baselineModel: config.models.opus, directory: root });
      const actualRouter = new Router(config, options.simulateJevOutage ? { fetchImpl: async () => { throw new TypeError('simulated_classifier_outage'); } } : {});
      const router = { async route(body, context) {
        if (scenario.syntheticReminder && !['compaction', 'auxiliary'].includes(context.requestClass)) {
          const first = body.messages.find(message => message.role === 'user');
          const content = typeof first.content === 'string' ? [{ type: 'text', text: first.content }] : first.content;
          first.content = [{ type: 'text', text: SYNTHETIC_REMINDER }, ...content];
        }
        const evidence = metadata(body, context);
        if (scenario.syntheticReminder) {
          evidence.context_guard_bytes = contextSizeBytes(body);
          evidence.classifier_contains_example = JSON.stringify(buildState(body)).includes(JSON.stringify(scenario.prompts[0]).slice(1, -1));
        }
        const decision = await actualRouter.route(body, context);
        routes.push({ ...evidence, ...decision });
        return decision;
      } };
      const server = createRouterServer(config, { router, onStatus: entry => {
        statusState.update(entry);
        if (entry.event === 'upstream_usage') usageReports.push(entry.usage);
      }, log: entry => {
        if (entry.event === 'upstream_model' && typeof entry.model === 'string') upstreamModels.add(entry.model);
        if (entry.event === 'upstream_response') upstreamStatuses.push(entry.status);
        if (['proxy_error', 'invalid_request_shape'].includes(entry.event)) proxyErrors.push({ event: entry.event, status: entry.status });
      } });
      let run, statusSnapshot;
      try {
        const address = await listen(server, 0);
        const env = buildClaudeEnv(config, `http://127.0.0.1:${address.port}`);
        if (options.noThinking) env.MAX_THINKING_TOKENS = '0';
        run = await runClaude({ cwd: dir, env,
          ...scenario, timeoutMs: options.timeoutMs, model: options.model, maxTurnsSupported, appendSystemPromptFile });
      } finally {
        server.closeAllConnections();
        await new Promise(resolve => server.close(resolve));
        try {
          statusState.flush();
          if (statusState.path) statusSnapshot = JSON.parse(await readFile(statusState.path, 'utf8'));
        } finally { statusState.close(); }
      }
      const checks = {
        claude_success: run.exit_code === 0 && !run.timed_out && !run.output_limit_exceeded && run.failures.length === 0,
        expected_result: scenario.check(run.results),
        requests_reached_router: routes.length > 0,
        // Claude's assistant and modelUsage fields can retain its requested
        // model. The server observes the actual upstream response model.
        upstream_model_evidence: upstreamModels.size > 0 && [...upstreamModels].every(model => routes.some(route => family(route.model) === family(model))),
        no_upstream_api_errors: upstreamStatuses.length > 0 && upstreamStatuses.every(status => status >= 200 && status < 300),
        no_proxy_errors: proxyErrors.length === 0,
        no_permission_denials: run.permission_denials === 0,
      };
      if (scenario.tools) {
        checks.original_tests_preserved = digest(await readFile(join(dir, 'merge-intervals.test.mjs'), 'utf8')) === digest(TEST_SOURCE);
        // Reinstall the known test before independent verification even if a
        // model unexpectedly changed it. Never execute a modified test file.
        await writeFile(join(dir, 'merge-intervals.test.mjs'), TEST_SOURCE);
        const test = await runCommand(process.execPath, ['--test', 'merge-intervals.test.mjs'], dir);
        checks.independent_tests_pass = test.code === 0;
        checks.read_edit_bash_exercised = ['Read', 'Edit', 'Bash'].every(tool => run.successful_tools.includes(tool));
        checks.tool_continuation_reached_router = routes.some(route => route.tool_result_count > 0);
      }
      if (name === 'thinking_continuation') {
        checks.first_request_routed_to_opus = family(routes[0]?.model ?? '') === 'opus';
        checks.thinking_history_present = routes.slice(1).some(route => route.thinking_history_count > 0);
        checks.thinking_continuation_pinned = routes.slice(1).some(route => route.reason === 'thinking_history' && family(route.model) === 'opus');
        checks.actual_upstream_stayed_opus = upstreamModels.size > 0 && [...upstreamModels].every(model => family(model) === 'opus');
      }
      if (name === 'large_context') {
        const mainRoutes = routes.filter(route => ['main', 'unspecified'].includes(route.request_class));
        checks.compatible_client_requested_haiku = mainRoutes.length > 0 && mainRoutes.every(route => family(route.requested_model) === 'haiku');
        checks.large_system_context_reached_router = mainRoutes.some(route => route.system_bytes >= Buffer.byteLength(LARGE_CONTEXT_SYSTEM)
          && route.system_bytes > route.request_bytes * 0.9 && route.messages_bytes < 10000);
        checks.capacity_guard_selected_sonnet = mainRoutes.length > 0 && mainRoutes.every(route => family(route.model) === 'sonnet'
          && route.reason === 'context_capacity');
        checks.actual_upstream_used_sonnet = upstreamModels.size > 0 && [...upstreamModels].every(model => family(model) === 'sonnet');
        checks.provider_input_exceeds_haiku_window = usageReports.some(usage => ['input_tokens', 'cache_creation_input_tokens', 'cache_read_input_tokens']
          .reduce((sum, key) => sum + (Number.isSafeInteger(usage[key]) ? usage[key] : 0), 0) > 200000);
      }
      if (scenario.expectedTier) {
        const mainRoutes = routes.filter(route => ['main', 'unspecified'].includes(route.request_class));
        checks.expected_tier = mainRoutes.length > 0 && mainRoutes.every(route => family(route.model) === scenario.expectedTier);
        checks.actual_expected_model = upstreamModels.size > 0 && [...upstreamModels].every(model => family(model) === scenario.expectedTier);
        checks.actual_prompt_reached_classifier = mainRoutes.every(route => route.classifier_contains_example);
        checks.large_reminder_fixture = mainRoutes.every(route => route.context_guard_bytes > 150000);
        if (scenario.expectedTier === 'haiku') checks.counted_context_fits_haiku = mainRoutes.every(route => route.context_check === 'within_budget' && route.counted_input_tokens <= 190000);
      }
      if (options.simulateJevOutage) {
        checks.classifier_outage_fell_back = routes.length > 0 && routes.every(route => route.source === 'fallback');
        checks.fallback_retained_capability_floor = routes.every(route => family(route.model) === (family(route.requested_model) === 'opus' ? 'opus' : 'sonnet'));
      }
      const { results, ...safeRun } = run;
      const item = { case: name, passed: Object.values(checks).every(Boolean), checks, ...safeRun, routes,
        upstream_models: [...upstreamModels], upstream_statuses: upstreamStatuses, proxy_errors: proxyErrors,
        classifier_succeeded: routes.some(route => route.source === config.evaluator),
        model_changed: routes.some(route => route.requested_model !== route.model),
        provider_usage: usageReports,
        api_equivalent_savings: Object.values(statusSnapshot?.savings ?? {}),
        status_lines: Object.keys(statusSnapshot?.savings ?? {}).map(session_id => renderStatusLine({ session_id }, statusSnapshot, { color: false, columns: 160 })),
      };
      report.cases.push(item);
      process.stdout.write(`${JSON.stringify({ type: 'case_result', ...item })}\n`);
    }
  } finally { await rm(root, { recursive: true, force: true }); }
  report.passed = report.cases.every(item => item.passed);
  report.classifier_succeeded = report.cases.every(item => item.classifier_succeeded);
  report.model_changed = report.cases.some(item => item.model_changed);
  process.stdout.write(`${JSON.stringify(report)}\n`);
  if (!report.passed) process.exitCode = 1;
}

main().catch(() => {
  // Avoid accidentally emitting credentials or provider response bodies.
  process.stderr.write('Live validation could not complete. Check CLI availability, configuration, and --help. No request content or credentials were logged.\n');
  process.exitCode = 1;
});
