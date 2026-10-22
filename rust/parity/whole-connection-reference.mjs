// Ordinary frozen gateway/HTTP clients, development-only immediate socket IO.
import http from 'node:http';
import net from 'node:net';
import { createHash } from 'node:crypto';
import { readSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import { ImmediateSocket, ProviderCodec, parseWire, MAX_REQUEST, MAX_RESPONSE, RESPONSE, PROVIDER_WIRE } from './whole-connection-reference-sockets.mjs';

const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const load = path => import(pathToFileURL(`${process.argv[2]}/src/${path}`).href);
const CLOCKS = new Set(['timestamp', 'latency_ms', 'evaluation_latency_ms', 'routing_latency_ms', 'decision_latency_ms', 'first_response_ms', 'total_latency_ms']);
function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical);
  if (!value || typeof value !== 'object') return value;
  return Object.fromEntries(Object.keys(value).sort().filter(key => !CLOCKS.has(key) && value[key] !== undefined)
    .map(key => [key, key === 'request_id' ? '<synthetic-request-id>' : canonical(value[key])]));
}
const hashObject = value => sha(JSON.stringify(canonical(value)));
const tick = () => new Promise(resolve => setImmediate(resolve));
async function wait(predicate) {
  const started = performance.now();
  while (!predicate()) { if (performance.now() - started > 10000) throw new Error('Synthetic completion deadline'); await tick(); }
}
function input() {
  const chunks = [];
  let total = 0;
  for (;;) {
    const buffer = Buffer.alloc(4096);
    const count = readSync(0, buffer, 0, buffer.length, null);
    if (!count) break;
    total += count;
    if (total > MAX_REQUEST) throw new Error('Synthetic control bound');
    chunks.push(buffer.subarray(0, count));
  }
  const spec = JSON.parse(Buffer.concat(chunks));
  const numeric = spec.mode === 'measure';
  if (spec.kind !== 'synthetic_whole_connection_input_v1' || ![undefined, 'validate', 'measure'].includes(spec.mode)
    || spec.warmup !== (numeric ? 200 : 8) || spec.measured !== (numeric ? 2000 : 32) || typeof spec.fixture !== 'string') throw new Error('Invalid synthetic input');
  return spec;
}
function rawRequest(bytes, index, credential) {
  return Buffer.concat([Buffer.from(`POST /v1/messages?probe=whole HTTP/1.1\r\nHost: localhost\r\nX-Api-Key: ${credential}\r\nContent-Type: application/json\r\nContent-Length: ${bytes.length}\r\nX-Claude-Code-Session-Id: synthetic-benchmark-session\r\nX-Claude-Code-Agent-Id: agent-0\r\nX-Claude-Code-Prompt-Id: synthetic-prompt-${index}\r\nX-Claude-Code-Request-Class: main\r\nX-Synthetic-Permission: preserve\r\nCookie: remove-synthetic-cookie\r\nConnection: keep-alive\r\n\r\n`), bytes]);
}

