import http from 'node:http';
import https from 'node:https';
import { randomUUID, timingSafeEqual } from 'node:crypto';
import { pipeline } from 'node:stream/promises';
import { Router } from './router.mjs';
import { LOCAL_AUTH_HEADER, isSubscriptionRequest } from './auth.mjs';
import { prepareRequest } from './model-request.mjs';
import { createResponseObserver } from './response-observer.mjs';
import { createTokenCounter } from './token-counter.mjs';

function cleanHeaders(headers) {
  const blocked = new Set(['host', 'connection', 'keep-alive', 'proxy-authenticate', 'proxy-authorization', 'te', 'trailer', 'transfer-encoding', 'upgrade', 'content-length']);
  for (const name of String(headers.connection ?? '').split(',')) blocked.add(name.trim().toLowerCase());
  return Object.fromEntries(Object.entries(headers).filter(([name, value]) => value !== undefined && !blocked.has(name.toLowerCase())));
}

function authorized(req, config) {
  const credential = config.authMode === 'subscription'
    ? req.headers[LOCAL_AUTH_HEADER]
    : req.headers['x-api-key'] ?? req.headers.authorization?.replace(/^Bearer /i, '');
  if (typeof credential !== 'string') return false;
  const actual = Buffer.from(credential);
  const expected = Buffer.from(config.localToken);
  return actual.length === expected.length && timingSafeEqual(actual, expected);
}

function readBody(req, limit) {
  return new Promise((resolve, reject) => {
    let bytes = 0;
    const chunks = [];
    req.on('data', chunk => {
      bytes += chunk.length;
      if (bytes > limit) {
        chunks.length = 0;
        reject(Object.assign(new Error('Request body too large'), { status: 413 }));
      } else chunks.push(chunk);
    });
    req.on('end', () => resolve(Buffer.concat(chunks)));
    req.on('error', reject);
    req.on('aborted', () => reject(new Error('Client disconnected')));
  });
}

function jsonError(res, status, message) {
  if (res.headersSent || res.destroyed) { res.destroy(); return; }
  res.writeHead(status, { 'content-type': 'application/json' });
  res.end(JSON.stringify({ type: 'error', error: { type: 'api_error', message } }));
}

function upstreamHeaders(incoming, config) {
  const headers = cleanHeaders(incoming);
  delete headers[LOCAL_AUTH_HEADER];
  delete headers.cookie;
  // Negotiate an uncompressed stream so the model ID can be observed without
  // altering any response bytes or buffering generation output.
  headers['accept-encoding'] = 'identity';
  if (config.authMode === 'subscription') {
    // Do not cache or refresh this credential; Claude Code owns its lifecycle.
    // Keep Authorization and anthropic-beta intact, including on token refresh.
    delete headers['x-api-key'];
  } else {
    delete headers.authorization;
    headers['x-api-key'] = config.anthropicKey;
  }
  return headers;
}

async function forward(url, req, res, body, config, signal, log, status) {
  const headers = upstreamHeaders(req.headers, config);
  if (body) headers['content-length'] = String(body.length);
  const transport = url.protocol === 'https:' ? https : http;
  const upstream = await new Promise((resolve, reject) => {
    const outgoing = transport.request(url, { method: req.method, headers, signal }, resolve);
    outgoing.on('error', reject);
    outgoing.end(body);
  });
  // Native HTTP streams retain compression and SSE bytes verbatim.
  log({ event: 'upstream_response', status: upstream.statusCode });
  status('upstream_response', { status: upstream.statusCode });
  // An upstream disconnect can make pipeline destroy the downstream response.
  // Record it before that close is mistaken for a user cancellation.
  upstream.once('aborted', () => {
    if (!signal.aborted) status('request_error', { status: 502 });
  });
  res.writeHead(upstream.statusCode, cleanHeaders(upstream.headers));
  try {
    if (!upstream.headers['content-encoding'] || upstream.headers['content-encoding'] === 'identity') {
      const observer = createResponseObserver({
        contentType: upstream.headers['content-type'],
        onModel: ({ model }) => { log({ event: 'upstream_model', model }); status('upstream_model', { model }); },
        onError: ({ error_type }) => { log({ event: 'upstream_error', error_type }); status('upstream_error', { error_type }); },
        onUsage: ({ usage }) => { if (upstream.statusCode >= 200 && upstream.statusCode < 300) status('upstream_usage', { usage }); },
      });
      await pipeline(upstream, observer, res, { signal });
    } else {
      await pipeline(upstream, res, { signal });
    }
  } catch (error) {
    if (!signal.aborted || signal.reason?.name === 'TimeoutError') status('request_error', { status: 502 });
    throw error;
  }
}

