import test from 'node:test';
import assert from 'node:assert/strict';
import { inspectOllama, setupOllama } from '../src/ollama-setup.mjs';
import { DEFAULT_OLLAMA_MODEL, validateOllamaEndpoint, validateOllamaModel } from '../src/ollama-models.mjs';
import { OLLAMA_QUESTIONS } from '../src/ollama-evaluator.mjs';

const config = () => ({ ollamaEndpoint: 'http://127.0.0.1:11434', ollamaModel: DEFAULT_OLLAMA_MODEL, ollamaKeepAlive: '5m' });
const tags = installed => Response.json({ models: installed ? [{ name: DEFAULT_OLLAMA_MODEL }] : [] });
const localDetails = () => Response.json({ details: { parameter_size: '9B' }, capabilities: ['completion'] });
const currentVersion = () => Response.json({ version: '0.35.0' });
const warmed = () => Response.json({ model: DEFAULT_OLLAMA_MODEL, answers: { tier: { type: 'choice', choice: 'haiku',
  probabilities: { haiku: 1, sonnet: 0, opus: 0 }, confidence: 1 } }, usage: { input_tokens: 900, output_tokens: 1 } });
const stream = chunks => new Response(new ReadableStream({ start(controller) {
  for (const chunk of chunks) controller.enqueue(new TextEncoder().encode(chunk));
  controller.close();
} }));

test('local setup checks the minimum Ollama version before any model download', async () => {
  for (const version of ['0.33.3', '0.34.9', '', 'PRIVATE_INVALID_VERSION', undefined]) {
    const calls = [];
    await assert.rejects(setupOllama({ ...config(), ollamaModel: 'team/local-decider:v1' }, {
      pull: true, write: () => {}, fetchImpl: async url => {
        calls.push(new URL(url).pathname);
        return Response.json({ version });
      },
    }), error => error.code === 'OLLAMA_VERSION' && /0\.35/.test(error.message) && !error.message.includes('PRIVATE_'));
    assert.deepEqual(calls, ['/api/version']);
  }
  for (const version of ['0.35.0', '0.35.0-rc.1', '0.36.0', '1.0.0']) {
    const calls = [];
    const result = await inspectOllama({ ...config(), ollamaModel: DEFAULT_OLLAMA_MODEL }, { fetchImpl: async url => {
      const path = new URL(url).pathname;
      calls.push(path);
      return path === '/api/version' ? Response.json({ version }) : tags(false);
    } });
    assert.equal(result.installed, false);
    assert.deepEqual(calls, ['/api/version', '/api/tags']);
  }
});

test('local startup warms native scoring with synthetic text and exposes missing-endpoint guidance', async () => {
  for (const missingEndpoint of [false, true]) {
    const calls = [];
    const pending = setupOllama({ ...config(), ollamaModel: DEFAULT_OLLAMA_MODEL }, { write: () => {}, fetchImpl: async (url, options) => {
      const path = new URL(url).pathname;
      calls.push(path);
      if (path === '/api/version') return currentVersion();
      if (path === '/api/tags') return Response.json({ models: [{ name: DEFAULT_OLLAMA_MODEL }] });
      if (path === '/api/show') return localDetails();
      assert.equal(path, '/v1/systemone');
      const body = JSON.parse(options.body);
      assert.equal(body.state.current_task, 'Return the literal word ready.');
      assert.equal(body.questions.tier.type, 'choice');
      assert.equal(body.keep_alive, '5m');
      assert.equal(body.messages, undefined);
      assert.equal(body.think, undefined);
      assert.equal(body.options, undefined);
      return missingEndpoint ? new Response('PRIVATE_PROVIDER_ERROR', { status: 404 }) : Response.json({
        model: DEFAULT_OLLAMA_MODEL, answers: { tier: { type: 'choice', choice: 'haiku',
          probabilities: { haiku: 1, sonnet: 0, opus: 0 }, confidence: 1 } }, usage: { input_tokens: 900, output_tokens: 1 },
      });
    } });
    if (missingEndpoint) await assert.rejects(pending, error => error.code === 'OLLAMA_VERSION'
      && /Ollama 0\.35 or newer/.test(error.message) && !error.message.includes('PRIVATE_'));
    else assert.deepEqual(await pending, { model: DEFAULT_OLLAMA_MODEL, pulled: false, warmed: true });
    assert.deepEqual(calls, ['/api/version', '/api/tags', '/api/show', '/api/show', '/v1/systemone']);
  }
});

