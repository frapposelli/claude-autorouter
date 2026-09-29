#!/usr/bin/env node
import { createHash } from 'node:crypto';
import { execFile } from 'node:child_process';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { arch, cpus, freemem, platform, totalmem } from 'node:os';
import { dirname, resolve } from 'node:path';
import { promisify } from 'node:util';
import { OLLAMA_RUBRIC, buildOllamaState, buildOllamaRequest, evaluateOllama } from '../src/ollama-evaluator.mjs';
import { validateOllamaEndpoint, validateOllamaModel } from '../src/ollama-models.mjs';

const execute = promisify(execFile);
const TIERS = ['haiku', 'sonnet', 'opus'];
const FIXTURES = new URL('../test/fixtures/ollama-routing.json', import.meta.url);
const digest = value => createHash('sha256').update(value).digest('hex');
const ms = value => Math.round(value * 100) / 100;
const percentile = (values, fraction) => values.length ? [...values].sort((a, b) => a - b)[Math.ceil(values.length * fraction) - 1] : null;

function parseArgs(args) {
  const options = { models: ['qwen3:1.7b'], split: 'heldout', rounds: 3, timeoutMs: 1500, coldTimeoutMs: 60000,
    endpoint: 'http://127.0.0.1:11434', output: undefined, stressRounds: 0 };
  for (let index = 0; index < args.length; index++) {
    const key = args[index];
    if (key === '--help') { options.help = true; continue; }
    if (!['--models', '--split', '--rounds', '--stress-rounds', '--timeout-ms', '--cold-timeout-ms', '--endpoint', '--output'].includes(key) || !args[index + 1]) {
      throw new Error('Unknown or incomplete option; use --help');
    }
    const value = args[++index];
    if (key === '--models') options.models = value.split(',').map(validateOllamaModel);
    if (key === '--split') options.split = value;
    if (key === '--rounds') options.rounds = Number(value);
    if (key === '--stress-rounds') options.stressRounds = Number(value);
    if (key === '--timeout-ms') options.timeoutMs = Number(value);
    if (key === '--cold-timeout-ms') options.coldTimeoutMs = Number(value);
    if (key === '--endpoint') options.endpoint = validateOllamaEndpoint(value);
    if (key === '--output') options.output = resolve(value);
  }
  if (!['tuning', 'heldout', 'all'].includes(options.split)) throw new Error('--split must be tuning, heldout, or all');
  for (const [name, value, limit] of [['rounds', options.rounds, 20], ['timeout-ms', options.timeoutMs, 30000], ['cold-timeout-ms', options.coldTimeoutMs, 120000]]) {
    if (!Number.isInteger(value) || value < 1 || value > limit) throw new Error(`Invalid --${name}`);
  }
  if (!Number.isInteger(options.stressRounds) || options.stressRounds < 0 || options.stressRounds > 20) throw new Error('Invalid --stress-rounds');
  if (!options.models.length || new Set(options.models).size !== options.models.length) throw new Error('Model tags must be distinct');
  options.endpoint = validateOllamaEndpoint(options.endpoint);
  return options;
}

async function api(endpoint, path, body) {
  const response = await fetch(`${endpoint}${path}`, { method: body ? 'POST' : 'GET', redirect: 'error',
    headers: body ? { 'content-type': 'application/json' } : undefined,
    body: body ? JSON.stringify(body) : undefined, signal: AbortSignal.timeout(60000) });
  if (!response.ok) throw new Error(`Local Ollama ${path} returned HTTP ${response.status}`);
  return response.json();
}

function bodyFor(item) {
  const messages = item.messages ?? [...(item.history ?? []), { role: 'user', content: item.prompt }];
  return { model: 'claude-haiku-4-5-20251001', max_tokens: 4096,
    ...(item.system ? { system: item.system } : {}), messages };
}

function stressCase(index) {
  // Put a different nonce in the first serialized state field so the previous
  // user-state prefix cannot be reused. Keep the production system rubric.
  const nonce = digest(`synthetic-full-budget-${index}`).slice(0, 24);
  const lines = Array.from({ length: 160 }, (_, row) =>
    `Synthetic file entry ${row}: export const label_${row} = 'amber'; unrelated background description ${nonce}.`).join('\n');
  return { id: `full-budget-${index}`, split: 'performance_stress', expected: 'haiku',
    system: `Synthetic performance nonce ${nonce}. The following background text is not a new task.`,
    messages: [
      { role: 'user', content: "Replace the exact label 'amber' with 'blue' in the supplied file. Make only this literal text replacement." },
      { role: 'assistant', content: [{ type: 'tool_use', id: 'synthetic-read', name: 'Read', input: { file_path: 'synthetic.txt' } }] },
      { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'synthetic-read', content: lines }] },
    ] };
}

