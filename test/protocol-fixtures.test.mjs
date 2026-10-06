import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { readFile } from 'node:fs/promises';
import { setImmediate as nextTurn } from 'node:timers/promises';
import { readConfig } from '../src/config.mjs';
import { Router } from '../src/router.mjs';
import { createRouterServer, listen } from '../src/server.mjs';

const corpus = JSON.parse(await readFile(new URL('./fixtures/claude-protocol-v1.json', import.meta.url), 'utf8'));
const localToken = 'synthetic-protocol-local-token';
const identityHeaders = identity => Object.fromEntries(Object.entries(identity)
  .map(([name, value]) => [`x-claude-code-${name.replaceAll('_', '-')}`, value]));
function wire(response) {
  const ending = response.line_ending;
  return Buffer.from(response.prelude + response.events.map(event =>
    `event: ${event.type}${ending}data: ${JSON.stringify(event)}${ending}${ending}`).join(''));
}

test('protocol corpus versions synthetic examples separately from historical client evidence', () => {
  assert.equal(corpus.fixture_version, 1);
  assert.equal(corpus.synthetic, true);
  assert.match(corpus.reviewed_at, /^\d{4}-\d{2}-\d{2}$/);
  assert.match(corpus.description, /not captured traffic/);
  assert.match(corpus.limits.join(' '), /not evaluator quality or downstream task success/);
  assert.deepEqual([...new Set(corpus.historical_evidence.map(row => row.claude_version))], ['2.1.284', '2.1.285']);
  for (const evidence of corpus.historical_evidence) {
    assert.match(evidence.sha256, /^[a-f0-9]{64}$/);
    assert.match(evidence.artifact, /^artifacts\/[\w-]+\.json$/);
    if (evidence.observed_at !== null) assert.ok(Number.isFinite(Date.parse(evidence.observed_at)));
  }
  // This report has no recorded observation timestamp. Do not manufacture one
  // from a filesystem mtime or the date when the fixture corpus was authored.
  assert.equal(corpus.historical_evidence.find(row => row.kind === 'historical_synthetic_auto_permission_probe').observed_at, null);
  assert.deepEqual(corpus.official_contracts.map(row => row.area), ['messages', 'streaming', 'fallback', 'tool_search']);
  assert.ok(corpus.official_contracts.every(row => new URL(row.url).origin === 'https://platform.claude.com'));
  assert.equal(new Set(corpus.cases.map(row => row.id)).size, corpus.cases.length);
  const coverage = new Set(corpus.cases.flatMap(row => row.covers));
  for (const required of ['haiku_floor', 'opus', 'thinking', 'deferred_tools', 'compaction', 'goal_feedback',
    'agent_scope', 'auxiliary_verdict', 'provider_fallback', 'mixed_usage', 'incomplete_response']) assert.ok(coverage.has(required), required);
  for (const scenario of corpus.cases) for (const step of scenario.steps) {
    assert.ok(step.request && step.response && step.expected);
    assert.ok(step.evaluator_tier === null || ['haiku', 'sonnet', 'opus'].includes(step.evaluator_tier));
    assert.deepEqual(Object.keys(step.expected_request_overrides).filter(key => !['model', 'thinking'].includes(key)), []);
    // Newly authored examples must not invent provider-signed history.
    assert.ok(!JSON.stringify(step.request).includes('"signature"'));
  }
});

async function fixtureGateway(t, scenario) {
  const received = [], records = [], statuses = [], failures = [];
  let activeStep, evaluations = 0;
  const upstream = http.createServer(async (req, res) => {
    try {
      const chunks = [];
      for await (const chunk of req) chunks.push(chunk);
      received.push({ body: JSON.parse(Buffer.concat(chunks).toString('utf8')), headers: req.headers, path: req.url });
      const bytes = wire(activeStep.response);
      res.writeHead(200, { 'content-type': activeStep.response.content_type, 'request-id': `synthetic-${activeStep.id}` });
      // Split a frame header and, where present, a multibyte code point. The
      // observer must preserve the original CRLF, comments and response bytes.
      const multibyte = bytes.indexOf(Buffer.from('😀'));
      const boundaries = [1, 19, ...(multibyte > 19 ? [multibyte + 1] : [])];
      let start = 0;
      for (const end of boundaries) { res.write(bytes.subarray(start, end)); start = end; await nextTurn(); }
      res.end(bytes.subarray(start));
    } catch (error) { failures.push(error); res.writeHead(500); res.end('Synthetic fixture failed.'); }
  });
  t.after(() => { upstream.closeAllConnections(); upstream.close(); });
  const upstreamAddress = await listen(upstream, 0);
  const config = { ...readConfig({ AUTOROUTER_EVALUATOR: 'jev',
    ANTHROPIC_API_KEY: 'synthetic-upstream-key', TYPESAFE_API_KEY: 'synthetic-evaluator-key',
    AUTOROUTER_CLIENT_PROFILE: scenario.profile,
    AUTOROUTER_HAIKU_MODEL: corpus.models.haiku, AUTOROUTER_SONNET_MODEL: corpus.models.sonnet,
    AUTOROUTER_OPUS_MODEL: corpus.models.opus,
  }), localToken, sessionLogMode: 'metadata', upstream: `http://127.0.0.1:${upstreamAddress.port}` };
  const router = new Router(config, { fetchImpl: async url => {
    assert.equal(url, config.jevEndpoint);
    assert.notEqual(activeStep.evaluator_tier, null, 'Auxiliary verdict requests must bypass the evaluator');
    evaluations++;
    return Response.json({ answers: { tier: { choice: activeStep.evaluator_tier, confidence: 0.99 } } });
  } });
  const server = createRouterServer(config, { router, tokenCounter: () => assert.fail('Small fixtures do not require token counting'),
    log: () => {}, onStatus: entry => statuses.push(entry), onRecord: entry => records.push(entry) });
  t.after(() => { server.closeAllConnections(); server.close(); });
  const address = await listen(server, 0);
  return { received, records, statuses, failures, router, evaluations: () => evaluations,
    send: async step => {
      activeStep = step;
      return fetch(`http://127.0.0.1:${address.port}/v1/messages?beta=true`, { method: 'POST',
        headers: { 'x-api-key': localToken, 'content-type': 'application/json',
          'anthropic-beta': 'synthetic-protocol-contract', 'anthropic-version': '2023-06-01', ...identityHeaders(step.identity) },
        body: JSON.stringify(step.request) });
    } };
}

