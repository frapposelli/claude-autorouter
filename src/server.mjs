import http from 'node:http';
import https from 'node:https';
import { randomUUID, timingSafeEqual } from 'node:crypto';
import { pipeline } from 'node:stream/promises';
import { Router } from './router.mjs';
import { LOCAL_AUTH_HEADER, isSubscriptionRequest } from './auth.mjs';
import { prepareRequest } from './model-request.mjs';
import { createResponseObserver } from './response-observer.mjs';
import { createTokenCounter } from './token-counter.mjs';
import { promptExcerpt } from './prompt-state.mjs';
import { validateRequestShape } from './request-validation.mjs';
import { normalizeSessionRecord } from './telemetry-event.mjs';
import { PRICING_VERSION, estimateOutcomeSavings } from './savings.mjs';

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
  // An oversized body is no longer read; close rather than keep the socket.
  res.writeHead(status, { 'content-type': 'application/json', ...(status === 413 ? { connection: 'close' } : {}) });
  res.end(JSON.stringify({ type: 'error', error: { type: 'api_error', message } }));
  if (status === 413) res.once('finish', () => res.socket?.destroy());
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
  let execution;
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
        onComplete: evidence => { execution = evidence; },
      });
      await pipeline(upstream, observer, res, { signal });
    } else {
      await pipeline(upstream, res, { signal });
    }
  } catch (error) {
    if (!signal.aborted || signal.reason?.name === 'TimeoutError') status('request_error', { status: 502 });
    throw error;
  }
  return upstream.statusCode >= 200 && upstream.statusCode < 300 ? execution : undefined;
}

