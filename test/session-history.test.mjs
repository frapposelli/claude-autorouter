import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { readSessionHistory, sessionsCommand } from '../src/session-history.mjs';
import { saveUserConfig } from '../src/user-config.mjs';
import { PRICING_VERSION, PRICING_DATE, PRICING_SOURCE } from '../src/savings.mjs';
import { execFileSync } from 'node:child_process';

const stamp = '2026-10-05T12:00:00.000Z';
const id = 'autorouter-session-20261005T120000Z-run-a';
const decision = (request_id, extra = {}) => ({ schema_version: 2, event: 'decision', timestamp: stamp,
  request_id, session_id: 'session-a', requested_model: 'claude-haiku-4-5-20251001', selected_model: 'claude-haiku-4-5-20251001',
  source: 'jev', reason: 'classified', decision_latency_ms: 12, prompt_excerpt: 'Synthetic task', ...extra });
const outcome = (request_id, extra = {}) => ({ schema_version: 2, event: 'outcome', timestamp: stamp,
  request_id, session_id: 'session-a', status: 'completed', http_status: 200, completion_confirmed: true,
  confirmed_model: 'claude-haiku-4-5-20251001', baseline_model: 'claude-opus-5-5', pricing_version: PRICING_VERSION,
  usage_complete: true, usage: { input_tokens: 1000, output_tokens: 100 }, total_latency_ms: 600, ...extra });
async function fixture(t) {
  const root = await fs.mkdtemp(join(tmpdir(), 'autorouter-history-'));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const directory = join(root, 'logs');
  await fs.mkdir(directory);
  return { root, directory, env: { AUTOROUTER_CONFIG: join(root, 'config.json') },
    async write(rows, name = id) { await fs.writeFile(join(directory, `${name}.jsonl`), rows.map(row => JSON.stringify(row)).join('\n') + '\n'); } };
}

test('history correlates selections and confirmed outcomes and prices only covered usage', async t => {
  const f = await fixture(t);
  await f.write([decision('success'), outcome('success'), decision('failed'), outcome('failed', { status: 'error', http_status: 429, completion_confirmed: false }),
    decision('pending', { source: 'fallback', classifier_error: 'timeout', reason: 'classifier_error', selected_model: 'claude-sonnet-5-5', decision_latency_ms: 1500 }),
    decision('legacy', { schema_version: 1 }), outcome('invalid', { status: 'error', http_status: 400, completion_confirmed: false, confirmed_model: undefined }),
    decision('unfinished'), outcome('unfinished', { completion_confirmed: false }), outcome('cancelled', { status: 'cancelled', completion_confirmed: false })]);
  const { summary, records } = await readSessionHistory(f.directory, { id });
  assert.equal(summary.id, id);
  assert.equal(summary.session_id, 'session-a');
  assert.deepEqual([summary.requests, summary.decisions, summary.outcomes, summary.completed, summary.failed, summary.cancelled, summary.pending, summary.unconfirmed, summary.outcome_only], [7, 5, 5, 1, 2, 1, 2, 1, 2]);
  assert.equal(summary.fallbacks, 1);
  assert.equal(summary.fallback_rate, .2);
  assert.deepEqual(summary.classifier_errors, { timeout: 1 });
  assert.deepEqual(summary.decision_latency_ms, { samples: 5, p50: 12, p95: 1500, max: 1500 });
  assert.equal(summary.savings.priced_requests, 1);
  assert.equal(summary.savings.unpriced_requests, 6);
  assert.equal(summary.savings.saved_usd, .0045);
  assert.equal(summary.savings.unpriced_reasons.missing_outcome, 2);
  assert.deepEqual(summary.pricing_versions, [PRICING_VERSION]);
  assert.equal(summary.mixed_pricing_versions, false);
  assert.equal(summary.unversioned_outcomes, 0);
  assert.equal(summary.coverage.legacy_decisions, 1);
  assert.equal(summary.coverage.partial, false);
  assert.equal(records.find(row => row.request_id === 'legacy').schema_version, 1);
});