try {
  const spec = input();
  const [{ readConfig }, { Router }, { createTokenCounter }, { createRouterServer }] = await Promise.all([
    load('config.mjs'), load('router.mjs'), load('token-counter.mjs'), load('server.mjs'),
  ]);
  const credential = 'synthetic-benchmark-local-credential';
  const originalRequest = http.request;
  const raw = Buffer.from(spec.fixture);
  const offset = raw.indexOf('000000000000');
  if (offset < 0 || raw.indexOf('000000000000', offset + 1) >= 0) throw new Error('Ambiguous synthetic index');
  async function run(loopback, requests, heldWriter = false, numerical = false) {
    let current;
    const sockets = new Set();
    let provider;
    let gateway;
    let downstream;
    let agent;
    let providerAddress = 'http://127.0.0.1:1';
    try {
      if (loopback) {
        provider = net.createServer(socket => {
          sockets.add(socket);
          let received = Buffer.alloc(0);
          socket.on('data', bytes => {
            try {
              if (bytes.length > MAX_REQUEST - received.length) throw new Error();
              received = Buffer.concat([received, bytes]);
              const request = parseWire(received, true);
              if (!request) return;
              if (request.first !== 'POST /v1/messages?probe=whole HTTP/1.1') throw new Error();
              current.inference.push(request);
              received = received.subarray(request.consumed);
              socket.write(PROVIDER_WIRE);
            } catch { socket.destroy(); }
          });
          socket.on('error', () => {});
        });
        await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
        providerAddress = `http://127.0.0.1:${provider.address().port}`;
      } else {
        agent = new http.Agent({ ...http.globalAgent.options });
        agent.createConnection = () => {
          const codec = new ProviderCodec(request => current.inference.push(request));
          const socket = new ImmediateSocket((bytes, socket) => codec.write(bytes, socket));
          sockets.add(socket);
          socket.on('error', () => {});
          return socket;
        };
        http.request = function (url, options, callback) {
          if (url.origin !== 'http://127.0.0.1:1' || url.pathname !== '/v1/messages' || url.search !== '?probe=whole') throw new Error('Forbidden synthetic destination');
          return originalRequest.call(http, url, { ...options, agent }, callback);
        };
      }
      const config = readConfig({ AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_AUTH_MODE: 'api-key', AUTOROUTER_TOKEN: credential,
        TYPESAFE_API_KEY: 'synthetic-processing-evaluator-key', ANTHROPIC_API_KEY: 'synthetic-processing-provider-key',
        AUTOROUTER_JEV_URL: 'http://127.0.0.1:1/v1/systemone', AUTOROUTER_UPSTREAM_URL: providerAddress });
      const fetchImpl = async (url, options) => {
        const parsed = new URL(url);
        if (!['http://127.0.0.1:1', providerAddress].includes(parsed.origin) || typeof options.body !== 'string') throw new Error('Forbidden synthetic fetch');
        const endpoint = parsed.pathname === '/v1/systemone' ? 'evaluator' : parsed.pathname === '/v1/messages/count_tokens' ? 'count' : undefined;
        if (!endpoint || current[endpoint].length !== 0) throw new Error('Unexpected synthetic call');
        current[endpoint].push(options.body);
        return new Response(endpoint === 'evaluator' ? '{"answers":{"tier":{"choice":"sonnet","confidence":1}}}' : '{"input_tokens":1000}', { status: 200, headers: { 'content-type': 'application/json' } });
      };
      const router = new Router(config, { fetchImpl });
      const counter = createTokenCounter(config, { fetchImpl });
      const complete = router.complete.bind(router);
      router.complete = (id, evidence) => {
        const accepted = complete(id, evidence);
        if (accepted) current.commits.push(evidence);
        return accepted;
      };
      gateway = createRouterServer(config, { router,
        tokenCounter: (...args) => { const job = counter(...args); current.jobs.push(job); return job; },
        log: entry => current.log.push(entry), onStatus: entry => current.status.push(entry), onDecision: entry => current.decision.push(entry) });
      const handler = gateway.listeners('request')[0];
      gateway.removeListener('request', handler);
      gateway.on('request', (request, response) => {
        Promise.resolve(handler(request, response)).then(() => { current.handlerDone = true; }, () => { current.failed = true; });
      });
      if (loopback) {
        await new Promise(resolve => gateway.listen(0, '127.0.0.1', resolve));
        downstream = net.createConnection(gateway.address().port, '127.0.0.1');
        await new Promise(resolve => downstream.once('connect', resolve));
        downstream.on('data', bytes => {
          if (bytes.length > MAX_RESPONSE - current.wire.length) current.failed = true;
          else current.wire = Buffer.concat([current.wire, bytes]);
        });
      } else {
        downstream = new ImmediateSocket(bytes => {
          if (bytes.length > MAX_RESPONSE - current.wire.length) throw new Error('Synthetic response bound');
          current.wire = Buffer.concat([current.wire, bytes]);
        }, MAX_REQUEST);
        gateway.emit('connection', downstream);
      }
      downstream.on('error', () => { if (current) current.failed = true; });
      sockets.add(downstream);
      const rows = [];
      const times = [];
      for (let index = 0; index < requests; index++) {
        const bytes = Buffer.from(raw);
        bytes.write(String(index).padStart(12, '0'), offset, 12, 'ascii');
        const request = rawRequest(bytes, index, credential);
        current = { evaluator: [], count: [], inference: [], log: [], status: [], decision: [], commits: [], jobs: [], wire: Buffer.alloc(0), handlerDone: false, failed: false };
        if (heldWriter) downstream.holdWrites = true;
        // Ordinary handler/delivery and every count reply precede the end edge.
        const started = performance.now();
        if (loopback) downstream.write(request); else downstream.feed(request);
        if (heldWriter) {
          await wait(() => current.failed || current.status.some(row => row.event === 'upstream_model'));
          if (current.failed || current.jobs.length !== 1 || (await Promise.all(current.jobs)).some(value => value !== 1000)
            || current.handlerDone || current.commits.length || current.status.at(-1)?.event === 'request_complete') throw new Error('Node clock/commit ended before writer completion');
          downstream.releaseWrites();
        }
        await wait(() => current.failed || current.handlerDone);
        const parsedCounts = await Promise.all(current.jobs);
        if (current.failed || current.jobs.length !== 1 || parsedCounts.some(value => value !== 1000)) throw new Error('Incomplete synthetic count/handler');
        const elapsed = performance.now() - started;
        if (elapsed > 10000) throw new Error('Incomplete synthetic writer');
        if (numerical && index >= spec.warmup) times.push(elapsed);
        // Processing clock stops before detailed response parsing/hashing.
        if (loopback) await wait(() => current.failed || parseWire(current.wire));
        if (current.failed) throw new Error('Incomplete synthetic writer');
        const response = parseWire(current.wire);
        if (response.status !== 200 || response.consumed !== current.wire.length || !response.body.equals(RESPONSE)
          || current.evaluator.length !== 1 || current.count.length !== 1 || current.inference.length !== 1
          || current.commits.length !== 1 || current.decision.length !== 1
          || current.inference[0].body.toString() !== bytes.toString()
          || current.status.at(-1)?.event !== 'request_complete' || current.status.at(-1)?.completion_confirmed !== true) throw new Error('Synthetic whole-connection mismatch');
        const observed = current.decision[0];
        const branch = { decision_source: observed.source, decision_reason: observed.reason,
          classified_tier: observed.classified_tier, selected_model: observed.selected_model,
          inference_model: JSON.parse(current.inference[0].body).model,
          count_target_model: JSON.parse(current.count[0]).model, count_parsed_input_tokens: parsedCounts[0],
          count_result_used_for_selected_target: observed.context_check !== undefined || observed.counted_input_tokens !== undefined,
          provider_observed_model: JSON.parse(response.body).model };
        const expected = { decision_source: 'jev', decision_reason: 'large_or_multimodal_request', classified_tier: 'sonnet',
          selected_model: 'claude-opus-5-5', inference_model: 'claude-opus-5-5', count_target_model: 'claude-haiku-4-5-20251001',
          count_parsed_input_tokens: 1000, count_result_used_for_selected_target: false, provider_observed_model: 'claude-sonnet-5' };
        if (JSON.stringify(branch) !== JSON.stringify(expected)) throw new Error('Synthetic frozen policy/count result mismatch');
        rows.push({ index, raw_request_sha256: sha(request), prepared_sha256: sha(current.inference[0].body),
          response_sha256: sha(response.body), response_status: response.status,
          response_headers_sha256: hashObject(response.headers), inference_headers_sha256: hashObject(current.inference[0].headers),
          evaluator_payload_sha256: sha(current.evaluator[0]), count_payload_sha256: sha(current.count[0]),
          log_sha256: hashObject(current.log), status_sha256: hashObject(current.status), decision_sha256: hashObject(current.decision),
          commits_sha256: hashObject(current.commits), evaluator_calls: 1, inference_calls: 1, count_calls: 1,
          count_responses_completed: 1, count_responses_abandoned: 0, delivery_confirmed: true, handler_complete: true, ...branch });
      }
      return { rows, times };
    } finally {
      http.request = originalRequest;
      agent?.destroy();
      for (const socket of sockets) socket.destroy();
      if (gateway?.listening) await new Promise(resolve => gateway.close(resolve));
      if (provider?.listening) await new Promise(resolve => provider.close(resolve));
    }
  }
  const numerical = spec.mode === 'measure';
  const result = await run(false, spec.warmup + spec.measured, false, numerical);
  const controls = await run(true, 2);
  if (JSON.stringify(result.rows.slice(0, 2)) !== JSON.stringify(controls.rows)) throw new Error('Ordinary loopback control mismatch');
  const held = await run(false, 1, true);
  if (JSON.stringify(result.rows[0]) !== JSON.stringify(held.rows[0])) throw new Error('Held writer changed output/state');
  const report = { implementation: 'node', fixture_sha256: sha(raw), requests: spec.measured, warmup_requests: spec.warmup,
    deterministic_checks_passed: true, genuine_inference_client: true, ordinary_loopback_control: true, held_writer_control: true,
    count_responses_completed: result.rows.length, count_responses_abandoned: 0, rows: result.rows, loopback_controls: controls.rows,
    numerical_samples_retained: numerical, clean_close: true };
  if (numerical) report.raw_processing_ms = result.times;
  console.log(JSON.stringify(report));
} catch {
  console.error('Synthetic whole-connection helper failed');
  process.exitCode = 1;
}
