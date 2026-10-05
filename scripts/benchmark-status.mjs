#!/usr/bin/env node
// Local synthetic telemetry only. No sockets, credentials, or inference.
import fs from 'node:fs';
import * as promises from 'node:fs/promises';
import { syncBuiltinESMExports } from 'node:module';
import { cpus, totalmem, loadavg, platform, release, arch, tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { monitorEventLoopDelay, performance } from 'node:perf_hooks';
import { PassThrough } from 'node:stream';
import { setTimeout as sleep } from 'node:timers/promises';

const args = process.argv.slice(2);
const option = (name, fallback) => args.includes(name) ? args[args.indexOf(name) + 1] : fallback;
const label = option('--label', 'current');
const output = option('--output', 'docs/status-performance.json');
const modulePath = option('--module', 'src/status-state.mjs');
const { createStatusState } = await import(pathToFileURL(resolve(modulePath)).href);
const rounds = 60, repetitions = 3, intervalMs = 5, slowMs = 20;
const originalWrite = fs.writeFileSync;
const stall = new Int32Array(new SharedArrayBuffer(4));
const round = value => Math.round(value * 1000) / 1000;
const distribution = values => {
  const ordered = [...values].sort((a, b) => a - b);
  return { samples: values.length, p50: round(ordered[Math.floor((ordered.length - 1) * .5)] ?? 0),
    p95: round(ordered[Math.floor((ordered.length - 1) * .95)] ?? 0), max: round(ordered.at(-1) ?? 0) };
};
const scenarios = [];
for (const storageDelay of [0, slowMs]) {
  const timings = { update_ms: [], flush_call_ms: [], flush_completion_ms: [], stream_timer_lateness_ms: [], startup_ms: [], close_ms: [] };
  const loopDelays = [], writes = [], rss = [];
  for (let run = 0; run < repetitions; run++) {
    let writeCount = 0;
    fs.writeFileSync = (...input) => {
      writeCount++;
      if (storageDelay) Atomics.wait(stall, 0, 0, storageDelay);
      return originalWrite(...input);
    };
    syncBuiltinESMExports();
    const fileSystem = { ...promises, async open(...input) {
      const handle = await promises.open(...input);
      const writeFile = handle.writeFile.bind(handle);
      handle.writeFile = async (...data) => {
        writeCount++;
        if (storageDelay) await sleep(storageDelay);
        return writeFile(...data);
      };
      return handle;
    } };
    const root = await promises.mkdtemp(join(tmpdir(), 'autorouter-status-benchmark-'));
    const started = performance.now();
    const state = createStatusState({ directory: root, fileSystem });
    await state.ready;
    timings.startup_ms.push(performance.now() - started);
    const loop = monitorEventLoopDelay({ resolution: 1 });
    loop.enable();
    const stream = new PassThrough();
    let previous = performance.now();
    stream.on('data', () => {
      const now = performance.now();
      timings.stream_timer_lateness_ms.push(Math.max(0, now - previous - intervalMs));
      previous = now;
    });
    const timer = setInterval(() => stream.write('synthetic chunk'), intervalMs);
    const pending = [];
    try {
      for (let i = 0; i < rounds; i++) {
        await sleep(intervalMs);
        const updateStarted = performance.now();
        for (const [event, fields] of [['request_start', {}], ['route', { model: 'claude-sonnet-5-5', source: 'jev', evaluation_latency_ms: 200 }],
          ['upstream_response', { status: 200 }], ['upstream_model', { model: 'claude-sonnet-5-5' }],
          ['upstream_usage', { usage: { input_tokens: 1000, output_tokens: 100 } }], ['request_complete', {}]]) {
          state.update({ event, request_id: `r-${i}`, session_id: `s-${i % 20}`, ...fields });
        }
        timings.update_ms.push(performance.now() - updateStarted);
        const flushStarted = performance.now();
        const flushed = state.flush();
        timings.flush_call_ms.push(performance.now() - flushStarted);
        pending.push(Promise.resolve(flushed).then(() => timings.flush_completion_ms.push(performance.now() - flushStarted)));
        rss.push(process.memoryUsage().rss);
      }
      await Promise.all(pending);
      await sleep(intervalMs * 2);
    } finally {
      clearInterval(timer); stream.destroy(); loop.disable();
      loopDelays.push({ p50: round(loop.percentile(50) / 1e6), p95: round(loop.percentile(95) / 1e6), max: round(loop.max / 1e6) });
      const closing = performance.now();
      await state.close();
      timings.close_ms.push(performance.now() - closing);
      await promises.rm(root, { recursive: true, force: true });
      writes.push(writeCount);
      fs.writeFileSync = originalWrite; syncBuiltinESMExports();
    }
  }
  scenarios.push({ storage: storageDelay ? 'injected_slow' : 'normal_temporary_directory', injected_write_delay_ms: storageDelay,
    timings: Object.fromEntries(Object.entries(timings).map(([key, values]) => [key, distribution(values)])),
    event_loop_delay_ms_by_run: loopDelays, snapshot_writes_by_run: writes, rss_peak_bytes: Math.max(...rss) });
}
const report = { schema_version: 1, label, recorded_at: new Date().toISOString(),
  environment: { node: process.version, platform: platform(), release: release(), arch: arch(), cpu: cpus()[0]?.model,
    logical_cpus: cpus().length, memory_bytes: totalmem(), load_average: loadavg(),
    background_load: 'Other workspace activity was not suspended; no process identities or personal data captured.' },
  method: { rounds_per_run: rounds, repetitions, events_per_round: 6, concurrent_sessions: 20, producer_interval_ms: intervalMs,
    mock_stream_interval_ms: intervalMs, injection: 'One 20ms delay per snapshot write: blocking wait for synchronous writer, timer wait for asynchronous writer. Metadata operations are not delayed.',
    note: 'Stream timer lateness measures event-loop interference, not a network benchmark. Flush completion may include coalesced subsequent snapshots.' }, scenarios };
let prior = { schema_version: 1, runs: [] };
try { prior = JSON.parse(await promises.readFile(output, 'utf8')); } catch {}
prior.runs = [...(Array.isArray(prior.runs) ? prior.runs : []).filter(item => item.label !== label), report];
const baseline = prior.runs.find(item => item.label === 'synchronous-baseline');
if (baseline && label !== baseline.label) {
  const baseNormal = baseline.scenarios.find(item => item.injected_write_delay_ms === 0);
  const baseSlow = baseline.scenarios.find(item => item.injected_write_delay_ms === slowMs);
  const normal = scenarios[0], slow = scenarios[1];
  const gate = (name, actual, maximum, rationale) => ({ name, actual: round(actual), maximum: round(maximum), passed: actual <= maximum, rationale });
  report.baseline_comparison = {
    baseline_label: baseline.label,
    scope: 'Local synthetic regression thresholds derived from this recorded baseline; not portable latency promises or ordinary CI gates.',
    gates: [
      gate('slow_storage_stream_p95_ms', slow.timings.stream_timer_lateness_ms.p95, baseSlow.timings.stream_timer_lateness_ms.p95 / 4,
        'Require at least 75% less interference than the measured blocking-storage baseline.'),
      gate('slow_storage_flush_call_p95_ms', slow.timings.flush_call_ms.p95, baseSlow.timings.flush_call_ms.p95 / 10,
        'Require at least 90% less synchronous caller time when storage is slow.'),
      gate('normal_storage_stream_p95_ms', normal.timings.stream_timer_lateness_ms.p95, baseNormal.timings.stream_timer_lateness_ms.p95 * 1.25,
        'Allow 25% baseline headroom for scheduler noise while checking normal-storage regression.'),
      gate('normal_storage_update_p95_ms', normal.timings.update_ms.p95, baseNormal.timings.update_ms.p95 * 2,
        'Allow twice the small pre-existing normalization/update cost; this is not a disk-latency bound.'),
    ],
  };
}
await promises.writeFile(output, JSON.stringify(prior, null, 2) + '\n');
process.stdout.write(JSON.stringify({ output, label, scenarios, baseline_comparison: report.baseline_comparison }, null, 2) + '\n');
if (args.includes('--check') && (!report.baseline_comparison || report.baseline_comparison.gates.some(gate => !gate.passed))) process.exitCode = 1;