test('unknown pricing versions and mixed recorded baselines stay explicit, duplicate rows do not double count', async t => {
  const f = await fixture(t);
  const one = outcome('one');
  await f.write([decision('one'), one, one, outcome('two', { baseline_model: 'claude-opus-5' }),
    outcome('future', { pricing_version: 'future.9' }), outcome('missing-baseline', { baseline_model: undefined })]);
  const { summary } = await readSessionHistory(f.directory, { id });
  assert.equal(summary.requests, 4);
  assert.equal(summary.savings.priced_requests, 2);
  assert.equal(summary.savings.unpriced_reasons.unknown_pricing_version, 1);
  assert.equal(summary.savings.unpriced_reasons.unknown_baseline, 1);
  assert.deepEqual(summary.baseline_models, ['claude-opus-5', 'claude-opus-5-5']);
  assert.equal(summary.mixed_baselines, true);
  assert.deepEqual(summary.pricing_versions, [PRICING_VERSION, 'future.9']);
  assert.equal(summary.mixed_pricing_versions, true);
  assert.equal(summary.coverage.duplicate_records, 1);
  assert.equal(summary.coverage.partial, true);
});

test('list and show report pricing versions and missing provenance in human and JSON summaries', async t => {
  const f = await fixture(t);
  saveUserConfig({ AUTOROUTER_SESSION_LOG_DIR: f.directory }, { env: f.env });
  await f.write([decision('current'), outcome('current'), outcome('future', { pricing_version: 'future.9' }),
    outcome('unversioned', { pricing_version: undefined })]);
  for (const operation of [['list'], ['show', id]]) {
    const text = [];
    await sessionsCommand(operation, { env: f.env, write: line => text.push(line) });
    assert.ok(text.some(line => line.includes(`Recorded pricing versions (mixed): ${PRICING_VERSION}, future.9`)));
    assert.match(text.join('\n'), /1 outcomes without a recorded version/);
    assert.match(text.join('\n'), /unknown_pricing_version \(2\)/);
    assert.ok(text.some(line => line.includes(`reviewed ${PRICING_DATE}: ${PRICING_SOURCE}`)));
    const json = [];
    await sessionsCommand([...operation, '--json'], { env: f.env, write: line => json.push(line) });
    const report = JSON.parse(json[0]);
    const summary = report.summary ?? report.sessions[0];
    assert.deepEqual(summary.pricing_versions, [PRICING_VERSION, 'future.9']);
    assert.equal(summary.mixed_pricing_versions, true);
    assert.equal(summary.unversioned_outcomes, 1);
    assert.deepEqual(summary.pricing_facts, [{ version: PRICING_VERSION, date: PRICING_DATE, source: PRICING_SOURCE }]);
    assert.equal(summary.savings.priced_requests, 1);
  }
});

test('fallback rates use observed selections including legacy/pending records, not HTTP failures or outcomes', async t => {
  const f = await fixture(t);
  saveUserConfig({ AUTOROUTER_SESSION_LOG_DIR: f.directory }, { env: f.env });
  await f.write([decision('legacy-pending', { schema_version: 1, source: 'fallback' }),
    decision('fallback-completed', { source: 'fallback' }), outcome('fallback-completed'),
    decision('evaluator-success'), outcome('evaluator-success', { status: 'error', http_status: 429, completion_confirmed: false }),
    outcome('unselected-http-error', { status: 'error', http_status: 500, completion_confirmed: false })]);
  let report = await readSessionHistory(f.directory, { id });
  assert.equal(report.summary.fallbacks, 2);
  assert.equal(report.summary.decisions, 3);
  assert.equal(report.summary.failed, 2);
  assert.equal(report.summary.fallback_rate, 2 / 3);
  const text = [];
  await sessionsCommand(['show', id], { env: f.env, write: line => text.push(line) });
  assert.match(text.join('\n'), /Fallbacks: 2 of 3 selections \(66\.7%\)/);
  await f.write([outcome('only-outcome', { pricing_version: 'future.9', status: 'error', completion_confirmed: false })]);
  report = await readSessionHistory(f.directory, { id });
  assert.equal(report.summary.fallback_rate, 0);
  assert.equal(report.summary.fallbacks, 0);
  assert.deepEqual(report.summary.pricing_facts, []);
});

test('legacy records and metadata-only sessions never invent successful responses or reveal private fields', async t => {
  const f = await fixture(t);
  await f.write([decision('legacy', { schema_version: 1, body: { private: 'PRIVATE_BODY' }, headers: { authorization: 'PRIVATE_AUTH' } }),
    decision('metadata', { prompt_excerpt: undefined })]);
  const result = await readSessionHistory(f.directory, { id });
  assert.equal(result.summary.completed, 0);
  assert.equal(result.summary.pending, 2);
  assert.deepEqual(result.summary.confirmed_models, {});
  assert.equal(result.summary.savings.priced_requests, 0);
  assert.ok(!JSON.stringify(result).includes('PRIVATE_'));
});