export function createRouterServer(config, { router = new Router(config), tokenCounter = createTokenCounter(config), log = entry => process.stderr.write(`${JSON.stringify(entry)}\n`), onStatus = () => {}, onDecision, onRecord } = {}) {
  if (!config.localToken || config.localToken.length < 16) throw new Error('AUTOROUTER_TOKEN must contain at least 16 characters');
  const server = http.createServer(async (req, res) => {
    const controller = new AbortController();
    let context;
    let finished = false;
    const startedAt = performance.now();
    let forwardedAt;
    const outcome = { baseline_model: config.models.opus, pricing_version: PRICING_VERSION, completion_confirmed: false };
    const record = entry => {
      if (!onRecord) return;
      try {
        const row = normalizeSessionRecord({ timestamp: new Date().toISOString(), ...context, ...entry },
          { includePrompts: config.sessionLogMode !== 'metadata' });
        if (row) Promise.resolve(onRecord(row)).catch(() => {});
      } catch {}
    };
    const status = (event, fields = {}) => {
      if (!context || finished) return;
      if (event === 'route') Object.assign(outcome, fields, { selected_model: fields.model,
        routing_latency_ms: fields.latency_ms, decision_latency_ms: fields.latency_ms });
      if (event === 'upstream_response') {
        outcome.http_status = fields.status;
        outcome.first_response_ms = Math.round((performance.now() - (forwardedAt ?? startedAt)) * 100) / 100;
        fields.first_response_ms = outcome.first_response_ms;
        if (fields.status >= 400) outcome.error_type = 'http_error';
      }
      if (event === 'upstream_model') {
        outcome.confirmed_model = fields.model;
        if (outcome.model_transitions?.at(-1) !== fields.model) {
          outcome.model_transitions ??= [];
          if (outcome.model_transitions.length < 16) outcome.model_transitions.push(fields.model);
          else outcome.model_transitions_truncated = true;
        }
      }
      if (event === 'upstream_usage') outcome.usage = fields.usage;
      if (['upstream_error', 'request_error'].includes(event)) {
        outcome.error_type = fields.error_type ?? 'request_error';
        if (fields.status) outcome.http_status = fields.status;
      }
      if (['request_complete', 'request_error', 'request_cancelled'].includes(event)) finished = true;
      if (finished) {
        fields.total_latency_ms = Math.round((performance.now() - startedAt) * 100) / 100;
        fields.completion_confirmed = outcome.completion_confirmed;
      }
      // Optional observability must never delay or fail an inference request.
      try { Promise.resolve(onStatus({ ...context, event, ...fields })).catch(() => {}); } catch {}
      if (finished && onRecord) {
        const completed = { ...context, ...outcome, event: 'outcome',
          status: outcome.error_type ? 'error' : event === 'request_cancelled' ? 'cancelled' : 'completed',
          usage_complete: outcome.completion_confirmed === true && Number.isSafeInteger(outcome.usage?.input_tokens)
            && outcome.usage.input_tokens >= 0 && Number.isSafeInteger(outcome.usage.output_tokens) && outcome.usage.output_tokens >= 0,
          total_latency_ms: Math.round((performance.now() - startedAt) * 100) / 100 };
        const estimate = estimateOutcomeSavings(completed);
        record({ ...completed, pricing_eligible: estimate.priced,
          ...(!estimate.priced ? { unpriced_reason: estimate.unpriced_reason } : {}) });
      }
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
        const shape = validateRequestShape(parsed);
        if (!shape.valid) {
          log({ event: 'invalid_request_shape', model_type: typeof parsed?.model, messages_type: Array.isArray(parsed?.messages) ? 'array' : typeof parsed?.messages,
            messages: Array.isArray(parsed?.messages) ? parsed.messages.slice(0, 10).map(m => ({
              role: ['user', 'assistant', 'system'].includes(m?.role) ? m.role : typeof m?.role,
              content_type: Array.isArray(m?.content) ? 'array' : typeof m?.content,
              blocks: Array.isArray(m?.content) ? m.content.slice(0, 10).map(b => ({ value_type: b === null ? 'null' : typeof b, type_type: typeof b?.type })) : undefined,
            })) : undefined });
          return rejectRequest(400, shape.error);
        }
        if (inference) {
          const decision = await router.route(parsed, {
            requestId: context.request_id,
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
          // Prompt excerpts go only to this explicit opt-in sink, never to
          // ordinary diagnostics or the status snapshot. Optional logging
          // cannot delay or fail forwarding, including an async sink failure.
          if (onDecision || onRecord) {
            try {
              const includePrompts = config.sessionLogMode !== 'metadata';
              const chars = [...(includePrompts && (!context.request_class || context.request_class === 'main') ? promptExcerpt(parsed, 501) : '')];
              const row = {
                schema_version: 2, event: 'decision', timestamp: new Date().toISOString(), ...context,
                ...(includePrompts ? { prompt_excerpt: chars.slice(0, 500).join(''), prompt_truncated: chars.length > 500 } : {}),
                requested_model: parsed.model, selected_model: decision.model, decision_latency_ms: decision.latency_ms,
                routing_latency_ms: decision.latency_ms, evaluation_latency_ms: decision.evaluation_latency_ms,
                source: decision.source, reason: decision.reason, evaluator: decision.evaluator,
                classified_tier: decision.classified_tier, classifier_error: decision.classifier_error,
                classifier_status: decision.classifier_status, compatibility_reason: decision.compatibility_reason,
                continuity_state: decision.continuity_state, context_check: decision.context_check, counted_input_tokens: decision.counted_input_tokens,
              };
              record(row);
              if (onDecision) Promise.resolve(onDecision(row)).catch(() => {});
            } catch {}
          }
          log({ event: 'route', requested_model: parsed.model, ...decision, request_adjustments: prepared.adjustments });
          const { model, source, evaluator, reason, latency_ms, evaluation_latency_ms, classifier_error, classifier_status, classified_tier, context_check, counted_input_tokens, continuity_state, compatibility_reason } = decision;
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
            evaluation_latency_ms, routing_latency_ms: latency_ms, classified_tier, context_check, counted_input_tokens, continuity_state, compatibility_reason, pricing_context });
        }
      }
      if (controller.signal.aborted) { status('request_cancelled'); return; }
      const target = new URL(`${config.upstream}${url.pathname}${url.search}`);
      forwardedAt = performance.now();
      const execution = await forward(target, req, res, body, config, AbortSignal.any([controller.signal, AbortSignal.timeout(config.upstreamTimeoutMs)]), log, status);
      outcome.completion_confirmed = Boolean(execution) && !controller.signal.aborted;
      if (context) router.complete?.(context.request_id, controller.signal.aborted ? undefined : execution);
      status('request_complete');
    } catch (error) {
      if (controller.signal.aborted) { status('request_cancelled'); return; }
      // Error strings from providers can contain request data. Keep logs local
      // and metadata-only, including on error paths.
      log({ event: 'proxy_error', status: error.status ?? 502 });
      rejectRequest(error.status ?? 502, error.status === 413 ? 'Request body too large' : 'Router could not complete the upstream request');
    } finally {
      // Release staged attempts on every early return, abort and error. This
      // is a no-op after a successfully committed execution.
      if (context) router.complete?.(context.request_id);
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
