# Native library integration

The native application does not provide a drop-in replacement for an ESM import of `src/router.mjs`. Existing JavaScript embeddings must keep the frozen implementation during migration, use the local HTTP gateway, or move their integration to Rust. No JavaScript compatibility sidecar is included in the native package. Repository tests exercise both interfaces; usage by external ESM consumers has not been established.

Use `autorouter-runtime::server::Gateway` for the complete authenticated HTTP boundary. Construct it with a validated `RouterConfig`, an `Arc<impl HttpTransport>`, and `EventSinks`; call `listen()` and retain the returned handle. `GatewayHandle::close().await` stops admission and waits for shutdown. The CLI additionally owns status/history drains and child-process cleanup; library callers own any sinks or child processes they create.

For a routing-only integration, `autorouter-runtime::router::Router` accepts an injected transport and configuration. `with_clock` supplies a deterministic clock for tests.

`RouterConfig::turn_entries` is the optional native equivalent of the original
embedding configuration's `turnEntries`. `None` retains the 1,000-entry limit;
`Some(limit)` applies that limit to both task records and pending attempts when
the router is constructed. CLI configuration leaves it absent. Capacity rejection
preserves active tasks and their confirmed tool ownership.

The routing interface is:

| Operation | Contract |
| --- | --- |
| `route_exact(document, options, headers, cancellation, search).await` | Returns a `RouteDecision` containing the selected exact model identity and display metadata. It does not forward a provider response or confirm execution. |
| `RouteOptions::request_id` | Set a unique ID when the caller will report the forwarding outcome. Omitting it retains the documented selected, unconfirmed continuity behavior of the original embedding API. |
| `RouteOptions::scope`, `prompt_id`, `request_class` | Preserve caller session/agent/prompt isolation and main versus auxiliary request classification. Do not reuse another caller's scope. |
| `complete_exact(request_id, evidence)` | Submit typed observer evidence only after a successful provider status, clean protocol completion, successful downstream forwarding, and cancellation checks. Returns whether the current attempt was committed. |
| `complete(request_id, &Value::Null)` | Abandon a failed, cancelled, refused, or otherwise unconfirmed attempt. A stale completion cannot replace a newer confirmed attempt. |
| `shutdown()` | Cancels shared classifier work; callers also cancel and join their request tasks. |

Use `JsDocument` as the authoritative request representation and `JsString` for identities. The arena preserves unknown fields, UTF-16 string units, JavaScript number conversion, object order, and deep opaque data. `read_config_document` similarly retains saved custom model identities. Convert a string with `to_well_formed()` only at an explicit display or operating-system boundary. `to_serde_observation_lossy()` is for bounded metadata observation and cannot establish model compatibility, task identity, or forwarded bytes.

The convenience `route()`/`complete()` adapters use `serde_json::Value` and serve scalar-string embeddings and historical fixtures. The product gateway uses the exact APIs. A displayed replacement character cannot stand in for an escaped lone surrogate when deciding which request or model owns a continuation.

An observer seeing a terminal frame is insufficient evidence that the client received the response. The gateway uses one bounded `CompletionRegistry` per connection and the pinned `serve_http1` transport adapter. Body EOF arms a record; the actual response-head submission and a successful downstream writer flush are separate obligations. Errors, disconnects, abandoned bodies, and ambiguous completion fail the obligation. The caller then combines this transport result with clean protocol evidence. A local flush establishes transport acceptance, not an application-level acknowledgement from Claude.

Do not layer another buffered writer around this completion adapter or enable a transport mode whose flush ordering has not been qualified. Custom HTTP or HTTP/2 embeddings must supply equivalent forwarding evidence before using `complete_exact`; calling it at upstream EOF would violate the continuity contract.

`HttpTransport::request` provides the fetch-style evaluator/count/setup boundary. `request_raw` provides the provider-forwarding boundary corresponding to the original Node HTTP client. Custom transports may share their implementation through the default method, but their tests must still retain byte limits, absolute deadlines, disconnect propagation, redirect behavior, and header policy for each consumer. Use local synthetic services to validate integrations; the workspace's ordinary tests do not contact a provider.

Evaluator operations and `Classifier::classify` accept a caller-owned `CancellationToken` and return `EvaluationError::Cancelled` for cancellation. An already-cancelled token prevents evaluator I/O. The Rust API does not carry an arbitrary JavaScript `AbortSignal.reason`, preserve an `Error` object's identity, or expose JavaScript signal identity. Embeddings that need a reason retain it alongside their token. The frozen JavaScript tests still assert the original reason identity; native tests cover the typed outcome and resource behavior, not identity equivalence.

Identical classifier work may have several subscribers. Cancelling one subscriber returns promptly while another live subscriber retains the shared work. Cancelling the last live subscriber waits for that exact worker to release its evaluator request future and response body before returning `Cancelled`; a cancelled, unpolled subscriber does not count as live. A replacement request for the same input owns a different worker and cannot be cancelled or removed by the old one. Shutdown cancellation keeps precedence over a late result, and a shutdown notification alone is not proof of worker cleanup. Callers still await their request tasks after `shutdown()`. Dropping a classification future propagates cancellation but cannot synchronously join the worker; transport background tasks remain the transport owner's responsibility.

An Ollama timeout of zero disables only the evaluator timer. Cancellation and response byte limits remain active. A positive timeout covers metadata, inference headers, and response-body consumption under one budget. The timed contracts exercise production timers with a paused native clock and real synthetic Node timers; they establish deadline and cleanup behavior without claiming equal wall-clock latency.

The local diagnostic library operations, `run_local_diagnostic` and `run_local_diagnostic_with_progress`, also accept a caller-owned `CancellationToken`. Setting the Ollama runtime timeout to zero disables the measured evaluator deadline; the initial preparation call retains its separate 60-second deadline. Cancellation still interrupts either phase and releases its owned request. Cancellation returns `EvaluationError::Cancelled`; JavaScript `AbortSignal` object identity and arbitrary JavaScript error reasons are not part of this Rust API. A runtime timeout produces fallback rows and a failed diagnostic gate, without counting the fallback tier as a successful classifier result.
