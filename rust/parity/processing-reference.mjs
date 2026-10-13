// Development-only matched body-processing helper. No real transport exists.
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const sha = value => createHash('sha256').update(value).digest('hex');
const load = path => import(pathToFileURL(`${process.argv[2]}/src/${path}`).href);
try {
  const control = readFileSync(0);
  if (control.length > 1024 * 1024) throw new Error();
  const spec = JSON.parse(control);
  if (spec.kind !== 'synthetic_processing_input_v1' || ![8, 200].includes(spec.warmup)
    || ![32, 2000].includes(spec.measured) || typeof spec.fixture !== 'string') throw new Error();
  const [{ readConfig }, { Router }, { createTokenCounter }, { validateRequestShape }, { prepareRequest }] = await Promise.all([
    load('config.mjs'), load('router.mjs'), load('token-counter.mjs'), load('request-validation.mjs'), load('model-request.mjs'),
  ]);
  const config = readConfig({
    AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_AUTH_MODE: 'api-key',
    TYPESAFE_API_KEY: 'synthetic-processing-evaluator-key', ANTHROPIC_API_KEY: 'synthetic-processing-provider-key',
    AUTOROUTER_JEV_URL: 'http://127.0.0.1:1/v1/systemone', AUTOROUTER_UPSTREAM_URL: 'http://127.0.0.1:1',
  });
  const calls = { evaluator: [], count: [] };
  const totals = { evaluator: 0, count: 0 };
  const digests = { evaluator: createHash('sha256'), count: createHash('sha256'), prepared: createHash('sha256'), decision: createHash('sha256') };
  const fetchImpl = async (url, options) => {
    const parsed = new URL(url);
    if (parsed.origin !== 'http://127.0.0.1:1' || typeof options.body !== 'string') throw new Error();
    const endpoint = parsed.pathname === '/v1/systemone' ? 'evaluator'
      : parsed.pathname === '/v1/messages/count_tokens' ? 'count' : undefined;
    if (!endpoint) throw new Error();
    calls[endpoint].push(options.body);
    return new Response(endpoint === 'evaluator' ? '{"answers":{"tier":{"choice":"sonnet","confidence":1}}}' : '{"input_tokens":1000}',
      { status: 200, headers: { 'content-type': 'application/json' } });
  };
  const router = new Router(config, { fetchImpl });
  const counter = createTokenCounter(config, { fetchImpl });
  let countJobs = [];
  const raw = Buffer.from(spec.fixture);
  const offset = raw.indexOf('000000000000');
  if (offset < 0 || raw.indexOf('000000000000', offset + 1) >= 0) throw new Error();
  const samples = [];
  for (let index = 0; index < spec.warmup + spec.measured; index++) {
    const bytes = Buffer.from(raw);
    bytes.write(String(index).padStart(12, '0'), offset, 12, 'ascii');
    const requestId = `synthetic-processing-request-${index}`;
    const scope = JSON.stringify(['synthetic-benchmark-session', 'agent-0']);
    const signal = new AbortController().signal;
    const headers = { 'x-api-key': 'synthetic-processing-provider-key' };
    const started = performance.now();
    const body = JSON.parse(bytes);
    const valid = validateRequestShape(body);
    if (!valid.valid) throw new Error();
    const decision = await router.route(body, {
      requestId, scope, promptId: `synthetic-prompt-${index}`, signal,
      countTokens: (value, model) => {
        const job = counter(value, model, { headers, signal });
        countJobs.push(job);
        return job;
      },
    });
    const prepared = JSON.stringify(prepareRequest(body, decision.model).request);
    while (calls.evaluator.length !== 1 || calls.count.length !== 1) {
      if (performance.now() - started > 10000 || calls.evaluator.length > 1 || calls.count.length > 1) throw new Error();
      await new Promise(resolve => setImmediate(resolve));
    }
    if (countJobs.length !== 1 || (await Promise.all(countJobs)).some(value => value !== 1000)) throw new Error();
    countJobs = [];
    const elapsed = performance.now() - started;
    if (decision.model !== 'claude-opus-5-5' || prepared !== bytes.toString()) throw new Error();
    for (const endpoint of ['evaluator', 'count']) {
      digests[endpoint].update(calls[endpoint][0]).update('\n');
      calls[endpoint].length = 0;
      totals[endpoint]++;
    }
    digests.prepared.update(prepared).update('\n');
    const metadata = Object.fromEntries(Object.keys(decision).sort()
      .filter(key => !['latency_ms', 'evaluation_latency_ms'].includes(key)).map(key => [key, decision[key]]));
    digests.decision.update(JSON.stringify(metadata)).update('\n');
    if (!router.complete(requestId, { continuation_model: 'claude-opus-5-5', tool_uses: [] })) throw new Error();
    if (index >= spec.warmup) samples.push(elapsed);
  }
  console.log(JSON.stringify({ implementation: 'node', fixture_sha256: sha(raw), requests: spec.measured,
    warmup_requests: spec.warmup, deterministic_checks_passed: true, evaluator_calls: totals.evaluator,
    count_token_calls: totals.count, count_responses_completed: totals.count, count_responses_abandoned: 0,
    prepared_sha256: digests.prepared.digest('hex'),
    decision_metadata_sha256: digests.decision.digest('hex'),
    evaluator_payloads_sha256: digests.evaluator.digest('hex'), count_payloads_sha256: digests.count.digest('hex'),
    raw_pipeline_ms: samples }));
} catch {
  console.error('Synthetic processing helper failed');
  process.exitCode = 1;
}