test('human show sanitizes terminal controls and JSON retains only bounded prompt excerpts', async t => {
  const f = await fixture(t), lines = [];
  saveUserConfig({ AUTOROUTER_SESSION_LOG_DIR: f.directory, AUTOROUTER_JEV_URL: 'invalid-inactive-settings', AUTOROUTER_EVALUATOR: 'ollama' }, { env: f.env });
  await f.write([decision('prompt', { prompt_excerpt: '\u001b[31m\u202eSECRET\n😀'.repeat(100), prompt_truncated: true })]);
  assert.equal(await sessionsCommand(['show', id], { env: f.env, write: line => lines.push(line) }), true);
  assert.ok(lines.some(line => line === `ID: ${id}`));
  assert.ok(!lines.join('\n').includes('\u001b'));
  assert.ok(!lines.join('\n').includes('\u202e'));
  assert.match(lines.join('\n'), /0 confirmed completed/);
  lines.length = 0;
  await sessionsCommand(['show', id, '--json'], { env: f.env, write: line => lines.push(line) });
  assert.equal(lines.length, 1);
  const report = JSON.parse(lines[0]);
  assert.equal([...report.records[0].prompt_excerpt].length, 500);
  assert.ok(report.records[0].prompt_excerpt.includes('\u001b'));
});

test('bounded reads report partial coverage for bytes, records, lines, and incomplete tails', async t => {
  const f = await fixture(t);
  await f.write([decision('one'), decision('two'), outcome('one')]);
  let result = await readSessionHistory(f.directory, { id, limits: { maxRecords: 1 } });
  assert.equal(result.records.length, 1);
  assert.equal(result.summary.coverage.truncated, true);
  result = await readSessionHistory(f.directory, { id, limits: { maxLines: 1 } });
  assert.equal(result.summary.coverage.partial, true);
  result = await readSessionHistory(f.directory, { id, limits: { maxFileBytes: 30 } });
  assert.equal(result.summary.coverage.bytes_read, 30);
  assert.equal(result.summary.coverage.truncated, true);
  assert.equal(result.records.length, 0);
  await fs.appendFile(join(f.directory, `${id}.jsonl`), '{"unfinished":');
  result = await readSessionHistory(f.directory, { id });
  assert.equal(result.records.length, 3);
  assert.equal(result.summary.coverage.incomplete_tail, true);
  assert.equal(result.summary.coverage.partial, true);
});

test('oversized, malformed, foreign-session, and unknown-version records are skipped with coverage counts', async t => {
  const f = await fixture(t);
  await f.write([decision('good'), decision('foreign', { session_id: 'session-other' }), decision('future', { schema_version: 99 })]);
  await fs.appendFile(join(f.directory, `${id}.jsonl`), `${'x'.repeat(17000)}\nPRIVATE_MALFORMED_JSON\n`);
  const result = await readSessionHistory(f.directory, { id });
  assert.equal(result.records.length, 1);
  assert.equal(result.summary.coverage.mixed_session_records, 1);
  assert.equal(result.summary.coverage.invalid_records, 2);
  assert.equal(result.summary.coverage.oversized_lines, 1);
  assert.equal(result.summary.coverage.partial, true);
  assert.ok(!JSON.stringify(result).includes('PRIVATE_'));
});

test('list chooses bounded newest IDs deterministically and limits total bytes without deleting files', async t => {
  const f = await fixture(t);
  for (const suffix of ['a', 'b', 'c']) await f.write([decision(suffix)], `autorouter-session-${suffix}`);
  let result = await readSessionHistory(f.directory, { limits: { maxFiles: 2 } });
  assert.deepEqual(result.sessions.map(session => session.id), ['autorouter-session-c', 'autorouter-session-b']);
  assert.equal(result.coverage.partial, true);
  assert.equal(result.coverage.skipped_files, 1);
  result = await readSessionHistory(f.directory, { limits: { maxTotalBytes: 100 } });
  assert.equal(result.coverage.bytes_read, 100);
  assert.equal(result.coverage.byte_limit_reached, true);
  assert.equal(result.coverage.partial, true);
  result = await readSessionHistory(f.directory, { limits: { maxDirectoryEntries: 1 } });
  assert.equal(result.coverage.directory_scan_truncated, true);
  assert.deepEqual((await fs.readdir(f.directory)).sort(), ['autorouter-session-a.jsonl', 'autorouter-session-b.jsonl', 'autorouter-session-c.jsonl']);
});

