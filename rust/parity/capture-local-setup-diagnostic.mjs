// Finite synthetic error matrices only. Never clone/tee stalled reference bodies.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/local-contracts-${Date.now()}`));
assert.ok(process.argv.length <= 4, 'Usage: capture-local-setup-diagnostic.mjs [reference] [new-output]');
await verifyBaseline(reference);
const { inspectOllama } = await import(pathToFileURL(join(reference, 'src/ollama-setup.mjs')).href);
const { DEFAULT_OLLAMA_MODEL } = await import(pathToFileURL(join(reference, 'src/ollama-models.mjs')).href);
const { runLocalDiagnostic } = await import(pathToFileURL(join(reference, 'src/local-diagnostic.mjs')).href);
const { readConfig } = await import(pathToFileURL(join(reference, 'src/config.mjs')).href);
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const env = { AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_OLLAMA_MODEL: 'tev1:4b-q4_K_M' };
const json = value => ({ status: 200, body: { text: JSON.stringify(value) } });
const inspectCases = [
  ['network', { network_error: 'PRIVATE_NETWORK_ERROR' }],
  ['http', { ...json({ error: 'PRIVATE_HTTP_ERROR' }), status: 500 }],
  ['json', { status: 200, body: { text: 'PRIVATE_INVALID_JSON' } }],
  ['oversized', { status: 200, body: { repeat: 'PRIVATE_LARGE_BODY', times: 100000 } }],
  ['redirect', { status: 302, headers: { location: 'https://example.test/private' }, body: { text: '' } }],
];
const residencyCases = [
  ['models-null', json({ models: [null] })],
  ['json', { status: 200, body: { text: 'PRIVATE_BAD_JSON' } }],
  ['oversized', { status: 200, body: { repeat: 'x', times: 1024 * 1024 + 1 } }],
  ['http', { status: 500, body: { text: 'PRIVATE_ERROR' } }],
];
const cases = [
  ...inspectCases.map(([name, response]) => ({ id: `setup-inspect-${name}`, kind: 'inspect',
    source_test: 'test/ollama-setup.test.mjs#12', env: { ...env, AUTOROUTER_OLLAMA_MODEL: DEFAULT_OLLAMA_MODEL }, responses: [response] })),
  ...residencyCases.map(([name, response]) => ({ id: `diagnostic-residency-${name}`, kind: 'diagnostic',
    source_test: 'test/local-diagnostic.test.mjs#11', env, responses: [json({ version: '0.35.0' }),
      json({ models: [{ name: env.AUTOROUTER_OLLAMA_MODEL }] }), json({ details: { parameter_size: '4B' } }), response] })),
];
for (const item of cases) {
  const config = readConfig(item.env);
  const calls = [];
  const fetchImpl = async (url, options = {}) => {
    assert.equal(new URL(url).origin, config.ollamaEndpoint);
    assert.equal(options.redirect, 'error');
    const headers = Object.fromEntries(new Headers(options.headers).entries());
    assert.ok(!('authorization' in headers) && !('x-api-key' in headers));
    calls.push({ url: String(url), method: options.method ?? 'GET', headers,
      body: options.body === undefined ? null : JSON.parse(options.body) });
    assert.ok(calls.length <= item.responses.length, 'No redirect, retry or unexpected task request');
    const response = item.responses[calls.length - 1];
    if (response.network_error) throw new Error(response.network_error);
    const body = response.body.text ?? response.body.repeat.repeat(response.body.times);
    return new Response(body, { status: response.status, headers: response.headers });
  };
  let result;
  if (item.kind === 'inspect') {
    let thrown;
    try { await inspectOllama(config, { fetchImpl }); } catch (error) { thrown = error; }
    assert.ok(thrown);
    assert.ok(!thrown.message.includes('PRIVATE_'));
    assert.equal(thrown.cause, undefined);
    result = { error: { code: thrown.code, message: thrown.message, has_cause: false } };
  } else {
    const report = await runLocalDiagnostic(config, { fetchImpl });
    assert.equal(report.passed, false);
    assert.equal(report.error.code, 'residency_unavailable');
    assert.equal(calls.filter(call => new URL(call.url).pathname === '/v1/systemone').length, 0);
    assert.ok(!JSON.stringify(report).includes('PRIVATE_'));
    result = { report };
  }
  assert.equal(calls.length, item.responses.length);
  item.node_expected = { calls, ...result };
}
mkdirSync(output); // Fresh output only: retained captures are never overwritten.
const bytes = cases.map(item => JSON.stringify(item)).join('\n') + '\n';
writeFileSync(join(output, 'local-setup-diagnostic.jsonl'), bytes);
const sourcePaths = ['test/ollama-setup.test.mjs', 'test/local-diagnostic.test.mjs', 'src/ollama-setup.mjs', 'src/local-diagnostic.mjs'];
const report = { schema_version: 1, kind: 'finite_frozen_local_contract_capture',
  baseline_commit: JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json'))).baseline_commit,
  node: process.version, cases: cases.length, source_files: sourcePaths.map(path => ({ path,
    sha256: hash(readFileSync(join(reference, path))) })), corpus_sha256: hash(bytes),
  limitations: ['Only finite injected-response error matrices; no network/Ollama/provider/model download.',
    'Original full test definitions are verified separately; timing/cancellation uses unmodified source assertions and native drop-aware mocks.',
    'No report field or elapsed value normalized: these preflight failures contain no inference timing.'] };
writeFileSync(join(output, 'capture.json'), JSON.stringify(report, null, 2) + '\n');
console.log(JSON.stringify(report));
