// Deterministic synthetic workloads only: no sockets, credentials or models.
import { cpus, freemem, loadavg, platform, release, totalmem } from 'node:os';
import { monitorEventLoopDelay, performance } from 'node:perf_hooks';
import { readFile, writeFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import { Router } from '../src/router.mjs';
import { readConfig } from '../src/config.mjs';

const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const percentile = (values, fraction) => [...values].sort((a, b) => a - b)[Math.ceil(values.length * fraction) - 1] ?? 0;
const stats = values => ({ samples: values.length, p50: percentile(values, .5), p95: percentile(values, .95), max: Math.max(0, ...values) });
const rounded = value => Math.round(value * 1000) / 1000;
const body = text => ({ model: 'claude-haiku-4-5-20251001', max_tokens: 128, messages: [{ role: 'user', content: text }] });
const tools = Array.from({ length: 300 }, (_, index) => ({ name: `synthetic_${index}`, description: 'Synthetic tool description. '.repeat(30),
  input_schema: { type: 'object', properties: { value: { type: 'string' } } } }));

export async function runRouterBenchmark({ iterations = 60, rounds = 3, evaluatorDelayMs = 1 } = {}) {
  const results = [];
  const names = ['small', 'large_tools', 'cache', 'pinned', 'concurrent_identical', 'concurrent_identical_agents', 'concurrent_distinct_agents', 'cancellation'];
  for (let round = 0; round < rounds; round++) for (const name of names) {
    let calls = 0, aborted = 0;
    const config = readConfig({ AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: 'synthetic-benchmark-key', AUTOROUTER_AUTH_MODE: 'subscription' });
    const router = new Router(config, { fetchImpl: async (_url, { signal }) => {
      calls++;
      await new Promise((resolve, reject) => {
        const cancel = () => { aborted++; clearTimeout(timer); reject(signal.reason); };
        const timer = setTimeout(() => { signal.removeEventListener('abort', cancel); resolve(); }, evaluatorDelayMs);
        if (signal.aborted) cancel(); else signal.addEventListener('abort', cancel, { once: true });
      });
      return Response.json({ answers: { tier: { choice: 'sonnet', confidence: 1 } } });
    } });
    const cached = body(`cache-${round}`);
    const original = body(`pinned-${round}`);
    if (name === 'cache') await router.route(cached);
    if (name === 'pinned') await router.route(original, { scope: 'pin', promptId: 'pin' });
    const setupCalls = calls;
    global.gc?.();
    const before = process.memoryUsage();
    let peakHeap = before.heapUsed, peakRss = before.rss;
    const sample = setInterval(() => {
      const value = process.memoryUsage(); peakHeap = Math.max(peakHeap, value.heapUsed); peakRss = Math.max(peakRss, value.rss);
    }, 1);
    const lag = monitorEventLoopDelay({ resolution: 10 }); lag.enable();
    await pause(12);
    const times = [], overhead = [];
    let requests = 0;
    const route = async (request, options) => {
      const start = performance.now(); requests++;
      const result = await router.route(request, options);
      times.push(performance.now() - start);
      overhead.push(Math.max(0, result.latency_ms - result.evaluation_latency_ms));
      return result;
    };
    for (let index = 0; index < iterations; index++) {
      const text = `${name}-${round}-${index}`;
      if (name.startsWith('concurrent_')) {
        await Promise.all(Array.from({ length: 8 }, (_, agent) => route(body(name === 'concurrent_distinct_agents' ? `${text}-${agent}` : text),
          { scope: name === 'concurrent_identical' ? 'same' : `agent-${agent}`, promptId: `${text}-${agent}` })));
      } else if (name === 'cancellation') {
        const controller = new AbortController();
        const start = performance.now(); requests++;
        const pending = router.route(body(text), { signal: controller.signal });
        controller.abort(new Error('Synthetic benchmark cancellation'));
        await pending.catch(() => {});
        times.push(performance.now() - start);
      } else if (name === 'pinned') {
        await route({ ...original, messages: [...original.messages,
          { role: 'assistant', content: [{ type: 'tool_use', id: `tool-${index}`, name: 'Read', input: {} }] },
          { role: 'user', content: [{ type: 'tool_result', tool_use_id: `tool-${index}`, content: text }] }] },
        { scope: 'pin', promptId: 'pin' });
      } else await route(name === 'cache' ? cached : { ...body(text), ...(name === 'large_tools' ? { tools } : {}) });
    }
    await pause(12);
    clearInterval(sample); lag.disable();
    global.gc?.();
    const after = process.memoryUsage();
    results.push({ scenario: name, round: round + 1, requests, setup_evaluator_calls: setupCalls, evaluator_calls: calls - setupCalls,
      aborted_evaluations: aborted, route_ms: stats(times), policy_overhead_ms: stats(overhead),
      memory_bytes: { retained_heap_delta: after.heapUsed - before.heapUsed, retained_rss_delta: after.rss - before.rss,
        sampled_peak_heap_delta: peakHeap - before.heapUsed, sampled_peak_rss_delta: peakRss - before.rss },
      event_loop_delay_ms: { p50: lag.percentile(50) / 1e6, p95: lag.percentile(95) / 1e6, max: lag.max / 1e6 } });
  }
  return { schema_version: 1, type: 'synthetic_router_benchmark', timestamp: new Date().toISOString(),
    inputs: { iterations, rounds, concurrency: 8, evaluator_delay_ms: evaluatorDelayMs,
      large_request_bytes: Buffer.byteLength(JSON.stringify({ ...body('large_tools'), tools })),
      real_provider_calls: 0, gc_exposed: typeof global.gc === 'function' },
    environment: { node: process.version, platform: platform(), os_release: release(), arch: process.arch,
      cpu: cpus()[0]?.model, logical_cpus: cpus().length, total_memory_bytes: totalmem(), free_memory_bytes: freemem(),
      load_average: loadavg(), background_processes: 'Uncontrolled developer workstation; no user process names or arguments collected.' }, results };
}

export function compareRouterBenchmarks(baseline, candidate) {
  const comparable = { passed: ['iterations', 'rounds', 'concurrency', 'evaluator_delay_ms', 'large_request_bytes']
    .every(key => baseline.inputs[key] === candidate.inputs[key])
    && ['node', 'platform', 'arch', 'cpu', 'total_memory_bytes'].every(key => baseline.environment[key] === candidate.environment[key]),
  expected: 'Same workload parameters, Node version and hardware; background load remains reported separately.' };
  const gates = [], resources = [];
  for (const scenario of new Set(baseline.results.map(row => row.scenario))) {
    const before = baseline.results.filter(row => row.scenario === scenario);
    const after = candidate.results.filter(row => row.scenario === scenario);
    const baselineP95 = Math.max(...before.map(row => row.route_ms.p95));
    const limit = baselineP95 * 2;
    gates.push({ scenario, baseline_max_p95_ms: rounded(baselineP95), candidate_max_p95_ms: rounded(Math.max(...after.map(row => row.route_ms.p95))),
      limit_ms: rounded(limit), passed: after.length === before.length && after.every(row => row.route_ms.p95 <= limit),
      rationale: 'Local regression guard: twice the largest p95 across three pre-change rounds; not a universal latency target.' });
    const peakHeap = Math.max(...before.map(row => row.memory_bytes.sampled_peak_heap_delta));
    const loopDelay = Math.max(...before.map(row => row.event_loop_delay_ms.p95));
    resources.push({ scenario, sampled_peak_heap_limit_bytes: peakHeap * 2,
      candidate_sampled_peak_heap_bytes: Math.max(...after.map(row => row.memory_bytes.sampled_peak_heap_delta)),
      event_loop_p95_limit_ms: rounded(loopDelay * 2),
      candidate_event_loop_p95_ms: rounded(Math.max(...after.map(row => row.event_loop_delay_ms.p95))),
      passed: after.length === before.length && after.every(row => row.memory_bytes.sampled_peak_heap_delta <= peakHeap * 2
        && row.event_loop_delay_ms.p95 <= loopDelay * 2) });
  }
  const coalesced = candidate.results.filter(row => ['concurrent_identical', 'concurrent_identical_agents'].includes(row.scenario));
  const evaluator_calls = { passed: coalesced.length > 0 && coalesced.every(row => row.evaluator_calls === row.requests / candidate.inputs.concurrency),
    expected: 'One evaluation per group of eight identical concurrent requests, including different agents.' };
  return { passed: comparable.passed && evaluator_calls.passed && gates.every(gate => gate.passed) && resources.every(gate => gate.passed),
    comparable, evaluator_calls, latency: gates, resources };
}

async function main(args) {
  const label = args[0], output = args[1];
  if (!['baseline', 'candidate'].includes(label) || !output || args.length !== 2) throw new Error('Usage: node --expose-gc scripts/benchmark-router.mjs baseline|candidate OUTPUT.json');
  let document = {};
  try { document = JSON.parse(await readFile(output, 'utf8')); } catch (error) { if (error.code !== 'ENOENT') throw error; }
  if (label === 'baseline' && document.baseline) throw new Error('Refusing to overwrite the recorded pre-change baseline.');
  document[label] = await runRouterBenchmark();
  if (document.baseline && document.candidate) document.comparison = compareRouterBenchmarks(document.baseline, document.candidate);
  await writeFile(output, `${JSON.stringify(document, null, 2)}\n`);
  console.log(JSON.stringify({ type: 'synthetic_router_benchmark', label, output,
    ...(document.comparison ? { passed: document.comparison.passed } : {}) }));
  if (document.comparison?.passed === false) process.exitCode = 1;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main(process.argv.slice(2)).catch(error => { console.error(error.message); process.exitCode = 1; });
}
