import { constants } from 'node:fs';
import fs from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { loadUserConfig } from './user-config.mjs';
import { parseSessionLogDir } from './config.mjs';
import { normalizeSessionRecord } from './telemetry-event.mjs';
import { estimateOutcomeSavings, PRICING_FACTS } from './savings.mjs';

export const HISTORY_LIMITS = Object.freeze({ maxFiles: 100, maxDirectoryEntries: 10000,
  maxFileBytes: 4 * 1024 * 1024, maxTotalBytes: 16 * 1024 * 1024, maxLineBytes: 16384,
  maxRecords: 5000, maxLines: 10000 });
const ID = /^autorouter-session-[A-Za-z0-9-]{1,160}$/;
const safeText = value => String(value).replace(/[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/g, '');
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const increment = (counts, key) => {
  if (key) Object.defineProperty(counts, key, { value: (Object.hasOwn(counts, key) ? counts[key] : 0) + 1,
    enumerable: true, writable: true, configurable: true });
};
const sortedCounts = counts => Object.fromEntries(Object.entries(counts).sort(([a], [b]) => a.localeCompare(b)));
function limitsFor(overrides) {
  const limits = { ...HISTORY_LIMITS };
  for (const [key, value] of Object.entries(overrides)) {
    if (!Object.hasOwn(limits, key) || !Number.isSafeInteger(value) || value < 1 || value > limits[key]) throw new Error('Invalid session-history read limit.');
    limits[key] = value;
  }
  return limits;
}
function latencyStats(values) {
  values.sort((a, b) => a - b);
  return values.length ? { samples: values.length, p50: values[Math.ceil(values.length * .5) - 1],
    p95: values[Math.ceil(values.length * .95) - 1], max: values.at(-1) } : { samples: 0 };
}
function summarize(id, rows, coverage) {
  const requests = new Map(), selected = {}, confirmed = {}, sources = {}, reasons = {}, classifierErrors = {};
  const decisionLatencies = [], totalLatencies = [], baselines = new Set(), pricingVersions = new Set();
  let decisions = 0, outcomes = 0, legacy = 0, duplicates = 0, unversionedOutcomes = 0;
  for (const row of rows) {
    let request = requests.get(row.request_id);
    if (!request) { request = {}; requests.set(row.request_id, request); }
    if (request[row.event]) {
      duplicates++;
      if (JSON.stringify(request[row.event]) !== JSON.stringify(row)) request.conflicting = true;
      continue;
    }
    request[row.event] = row;
    if (row.event === 'decision') {
      decisions++;
      if (row.schema_version === 1) legacy++;
      increment(selected, row.selected_model); increment(sources, row.source); increment(reasons, row.reason);
      increment(classifierErrors, row.classifier_error);
      if (Number.isFinite(row.decision_latency_ms)) decisionLatencies.push(row.decision_latency_ms);
    } else {
      outcomes++;
      increment(confirmed, row.confirmed_model);
      if (row.baseline_model) baselines.add(row.baseline_model);
      if (row.pricing_version) pricingVersions.add(row.pricing_version); else unversionedOutcomes++;
      if (Number.isFinite(row.total_latency_ms)) totalLatencies.push(row.total_latency_ms);
    }
  }
  let completed = 0, failed = 0, cancelled = 0, pending = 0, unconfirmed = 0, outcomeOnly = 0, conflicting = 0;
  const savings = { basis: 'API-equivalent estimate, not subscription charges', actual_usd: 0, baseline_usd: 0,
    saved_usd: 0, percent: 0, priced_requests: 0, unpriced_requests: 0, unpriced_reasons: {} };
  for (const request of requests.values()) {
    const { decision, outcome } = request;
    if (request.conflicting) conflicting++;
    const estimate = request.conflicting ? { priced: false, unpriced_reason: 'invalid_telemetry' }
      : outcome ? estimateOutcomeSavings(outcome) : { priced: false, unpriced_reason: 'missing_outcome' };
    if (estimate.priced) {
      savings.priced_requests++;
      for (const field of ['actual_usd', 'baseline_usd', 'saved_usd']) savings[field] += estimate[field];
    } else { savings.unpriced_requests++; increment(savings.unpriced_reasons, estimate.unpriced_reason); }
    if (!outcome) { pending++; continue; }
    if (!decision) outcomeOnly++;
    if (outcome.status === 'cancelled') cancelled++;
    else if (outcome.status === 'error' || (outcome.http_status !== undefined && (outcome.http_status < 200 || outcome.http_status >= 300))) failed++;
    else if (outcome.completion_confirmed === true && !request.conflicting) completed++;
    else unconfirmed++;
  }
  savings.percent = savings.baseline_usd === 0 ? 0 : savings.saved_usd / savings.baseline_usd * 100;
  savings.unpriced_reasons = sortedCounts(savings.unpriced_reasons);
  const timestamps = rows.map(row => row.timestamp).sort();
  return { id, ...(rows[0]?.session_id ? { session_id: rows[0].session_id } : {}),
    started_at: timestamps[0] ?? null, updated_at: timestamps.at(-1) ?? null,
    requests: requests.size, decisions, outcomes, completed, failed, cancelled, pending, unconfirmed, outcome_only: outcomeOnly,
    selected_models: sortedCounts(selected), confirmed_models: sortedCounts(confirmed), sources: sortedCounts(sources),
    routing_reasons: sortedCounts(reasons), classifier_errors: sortedCounts(classifierErrors),
    fallbacks: sources.fallback ?? 0, fallback_rate: decisions ? (sources.fallback ?? 0) / decisions : 0,
    decision_latency_ms: latencyStats(decisionLatencies), total_latency_ms: latencyStats(totalLatencies),
    baseline_models: [...baselines].sort(), mixed_baselines: baselines.size > 1,
    pricing_versions: [...pricingVersions].sort(), mixed_pricing_versions: pricingVersions.size > 1,
    pricing_facts: pricingVersions.has(PRICING_FACTS.version)
      ? [{ version: PRICING_FACTS.version, date: PRICING_FACTS.date, source: PRICING_FACTS.source }] : [],
    unversioned_outcomes: unversionedOutcomes, savings,
    coverage: { ...coverage, legacy_decisions: legacy, duplicate_records: duplicates, conflicting_requests: conflicting,
      partial: coverage.partial || duplicates > 0 },
  };
}

async function sameDirectory(root, identity) {
  const current = await fs.lstat(root);
  if (!current.isDirectory() || current.isSymbolicLink() || current.dev !== identity.dev || current.ino !== identity.ino) {
    throw new Error('Session log directory changed or is not a regular directory.');
  }
}

async function readSession(root, identity, id, limits, remainingBytes) {
  let handle;
  try {
    await sameDirectory(root, identity);
    // Nonblocking open also prevents a replaced FIFO/device from hanging the
    // local inspection before we can verify that it is a regular file.
    handle = await fs.open(join(root, `${id}.jsonl`), constants.O_RDONLY | constants.O_NOFOLLOW | (constants.O_NONBLOCK ?? 0));
    const stat = await handle.stat();
    if (!stat.isFile() || stat.nlink !== 1) throw new Error('Session log is not a regular private file.');
    await sameDirectory(root, identity);
    const capacity = Math.min(stat.size, limits.maxFileBytes, remainingBytes);
    const buffer = Buffer.alloc(capacity);
    let bytesRead = 0;
    while (bytesRead < capacity) {
      const result = await handle.read(buffer, bytesRead, capacity - bytesRead, bytesRead);
      if (!result.bytesRead) break;
      bytesRead += result.bytesRead;
    }
    const coverage = { bytes_read: bytesRead, file_bytes: stat.size, lines_read: 0, invalid_records: 0,
      oversized_lines: 0, mixed_session_records: 0, truncated: bytesRead < stat.size, incomplete_tail: false, partial: false };
    const rows = [];
    let offset = 0, sessionIdentity;
    while (offset < bytesRead) {
      if (rows.length >= limits.maxRecords || coverage.lines_read >= limits.maxLines) { coverage.truncated = true; break; }
      const end = buffer.indexOf(10, offset);
      if (end < 0 || end >= bytesRead) { coverage.incomplete_tail = true; break; }
      const length = end - offset;
      coverage.lines_read++;
      if (length > limits.maxLineBytes) { coverage.oversized_lines++; offset = end + 1; continue; }
      try {
        const entry = JSON.parse(buffer.toString('utf8', offset, end));
        if (!object(entry) || ![1, 2].includes(entry.schema_version)
          || (entry.schema_version === 1 && entry.event !== 'decision')
          || typeof entry.timestamp !== 'string' || !/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/.test(entry.timestamp)
          || !Number.isFinite(Date.parse(entry.timestamp))) throw new Error('Invalid record');
        const row = normalizeSessionRecord(entry, { includePrompts: Object.hasOwn(entry, 'prompt_excerpt') });
        if (!row) throw new Error('Invalid record');
        const rowSession = row.session_id ?? '';
        if (sessionIdentity === undefined) sessionIdentity = rowSession;
        if (rowSession !== sessionIdentity) coverage.mixed_session_records++;
        else rows.push({ ...row, schema_version: entry.schema_version });
      } catch { coverage.invalid_records++; }
      offset = end + 1;
    }
    coverage.partial = coverage.truncated || coverage.incomplete_tail || coverage.invalid_records > 0
      || coverage.oversized_lines > 0 || coverage.mixed_session_records > 0;
    return { summary: summarize(id, rows, coverage), records: rows };
  } finally { if (handle) await handle.close(); }
}

/** Reads only bounded, recognized JSONL files; never modifies or deletes logs. */
export async function readSessionHistory(directory, { id, limits: overrides = {} } = {}) {
  const limits = limitsFor(overrides);
  if (id !== undefined && (typeof id !== 'string' || !ID.test(id))) throw new Error('Use an exact session ID from sessions list.');
  if (typeof directory !== 'string' || !directory.trim() || typeof constants.O_NOFOLLOW !== 'number') throw new Error('A regular session log directory is required.');
  const root = resolve(directory);
  let identity;
  try { identity = await fs.lstat(root); }
  catch (error) {
    if (error.code === 'ENOENT' && id === undefined) return { schema_version: 1, type: 'session_history', sessions: [], limits, coverage: { partial: false, directory_missing: true } };
    throw new Error('Could not read the session log directory.');
  }
  if (!identity.isDirectory() || identity.isSymbolicLink()) throw new Error('Session logs must be read from a regular directory, not a symbolic link.');
  if (id !== undefined) {
    try {
      const result = await readSession(root, identity, id, limits, limits.maxTotalBytes);
      return { schema_version: 1, type: 'session_history', ...result, limits };
    } catch { throw new Error('Could not read that session as a regular log file. Use sessions list for available IDs.'); }
  }
  const ids = [], coverage = { partial: false, directory_entries: 0, matching_files: 0, skipped_files: 0,
    unreadable_files: 0, bytes_read: 0, directory_scan_truncated: false, byte_limit_reached: false };
  let directoryHandle;
  try {
    directoryHandle = await fs.opendir(root);
    for await (const entry of directoryHandle) {
      if (++coverage.directory_entries > limits.maxDirectoryEntries) { coverage.directory_scan_truncated = true; break; }
      if (!entry.name.endsWith('.jsonl')) continue;
      const candidate = entry.name.slice(0, -6);
      if (!ID.test(candidate)) continue;
      if (!entry.isFile()) { coverage.skipped_files++; continue; }
      coverage.matching_files++;
      ids.push(candidate);
      ids.sort().reverse();
      if (ids.length > limits.maxFiles) ids.pop();
    }
    await sameDirectory(root, identity);
  } catch { throw new Error('Could not safely list the session log directory.'); }
  const sessions = [];
  for (const candidate of ids) {
    if (coverage.bytes_read >= limits.maxTotalBytes) { coverage.byte_limit_reached = true; break; }
    try {
      const result = await readSession(root, identity, candidate, limits, limits.maxTotalBytes - coverage.bytes_read);
      coverage.bytes_read += result.summary.coverage.bytes_read;
      sessions.push(result.summary);
    } catch { coverage.unreadable_files++; }
  }
  coverage.skipped_files += coverage.matching_files - sessions.length - coverage.unreadable_files;
  coverage.partial = coverage.directory_scan_truncated || coverage.byte_limit_reached || coverage.skipped_files > 0
    || coverage.unreadable_files > 0 || sessions.some(session => session.coverage.partial);
  return { schema_version: 1, type: 'session_history', sessions, limits, coverage };
}

const modelsText = models => Object.entries(models).map(([name, count]) => `${name} (${count})`).join(', ') || 'none';
function printSummary(summary, write) {
  write(`ID: ${summary.id}`);
  write(`Session: ${summary.session_id ?? 'anonymous'}; ${summary.started_at ?? 'no valid records'} to ${summary.updated_at ?? 'unknown'}`);
  write(`Observed: ${summary.decisions} selections; ${summary.completed} confirmed completed; ${summary.failed} failed; ${summary.cancelled} cancelled; ${summary.pending} without outcome; ${summary.unconfirmed} unconfirmed outcomes.`);
  write(`Selected models: ${modelsText(summary.selected_models)}`);
  write(`Response models observed: ${modelsText(summary.confirmed_models)}`);
  write(`Fallbacks: ${summary.fallbacks} of ${summary.decisions} selections (${(summary.fallback_rate * 100).toFixed(1)}%); routing reasons: ${modelsText(summary.routing_reasons)}`);
  const saved = summary.savings.priced_requests ? `$${summary.savings.saved_usd.toFixed(4)}` : 'unavailable';
  write(`API-equivalent savings: ${saved}; priced ${summary.savings.priced_requests}/${summary.requests} observed requests; ${summary.savings.unpriced_requests} unpriced. These are not subscription charges.`);
  if (summary.baseline_models.length) write(`Recorded Opus baseline${summary.mixed_baselines ? 's (mixed)' : ''}: ${summary.baseline_models.join(', ')}`);
  write(`Recorded pricing version${summary.mixed_pricing_versions ? 's (mixed)' : ''}: ${summary.pricing_versions.join(', ') || 'not recorded'}${summary.unversioned_outcomes ? `; ${summary.unversioned_outcomes} outcomes without a recorded version` : ''}.`);
  for (const facts of summary.pricing_facts) write(`Pricing facts ${facts.version}, reviewed ${facts.date}: ${facts.source}`);
  if (summary.savings.unpriced_requests) write(`Unpriced reasons: ${modelsText(summary.savings.unpriced_reasons)}`);
  if (summary.decision_latency_ms.samples) write(`Decision latency: p50 ${summary.decision_latency_ms.p50} ms; p95 ${summary.decision_latency_ms.p95} ms (${summary.decision_latency_ms.samples} samples).`);
  if (summary.coverage.partial) write('Partial history: limits, incomplete writes, duplicate or unreadable records affect these observed counts.');
  if (summary.coverage.legacy_decisions) write(`${summary.coverage.legacy_decisions} legacy decisions record selection only; no successful response is implied.`);
}

export async function sessionsCommand(args, { env = process.env, write = console.log } = {}) {
  const operation = args[0], json = args.includes('--json');
  const positional = args.slice(1).filter(arg => arg !== '--json');
  if (!['list', 'show'].includes(operation) || positional.some(arg => arg.startsWith('--'))
    || (operation === 'list' ? positional.length !== 0 : positional.length !== 1)) {
    throw new Error('Usage: claude-autorouter sessions list [--json] | sessions show ID [--json]');
  }
  const loaded = loadUserConfig(env, { allowMissing: true });
  const directory = parseSessionLogDir(loaded.env.AUTOROUTER_SESSION_LOG_DIR);
  if (!directory) {
    const report = { schema_version: 1, type: 'session_history', logging_enabled: false, sessions: [],
      message: 'Session logging is disabled. Set AUTOROUTER_SESSION_LOG_DIR to record future sessions.' };
    write(json ? JSON.stringify(report, null, 2) : report.message);
    return operation === 'list';
  }
  const report = await readSessionHistory(directory, operation === 'show' ? { id: positional[0] } : {});
  if (json) write(JSON.stringify({ ...report, logging_enabled: true }, null, 2));
  else {
    if (operation === 'list') {
      if (!report.sessions.length) write('No readable session logs found.');
      for (const summary of report.sessions) printSummary(summary, write);
      if (report.coverage.partial) write('Partial scan: displayed sessions or counts are limited; use sessions show ID for an individual file.');
    } else {
      printSummary(report.summary, write);
      for (const row of report.records) {
        const description = row.event === 'decision'
          ? `selected ${row.selected_model}; ${row.source ?? 'unknown source'}; ${row.reason ?? 'unspecified reason'}`
          : `${row.status}${row.http_status ? ` (HTTP ${row.http_status})` : ''}${row.error_type ? `; ${row.error_type}` : ''}; response model ${row.confirmed_model ?? 'not observed'}; completion ${row.completion_confirmed ? 'confirmed' : 'unconfirmed'}`;
        write(`${row.timestamp} ${row.request_id}: ${description}`);
        if (row.prompt_excerpt) write(`  Prompt excerpt: ${safeText(row.prompt_excerpt)}${row.prompt_truncated ? '… [truncated]' : ''}`);
      }
    }
  }
  return true;
}
