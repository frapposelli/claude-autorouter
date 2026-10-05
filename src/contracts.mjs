// Development-time contracts only; JSDoc is erased by JavaScript engines.
/** @typedef {'haiku'|'sonnet'|'opus'} Tier */
/** @typedef {'jev'|'ollama'} Evaluator */
/** @typedef {'compatible'|'native'|'auto'} ClientProfile */
/** @typedef {'jev'|'ollama'|'cache'|'fallback'|'passthrough'} DecisionSource */
/** @typedef {'timeout'|'http_error'|'invalid_response'|'network_error'|'capacity_exhausted'} ClassifierError */
/** @typedef {'request_start'|'route'|'upstream_response'|'upstream_model'|'upstream_usage'|'upstream_error'|'request_complete'|'request_error'|'request_cancelled'} LifecycleName */
/** @typedef {'selected'|'confirmed'|'unknown'|'capacity_exhausted'} ContinuityState */
/**
 * @typedef {object} RouterConfig
 * @property {Evaluator} evaluator
 * @property {'api-key'|'subscription'} authMode
 * @property {ClientProfile} clientProfile
 * @property {string|undefined} sessionLogDir
 * @property {'metadata'|'prompts'} sessionLogMode
 * @property {number|undefined} stopHookBlockCap
 * @property {string|undefined} anthropicKey
 * @property {string|undefined} jevKey
 * @property {string|undefined} localToken
 * @property {string} upstream
 * @property {string} jevEndpoint
 * @property {string} jevModel
 * @property {string} ollamaEndpoint
 * @property {string} ollamaModel
 * @property {number} ollamaTimeoutMs
 * @property {number} ollamaStateChars
 * @property {string} ollamaKeepAlive
 * @property {Record<Tier,string>} models
 * @property {number} port
 * @property {number} jevTimeoutMs
 * @property {number} tokenCountTimeoutMs
 * @property {number} [tokenCountCacheEntries]
 * @property {number} [tokenCountCacheTtlMs]
 * @property {number} minConfidence
 * @property {number} stateChars
 * @property {number} maxBodyBytes
 * @property {number} cacheEntries
 * @property {number} cacheTtlMs
 * @property {number} turnTtlMs
 * @property {number|undefined} [turnEntries]
 * @property {number} upstreamTimeoutMs
 */
/**
 * @typedef {object} ClassifierDecision
 * @property {Tier} tier
 * @property {Tier} [classified_tier]
 * @property {number} [confidence]
 * @property {Evaluator} evaluator
 * @property {DecisionSource} source
 * @property {string} reason
 * @property {ClassifierError} [classifier_error]
 * @property {number} [classifier_status]
 */
/**
 * @typedef {object} RoutingDecision
 * @property {string} model
 * @property {DecisionSource} source
 * @property {string} reason
 * @property {number} latency_ms
 * @property {number} evaluation_latency_ms
 * @property {Tier} [tier]
 * @property {Tier} [classified_tier]
 * @property {number} [confidence]
 * @property {Evaluator} [evaluator]
 * @property {ClassifierError} [classifier_error]
 * @property {number} [classifier_status]
 * @property {string} [compatibility_reason]
 * @property {ContinuityState} [continuity_state]
 * @property {'within_budget'|'over_budget'|'count_unavailable'} [context_check]
 * @property {number} [counted_input_tokens]
 */
/**
 * @typedef {object} TelemetryFields
 * @property {number} [schema_version]
 * @property {string} [timestamp]
 * @property {string} [request_id]
 * @property {string} [session_id]
 * @property {string} [agent_id]
 * @property {string} [prompt_id]
 * @property {string} [request_class]
 * @property {string} [requested_model]
 * @property {string} [selected_model]
 * @property {string} [confirmed_model]
 * @property {string} [model]
 * @property {string} [baseline_model]
 * @property {string} [pricing_version]
 * @property {string} [reason]
 * @property {string} [compatibility_reason]
 * @property {ContinuityState} [continuity_state]
 * @property {DecisionSource} [source]
 * @property {Evaluator} [evaluator]
 * @property {Tier} [tier]
 * @property {Tier} [classified_tier]
 * @property {number} [latency_ms]
 * @property {number} [evaluation_latency_ms]
 * @property {number} [routing_latency_ms]
 * @property {number} [decision_latency_ms]
 * @property {number} [first_response_ms]
 * @property {number} [upstream_latency_ms]
 * @property {number} [total_latency_ms]
 * @property {ClassifierError} [classifier_error]
 * @property {number} [classifier_status]
 * @property {string} [error_type]
 * @property {number} [http_status]
 * @property {number|string} [status]
 * @property {'within_budget'|'over_budget'|'count_unavailable'} [context_check]
 * @property {number} [counted_input_tokens]
 * @property {string[]} [model_transitions]
 * @property {boolean} [model_transitions_truncated]
 * @property {object} [usage]
 * @property {object} [pricing_context]
 * @property {boolean} [usage_complete]
 * @property {boolean} [pricing_eligible]
 * @property {boolean} [completion_confirmed]
 * @property {string} [unpriced_reason]
 * @property {object} [savings]
 * @property {object} [savings_coverage]
 * @property {string} [prompt_excerpt]
 * @property {boolean} [prompt_truncated]
 */
/** @typedef {TelemetryFields & {event:LifecycleName,request_id:string}} LifecycleEvent */
/** @typedef {TelemetryFields & {event:'decision'|'outcome',request_id:string}} SessionRecord */
export {};