async function residentMemory(endpoint, model) {
  const current = (await api(endpoint, '/api/ps')).models ?? [];
  const resident = current.find(item => item.name === model || item.model === model);
  let runnerRssBytes;
  try {
    const { stdout } = await execute('ps', ['-axo', 'rss=,comm='], { maxBuffer: 1024 * 1024 });
    runnerRssBytes = stdout.split('\n').reduce((sum, line) => {
      const match = /^\s*(\d+)\s+(.+)$/.exec(line);
      return sum + (match && /(?:^|\/)ollama(?:\s|$)/.test(match[2]) ? Number(match[1]) * 1024 : 0);
    }, 0);
  } catch {}
  return {
    model_size_bytes: resident?.size, model_vram_bytes: resident?.size_vram,
    context_length: resident?.context_length, parameter_size: resident?.details?.parameter_size,
    quantization: resident?.details?.quantization_level,
    ollama_process_rss_bytes: runnerRssBytes, system_free_bytes: freemem(),
  };
}

function summarize(rows) {
  const matrix = Object.fromEntries(TIERS.map(tier => [tier, Object.fromEntries([...TIERS, 'error'].map(label => [label, 0]))]));
  for (const row of rows) matrix[row.expected][row.actual ?? 'error']++;
  const valid = rows.filter(row => row.actual !== null);
  const latencies = rows.map(row => row.wall_ms);
  const successfulLatencies = valid.map(row => row.wall_ms);
  return {
    requests: rows.length, valid: valid.length, errors: rows.length - valid.length,
    exact_rubric_agreement: rows.filter(row => row.actual === row.expected).length / rows.length,
    under_routes: valid.filter(row => TIERS.indexOf(row.actual) < TIERS.indexOf(row.expected)).length,
    over_routes: valid.filter(row => TIERS.indexOf(row.actual) > TIERS.indexOf(row.expected)).length,
    opus_under_routes: valid.filter(row => row.expected === 'opus' && row.actual !== 'opus').length,
    wall_p50_ms: percentile(latencies, 0.5), wall_p95_ms: percentile(latencies, 0.95),
    successful_wall_p50_ms: percentile(successfulLatencies, 0.5), successful_wall_p95_ms: percentile(successfulLatencies, 0.95),
    confusion: matrix,
  };
}

// Vary order deterministically between rounds so latency is not measured from
// repeated identical consecutive prompts. The common system prefix can still
// benefit from Ollama's normal prompt cache, as it does in the real evaluator.
function orderForRound(items, round) {
  const ordered = [...items];
  let state = 1907 + round * 7919;
  for (let index = ordered.length - 1; index > 0; index--) {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    const other = state % (index + 1);
    [ordered[index], ordered[other]] = [ordered[other], ordered[index]];
  }
  return ordered;
}

