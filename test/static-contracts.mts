import type { RouterConfig, ClassifierDecision, RoutingDecision, LifecycleEvent, SessionRecord, TelemetryFields } from '../src/contracts.mjs';
import { readConfig } from '../src/config.mjs';
import { Router } from '../src/router.mjs';

const config: RouterConfig = readConfig({ AUTOROUTER_EVALUATOR: 'jev' });
const classification: ClassifierDecision = { tier: 'opus', evaluator: 'jev', source: 'jev', reason: 'classified' };
const route: RoutingDecision = { ...classification, model: config.models.opus, latency_ms: 1, evaluation_latency_ms: 1 };
const status: LifecycleEvent = { event: 'route', request_id: 'example', ...route };
const outcome: SessionRecord = { event: 'outcome', request_id: status.request_id, status: 'completed', completion_confirmed: true };
void outcome;
const router = new Router(config);
const classified: ClassifierDecision = await router.classify({ model: config.models.haiku, messages: [] });
const routed: RoutingDecision = await router.route({ model: config.models.haiku, messages: [] });
void [classified, routed];

// These negative fixtures fail the checker if the contract becomes permissive.
// @ts-expect-error Unknown event names cannot be persisted as lifecycle events.
const badEvent: LifecycleEvent = { event: 'answered', request_id: 'example' };
// @ts-expect-error Timing is numeric.
const badTiming: RoutingDecision = { ...route, latency_ms: '1ms' };
// @ts-expect-error Configuration cannot silently acquire an unrecognized setting.
const badConfig: RouterConfig = { ...config, timeout: 1 };
// @ts-expect-error Only supported capability tiers are evaluator results.
const badTier: ClassifierDecision = { ...classification, tier: 'cheap' };
// @ts-expect-error Prompt payloads are absent from the metadata contract.
const badPayload: LifecycleEvent = { ...status, body: { messages: [] } };
// @ts-expect-error Misspelled fields cannot pass a producer check.
const badModel: SessionRecord = { ...outcome, selected_modle: 'opus' };
void [badEvent, badTiming, badConfig, badTier, badPayload, badModel];