test('official library prefixes and custom aliases match canonical installed model tags', async () => {
  for (const [requested, installed] of [
    ['nimble', 'nimble:latest'],
    ['library/nimble:9b-q4_K_M', DEFAULT_OLLAMA_MODEL],
    ['registry.ollama.ai/library/nimble:9b-q4_K_M', DEFAULT_OLLAMA_MODEL],
    ['team/local-decider:v1', 'team/local-decider:v1'],
    ['registry.ollama.ai/library/local-decider:v1', 'local-decider:v1'],
    ['registry.ollama.ai/team/local-decider:v1', 'team/local-decider:v1'],
  ]) {
    const calls = [];
    const result = await inspectOllama({ ...config(), ollamaModel: requested }, { fetchImpl: async (url, options) => {
      const path = new URL(url).pathname;
      calls.push(path);
      if (path === '/api/version') return currentVersion();
      if (path === '/api/tags') return Response.json({ models: [{ name: installed }] });
      assert.equal(path, '/api/show');
      assert.equal(JSON.parse(options.body).model, requested);
      return localDetails();
    } });
    assert.deepEqual(result, { model: requested, installed: true });
    assert.deepEqual(calls, ['/api/version', '/api/tags', '/api/show']);
  }
});

test('only local endpoints and safe local model tags are accepted before any request', async () => {
  assert.equal(validateOllamaEndpoint('http://localhost:11434/'), 'http://localhost:11434');
  assert.equal(validateOllamaEndpoint('http://[::1]:11434'), 'http://[::1]:11434');
  for (const value of ['https://ollama.com', 'http://127.0.0.1:11434/api', 'http://secret@localhost:11434', 'http://localhost:11434/?secret=1', 'invalid-secret-url']) {
    assert.throws(() => validateOllamaEndpoint(value), error => !error.message.includes('secret'));
    await assert.rejects(inspectOllama({ ...config(), ollamaEndpoint: value }, { fetchImpl: () => assert.fail('Must not request an invalid endpoint') }));
  }
  for (const model of ['model:cloud', 'model:120b-cloud', 'https://secret@example.test', 'model\nsecret', '']) {
    assert.throws(() => validateOllamaModel(model), error => !error.message.includes('secret'));
  }
});

test('missing model inspection is read-only and default setup never downloads it', async () => {
  const calls = [];
  const fetchImpl = async (url, options) => {
    const path = new URL(url).pathname;
    calls.push([path, options.method]);
    return path === '/api/version' ? currentVersion() : tags(false);
  };
  assert.deepEqual(await inspectOllama(config(), { fetchImpl }), { model: DEFAULT_OLLAMA_MODEL, installed: false });
  await assert.rejects(setupOllama(config(), { fetchImpl, write: () => {} }), /--pull/);
  assert.deepEqual(calls, [['/api/version', 'GET'], ['/api/tags', 'GET'], ['/api/version', 'GET'], ['/api/tags', 'GET']]);
});