async function measure(item, config) {
  const state = buildOllamaState(bodyFor(item), 3000);
  const started = performance.now();
  try {
    const result = await evaluateOllama(state, config);
    return { case: item.id, split: item.split, expected: item.expected, actual: result.choice,
      wall_ms: ms(performance.now() - started), state_bytes: Buffer.byteLength(JSON.stringify(state)), metrics: result.metrics };
  } catch (error) {
    const category = error.name === 'TimeoutError' ? 'timeout'
      : error.classifierStatus ? 'http_error'
      : error.message === 'classifier_invalid_response' || error instanceof SyntaxError ? 'invalid_response' : 'local_error';
    return { case: item.id, split: item.split, expected: item.expected, actual: null,
      wall_ms: ms(performance.now() - started), state_bytes: Buffer.byteLength(JSON.stringify(state)), error: category,
      ...(error.classifierStatus ? { http_status: error.classifierStatus } : {}) };
  }
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help) {
    console.log('Usage: node scripts/evaluate-ollama.mjs --models qwen3:1.7b,qwen3:4b [--split tuning|heldout|all] [--rounds 3] [--stress-rounds 8] [--timeout-ms 1500] [--cold-timeout-ms 60000] [--output artifacts/ollama-evaluation.json]\nUses only checked-in synthetic cases and an already-running local Ollama. Does not download models or contact Claude/Jev. Requires no models currently loaded; loads one candidate at a time and unloads it afterward. A cold call is reported separately and excluded from warm agreement/latencies. Optional stress cases fill the excerpt budget and vary an early nonce; their results are separate from fixture agreement. Labels are subjective rubric judgments, not downstream task-quality measurements.');
    return;
  }
  const fixtureText = await readFile(FIXTURES, 'utf8');
  const cases = JSON.parse(fixtureText);
  const selected = cases.filter(item => options.split === 'all' || item.split === options.split);
  if (!selected.length || new Set(cases.map(item => item.id)).size !== cases.length
    || cases.some(item => !TIERS.includes(item.expected) || !['tuning', 'heldout'].includes(item.split)
      || (!Array.isArray(item.messages) && typeof item.prompt !== 'string'))) throw new Error('Invalid benchmark fixtures');
  const active = (await api(options.endpoint, '/api/ps')).models ?? [];
  if (active.length) throw new Error('Ollama already has resident models. Leave them unchanged and rerun when the instance is idle.');
  const installed = (await api(options.endpoint, '/api/tags')).models ?? [];
  for (const model of options.models) {
    if (!installed.some(item => item.name === model || item.model === model)) throw new Error(`Pull ${model} explicitly before benchmarking`);
  }
  const report = {
    type: 'ollama_routing_evaluation', timestamp: new Date().toISOString(),
    hardware: { cpu: cpus()[0]?.model, architecture: arch(), platform: platform(), total_memory_bytes: totalmem() },
    ollama_version: (await api(options.endpoint, '/api/version')).version,
    fixture_sha256: digest(fixtureText), rubric_sha256: digest(OLLAMA_RUBRIC),
    split: options.split, rounds: options.rounds, stress_rounds: options.stressRounds, warm_timeout_ms: options.timeoutMs, cold_timeout_ms: options.coldTimeoutMs,
    state_character_budget: 3000, model_confidence: 'not requested or fabricated', models: [],
  };
  const persist = async () => {
    if (!options.output) return;
    await mkdir(dirname(options.output), { recursive: true });
    await writeFile(options.output, `${JSON.stringify(report, null, 2)}\n`);
  };
  for (const model of options.models) {
    const config = { ollamaEndpoint: options.endpoint, ollamaModel: model, ollamaKeepAlive: '5m', ollamaTimeoutMs: options.timeoutMs };
    const info = installed.find(item => item.name === model || item.model === model);
    const entry = { model, digest: info.digest, download_bytes: info.size,
      request_options: buildOllamaRequest({}, config).options, cold: undefined, resident_memory: undefined, rows: [] };
    report.models.push(entry);
    try {
      console.log(`Measuring ${model}: cold load, then ${selected.length * options.rounds} warm synthetic requests.`);
      entry.cold = await measure(selected[0], { ...config, ollamaTimeoutMs: options.coldTimeoutMs });
      entry.resident_memory = await residentMemory(options.endpoint, model);
      if (entry.cold.actual === null) {
        entry.skipped = 'cold request did not produce a valid tier';
        console.log(`${model}: cold request failed (${entry.cold.error}); skipping warm run.`);
        continue;
      }
      for (let round = 0; round < options.rounds; round++) {
        for (const item of orderForRound(selected, round)) entry.rows.push({ round: round + 1, ...await measure(item, config) });
        await persist();
        console.log(`${model}: completed round ${round + 1}/${options.rounds}.`);
      }
      entry.summary = summarize(entry.rows);
      entry.final_resident_memory = await residentMemory(options.endpoint, model);
      console.log(JSON.stringify({ model, cold_wall_ms: entry.cold.wall_ms, resident_memory: entry.resident_memory, ...entry.summary }));
      if (options.stressRounds) {
        entry.max_budget_stress = { rows: [] };
        for (let index = 0; index < options.stressRounds; index++) {
          entry.max_budget_stress.rows.push(await measure(stressCase(index), config));
        }
        entry.max_budget_stress.summary = summarize(entry.max_budget_stress.rows);
        console.log(JSON.stringify({ model, max_budget_stress: entry.max_budget_stress.summary }));
      }
    } finally {
      // An empty generate request changes only the keep-alive state. No model
      // files are deleted and no other model is stopped or replaced.
      await api(options.endpoint, '/api/generate', { model, stream: false, keep_alive: 0 });
      await persist();
    }
  }
  report.complete = true;
  await persist();
  if (options.output) console.log(`Saved ${options.output}`);
  if (report.models.some(entry => entry.skipped || entry.summary?.errors || entry.max_budget_stress?.summary.errors)) process.exitCode = 1;
}

main().catch(error => { console.error(`Ollama evaluation failed: ${error.message}`); process.exitCode = 1; });
