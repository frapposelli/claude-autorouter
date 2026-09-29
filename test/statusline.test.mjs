import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { renderStatusLine } from '../src/statusline.mjs';

const now = 1750000000000;
const snapshot = state => ({ version: 1, pid: process.pid, heartbeat_at: now, sessions: { 'session-a': state } });
const render = (state, options = {}) => renderStatusLine({ session_id: 'session-a' }, snapshot(state), { now, color: false, columns: 160, ...options });

test('selection and confirmation are distinct and requested client models never become actual models', () => {
  const state = { phase: 'connecting', requested_model: 'claude-haiku-4-5-20251001', selected_model: 'claude-sonnet-5', source: 'jev', latency_ms: 243.2 };
  assert.match(render(state), /Sonnet 5 selected · connecting · Jev 243ms/);
  assert.ok(!render(state).includes('Haiku'));
  assert.match(render({ ...state, phase: 'streaming' }), /Sonnet 5 unconfirmed · streaming/);
  assert.match(render({ ...state, phase: 'streaming', actual_model: 'claude-opus-5-5' }), /Opus 5\.5 · streaming/);
  assert.ok(!render({ ...state, phase: 'streaming', actual_model: 'claude-opus-5-5' }).includes('Sonnet'));
  assert.match(render({ ...state, phase: 'ready' }), /Sonnet 5 unconfirmed · ready/);
  assert.match(render({ ...state, phase: 'ready', last_model: 'claude-opus-5-5' }), /Sonnet 5 unconfirmed · ready/);
  assert.ok(!render({ ...state, phase: 'ready', last_model: 'claude-opus-5-5' }).includes('Opus'));
  assert.match(render({ ...state, phase: 'ready', actual_model: 'claude-opus-5-5', last_model: 'claude-haiku-4-5-20251001' }), /last Opus 5\.5 · ready/);
  assert.ok(!render({ phase: 'streaming', requested_model: 'claude-haiku-4-5-20251001' }).includes('Haiku'));
});

test('routing and idle states label confirmed history; capability guards do not display classifier tiers as models', () => {
  assert.match(render({ phase: 'ready' }), /awaiting request/);
  assert.match(render({ phase: 'routing', last_model: 'claude-haiku-4-5-20251001' }), /last Haiku 4\.5 · routing/);
  const guarded = render({ phase: 'connecting', tier: 'haiku', selected_model: 'claude-opus-5-5', source: 'cache', reason: 'thinking_history', latency_ms: 0.2 });
  assert.match(guarded, /Opus 5\.5 selected/);
  assert.match(guarded, /cache 0ms · thinking pinned/);
  assert.ok(!guarded.includes('Haiku'));
  assert.match(render({ phase: 'ready', last_model: 'claude-sonnet-5', reason: 'tool_turn_pinned' }), /turn pinned/);
  assert.match(render({ phase: 'connecting', selected_model: 'claude-sonnet-5', reason: 'context_capacity' }), /Sonnet 5 selected · connecting · large context/);
  assert.match(render({ phase: 'connecting', selected_model: 'claude-sonnet-5', source: 'jev', classified_tier: 'haiku',
    reason: 'context_capacity', context_check: 'count_unavailable' }), /Sonnet 5 selected · connecting · Jev→Haiku · size unverified/);
  assert.match(render({ phase: 'connecting', selected_model: 'claude-sonnet-5', source: 'jev', classified_tier: 'opus',
    reason: 'model_specific_features' }), /Jev→Opus · model features/);
});