test('an explicit pull handles split NDJSON safely, deduplicates progress, verifies installation, then warms', async () => {
  const calls = [];
  const progress = [];
  let installed = false;
  const fetchImpl = async (url, options) => {
    assert.equal(options.redirect, 'error');
    assert.equal(options.headers?.authorization, undefined);
    const path = new URL(url).pathname;
    calls.push(path);
    if (path === '/api/version') return currentVersion();
    if (path === '/api/tags') return tags(installed);
    if (path === '/api/show') return localDetails();
    const body = JSON.parse(options.body);
    if (path === '/api/pull') {
      assert.deepEqual(body, { model: DEFAULT_OLLAMA_MODEL, stream: true });
      installed = true;
      return stream(['{"status":"pulling mani', 'fest"}\n',
        '{"status":"pulling PRIVATE_PROVIDER_DATA","completed":50,"total":100}\n'.repeat(3),
        '{"status":"verifying sha256 digest"}\n{"status":"success"}']);
    }
    assert.equal(path, '/v1/systemone');
    assert.equal(body.model, DEFAULT_OLLAMA_MODEL);
    assert.equal(body.keep_alive, '5m');
    assert.deepEqual(Object.keys(body).sort(), ['keep_alive', 'model', 'questions', 'state']);
    assert.deepEqual(body.questions, OLLAMA_QUESTIONS);
    assert.deepEqual(body.state, {
      system: '', original_task: 'Return the literal word ready.', current_task: 'Return the literal word ready.',
      recent_messages: [], message_count: 1, tool_count: 0, context_is_excerpt: true,
    });
    return warmed();
  };
  assert.deepEqual(await setupOllama(config(), { pull: true, fetchImpl, write: text => progress.push(text) }), {
    model: DEFAULT_OLLAMA_MODEL, pulled: true, warmed: true,
  });
  assert.deepEqual(calls, ['/api/version', '/api/tags', '/api/pull', '/api/version', '/api/tags', '/api/show', '/api/show', '/v1/systemone']);
  assert.equal(progress.filter(text => text.includes('(50%)')).length, 1);
  assert.ok(!progress.join('\n').includes('PRIVATE_PROVIDER_DATA'));
});

test('an already installed local model is inspected and warmed without pulling, even with --pull', async () => {
  const calls = [];
  const fetchImpl = async (url, options) => {
    const path = new URL(url).pathname;
    calls.push(path);
    if (path === '/api/version') return currentVersion();
    if (path === '/api/tags') return tags(true);
    if (path === '/api/show') return localDetails();
    assert.equal(path, '/v1/systemone');
    assert.equal(JSON.parse(options.body).keep_alive, '10m');
    assert.ok(!JSON.stringify(options).includes('PRIVATE_'));
    // Loading has a separate deadline from the short runtime classifier budget.
    await new Promise(resolve => setTimeout(resolve, 10));
    return warmed();
  };
  assert.deepEqual(await setupOllama({ ...config(), ollamaKeepAlive: '10m', ollamaTimeoutMs: 1,
    jevKey: 'PRIVATE_JEV_KEY', anthropicKey: 'PRIVATE_ANTHROPIC_KEY' }, { pull: true, fetchImpl, write: () => {} }), {
    model: DEFAULT_OLLAMA_MODEL, pulled: false, warmed: true,
  });
  assert.deepEqual(calls, ['/api/version', '/api/tags', '/api/show', '/api/show', '/v1/systemone']);
});

test('warming rejects invalid classifier output and a model that becomes remote before the synthetic request', async () => {
  for (const kind of ['invalid', 'remote']) {
    let inspections = 0;
    let decisions = 0;
    await assert.rejects(setupOllama(config(), { write: () => {}, fetchImpl: async url => {
      const path = new URL(url).pathname;
      if (path === '/api/version') return currentVersion();
      if (path === '/api/tags') return tags(true);
      if (path === '/api/show') {
        inspections++;
        return kind === 'remote' && inspections === 2
          ? Response.json({ remote_host: 'PRIVATE_REMOTE_HOST', details: { parameter_size: '1B' } }) : localDetails();
      }
      assert.equal(path, '/v1/systemone');
      decisions++;
      return Response.json({ error: 'PRIVATE_INVALID_ANSWER' });
    } }), error => error.code === 'OLLAMA_WARMUP' && !error.message.includes('PRIVATE_'));
    assert.equal(inspections, 2);
    assert.equal(decisions, kind === 'remote' ? 0 : 1);
  }
});

test('warmup timeout and caller cancellation stop stalled classifier bodies with safe errors', async () => {
  for (const cancelled of [false, true]) {
    const controller = new AbortController();
    let bodyCancelled = false;
    const started = performance.now();
    const pending = setupOllama(config(), { write: () => {}, signal: controller.signal,
      warmTimeoutMs: cancelled ? 1000 : 20, fetchImpl: async url => {
        const path = new URL(url).pathname;
        if (path === '/api/version') return currentVersion();
        if (path === '/api/tags') return tags(true);
        if (path === '/api/show') return localDetails();
        assert.equal(path, '/v1/systemone');
        return new Response(new ReadableStream({ cancel() { bodyCancelled = true; } }));
      } });
    if (cancelled) setTimeout(() => controller.abort(new Error('PRIVATE_CANCEL_REASON')), 10);
    await assert.rejects(pending, error => {
      assert.equal(error.code, cancelled ? 'OLLAMA_CANCELLED' : 'OLLAMA_TIMEOUT');
      assert.ok(!error.message.includes('PRIVATE_'));
      return true;
    });
    assert.equal(bodyCancelled, true);
    assert.ok(performance.now() - started < 500);
  }
});

