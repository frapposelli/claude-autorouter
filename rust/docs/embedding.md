# Native library integration

The native application does not provide a drop-in replacement for an ESM import of `src/router.mjs`. Existing JavaScript embeddings must keep the frozen implementation during migration, use the local HTTP gateway, or move their integration to Rust. No JavaScript compatibility sidecar is included in the native package. Repository tests exercise both interfaces; usage by external ESM consumers has not been established.

Use `autorouter-runtime::server::Gateway` for the complete authenticated HTTP boundary. Construct it with a validated `RouterConfig`, an `Arc<impl HttpTransport>`, and `EventSinks`; call `listen()` and retain the returned handle. `GatewayHandle::close().await` stops admission and waits for shutdown. The CLI additionally owns status/history drains and child-process cleanup; library callers own any sinks or child processes they create.

For a routing-only integration, `autorouter-runtime::router::Router` accepts an injected transport and configuration. `with_clock` supplies a deterministic clock for tests. The routing interface is:

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