for (const scenario of corpus.cases) test(`protocol v1: ${scenario.id}`, { timeout: 10000 }, async t => {
  const original = structuredClone(scenario);
  const gateway = await fixtureGateway(t, scenario);
  for (const [index, step] of scenario.steps.entries()) {
    const before = gateway.evaluations();
    const response = await gateway.send(step);
    assert.equal(response.status, 200, step.id);
    assert.equal(response.headers.get('request-id'), `synthetic-${step.id}`);
    assert.deepEqual(Buffer.from(await response.arrayBuffer()), wire(step.response), `${step.id}: response bytes`);
    for (let tries = 0; gateway.records.length < (index + 1) * 2 && tries < 100; tries++) await nextTurn();
    assert.deepEqual(gateway.failures, []);
    assert.equal(gateway.evaluations() - before, step.evaluator_tier === null ? 0 : 1, `${step.id}: evaluator calls`);
    const upstream = gateway.received[index];
    assert.deepEqual(upstream.body, { ...step.request, ...step.expected_request_overrides }, `${step.id}: only declared request adaptations`);
    assert.equal(upstream.path, '/v1/messages?beta=true');
    assert.equal(upstream.headers['anthropic-beta'], 'synthetic-protocol-contract');
    assert.equal(upstream.headers['anthropic-version'], '2023-06-01');
    assert.equal(upstream.headers['x-api-key'], 'synthetic-upstream-key');
    for (const [name, value] of Object.entries(identityHeaders(step.identity))) assert.equal(upstream.headers[name], value);
    const pair = gateway.records.slice(index * 2, index * 2 + 2);
    assert.deepEqual(pair.map(row => row.event), ['decision', 'outcome']);
    const [decision, outcome] = pair;
    assert.equal(decision.schema_version, 2);
    assert.equal(outcome.request_id, decision.request_id);
    for (const [field, value] of Object.entries(step.identity)) {
      assert.equal(decision[field], value);
      assert.equal(outcome[field], value);
    }
    for (const field of ['selected_model', 'source', 'reason', 'compatibility_reason']) assert.equal(decision[field], step.expected[field], `${step.id}: ${field}`);
    assert.equal(decision.classified_tier, step.evaluator_tier ?? undefined);
    assert.equal(outcome.status, 'completed');
    for (const field of ['confirmed_model', 'completion_confirmed', 'usage_complete', 'pricing_eligible', 'unpriced_reason', 'continuity_state']) {
      assert.equal(outcome[field], step.expected[field], `${step.id}: ${field}`);
    }
    assert.deepEqual(outcome.model_transitions, step.expected.model_transitions ?? [step.expected.confirmed_model]);
    if (step.expected.usage_complete) {
      assert.equal(outcome.usage.input_tokens, 100);
      assert.equal(outcome.usage.output_tokens, 12);
      assert.equal(outcome.usage.cache_read_input_tokens, 20);
    }
    assert.ok(!Object.hasOwn(decision, 'prompt_excerpt'));
    assert.ok(!JSON.stringify(pair).includes('synthetic-evaluator-key'));
    assert.equal(gateway.router.turns.attempts.size, 0, `${step.id}: no unfinished routing attempt`);
  }
  assert.deepEqual(scenario, original, 'Fixtures and request bodies must remain immutable');
  assert.equal(new Set(gateway.records.filter(row => row.event === 'decision').map(row => row.request_id)).size, scenario.steps.length);
});
