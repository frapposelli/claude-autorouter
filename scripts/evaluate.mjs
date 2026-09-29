import { readFile } from 'node:fs/promises';
import { readConfig } from '../src/config.mjs';
import { Router } from '../src/router.mjs';

// Explicitly invoked, paid Jev calls only. No Claude generations.
const config = readConfig();
if (!config.jevKey) throw new Error('Set TYPESAFE_API_KEY to run the routing evaluation');
const cases = JSON.parse(await readFile(new URL('../test/fixtures/routing.json', import.meta.url), 'utf8'));
const rows = [];
for (const item of cases) {
  const router = new Router(config); // No warm decision cache in this measurement.
  const result = await router.route({ model: config.models.sonnet, max_tokens: 4096, messages: [{ role: 'user', content: item.prompt }] });
  rows.push({ case: item.name, expected: item.expected, actual: result.tier, source: result.source, confidence: result.confidence ?? null, ms: result.latency_ms });
}
console.table(rows);
const times = rows.map(row => row.ms).sort((a, b) => a - b);
const percentile = p => times[Math.ceil(p * times.length) - 1];
console.log(JSON.stringify({
  cases: rows.length,
  rubric_agreement: rows.filter(row => row.actual === row.expected && row.source === 'jev').length / rows.length,
  fallback_count: rows.filter(row => row.source === 'fallback').length,
  routing_p50_ms: percentile(0.5),
  routing_p95_ms: percentile(0.95),
}, null, 2));
console.log('These labels are starting assumptions. This measures routing behavior, not downstream task quality or cost savings.');
if (rows.some(row => row.source === 'fallback')) process.exitCode = 1;
