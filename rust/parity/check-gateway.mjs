// Development-only executable differential. Every process has an isolated home;
// every request, credential, evaluator and upstream response is synthetic/local.
import assert from 'node:assert/strict';
import http from 'node:http';
import { spawn, spawnSync } from 'node:child_process';
import { createReadStream } from 'node:fs';
import { chmod, copyFile, mkdtemp, readFile, readdir, rm, mkdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createHash } from 'node:crypto';
import { isDeepStrictEqual } from 'node:util';
import { setTimeout as delay } from 'node:timers/promises';
const root = resolve(import.meta.dirname, '../..');
const unicode = process.argv.includes('--unicode');
const positional = process.argv.slice(2).filter(value => value !== '--unicode');
const reference = resolve(positional[0] ?? join(root, 'artifacts/rust-rewrite/reference'));
const originalCandidate = resolve(positional[1] ?? join(root, 'rust/target/debug/claude-autorouter'));
const corpusBytes = await readFile(join(reference, 'test/fixtures/claude-protocol-v1.json'));
const corpus = JSON.parse(corpusBytes);
if (unicode) {
  const template = corpus.cases[0].steps[0];
  corpus.cases = ['\ud800', '\ud801', '\udfff', '\ufffd'].map((suffix, index) => {
    const model = `synthetic-custom-${suffix}`;
    const step = structuredClone(template);
    step.id = `saved-utf16-${index}`;
    step.identity.prompt_id = step.id;
    step.request = { model, max_tokens: 128, stream: true, messages: [{ role: 'user', content: `Synthetic typo ${suffix}. Correct teh.` }] };
    step.response.events[0].message.model = model;
    return { id: step.id, profile: 'compatible', saved: { AUTOROUTER_HAIKU_MODEL: model, AUTOROUTER_JEV_MODEL: `synthetic-evaluator-${suffix}` }, steps: [step] };
  });
}
const baseline = JSON.parse(await readFile(join(root, 'rust/parity/baseline.json')));
const checked = spawnSync(process.execPath, [join(root, 'scripts/rust-reference.mjs'), '--root', reference, '--check-baseline'], { encoding: 'utf8', timeout: 15000, maxBuffer: 1024 * 1024 });
assert.equal(checked.status, 0, 'Frozen source verification failed');
const scratch = await mkdtemp(join(tmpdir(), 'autorouter-gateway-differential-'));
const candidate = join(scratch, 'candidate');
let identity;
try {
  await copyFile(originalCandidate, candidate);
  await chmod(candidate, 0o700);
  const hashFile = async path => {
    const hash = createHash('sha256');
    for await (const chunk of createReadStream(path)) hash.update(chunk);
    return hash.digest('hex');
  };
  identity = {
    frozen_source: JSON.parse(checked.stdout), node_version: process.version,
    node_executable_sha256: await hashFile(process.execPath),
    candidate_executable: originalCandidate, candidate_sha256: await hashFile(candidate),
    candidate_execution: 'isolated immutable byte-identical snapshot',
    harness_sha256: await hashFile(new URL(import.meta.url)),
  };
} catch (error) {
  await rm(scratch, { recursive: true, force: true });
  throw error;
}
const token = 'synthetic-gateway-differential-token';
const failures = [];
const sockets = new Set();
let active, received = [], evaluator = [];
function wire(response) { const e = response.line_ending; return Buffer.from(response.prelude + response.events.map(event => `event: ${event.type}${e}data: ${JSON.stringify(event)}${e}${e}`).join('')); }
let mockFailed = false;
const mock = http.createServer(async (req, res) => {
  try {
  const chunks = []; for await (const chunk of req) chunks.push(chunk);
  const bytes = Buffer.concat(chunks);
  if (req.url === '/v1/systemone') {
    evaluator.push({ path: req.url, body: JSON.parse(bytes), auth: req.headers.authorization, apiKey: req.headers['x-api-key'] });
    assert.notEqual(active.evaluator_tier, null);
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ answers: { tier: { choice: active.evaluator_tier, confidence: 0.99 } } }));
  } else {
    received.push({ path: req.url, body: bytes.toString(), headers: Object.fromEntries(Object.entries(req.headers).filter(([key]) => !['host', 'connection', 'content-length'].includes(key))) });
    const responseBytes = wire(active.response); res.writeHead(200, { 'content-type': active.response.content_type, 'request-id': `synthetic-${active.id}` });
    for (let offset = 0; offset < responseBytes.length;) { const end = Math.min(responseBytes.length, offset + 1 + offset * 17 % 43); res.write(responseBytes.subarray(offset, end)); offset = end; await delay(0); }
    res.end();
  }
  } catch { mockFailed = true; res.destroy(); }
});
mock.on('connection', socket => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)); });
await new Promise((ok, fail) => { mock.once('error', fail); mock.listen(0, '127.0.0.1', ok); });
const mockUrl = `http://127.0.0.1:${mock.address().port}`;
function send(base, step) {
  return new Promise((ok, fail) => {
    const req = http.request(`${base}/v1/messages?beta=true`, { method: 'POST', headers: { 'x-api-key': token, 'content-type': 'application/json', 'anthropic-beta': 'synthetic-protocol-contract', 'anthropic-version': '2023-06-01', ...Object.fromEntries(Object.entries(step.identity).map(([key, value]) => [`x-claude-code-${key.replaceAll('_', '-')}`, value])) } }, res => {
      const chunks = []; res.on('data', b => chunks.push(b)); res.on('error', fail); res.on('end', () => ok({ status: res.statusCode, content_type: res.headers['content-type'], request_id: res.headers['request-id'], bytes: [...Buffer.concat(chunks)] }));
    });
    req.setTimeout(10000, () => req.destroy(new Error('Synthetic HTTP deadline'))); req.on('error', fail); req.end(JSON.stringify(step.request));
  });
}
const durations = new Set(['latency_ms', 'evaluation_latency_ms', 'decision_latency_ms', 'routing_latency_ms', 'first_response_ms', 'total_latency_ms']);
function normalize(value, ids = new Map()) {
  if (Array.isArray(value)) return value.map(v => normalize(v, ids));
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, val]) => {
    if (key === 'timestamp') return [key, '<timestamp>'];
    if (key === 'request_id') { if (!ids.has(val)) ids.set(val, `request-${ids.size}`); return [key, ids.get(val)]; }
    if (durations.has(key)) return [key, '<duration>'];
    return [key, normalize(val, ids)];
  }));
  return value;
}
async function engine(name, scenario, index) {
  received = []; evaluator = [];
  const directory = join(scratch, `${index}-${name}`); await mkdir(directory, { mode: 0o700 });
  const history = join(directory, 'history');
  const env = { HOME: directory, XDG_CONFIG_HOME: directory, TMPDIR: directory, PATH: directory,
    AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_AUTH_MODE: 'api-key', ANTHROPIC_API_KEY: 'synthetic-provider-key', TYPESAFE_API_KEY: 'synthetic-evaluator-key', AUTOROUTER_TOKEN: token,
    AUTOROUTER_PORT: '0', AUTOROUTER_UPSTREAM_URL: mockUrl, AUTOROUTER_JEV_URL: `${mockUrl}/v1/systemone`, AUTOROUTER_CLIENT_PROFILE: scenario.profile,
    AUTOROUTER_HAIKU_MODEL: corpus.models.haiku, AUTOROUTER_SONNET_MODEL: corpus.models.sonnet, AUTOROUTER_OPUS_MODEL: corpus.models.opus,
    AUTOROUTER_SESSION_LOG_DIR: history, AUTOROUTER_SESSION_LOG_MODE: 'metadata' };
  if (scenario.saved) {
    env.AUTOROUTER_CONFIG = join(directory, 'config.json');
    await writeFile(env.AUTOROUTER_CONFIG, JSON.stringify(scenario.saved), { mode: 0o600 });
    for (const key of Object.keys(scenario.saved)) delete env[key];
  }
  const child = spawn(name === 'node' ? process.execPath : candidate, name === 'node' ? [join(reference, 'bin/autorouter.mjs'), 'serve'] : ['serve'], { cwd: directory, env, stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '', stderr = ''; child.stdout.on('data', data => { stdout += data; }); child.stderr.on('data', data => { stderr += data; });
  const ended = new Promise((ok, fail) => { child.once('error', fail); child.once('close', (code, signal) => ok({ code, signal })); });
  const watchdog = setTimeout(() => child.kill('SIGKILL'), 20000);
  try {
    let base;
    for (let attempt = 0; attempt < 1000; attempt++) { base = stderr.match(/AutoRouter listening on (http:\/\/127\.0\.0\.1:\d+)/)?.[1]; if (base) break; if (child.exitCode !== null) break; await delay(5); }
    assert.ok(base, 'Synthetic gateway did not become ready');
    const responses = [];
    for (const step of scenario.steps) { active = step; responses.push(await send(base, step)); }
    assert.equal(mockFailed, false, 'Synthetic upstream failed');
    child.kill('SIGTERM'); const exit = await ended; assert.equal(exit.code, 0); assert.equal(stdout, '');
    const records = []; for (const file of await readdir(history)) for (const line of (await readFile(join(history, file), 'utf8')).split('\n').filter(Boolean)) records.push(JSON.parse(line));
    // File identity/order is random. Each request has a deterministic prompt ID
    // and decision/outcome pairing; preserve order within each session file.
    records.sort((a,b) => JSON.stringify([a.session_id,a.agent_id,a.prompt_id,a.event === 'decision' ? 0 : 1]).localeCompare(JSON.stringify([b.session_id,b.agent_id,b.prompt_id,b.event === 'decision' ? 0 : 1])));
    return { responses, received, evaluator, records: normalize(records), logs: normalize(stderr.split('\n').filter(line => line.startsWith('{')).map(line => JSON.parse(line))) };
  } finally { clearTimeout(watchdog); if (child.exitCode === null) { child.kill('SIGKILL'); await ended; } }
}
try {
  for (const [index, scenario] of corpus.cases.entries()) {
    const expected = await engine('node', scenario, index); const actual = await engine('rust', scenario, index);
    for (const key of Object.keys(expected)) if (!isDeepStrictEqual(actual[key], expected[key])) {
      const pointers = [];
      function diff(a,b,path='') { if (isDeepStrictEqual(a,b)) return; if (a && b && typeof a === 'object' && typeof b === 'object') { for (const key of new Set([...Object.keys(a),...Object.keys(b)])) diff(a[key],b[key],`${path}/${key}`); } else pointers.push(path); }
      diff(actual[key], expected[key]); failures.push({ scenario: scenario.id, field: key, pointers });
    }
  }
  const report = { schema_version: 1, kind: 'native_gateway_executable_differential', identity, passed: failures.length === 0, scenarios: corpus.cases.length, requests: corpus.cases.reduce((sum,c) => sum+c.steps.length,0), baseline_commit: baseline.baseline_commit, fixture_sha256: createHash('sha256').update(unicode ? JSON.stringify(corpus) : corpusBytes).digest('hex'), variant: unicode ? 'saved_utf16' : 'frozen_protocol', normalized: ['generated request IDs', 'timestamps', 'present duration values', 'random session file enumeration order'], failures, scope: 'Synthetic actual executable HTTP protocol corpus, evaluator inputs, provider bytes, logs and persisted metadata; no live-provider evidence.' };
  await writeFile(join(root, `artifacts/rust-rewrite/parity-gateway-${unicode ? 'utf16' : 'executable'}.json`), JSON.stringify(report,null,2)+'\n'); console.log(JSON.stringify(report,null,2)); if (failures.length) process.exitCode = 1;
} finally { for (const socket of sockets) socket.destroy(); await new Promise(ok => mock.close(ok)); await rm(scratch,{recursive:true,force:true}); }
