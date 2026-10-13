// Supplemental exact public-API controls for cache capacities, TTL and ownership.
// Run alone: the deterministic cache wall clock replaces Date.now in this process.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

assert.equal(process.argv.length, 4, 'Pass the frozen reference and a new output directory');
const reference = resolve(process.argv[2]), output = resolve(process.argv[3]);
const root = resolve(process.env.AUTOROUTER_CACHE_CAPTURE_REPO ?? resolve(import.meta.dirname, '../..'));
const { verifyBaseline } = await import(pathToFileURL(join(root, 'scripts/rust-reference.mjs')).href);
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
assert.equal((await verifyBaseline(reference)).files_verified, 111);
const baseline = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json')));
const [{ readConfig }, { Router }, { createTokenCounter }] = await Promise.all(
  ['config', 'router', 'token-counter'].map(name => import(pathToFileURL(join(reference, `src/${name}.mjs`)).href)),
);
const env = { AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: 'synthetic-cache-evaluator-key', ANTHROPIC_API_KEY: 'synthetic-cache-provider-key' };
const config = readConfig(env);
assert.equal(config.cacheEntries, 1000);
assert.equal(config.cacheTtlMs, 300000);
const base = { model: 'claude-sonnet-5', max_tokens: 4096, messages: [{ role: 'user', content: 'Synthetic cache task INDEX' }] };
const body = index => ({ ...base, messages: [{ role: 'user', content: `Synthetic cache task ${index}` }] });
const countHeaders = { authorization: 'Bearer synthetic-count-account', 'anthropic-beta': 'synthetic-cache-beta', 'anthropic-version': '2023-06-01' };
const response = tier => JSON.stringify({ answers: { tier: { choice: tier, confidence: 0.99 } } });
const plans = [];
for (const kind of ['classifier', 'count']) {
  const capacity = kind === 'classifier' ? 1000 : 100;
  const call = (index, expect, expectedCalls, expectedCount) => ({ op: 'call', index, expect, expected_calls: expectedCalls, ...(expectedCount === undefined ? {} : { expected_count: expectedCount }) });
  plans.push({ id: `${kind}-default-capacity`, kind, cache_options: { defaults: true }, capacity, program: [
    { op: 'range', start: 0, count: capacity, expect: 'miss', calls_start: 1, ...(kind === 'count' ? { count_start: 1 } : {}) },
    { op: 'range', start: 0, count: capacity, expect: 'hit', calls_constant: capacity, ...(kind === 'count' ? { count_start: 1 } : {}) },
    call(0, 'hit', capacity, kind === 'count' ? 1 : undefined),
    call(capacity, 'miss', capacity + 1, kind === 'count' ? capacity + 1 : undefined),
    call(2, 'hit', capacity + 1, kind === 'count' ? 3 : undefined),
    call(0, 'hit', capacity + 1, kind === 'count' ? 1 : undefined),
    call(capacity, 'hit', capacity + 1, kind === 'count' ? capacity + 1 : undefined),
    call(1, 'miss', capacity + 2, kind === 'count' ? capacity + 2 : undefined),
    call(3, 'miss', capacity + 3, kind === 'count' ? capacity + 3 : undefined),
  ] });
  plans.push({ id: `${kind}-default-ttl`, kind, cache_options: { defaults: true }, program: [
    call(0, 'miss', 1, kind === 'count' ? 1 : undefined),
    { op: 'advance', milliseconds: 299999 }, call(0, 'hit', 1, kind === 'count' ? 1 : undefined),
    { op: 'advance', milliseconds: 1 }, call(0, 'miss', 2, kind === 'count' ? 2 : undefined),
    { op: 'advance', milliseconds: 299999 }, call(0, 'hit', 2, kind === 'count' ? 2 : undefined),
    { op: 'advance', milliseconds: 2 }, call(0, 'miss', 3, kind === 'count' ? 3 : undefined),
  ] });
  for (const mode of ['zero-ttl', 'zero-capacity']) plans.push({ id: `${kind}-${mode}`, kind,
    cache_options: mode === 'zero-ttl' ? { ttl_ms: 0 } : { entries: 0 },
    program: [call(0, 'miss', 1, kind === 'count' ? 1 : undefined), call(0, 'miss', 2, kind === 'count' ? 2 : undefined)],
  });
}
const toolInitial = { ...base, messages: [{ role: 'user', content: 'Active synthetic tool task' }], tools: [{ name: 'Read', input_schema: { type: 'object', properties: {} } }] };
const humanInitial = { ...base, messages: [{ role: 'user', content: 'Active synthetic human task' }] };
const toolContinue = { ...toolInitial, messages: [...toolInitial.messages,
  { role: 'assistant', content: [{ type: 'tool_use', id: 'owned-tool', name: 'Read', input: {} }] },
  { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'owned-tool', content: 'Synthetic tool result' }] },
] };
plans.push({ id: 'active-ownership-survives-cache-pressure', kind: 'continuity', cache_options: { defaults: true }, turn_ttl_ms: 0, fill_count: 1001,
  initial_tool_body: toolInitial, initial_human_body: humanInitial, tool_continuation_body: toolContinue,
  initial_tool_options: { scope: 'active-tool-scope', promptId: 'active-tool', requestId: 'active-tool-start' },
  initial_human_options: { scope: 'active-human-scope', promptId: 'active-human', requestId: 'active-human-start' },
  advance_ms: 300001,
});
const originalNow = Date.now;
const raw = [], summaries = [];
let now = 1000000, assertions = 0;
const equal = (got, wanted, message) => { assertions++; assert.deepEqual(got, wanted, message); };
const truth = (value, message) => { assertions++; assert.ok(value, message); };
function snapshot(value) {
  if (value === undefined) return { $js_type: 'undefined' };
  return JSON.parse(JSON.stringify(value));
}
try {
  Date.now = () => now;
  for (const plan of plans) {
    now = 1000000;
    const observed = { id: plan.id, calls: [], steps: [] };
    let calls = 0;
    const fetchImpl = async (url, options) => {
      calls++;
      truth(calls <= 1010, 'bounded synthetic transport');
      const serialized = options.body;
      truth(typeof serialized === 'string' && Buffer.byteLength(serialized) < 8192, 'complete bounded wire body');
      const headers = Object.fromEntries(new Headers(options.headers));
      const bytes = plan.kind === 'count' ? JSON.stringify({ input_tokens: calls }) : response(plan.kind === 'continuity' && calls <= 2 ? 'opus' : 'haiku');
      observed.calls.push({ url: String(url), method: options.method, headers, body_utf8: serialized, response_utf8: bytes, wall_ms: now });
      return new Response(bytes, { status: 200, headers: { 'content-type': 'application/json' } });
    };
    if (plan.kind === 'continuity') {
      const router = new Router({ ...config, turnTtlMs: plan.turn_ttl_ms }, { fetchImpl, now: () => now });
      const opus = config.models.opus, haiku = config.models.haiku;
      const evidence = (model, tools = []) => ({ model, continuation_model: model, stop_reason: tools.length ? 'tool_use' : 'end_turn', tool_uses: tools });
      const route = async (request, options, expectedModel, expectedReason, completeEvidence, expectedAcceptance) => {
        const before = JSON.stringify(request), decision = await router.route(request, options);
        equal(JSON.stringify(request), before, 'authoritative body immutable');
        equal(decision.model, expectedModel, 'actual route model');
        equal(decision.reason, expectedReason, 'actual continuity/routing reason');
        const accepted = router.complete(options.requestId, completeEvidence);
        equal(accepted, expectedAcceptance, 'actual completion acceptance');
        equal(router.turns.attempts.size, 0, 'owned attempts retired');
        observed.steps.push({ operation: 'route_complete', body_utf8: before, options, decision, complete_evidence: completeEvidence, accepted, calls, wall_ms: now,
          continuity: { records: router.turns.records.size, attempts: router.turns.attempts.size, aliases: router.turns.aliases.size } });
        return decision;
      };
      await route(plan.initial_tool_body, plan.initial_tool_options, opus, 'classified', evidence(opus, [{ id: 'owned-tool', model: opus }]), true);
      await route(plan.initial_human_body, plan.initial_human_options, opus, 'classified', evidence(opus), true);
      for (let index = 0; index < plan.fill_count; index++) {
        await route(body(index), { scope: 'filler-scope', promptId: `filler-${index}`, requestId: `filler-request-${index}` }, haiku, 'classified', evidence(haiku), true);
        equal(router.decisions.values.size, Math.min(index + 3, config.cacheEntries), 'default1000 decision entries after actual routing');
        truth(router.turns.records.size <= 1000, 'tool ownership is bounded independently');
      }
      equal(calls, 1003, '1001 filling tasks plus two active tasks');
      now += plan.advance_ms;
      observed.steps.push({ operation: 'advance', milliseconds: plan.advance_ms, wall_ms: now });
      const tool = await route(plan.tool_continuation_body, { ...plan.initial_tool_options, requestId: 'tool-continue' }, opus, 'tool_turn_pinned', null, false);
      equal(tool.classified_tier, 'haiku', 'Opus ownership overrides fresh Haiku evaluation');
      const human = await route(plan.initial_human_body, { ...plan.initial_human_options, requestId: 'human-repeat' }, opus, 'prompt_turn_pinned', null, false);
      equal(human.classified_tier, 'haiku', 'active human pin overrides fresh Haiku evaluation');
      await route(body('new-human'), { scope: 'active-human-scope', promptId: 'new-human', requestId: 'new-human-request' }, haiku, 'classified', evidence(haiku), true);
      equal(calls, 1006, 'all post-expiry decisions were fresh evaluations');
      const owned = await route(plan.tool_continuation_body, { ...plan.initial_tool_options, requestId: 'tool-after-new-human' }, opus, 'tool_turn_pinned', null, false);
      equal(owned.classified_tier, 'haiku', 'same owned tool still overrides cached Haiku after another scope advances');
      equal(owned.source, 'cache', 'ownership confirmation has actual evaluator cache hit');
      equal(calls, 1006, 'ownership confirmation does not invent an evaluator call');
      equal(router.turns.toolOwner('active-tool-scope', ['owned-tool'])?.pin?.model, opus, 'confirmed tool owner survives new human tasks');
      summaries.push({ id: plan.id, expanded_calls: observed.steps.filter(s => s.operation === 'route_complete').length, transport_calls: calls, final_decision_entries: router.decisions.values.size,
        final_continuity: { records: router.turns.records.size, attempts: router.turns.attempts.size, aliases: router.turns.aliases.size }, classified_after_expiry: 'haiku', owned_model: opus });
    } else {
      const options = plan.cache_options;
      const nativeConfig = { ...config, ...(options.entries === undefined ? {} : { cacheEntries: options.entries }), ...(options.ttl_ms === undefined ? {} : { cacheTtlMs: options.ttl_ms }) };
      const router = plan.kind === 'classifier' ? new Router(nativeConfig, { fetchImpl, now: () => now }) : undefined;
      const count = plan.kind === 'count' ? createTokenCounter({ ...config, ...(options.entries === undefined ? {} : { tokenCountCacheEntries: options.entries }), ...(options.ttl_ms === undefined ? {} : { tokenCountCacheTtlMs: options.ttl_ms }) }, { fetchImpl }) : undefined;
      const call = async step => {
        const request = body(step.index), before = JSON.stringify(request);
        const value = router ? await router.classify(request) : await count(request, base.model, { headers: countHeaders, search: '?beta=true' });
        equal(JSON.stringify(request), before, 'cache work leaves complete request immutable');
        equal(calls, step.expected_calls, 'actual evaluator/count calls');
        if (router) {
          equal(value.source, step.expect === 'hit' ? 'cache' : 'jev', 'exact hit/miss');
          equal(value.tier, 'haiku', 'actual successful cached decision');
          equal(router.decisions.values.size <= (options.entries ?? 1000), true, 'literal classifier cache capacity');
          equal(router.turns.records.size, 0, 'classification never owns human-task state');
        } else equal(value, step.expected_count, 'original successful count retained or re-evaluated');
        observed.steps.push({ operation: plan.kind, body_utf8: before, index: step.index, expected: step, value: snapshot(value), calls, wall_ms: now,
          ...(router ? { decision_entries: router.decisions.values.size, continuity: { records: router.turns.records.size, attempts: router.turns.attempts.size, aliases: router.turns.aliases.size } } : {}) });
      };
      for (const step of plan.program) {
        if (step.op === 'advance') { now += step.milliseconds; observed.steps.push({ operation: 'advance', milliseconds: step.milliseconds, wall_ms: now }); }
        else if (step.op === 'call') await call(step);
        else {
          equal(step.op, 'range', 'finite prescribed program');
          truth(step.count <= 1000, 'bounded schedule range');
          for (let offset = 0; offset < step.count; offset++) await call({ op: 'call', index: step.start + offset, expect: step.expect,
            expected_calls: step.calls_constant ?? step.calls_start + offset, ...(step.count_start === undefined ? {} : { expected_count: step.count_start + offset }) });
        }
      }
      summaries.push({ id: plan.id, expanded_calls: observed.steps.filter(s => s.operation !== 'advance').length, transport_calls: calls,
        ...(router ? { final_decision_entries: router.decisions.values.size, final_continuity: { records: router.turns.records.size, attempts: router.turns.attempts.size, aliases: router.turns.aliases.size } } : {}) });
    }
    raw.push(observed);
  }
} finally { Date.now = originalNow; }
assert.equal((await verifyBaseline(reference)).files_verified, 111);
equal(plans.length, 9, 'all nine declared controls executed');
const fixtures = plans.map(plan => ({ ...plan, config_env: env, base_body: base, count_headers: countHeaders, count_search: '?beta=true', clock_origin_ms: 1000000 }));
const corpus = fixtures.map(fixture => JSON.stringify(fixture)).join('\n') + '\n';
const serializedRaw = JSON.stringify(raw, null, 2) + '\n';
assert.ok(Buffer.byteLength(serializedRaw) <= 16 * 1024 * 1024, 'private full transcript bound');
mkdirSync(output);
writeFileSync(join(output, 'cache-boundary-contracts.jsonl'), corpus);
writeFileSync(join(output, 'raw-observations.json'), serializedRaw);
const sources = ['src/config.mjs', 'src/router.mjs', 'src/token-counter.mjs', 'src/turn-state.mjs'].map(path => ({ path, sha256: hash(readFileSync(join(reference, path))) }));
const capture = { schema_version: 1, kind: 'supplemental_exact_public_cache_boundary_controls', baseline_commit: baseline.baseline_commit, status: 'passed',
  sources, generator_sha256: hash(readFileSync(import.meta.filename)), cases_sha256: hash(corpus), raw_observations_sha256: hash(serializedRaw),
  node: { version: process.version, executable_sha256: hash(readFileSync(process.execPath)) }, clock: { source: 'isolated process Date.now replacement, Router.now also supplied', origin_ms: 1000000, no_real_sleep: true },
  network: 'Only injected synthetic fetch callbacks; no sockets, live evaluator/provider calls or user configuration writes',
  schedule_encoding: 'Finite explicit range programs expand every public-API call. Full source requests, evaluator/count wire bytes, responses, results, completion evidence and ownership snapshots are retained unmodified in the hashed private raw transcript. No request is omitted from evidence.',
  definitions: summaries, assertions_executed: assertions, fixtures_executed: raw.length, cleanup: { restored_date_now: Date.now === originalNow, baseline_files_verified: 111 } };
writeFileSync(join(output, 'cache-boundary-contracts.capture.json'), JSON.stringify(capture, null, 2) + '\n');
console.log(JSON.stringify({ status: 'passed', assertions, summaries, raw_bytes: Buffer.byteLength(serializedRaw), corpus_bytes: Buffer.byteLength(corpus), raw_sha256: hash(serializedRaw) }));