test('history rejects traversal, directory or file symlinks, and hardlinks without reading their targets', async t => {
  const f = await fixture(t);
  const target = join(f.root, 'PRIVATE_TARGET.jsonl');
  await fs.writeFile(target, JSON.stringify(decision('private', { prompt_excerpt: 'PRIVATE_TARGET_CONTENT' })) + '\n');
  await fs.symlink(target, join(f.directory, `${id}.jsonl`));
  await assert.rejects(readSessionHistory(f.directory, { id }), error => !error.message.includes('PRIVATE'));
  let result = await readSessionHistory(f.directory);
  assert.equal(result.sessions.length, 0);
  assert.equal(result.coverage.skipped_files, 1);
  const link = join(f.root, 'directory-link');
  await fs.symlink(f.directory, link);
  await assert.rejects(readSessionHistory(link), /symbolic link/);
  for (const attempted of ['../PRIVATE_TARGET', '/PRIVATE_TARGET', `${id}.jsonl`, 'x\nPRIVATE_TARGET']) {
    await assert.rejects(readSessionHistory(f.directory, { id: attempted }), error => !error.message.includes('PRIVATE'));
  }
  await fs.unlink(join(f.directory, `${id}.jsonl`));
  await fs.link(target, join(f.directory, `${id}.jsonl`));
  result = await readSessionHistory(f.directory);
  assert.equal(result.sessions.length, 0);
  assert.equal(result.coverage.unreadable_files, 1);
  assert.match(await fs.readFile(target, 'utf8'), /PRIVATE_TARGET_CONTENT/);
});

test('logging-disabled and empty-directory inspection creates no files and needs no credentials', async t => {
  const f = await fixture(t), lines = [];
  assert.equal(await sessionsCommand(['list', '--json'], { env: f.env, write: line => lines.push(line) }), true);
  assert.equal(JSON.parse(lines.pop()).logging_enabled, false);
  assert.equal(await sessionsCommand(['show', id], { env: f.env, write: line => lines.push(line) }), false);
  assert.deepEqual(await fs.readdir(f.directory), []);
  saveUserConfig({ AUTOROUTER_SESSION_LOG_DIR: join(f.root, 'not-created') }, { env: f.env });
  lines.length = 0;
  assert.equal(await sessionsCommand(['list', '--json'], { env: f.env, write: line => lines.push(line) }), true);
  assert.equal(JSON.parse(lines[0]).coverage.directory_missing, true);
  await assert.rejects(fs.stat(join(f.root, 'not-created')), { code: 'ENOENT' });
});

test('conflicting correlated outcomes remain unpriced and cannot imply confirmed completion', async t => {
  const f = await fixture(t);
  await f.write([decision('ambiguous'), outcome('ambiguous'), outcome('ambiguous', { confirmed_model: 'claude-opus-5-5' })]);
  const { summary } = await readSessionHistory(f.directory, { id });
  assert.equal(summary.completed, 0);
  assert.equal(summary.unconfirmed, 1);
  assert.equal(summary.savings.priced_requests, 0);
  assert.equal(summary.savings.unpriced_reasons.invalid_telemetry, 1);
  assert.equal(summary.coverage.conflicting_requests, 1);
  assert.equal(summary.coverage.partial, true);
});

test('metadata-only records stay without prompt fields when displayed as JSON', async t => {
  const f = await fixture(t);
  await f.write([decision('metadata', { prompt_excerpt: undefined })]);
  const { records } = await readSessionHistory(f.directory, { id });
  assert.ok(!Object.hasOwn(records[0], 'prompt_excerpt'));
  assert.ok(!Object.hasOwn(records[0], 'prompt_truncated'));
});

test('show rejects named pipes without blocking on a writer', { skip: process.platform === 'win32' }, async t => {
  const f = await fixture(t);
  execFileSync('mkfifo', [join(f.directory, `${id}.jsonl`)]);
  const script = `import { readSessionHistory } from ${JSON.stringify(new URL('../src/session-history.mjs', import.meta.url).href)};
    try { await readSessionHistory(${JSON.stringify(f.directory)}, { id: ${JSON.stringify(id)} }); process.exitCode = 2; }
    catch { process.stdout.write('rejected'); }`;
  assert.equal(execFileSync(process.execPath, ['--input-type=module', '-e', script], { encoding: 'utf8', timeout: 2000 }), 'rejected');
});