export function createRouterServer(config, { router = new Router(config), tokenCounter = createTokenCounter(config), log = entry => process.stderr.write(`${JSON.stringify(entry)}\n`), onStatus = () => {} } = {}) {
  if (!config.localToken || config.localToken.length < 16) throw new Error('AUTOROUTER_TOKEN must contain at least 16 characters');
  const server = http.createServer(async (req, res) => {
    const controller = new AbortController();
    let context;
    let finished = false;
    const status = (event, fields = {}) => {
      if (!context || finished) return;
      if (['request_complete', 'request_error', 'request_cancelled'].includes(event)) finished = true;
      // Optional observability must never delay or fail an inference request.
      try { Promise.resolve(onStatus({ ...context, event, ...fields })).catch(() => {}); } catch {}
    };
    const rejectRequest = (code, message) => {
      status('request_error', { status: code });
      return jsonError(res, code, message);
    };
    res.on('close', () => { if (!res.writableFinished) controller.abort(); });
    req.on('aborted', () => controller.abort());
    try {
      if (req.headers.origin) return jsonError(res, 403, 'Browser requests are not supported');
      if (!authorized(req, config)) return jsonError(res, 401, 'Invalid local router credential');
      // Use a fixed upstream and an allowlist; never interpret a client URL as
      // an upstream origin, or turn this into an open forwarding proxy.
      const url = new URL(req.url, 'http://127.0.0.1');
      if (req.method === 'GET' && url.pathname === '/health') {
        res.writeHead(200, { 'content-type': 'application/json' });
        return res.end(JSON.stringify({ status: 'ok' }));
      }
      if (req.method === 'HEAD' && url.pathname === '/api/hello') { res.writeHead(200); return res.end(); }
      const inference = req.method === 'POST' && url.pathname === '/v1/messages';
      const countTokens = req.method === 'POST' && url.pathname === '/v1/messages/count_tokens';
      const models = req.method === 'GET' && /^\/v1\/models(?:\/[^/]+)?$/.test(url.pathname);
      if (!inference && !countTokens && !models) return jsonError(res, 404, 'Unsupported endpoint');
      if (inference) {
        const hint = name => typeof req.headers[name] === 'string' ? req.headers[name] : undefined;
        context = {
          request_id: randomUUID(),
          session_id: hint('x-claude-code-session-id'), agent_id: hint('x-claude-code-agent-id'),
          prompt_id: hint('x-claude-code-prompt-id'), request_class: hint('x-claude-code-request-class'),
        };
        status('request_start');
      }
      if (config.authMode === 'subscription' && !isSubscriptionRequest(req.headers)) {
        return rejectRequest(401, 'Subscription mode requires Claude Code OAuth authentication and its OAuth beta header, without an API key. Sign in with claude auth login and remove conflicting API-key, auth-token, custom-header, or apiKeyHelper settings.');
      }
      if (req.headers['content-encoding'] && req.headers['content-encoding'] !== 'identity') return rejectRequest(415, 'Compressed request bodies are not supported');
      if (Number(req.headers['content-length']) > config.maxBodyBytes) return rejectRequest(413, 'Request body too large');
      let body;
      if (inference || countTokens) {
        body = await readBody(req, config.maxBodyBytes);
        let parsed;
        try { parsed = JSON.parse(body); } catch { return rejectRequest(400, 'Invalid JSON body'); }
        if (!parsed || typeof parsed.model !== 'string' || !Array.isArray(parsed.messages) || parsed.messages.some(m => !m || !['user', 'assistant', 'system'].includes(m.role) || !(typeof m.content === 'string' || (Array.isArray(m.content) && m.content.every(b => b && typeof b.type === 'string'))))) {
          log({ event: 'invalid_request_shape', model_type: typeof parsed?.model, messages_type: Array.isArray(parsed?.messages) ? 'array' : typeof parsed?.messages,
            messages: Array.isArray(parsed?.messages) ? parsed.messages.slice(0, 10).map(m => ({
              role: ['user', 'assistant', 'system'].includes(m?.role) ? m.role : typeof m?.role,
              content_type: Array.isArray(m?.content) ? 'array' : typeof m?.content,
              blocks: Array.isArray(m?.content) ? m.content.slice(0, 10).map(b => ({ value_type: b === null ? 'null' : typeof b, type_type: typeof b?.type })) : undefined,
            })) : undefined });
          return rejectRequest(400, 'Expected a model and Messages API messages');
        }
        if (inference) {
          const decision = await router.route(parsed, {
            signal: controller.signal,
            scope: JSON.stringify([req.headers['x-claude-code-session-id'], req.headers['x-claude-code-agent-id']]),
            promptId: req.headers['x-claude-code-prompt-id'],
            requestClass: req.headers['x-claude-code-request-class'],
            countTokens: (body, model) => tokenCounter(body, model, {
              headers: upstreamHeaders(req.headers, config), signal: controller.signal, search: url.search,
            }),
          });
          if (controller.signal.aborted) { status('request_cancelled'); return; }
          const prepared = prepareRequest(parsed, decision.model);
          body = Buffer.from(JSON.stringify(prepared.request));
          log({ event: 'route', requested_model: parsed.model, ...decision, request_adjustments: prepared.adjustments });
          const { model, source, evaluator, reason, latency_ms, classifier_error, classifier_status, classified_tier, context_check, counted_input_tokens } = decision;
          const pricingValue = (field, allowed, fallback) => prepared.request[field] === undefined ? fallback
            : allowed.includes(prepared.request[field]) ? prepared.request[field] : 'unknown';
          const pricing_context = {
            speed: pricingValue('speed', ['standard', 'fast'], 'standard'),
            inference_geo: pricingValue('inference_geo', ['global', 'us'], 'global'),
            service_tier: pricingValue('service_tier', ['auto', 'standard_only'], 'auto'),
          };
          if (prepared.request.fallbacks != null || prepared.request.fallback_credit_token != null
            || (Array.isArray(prepared.request.tools) && prepared.request.tools.some(tool => typeof tool?.type === 'string' && /^advisor(?:_|$)/.test(tool.type)))) pricing_context.pricing_unsupported = true;
          status('route', { requested_model: parsed.model, model, source, evaluator, reason, latency_ms, classifier_error, classifier_status,
            classified_tier, context_check, counted_input_tokens, pricing_context });
        }
      }
      if (controller.signal.aborted) { status('request_cancelled'); return; }
      const target = new URL(`${config.upstream}${url.pathname}${url.search}`);
      await forward(target, req, res, body, config, AbortSignal.any([controller.signal, AbortSignal.timeout(config.upstreamTimeoutMs)]), log, status);
      status('request_complete');
    } catch (error) {
      if (controller.signal.aborted) { status('request_cancelled'); return; }
      // Error strings from providers can contain request data. Keep logs local
      // and metadata-only, including on error paths.
      log({ event: 'proxy_error', status: error.status ?? 502 });
      rejectRequest(error.status ?? 502, error.status === 413 ? 'Request body too large' : 'Router could not complete the upstream request');
    }
  });
  server.requestTimeout = 30000;
  server.headersTimeout = 10000;
  return server;
}

export function listen(server, port) {
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, '127.0.0.1', () => { server.removeListener('error', reject); resolve(server.address()); });
  });
}
