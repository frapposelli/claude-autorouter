import test from 'node:test';
import assert from 'node:assert/strict';
import { compareRouterBenchmarks } from '../scripts/benchmark-router.mjs';

const report = calls => ({ inputs: { iterations: 1, rounds: 1, concurrency: 8, evaluator_delay_ms: 1, large_request_bytes: 2000 },
  environment: { node: 'synthetic', platform: 'synthetic', arch: 'synthetic', cpu: 'synthetic', total_memory_bytes: 1024 },
  results: [{ scenario: 'concurrent_identical', requests: 8, evaluator_calls: calls, route_ms: { p95: 2 },
    memory_bytes: { sampled_peak_heap_delta: 1000 }, event_loop_delay_ms: { p95: 10 } }] });

test('local benchmark gates derive limits from the recorded baseline and reject unshared work', () => {
  const baseline = report(8), candidate = report(1);
  const result = compareRouterBenchmarks(baseline, candidate);
  assert.equal(result.passed, true);
  assert.equal(result.latency[0].limit_ms, 4);
  assert.equal(result.resources[0].sampled_peak_heap_limit_bytes, 2000);
  assert.equal(result.resources[0].event_loop_p95_limit_ms, 20);
  assert.equal(compareRouterBenchmarks(baseline, report(8)).evaluator_calls.passed, false);
});

test('latency, heap, event-loop and incomparable-run regressions fail independently', () => {
  const changes = [
    value => { value.results[0].route_ms.p95 = 4.1; },
    value => { value.results[0].memory_bytes.sampled_peak_heap_delta = 2001; },
    value => { value.results[0].event_loop_delay_ms.p95 = 20.1; },
    value => { value.inputs.iterations = 2; },
    value => { value.environment.total_memory_bytes = 2048; },
    value => { value.results = []; },
  ];
  for (const change of changes) {
    const candidate = report(1); change(candidate);
    assert.equal(compareRouterBenchmarks(report(8), candidate).passed, false);
  }
});