test('cloud-backed aliases and ambiguous model details are rejected before warming', async () => {
  for (const details of [{ remote_host: 'PRIVATE_REMOTE_HOST', details: { parameter_size: '1B' } }, { remote_model: 'cloud' }, {}]) {
    const calls = [];
    await assert.rejects(setupOllama(config(), { write: () => {}, fetchImpl: async url => {
      const path = new URL(url).pathname;
      calls.push(path);
      if (path === '/api/version') return currentVersion();
      if (path === '/api/tags') return tags(true);
      assert.equal(path, '/api/show');
      return Response.json(details);
    } }), error => !error.message.includes('PRIVATE_REMOTE_HOST'));
    assert.deepEqual(calls, ['/api/version', '/api/tags', '/api/show']);
  }
});

test('server, HTTP, malformed, and oversized responses produce safe errors without following redirects', async () => {
  for (const fetchImpl of [
    async () => { throw new Error('PRIVATE_NETWORK_ERROR'); },
    async () => Response.json({ error: 'PRIVATE_HTTP_ERROR' }, { status: 500 }),
    async () => new Response('PRIVATE_INVALID_JSON'),
    async () => new Response('PRIVATE_LARGE_BODY'.repeat(100000)),
    async (_url, options) => { assert.equal(options.redirect, 'error'); return new Response(null, { status: 302, headers: { location: 'https://example.test/private' } }); },
  ]) {
    await assert.rejects(inspectOllama(config(), { fetchImpl }), error => {
      assert.ok(!error.message.includes('PRIVATE_'));
      assert.equal(error.cause, undefined);
      return true;
    });
  }
});

test('incomplete, failed, and oversized pull streams never proceed to warming', async () => {
  for (const chunks of [
    ['{"status":"pulling manifest"}\n'],
    ['{"error":"PRIVATE_DOWNLOAD_ERROR"}\n'],
    ['{"status":"PRIVATE_' + 'x'.repeat(70000) + '"}\n'],
    ['PRIVATE_INVALID_JSON\n'],
  ]) {
    const calls = [];
    await assert.rejects(setupOllama(config(), { pull: true, write: () => {}, fetchImpl: async url => {
      const path = new URL(url).pathname;
      calls.push(path);
      if (path === '/api/version') return currentVersion();
      if (path === '/api/tags') return tags(false);
      assert.equal(path, '/api/pull');
      return stream(chunks);
    } }), error => !error.message.includes('PRIVATE_'));
    assert.deepEqual(calls, ['/api/version', '/api/tags', '/api/pull']);
  }
});

test('pull timeout and user cancellation cancel stalled response bodies promptly', async () => {
  for (const cancelled of [false, true]) {
    const controller = new AbortController();
    let bodyCancelled = false;
    const started = performance.now();
    const pending = setupOllama(config(), { pull: true, warm: false, write: () => {}, signal: controller.signal,
      pullTimeoutMs: cancelled ? 1000 : 20, fetchImpl: async url => {
        if (new URL(url).pathname === '/api/version') return currentVersion();
        if (new URL(url).pathname === '/api/tags') return tags(false);
        return new Response(new ReadableStream({ cancel() { bodyCancelled = true; } }));
      } });
    if (cancelled) setTimeout(() => controller.abort(new Error('PRIVATE_CANCEL_REASON')), 10);
    await assert.rejects(pending, error => {
      assert.equal(error.code, cancelled ? 'OLLAMA_CANCELLED' : 'OLLAMA_TIMEOUT');
      assert.ok(!error.message.includes('PRIVATE_'));
      return true;
    });
    assert.equal(bodyCancelled, true);
    assert.ok(performance.now() - started < 500);
  }
});