test('Ollama routes, cache hits and fallbacks identify their evaluator while keeping Claude model confirmation separate', () => {
  const state = { phase: 'connecting', selected_model: 'claude-sonnet-5', source: 'ollama', evaluator: 'ollama', latency_ms: 72.4 };
  assert.match(render(state), /Sonnet 5 selected · connecting · Ollama 72ms/);
  assert.ok(!render(state).includes('Jev'));
  const guarded = render({ ...state, classified_tier: 'haiku', reason: 'context_capacity', context_check: 'over_budget' });
  assert.match(guarded, /Sonnet 5 selected · connecting · Ollama→Haiku 72ms · large context/);
  const cached = render({ ...state, source: 'cache', latency_ms: 0.2 });
  assert.match(cached, /Ollama cache 0ms/);
  assert.ok(!cached.includes('Jev'));
  const fallback = render({ ...state, source: 'fallback', latency_ms: 1500, classifier_error: 'timeout' });
  assert.match(fallback, /Ollama fallback 1500ms · timeout/);
  assert.ok(!fallback.includes('Jev'));
  assert.match(render({ ...state, source: 'fallback', classifier_error: 'network_error' }, { color: true }), /\x1b\[33m/);
  assert.match(render({ ...state, phase: 'ready', actual_model: 'claude-opus-5-5' }), /last Opus 5\.5 · ready · Ollama/);
  assert.match(render({ ...state, source: 'cache', evaluator: 'jev' }), /Jev cache 72ms/);
  assert.match(render({ ...state, source: 'fallback', evaluator: 'jev' }), /Jev fallback 72ms/);
  assert.match(render({ ...state, evaluator: undefined }), /Ollama 72ms/);
});

test('unknown evaluator metadata cannot leak into the terminal or change Claude model labels', () => {
  for (const evaluator of ['PRIVATE evaluator', '\x1b[2JPRIVATE', '__proto__']) {
    const line = render({ phase: 'connecting', selected_model: 'claude-sonnet-5', source: 'cache', evaluator,
      evaluator_model: 'PRIVATE model', evaluator_url: 'http://PRIVATE' });
    assert.match(line, /Sonnet 5 selected · connecting · cache/);
    assert.ok(!/PRIVATE|Jev|Ollama|\x1b/.test(line));
  }
});

test('sessions are isolated, including absent session IDs, and malformed stdin awaits status', () => {
  const saved = snapshot({ phase: 'ready', last_model: 'claude-opus-5-5' });
  saved.sessions[''] = { phase: 'ready', last_model: 'claude-haiku-4-5-20251001' };
  const options = { now, color: false };
  assert.match(renderStatusLine({ session_id: 'unrelated' }, saved, options), /awaiting request/);
  assert.ok(!renderStatusLine({ session_id: 'unrelated' }, saved, options).includes('Haiku'));
  assert.match(renderStatusLine({}, saved, options), /last Haiku 4\.5/);
  for (const input of [undefined, null, [], 'invalid', { session_id: 42 }]) {
    assert.match(renderStatusLine(input, saved, options), /awaiting request/);
  }
  assert.match(renderStatusLine({ session_id: '__proto__' }, saved, options), /awaiting request/);
});

test('fallbacks, HTTP and SSE errors, cancellation, and dead or stale routers remain visible', () => {
  assert.match(render({ phase: 'streaming', selected_model: 'claude-sonnet-5', source: 'fallback', classifier_error: 'timeout', latency_ms: 1501 }), /fallback 1501ms · timeout/);
  const failed = render({ phase: 'error', last_model: 'claude-sonnet-5', selected_model: 'claude-opus-5-5', status: 429, error_type: 'rate_limit_error' });
  assert.match(failed, /last Sonnet 5 · error 429 · rate_limit_error/);
  assert.ok(!failed.includes('Opus'));
  assert.match(render({ phase: 'error', status: 200, error_type: 'overloaded_error' }), /error overloaded_error/);
  assert.match(render({ phase: 'cancelled', last_model: 'claude-haiku-4-5-20251001' }), /last Haiku 4\.5 · cancelled/);
  const current = snapshot({ phase: 'ready', last_model: 'claude-sonnet-5' });
  for (const saved of [undefined, {}, { ...current, version: 2 }, { ...current, pid: -1 }, { ...current, heartbeat_at: now - 20001 }]) {
    assert.match(renderStatusLine({}, saved, { now, color: false }), /AutoRouter offline/);
  }
  assert.match(renderStatusLine({}, current, { now, color: false, alive: () => false }), /offline/);
  assert.match(renderStatusLine({}, current, { now, color: false, alive: () => { throw new Error('gone'); } }), /offline/);
  assert.match(render({ phase: 'streaming', source: 'fallback' }, { color: true }), /\x1b\[33m/);
  assert.match(render({ phase: 'error' }, { color: true }), /\x1b\[31m/);
});

test('narrow lines drop context then details while retaining model and state', () => {
  const saved = snapshot({ phase: 'streaming', actual_model: 'claude-sonnet-5', source: 'jev', latency_ms: 243, reason: 'tool_turn_pinned' });
  const input = { session_id: 'session-a', context_window: { used_percentage: 24.4 } };
  assert.match(renderStatusLine(input, saved, { now, color: false, columns: 160 }), /ctx 24%/);
  const narrow = renderStatusLine(input, saved, { now, color: false, columns: 48 });
  assert.ok(narrow.length <= 48);
  assert.match(narrow, /Sonnet 5 · streaming/);
  assert.ok(!narrow.includes('ctx'));
  assert.ok(!narrow.includes('turn pinned'));
  const selected = render({ phase: 'streaming', selected_model: 'claude-sonnet-5' }, { columns: 32 });
  assert.ok(selected.length <= 32);
  assert.match(selected, /unconfirmed/);
  assert.match(selected, /stream/);
  for (const columns of [1, 3, 8, 16, 20, 22, 23]) {
    for (const phase of ['connecting', 'streaming', 'ready']) {
      const tiny = render({ phase, selected_model: 'claude-sonnet-5' }, { columns });
      assert.ok([...tiny].length <= columns, `${columns}: ${tiny}`);
      if (tiny.includes('Sonnet')) assert.match(tiny, /selected|unconfirmed/);
      // A model abbreviation must retain the complete qualifier as well.
      if (tiny.includes('… ') && !tiny.startsWith('●')) assert.match(tiny, /selected|unconfirmed/);
    }
    assert.ok([...renderStatusLine({}, undefined, { now, color: false, columns })].length <= columns);
    assert.ok([...render({ phase: 'ready' }, { columns })].length <= columns);
  }
});

test('API context uses the confirmed model window and exposes a different CLI limit', () => {
  const state = { phase: 'ready', actual_model: 'claude-sonnet-5', context_usage: { model: 'claude-sonnet-5', input_tokens: 227338 } };
  const input = { session_id: 'session-a', context_window: { context_window_size: 200000, used_percentage: 100 } };
  const line = renderStatusLine(input, snapshot(state), { now, color: false, columns: 180 });
  assert.match(line, /API ctx 23%\/1M · CLI ctx 100%\/200K/);
  const same = renderStatusLine({ ...input, context_window: { context_window_size: 1000000, used_percentage: 23 } }, snapshot(state),
    { now, color: false, columns: 180 });
  assert.match(same, /API ctx 23%\/1M/);
  assert.ok(!same.includes('CLI ctx'));
  const haiku = { phase: 'ready', actual_model: 'claude-haiku-4-5-20251001', context_usage: { model: 'claude-haiku-4-5-20251001', input_tokens: 10000 } };
  assert.match(render(haiku), /API ctx 5%\/200K/);
  assert.match(render({ ...state, context_usage: { model: 'claude-sonnet-5', input_tokens: 1001000 } }), /API ctx 100%\/1M/);
});

test('context labels historical usage and clears it after compaction or a new unmeasured completion', () => {
  const historical = { model: 'claude-sonnet-5', input_tokens: 227338 };
  assert.match(render({ phase: 'connecting', selected_model: 'claude-opus-5-5', last_context_usage: historical }), /last API ctx 23%\/1M/);
  assert.ok(!render({ phase: 'ready', actual_model: 'claude-opus-5-5', last_context_usage: historical }).includes('API ctx'));
  const state = { phase: 'ready', actual_model: 'claude-sonnet-5', context_usage: historical };
  const reset = renderStatusLine({ session_id: 'session-a', context_window: { current_usage: null, used_percentage: null, context_window_size: 200000 } },
    snapshot(state), { now, color: false, columns: 180 });
  assert.ok(!reset.includes('ctx'));
  const fallback = renderStatusLine({ session_id: 'session-a', context_window: { used_percentage: 100 } }, snapshot({ phase: 'ready' }),
    { now, color: false, columns: 180 });
  assert.match(fallback, /CLI ctx 100%/);
});

test('unknown capacities and malformed or mismatched usage never imply API headroom', () => {
  for (const model of ['custom-sonnet-5', 'claude-sonnet-5-future', 'claude-opus-4-6', '__proto__']) {
    assert.ok(!render({ phase: 'ready', actual_model: model, context_usage: { model, input_tokens: 1000 } }).includes('API ctx'));
  }
  for (const usage of [{ model: 'claude-opus-5-5', input_tokens: 1000 }, { model: 'claude-sonnet-5', input_tokens: -1 },
    { model: 'claude-sonnet-5', input_tokens: Infinity }, { model: 'claude-sonnet-5', input_tokens: 'PRIVATE' }]) {
    const line = render({ phase: 'ready', actual_model: 'claude-sonnet-5', context_usage: usage });
    assert.ok(!/API ctx|PRIVATE|Infinity/.test(line));
  }
  const line = renderStatusLine({ session_id: 'unrelated', context_window: { used_percentage: 10, context_window_size: 200000 } },
    snapshot({ phase: 'ready', actual_model: 'claude-sonnet-5', context_usage: { model: 'claude-sonnet-5', input_tokens: 227338 } }),
    { now, color: false, columns: 180 });
  assert.match(line, /CLI ctx 10%\/200K/);
  assert.ok(!line.includes('API ctx'));
});

test('untrusted model and error strings cannot inject terminal controls or extra lines', () => {
  const malicious = '\x1b]8;;https://example.invalid\x07\x1b[31mcustom\nmodel\r\t\u202e' + 'x'.repeat(200);
  const line = render({ phase: 'streaming', actual_model: malicious });
  assert.ok(!/[\x00-\x1f\x7f\u202e]/.test(line));
  assert.ok(!line.includes('https://'));
  assert.ok(line.length < 100);
  const error = render({ phase: 'error', error_type: '\x1b[2Jbad\nerror' });
  assert.ok(!/[\x00-\x1f\x7f]/.test(error));
  assert.match(error, /baderror/);
  for (const reason of ['__proto__', 'constructor', 'toString']) {
    assert.equal(render({ phase: 'ready', reason }), '● AutoRouter · awaiting request');
  }
});

const savingsFixture = overrides => ({ baseline_model: 'claude-opus-5-5', actual_usd: 0.26, baseline_usd: 0.68,
  saved_usd: 0.42, percent: 100 * 0.42 / 0.68, requests: 3, unpriced_requests: 0, ...overrides });
function withSavings(entry = savingsFixture(), state = {}) {
  return { ...snapshot({ phase: 'ready', actual_model: 'claude-sonnet-5', ...state }), savings: { 'session-a': entry } };
}
const renderSavings = (saved, options = {}, input = { session_id: 'session-a' }) => renderStatusLine(input, saved,
  { now, color: false, columns: 180, ...options });

test('savings use session estimates rather than client cost, including cache-weighted values', () => {
  const line = renderSavings(withSavings(), {}, { session_id: 'session-a', cost: { total_cost_usd: 99999 } });
  assert.match(line, /est saved \$0\.42 \(62%\) vs Opus/);
  assert.ok(!line.includes('99999'));
  // Tracker fixture: 1k uncached input, 200 output, 10k cache read,
  // 2k five-minute cache write, 3k one-hour cache write, served by Haiku.
  const cached = savingsFixture({ actual_usd: 0.0115, baseline_usd: 0.044, saved_usd: 0.0325,
    percent: 100 * 0.0325 / 0.044, requests: 1 });
  assert.match(renderSavings(withSavings(cached)), /est saved \$0\.03 \(74%\) vs Opus/);
});

test('savings show partial coverage and unavailable usage without manufacturing a zero', () => {
  for (const entry of [savingsFixture({ partial: true }), savingsFixture({ unpriced_requests: 2 })]) {
    assert.match(renderSavings(withSavings(entry)), /est saved \$0\.42 \(62%\) vs Opus partial/);
  }
  const missing = renderSavings(withSavings(savingsFixture({ requests: 0, unpriced_requests: 1 })));
  assert.match(missing, /savings unavailable/);
  assert.ok(!missing.includes('saved'));
  assert.ok(!renderSavings(withSavings(savingsFixture({ requests: 0 }))).includes('saving'));
  assert.ok(!renderSavings(snapshot({ phase: 'ready' })).includes('saving'));
});

test('savings support extra cost, tiny amounts, and a zero baseline without infinity', () => {
  const extra = savingsFixture({ actual_usd: 1.1, baseline_usd: 0.68, saved_usd: -0.42, percent: -100 * 0.42 / 0.68 });
  assert.match(renderSavings(withSavings(extra)), /est extra \$0\.42 \(62%\) vs Opus/);
  assert.match(renderSavings(withSavings(savingsFixture({ saved_usd: 0.001, percent: 0.1 }))), /est saved <\$0\.01/);
  assert.match(renderSavings(withSavings(savingsFixture({ saved_usd: -0.001, percent: -0.1 }))), /est extra <\$0\.01/);
  const zero = renderSavings(withSavings(savingsFixture({ actual_usd: 0, baseline_usd: 0, saved_usd: 0, percent: null })));
  assert.match(zero, /est saved \$0\.00 vs Opus/);
  assert.ok(!zero.includes('%'));
  assert.ok(!zero.includes('Infinity'));
  const noSaving = renderSavings(withSavings(savingsFixture({ actual_usd: 0.68, saved_usd: 0, percent: 0 })));
  assert.match(noSaving, /est saved \$0\.00 \(0%\)/);
});

test('savings are isolated by exact session, including anonymous and prototype names', () => {
  const saved = withSavings();
  saved.savings[''] = savingsFixture({ saved_usd: 0.1, percent: 10 });
  assert.match(renderSavings(saved), /\$0\.42/);
  assert.match(renderSavings(saved, {}, {}), /\$0\.10/);
  for (const input of [{ session_id: 'session-b' }, { session_id: '__proto__' }, { session_id: 'constructor' },
    { session_id: 42 }, undefined, null, [], 'invalid']) {
    const result = renderStatusLine(input, saved, { now, color: false, columns: 180 });
    assert.ok(!result.includes('saving'), JSON.stringify(input));
    assert.ok(!result.includes('$'), JSON.stringify(input));
  }
});

test('malformed savings cannot inject controls, dollar claims, NaN, or infinity', () => {
  for (const entry of [
    savingsFixture({ actual_usd: -1 }), savingsFixture({ baseline_usd: Infinity }), savingsFixture({ saved_usd: NaN }),
    savingsFixture({ actual_usd: '\x1b[31mPRIVATE' }), savingsFixture({ requests: 1.5 }), savingsFixture({ unpriced_requests: -1 }),
  ]) {
    const line = renderSavings(withSavings(entry));
    assert.match(line, /savings unavailable/);
    assert.ok(!/PRIVATE|NaN|Infinity|\$|[\x00-\x1f\x7f]/.test(line));
  }
  const labels = renderSavings(withSavings(savingsFixture({ baseline_model: '\x1b[2JPRIVATE\n', percent: '\x1b[31mPRIVATE' })));
  assert.match(labels, /est saved \$0\.42 vs Opus/);
  assert.ok(!/PRIVATE|[\x00-\x1f\x7f]/.test(labels));
});

test('narrow status lines prioritize estimates over routine details and retain every estimate qualifier', () => {
  const saved = withSavings(savingsFixture(), { phase: 'streaming', source: 'jev', latency_ms: 321, reason: 'tool_turn_pinned' });
  const input = { session_id: 'session-a', context_window: { used_percentage: 24 } };
  const full = renderSavings(saved, {}, input);
  assert.match(full, /Jev 321ms.*est saved \$0\.42 \(62%\) vs Opus.*ctx 24%/);
  const noContext = renderSavings(saved, { columns: full.length - 1 }, input);
  assert.ok(!noContext.includes('ctx'));
  assert.match(noContext, /Jev 321ms.*est saved/);
  const noRoutine = renderSavings(saved, { columns: 80 }, input);
  assert.match(noRoutine, /est saved \$0\.42 \(62%\) vs Opus/);
  assert.ok(!noRoutine.includes('Jev'));
  const compact = renderSavings(saved, { columns: 65 }, input);
  assert.match(compact, /Sonnet 5 · streaming.*est saved 62% vs Opus/);
  assert.ok(!compact.includes('$'));
  for (const partial of [false, true]) {
    const current = withSavings(savingsFixture({ partial }), { phase: 'streaming' });
    for (let columns = 1; columns <= 180; columns++) {
      const line = renderSavings(current, { columns });
      assert.ok([...line].length <= columns, `${columns}: ${line}`);
      if (line.includes('saved')) {
        assert.match(line, /est saved .* vs Opus/);
        if (partial) assert.match(line, /partial/);
        assert.match(line, /Sonnet 5 · streaming/);
      }
    }
  }
});

test('fallback and error explanations take priority over savings on narrow lines', () => {
  const fallback = withSavings(savingsFixture(), { phase: 'streaming', source: 'fallback', classifier_error: 'timeout', latency_ms: 1500 });
  const line = renderSavings(fallback, { columns: 80 });
  assert.match(line, /Sonnet 5 · streaming · fallback 1500ms · timeout/);
  assert.ok(!line.includes('saved'));
  const error = withSavings(savingsFixture(), { phase: 'error', status: 429, error_type: 'rate_limit_error' });
  const errorLine = renderSavings(error, { columns: 80 });
  assert.match(errorLine, /error 429 · rate_limit_error/);
  assert.ok(!errorLine.includes('saved'));
});

test('compact fallback lines retain the cause and actual model before routine phase, estimates, and context', () => {
  for (const evaluator of ['jev', 'ollama']) {
    const saved = withSavings(savingsFixture(), { phase: 'ready', source: 'fallback', evaluator,
      classifier_error: 'timeout', latency_ms: 1501, reason: 'tool_turn_pinned',
      context_usage: { model: 'claude-sonnet-5', input_tokens: 227338 } });
    const input = { session_id: 'session-a', context_window: { used_percentage: 100, context_window_size: 200000 } };
    for (const columns of [40, 60, 80]) {
      const line = renderSavings(saved, { columns }, input);
      assert.ok([...line].length <= columns, `${columns}: ${line}`);
      assert.match(line, /last Sonnet 5/);
      assert.match(line, /fallback.*timeout/);
      assert.ok(!/saved|ctx/.test(line));
      assert.ok(!line.includes(evaluator === 'jev' ? 'Ollama' : 'Jev'));
      if (columns >= 60) assert.ok(line.includes(evaluator === 'jev' ? 'Jev' : 'Ollama'));
    }
    assert.match(renderSavings(saved, { columns: 60 }, input), /fallback: timeout/);
    assert.ok(!renderSavings(saved, { columns: 60 }, input).includes('ready'));
  }
});

test('HTTP fallback causes show safe classifier status separately from upstream errors', () => {
  const state = { phase: 'ready', actual_model: 'claude-sonnet-5', source: 'fallback', evaluator: 'ollama',
    classifier_error: 'http_error', classifier_status: 503, latency_ms: 211 };
  for (const columns of [60, 80, 160]) {
    const line = render(state, { columns });
    assert.ok([...line].length <= columns, `${columns}: ${line}`);
    assert.match(line, /last Sonnet 5/);
    assert.match(line, /fallback.*HTTP 503/);
    assert.ok(!line.includes('error 503'));
    const failed = render({ ...state, phase: 'error', status: 429, error_type: 'rate_limit_error' }, { columns });
    assert.ok([...failed].length <= columns, `${columns}: ${failed}`);
    assert.match(failed, /last Sonnet 5/);
    assert.match(failed, /error 429/);
    assert.match(failed, /fallback.*HTTP 503/);
  }
  for (const classifier_status of ['503 PRIVATE', -1, 600, 1.5, Infinity]) {
    const line = render({ ...state, classifier_status });
    assert.match(line, /fallback.*HTTP error/);
    assert.ok(!/PRIVATE|Infinity|HTTP (?:503|600|-1|1\.5)/.test(line));
  }
  assert.ok(!render({ ...state, classifier_error: 'timeout' }).includes('503'));
});

test('fallback causes are allowlisted and cannot masquerade as a successful classification', () => {
  const state = { phase: 'connecting', selected_model: 'claude-sonnet-5', source: 'fallback', evaluator: 'ollama',
    classified_tier: 'haiku', classifier_error: 'timeout', latency_ms: 1500 };
  for (const columns of [60, 80, 160]) {
    const line = render(state, { columns });
    assert.match(line, /Sonnet 5 selected/);
    assert.match(line, /fallback.*timeout/);
    assert.ok(!/Haiku|→/.test(line));
  }
  for (const classifier_error of ['/Users/PRIVATE/config', '\x1b[2JPRIVATE\nerror', '__proto__', 'constructor']) {
    const line = render({ ...state, classifier_error });
    assert.match(line, /Ollama fallback/);
    assert.ok(!/PRIVATE|Users|__proto__|constructor|\x1b|\n|Haiku/.test(line));
  }
  assert.match(render({ ...state, classifier_error: 'network_error' }, { columns: 60 }), /fallback: network error/);
  assert.match(render({ ...state, classifier_error: 'invalid_response' }, { columns: 60 }), /fallback: invalid response/);
});

test('tiny fallback lines fit without dropping selection qualifiers or reverting to a success phase', () => {
  const variants = [
    { phase: 'ready', actual_model: 'claude-sonnet-5' },
    ...['connecting', 'streaming', 'ready'].map(phase => ({ phase, selected_model: 'claude-sonnet-5' })),
    { phase: 'error', last_model: 'claude-sonnet-5', status: 429, error_type: 'rate_limit_error' },
    { phase: 'cancelled', last_model: 'claude-sonnet-5' },
  ];
  for (const variant of variants) {
    for (let columns = 1; columns <= 80; columns++) {
      for (const color of [false, true]) {
        const line = render({ ...variant, source: 'fallback', evaluator: 'ollama', classifier_error: 'timeout', latency_ms: 1500 },
          { columns, color }).replace(/\x1b\[[0-9;]*m/g, '');
        assert.ok([...line].length <= columns, `${columns}: ${line}`);
        if (columns >= 60) assert.match(line, /fallback.*timeout/);
        if (variant.selected_model && /Sonnet|… /.test(line)) assert.match(line, /selected|unconfirmed/);
        if (!['error', 'cancelled'].includes(variant.phase) && columns >= 8) assert.match(line, /fallback/);
        if (columns <= 40) assert.ok(!/ready|stream/.test(line));
      }
    }
  }
});

const cliPath = fileURLToPath(new URL('../bin/statusline.mjs', import.meta.url));
async function cli(input, env = {}) {
  return await new Promise((resolve, reject) => {
    const childEnv = { ...process.env, TERM: 'xterm', ...env };
    if (env.NO_COLOR === undefined) delete childEnv.NO_COLOR;
    const child = spawn(cliPath, [], { env: childEnv, stdio: ['pipe', 'pipe', 'pipe'] });
    let stdout = '', stderr = '';
    child.stdout.on('data', chunk => { stdout += chunk; });
    child.stderr.on('data', chunk => { stderr += chunk; });
    child.on('error', reject);
    child.on('close', code => resolve({ code, stdout, stderr }));
    child.stdin.on('error', reject);
    child.stdin.end(input);
  });
}

test('executable CLI reads only its status file, respects color settings, and keeps unrelated secrets out', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'autorouter-statusline-test-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const path = join(directory, 'status.json');
  await writeFile(path, JSON.stringify({ ...snapshot({ phase: 'ready', last_model: 'claude-sonnet-5' }), heartbeat_at: Date.now(), secret: 'PRIVATE_FILE_VALUE' }));
  const env = { AUTOROUTER_STATUS_FILE: path, TYPESAFE_API_KEY: 'PRIVATE_ENV_VALUE', NO_COLOR: '' };
  const result = await cli(JSON.stringify({ session_id: 'session-a' }), env);
  assert.equal(result.code, 0);
  assert.equal(result.stderr, '');
  assert.match(result.stdout, /last Sonnet 5 · ready/);
  assert.ok(!result.stdout.includes('\x1b'));
  assert.ok(!result.stdout.includes('PRIVATE'));
  const colored = await cli(JSON.stringify({ session_id: 'session-a' }), { AUTOROUTER_STATUS_FILE: path });
  assert.ok(colored.stdout.includes('\x1b['));
  const dumb = await cli('{}', { AUTOROUTER_STATUS_FILE: path, TERM: 'dumb' });
  assert.ok(!dumb.stdout.includes('\x1b'));
  assert.match((await cli('{bad', env)).stdout, /awaiting request/);
  assert.match((await cli('x'.repeat(1024 * 1024 + 1), env)).stdout, /awaiting request/);
  await writeFile(path, 'x'.repeat(1024 * 1024 + 1));
  assert.match((await cli('{}', env)).stdout, /offline/);
  assert.match((await cli('{}', { ...env, AUTOROUTER_STATUS_FILE: join(directory, 'missing') })).stdout, /offline/);
});
