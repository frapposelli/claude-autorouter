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
| `shutdown()` | Requests cancellation of the current shared classifier generation and returns immediately. Later routes and classifications may still be admitted; this operation does not drain owned tasks. |
| `close().await` | Permanently stops routing admission and cancels and drains internally spawned classifier and advisory token-count tasks, including retired tasks. Repeated calls await the same owners. The operation begins when its future is first polled. |

Use `JsDocument` as the authoritative request representation and `JsString` for identities. The arena preserves unknown fields, UTF-16 string units, JavaScript number conversion, object order, and deep opaque data. `read_config_document` similarly retains saved custom model identities. Convert a string with `to_well_formed()` only at an explicit display or operating-system boundary. `to_serde_observation_lossy()` is for bounded metadata observation and cannot establish model compatibility, task identity, or forwarded bytes.

The convenience `route()`/`complete()` adapters use `serde_json::Value` and serve scalar-string embeddings and historical fixtures. The product gateway uses the exact APIs. A displayed replacement character cannot stand in for an escaped lone surrogate when deciding which request or model owns a continuation.

For a per-route equivalent of JavaScript's `countTokens(body, model)` option,
construct a `route_counter::RouteTokenCounter` and call
`route_typed_exact_with_counter(document, options, headers, cancellation, search,
counter).await`. Supplying this counter enables counting even when
`RouteOptions::count_tokens` is false; ordinary small-request, compatibility and
hard-lock guards still decide whether counting is needed. Other route methods
and the product gateway retain the existing HTTP `TokenCounter`.

```rust,ignore
use autorouter_runtime::route_counter::RouteTokenCounter;
let counter = RouteTokenCounter::new(|original, target, cancellation| async move {
    // Read the authoritative request without modifying or projecting it.
    // Use target for the estimate, not original's requested model.
    // Cooperatively cancel any work owned by this future.
    let _ = (original, target, cancellation);
    Some(54_481.0)
});
```

The factory receives the unchanged `Arc<JsDocument>`, an owned target model and
an independent child cancellation token. It returns a `Send` future whose
`Option<f64>` result preserves numeric validation: `None` replaces JavaScript
`undefined`; negative, fractional, NaN, infinite and unsafe-integer values become
unavailable. Only finite nonnegative integers through `9_007_199_254_740_991`
reach routing policy. No JSON serialization converts these inputs first, and no
custom result cache is added. Return `None` for an ordinary counting error;
JavaScript Promise/Error identity is not part of this Rust interface.

Custom counting shares the configured `tokenCountTimeoutMs` deadline, caller
cancellation, speculative admission and terminal drain. A count admitted before
classification is first polled before the classifier and reused when its target
matches. At speculative capacity a required count still runs inline. Closing or
dropping the count cancels its child token and releases its future; it does not
cancel the caller's parent token. Caller-owned route futures still need to be
cancelled or dropped and joined by their owner. The callback must not block or
spawn detached work: cooperative timers cannot preempt blocking code, and the
router cannot join tasks hidden by the host.

Ordinary unwinding factory, poll and destructor panics become an unavailable
count with payload-free results. The explicit observer privacy initialization
described below is required to suppress the host panic hook. Panic-abort,
replaced or aborting hooks and panic payload destructors that panic remain
outside containment. No host hook is silently installed by the library.

The pure `autorouter_core::router::context_size_bytes()` API now returns
`Result<ContextByteCharge, ContextByteChargeUnavailable>`, replacing the native
`usize` return. Deferred schemas can be expanded repeatedly while the original
HTTP body stays small; the resulting context charge can exceed the 32-bit
pointer range. This migration preserves the original JavaScript Number
arithmetic rather than wrapping or saturating an exact metric.

`ContextByteCharge::as_source_number()` exposes the observed `f64` Number.
`exact_safe_bytes()` returns `Some(u64)` only for a finite nonnegative integer
through `9_007_199_254_740_991`; larger Numbers are not exact integer telemetry.
`exceeds(150000)` retains the existing capacity predicate. Multiplication and
addition follow the original tools order, including IEEE-754 rounding; do not
replace them with reassociated or exact-integer sums. `to_json_value()` preserves
the existing numeric JSON shape through `ryu_js` formatting and a JSON Number;
negative zero becomes zero and nonfinite Numbers become null.

An unavailable complete serialized length returns the typed error, with code
`serialized_length_unavailable` and display text `Context byte charge unavailable`.
Handle it explicitly: inability to calculate a charge cannot establish a small
context. The router conservatively retains the capacity check. Development
live/context-probe metadata records null plus `context_guard_error`; it does not
invent `usize::MAX` or a capped byte count. Successful observations keep their
existing number field. No request limit, model eligibility, token threshold,
provider bytes, permission or cancellation behavior changes. The private bounded
rejection control exercises the error path without claiming an enormous request
was allocated. The original JavaScript function can throw while serializing an
unrepresentable body; native callers handle the typed unavailable result instead.

Both diagnostic excerpt functions, `prompt_excerpt_document()` and
`prompt_excerpt()`, return `Result<String, PromptExcerptError>`. Their `usize`
bound must fit JavaScript's safe-integer range through
`9_007_199_254_740_991`. An invalid bound returns the singleton error before
document access or JSON-value serialization; its display text is
`maxChars must be a nonnegative safe integer`. Handle the result, or unwrap a
known valid constant. This replaces the unchecked native `String` return.

Rust's parameter type excludes negative, fractional, nonfinite and string
arguments. Parsed `JsDocument` and `serde_json::Value` inputs cannot represent
live JavaScript property accessors. These are explicit input-domain migrations;
compiler rejection does not execute the original getter or TypeError operation.
Valid zero bounds, Unicode scalar limits, lone-surrogate repair and redaction
retain their behavior. Excerpt collection reads text lazily through its
redaction window, while the required tool-result type scan still checks the
whole candidate turn before selecting it.
The `serde_json::Value` convenience function serializes its complete input into
the exact document representation after validating the limit. The lazy lookup
guarantee applies to excerpt collection from that parsed document; it does not
exclude the facade's conversion work on older or opaque input fields. Use the
document API when the request is already parsed.

First-party classification, policy and gateway producers use the closed contracts in `autorouter-core::contracts`. `ClassifierDecision::source` and `classifier_error` are enums; the runtime evaluator module re-exports `ClassifierDecision` and `Tier` for existing import paths. Native consumers constructing these fields must use `DecisionSource` and `ClassifierError` variants. `route_typed_exact()` returns `TypedRouteDecision` with typed display metadata and a separate exact `JsString` model. The existing `route()` and `route_exact()` operations retain their JSON observation facades.

`GatewayRouter::route_contract_exact()` defaults to `GatewayDecision::Legacy` for existing custom JSON adapters; the production router overrides it with `GatewayDecision::Typed`. A request using custom raw decision metadata keeps legacy observation bookkeeping throughout its lifecycle. Typed requests use `RequestContext`, `TelemetryFields`, closed lifecycle names and exact model overrides. Both paths retain the same durable normalization boundary, metadata-mode prompt omissions and JavaScript number projection. The native compile controls reject unknown fields/events, unsupported enum values and string timings before execution; arbitrary provider envelopes remain `JsDocument` values at explicit boundaries.

An observer seeing a terminal frame is insufficient evidence that the client received the response. The gateway uses one bounded `CompletionRegistry` per connection and the pinned `serve_http1` transport adapter. Body EOF arms a record; the actual response-head submission and a successful downstream writer flush are separate obligations. Errors, disconnects, abandoned bodies, and ambiguous completion fail the obligation. The caller then combines this transport result with clean protocol evidence. A local flush establishes transport acceptance, not an application-level acknowledgement from Claude.

Do not layer another buffered writer around this completion adapter or enable a transport mode whose flush ordering has not been qualified. Custom HTTP or HTTP/2 embeddings must supply equivalent forwarding evidence before using `complete_exact`; calling it at upstream EOF would violate the continuity contract.

`HttpTransport::request` provides the fetch-style evaluator/count/setup boundary. `request_raw` provides the provider-forwarding boundary corresponding to the original Node HTTP client. Custom transports may share their implementation through the default method, but their tests must still retain byte limits, absolute deadlines, disconnect propagation, redirect behavior, and header policy for each consumer. Use local synthetic services to validate integrations; the workspace's ordinary tests do not contact a provider.

Evaluator operations and `Classifier::classify` accept a caller-owned `CancellationToken` and return `EvaluationError::Cancelled` for cancellation. An already-cancelled token prevents evaluator I/O. The Rust API does not carry an arbitrary JavaScript `AbortSignal.reason`, preserve an `Error` object's identity, or expose JavaScript signal identity. Embeddings that need a reason retain it alongside their token. The frozen JavaScript tests still assert the original reason identity; native tests cover the typed outcome and resource behavior, not identity equivalence.

Identical classifier work may have several subscribers. Cancelling one subscriber returns promptly while another live subscriber retains the shared work. Cancelling the last live subscriber waits for that exact worker to release its evaluator request future and response body before returning `Cancelled`; a cancelled, unpolled subscriber does not count as live. A replacement request for the same input owns a different worker and cannot be cancelled or removed by the old one. Shutdown cancellation keeps precedence over a late result, and a shutdown notification alone is not proof of worker cleanup. Callers still await their request tasks after `shutdown()`. Dropping a classification future propagates cancellation but cannot synchronously join the worker; transport background tasks remain the transport owner's responsibility.

`Classifier::close().await` provides terminal admission closure and an asynchronous drain. Dropping a close future leaves worker ownership intact; another close future can await the same drain. Dropping a classification or routing future remains nonblocking and requests cancellation. Retired workers count toward the drain until their owned futures have actually been destroyed. Reusable `shutdown()` remains a cancellation request and does not become terminal.

`Router::close().await` drains tasks spawned internally by the router. Caller-owned route futures remain the caller's responsibility; embeddings must drop or cancel and join those futures as well. Owner closure interrupts required inline token-count work when its route future is polled. The gateway owns and joins its connection/request tasks in addition to awaiting the router drain. If a caller abandons the consuming `GatewayHandle::close()` future after polling it, the listener continues to own shutdown and cleanup; that caller has not observed completion.

Custom `GatewayRouter` adapters must implement the asynchronous `close()` method to stop their own admission and await owned work. Wrapping synchronous cancellation alone is sufficient only for adapters with no asynchronous owned work. The runtime router, live-tool router and context-probe router delegate to their actual asynchronous drain. Custom transport background tasks remain the transport owner's responsibility.

The classifier's pending-work limit bounds admitted, reserved and retiring worker futures until destruction. Joining an admitted request with the same key remains possible at this limit when a subscriber slot is available. Speculative token counts use the configured classifier subscriber limit as their task bound and retain slots through abort and destruction. At speculative capacity, a route still performs a required count inline; omitting a speculative task does not manufacture `count_unavailable` or change eligibility. A count that already started and returned no result is not retried.

`Classifier::with_limits()` returns a `Result`. Native callers may lower the
256 pending-worker and 1,024 subscriber ceilings; either field above its ceiling
returns `Invalid classifier limits.` before worker state is created. The fields
are independent. Zero is valid and closes that capacity without issuing an
evaluator request. `Classifier::new()` retains the fixed defaults.

A successful cached decision consumes no work or subscriber slot and remains
available when those capacities are full. An already-cancelled token or terminal
close still rejects classification before the cache lookup. Cache availability
does not establish successful provider execution or confirm task continuity.

An Ollama timeout of zero disables only the evaluator timer. Cancellation and response byte limits remain active. A positive timeout covers metadata, inference headers, and response-body consumption under one budget. The timed contracts exercise production timers with a paused native clock and real synthetic Node timers; they establish deadline and cleanup behavior without claiming equal wall-clock latency.

`inspect_ollama` rejects an invalid local endpoint before transport access and returns a `SetupError` with code `INVALID_CONFIGURATION`. The original JavaScript inspection throws an uncoded `Error` at that boundary; migrating library callers should handle the native code explicitly. `setup_ollama` downloads only when `SetupOptions::pull` authorizes it and the model is missing. An installed model skips the pull, and startup warmup retains its separate deadline and configured keep-alive value.

The local diagnostic library operations, `run_local_diagnostic` and `run_local_diagnostic_with_progress`, also accept a caller-owned `CancellationToken`. Setting the Ollama runtime timeout to zero disables the measured evaluator deadline; the initial preparation call retains its separate 60-second deadline. Cancellation still interrupts either phase and releases its owned request. Cancellation returns `EvaluationError::Cancelled`; JavaScript `AbortSignal` object identity and arbitrary JavaScript error reasons are not part of this Rust API. A runtime timeout produces fallback rows and a failed diagnostic gate, without counting the fallback tier as a successful classifier result.

The Rust local diagnostic accepts a typed `RouterConfig`: its `u64` runtime deadline cannot contain JavaScript `NaN` or infinity. The configuration reader rejects the text `NaN` before a diagnostic can run. This differs from passing a nonfinite field to the JavaScript diagnostic and receiving its `invalid_configuration` report.

The private native `doctor` dispatcher has no JavaScript `diagnosticRunner`
override. It emits the complete actual diagnostic report and maps its `passed`
result to the CLI exit status. Rust integrations use the public diagnostic
operation with an injected `HttpTransport`. This replaces the original
JavaScript wrapper's arbitrary injected-report passthrough; the original
four-field test object is retained in the [per-assertion review](../parity/assertion-mappings/doctor-dispatch-contracts.json).
Cancellation remains an error, with no final configuration-error report.
Native tokens and OS signals carry cancellation state; retain an application
reason alongside the token if needed, since they cannot preserve a caller's
JavaScript `Error` identity.

Before starting workers, an embedding host can explicitly call `autorouter_runtime::callback_privacy::initialize_observer_privacy()` after configuring its own panic hook. This installs one delegating process hook: ordinary observer panics are caught without printing their private payload or backtrace, while unrelated panics use the previous host hook. The CLI initializes this guard automatically. Initialization must precede concurrent work because stable Rust cannot atomically replace a hook; replacing the hook afterward revokes the guard. The library does not silently replace a host hook when emitting an event. Thread-local scopes cover each synchronous callback or future poll and are restored after nesting/unwinding; they never span an await. Panic-abort behavior, a subsequently replaced or aborting hook, and panic payloads with panicking destructors remain outside containment.

`run_local_diagnostic_with_progress` calls a synchronous `FnMut(&Value) -> Result<(), ()>` in event order. Returned errors and ordinary unwinding callback panics do not prevent the remaining diagnostic work. Without the explicit privacy initialization, the host's existing hook may still print a caught panic. Callbacks returning futures use the owned adapter described below; a synchronous `Err(())` alone does not establish asynchronous rejection behavior.

For callbacks that return futures, create an `ObserverTasks` owner in the active
Tokio runtime and use `owner.handle().event_sink(factory)` or
`owner.handle().record_sink(factory)`. An admitted factory starts synchronously;
its `Future<Output = Result<(), ObserverRejected>>` runs independently of
forwarding. Retain the owner and explicitly call `owner.close().await` during
shutdown. For diagnostic progress, forward each borrowed event with
`sink(event.clone())` and return `Ok(())` from the synchronous bridge. The
diagnostic regression polls actual returned futures to `ObserverRejected`,
preserves the completed report, releases every observer and joins its owner.
This is an explicit Rust API migration: futures are owned, `Send` and `'static`;
JavaScript Promise and Error object identities are not preserved. Close cancels
accepted observer work and joins its one worker; it does
not promise to finish arbitrary pending observers or make them durable. Dropping
the owner requests cancellation without observing joined cleanup.

`ResponseObserver::new` accepts a synchronous `FnMut(Observation)` sink.
To migrate JavaScript model, execution or completion callbacks that return
futures, dispatch the corresponding typed observation and use
`ObserverHandle::submit` with its exact serialized callback-metadata byte count.
Keep and explicitly close its `ObserverTasks` owner. The bridge returns unit;
forwarding and clean body EOF do not await its returned futures. Rust owned
`Send` + `'static` futures and `ObserverRejected` replace Promise settlement and
private Error details; JavaScript callback aliases, Promise/Error identities and
microtask scheduling are not part of this adapter contract.

Default limits are 256 pending factories/futures, 64 KiB per serialized payload
and 1 MiB of charged payload/inline factory/future storage. Callers may lower
these bounds. Exact payload counting preserves UTF-16 event identities and uses
bounded iterative traversal. Charges exclude hidden user allocations and work
spawned by the callback; factories, polls and destructors must not block. Closed,
oversized and saturated admissions decline before invoking a factory. Rejected
futures and ordinary factory/poll/drop panics cannot fail inference. A pending
timeout is optional and disabled by default, so never-ready callbacks remain
bounded until cancellation or close. Each observer can have its own cancellation
token. Snapshots contain numeric counters and lifecycle flags, never payloads.

`SessionLog::close().await` stops record admission and waits for the writer to finish and close its files. Healthy writers drain accepted records; write failures disable logging. Rust close futures begin this operation when first polled; constructing an unpolled future does not stop admission. Separate close futures wait for the same writer-owned worker, which closes the sink once. Cancelling a close waiter leaves that worker owned by the logger so another close call can await it. This is an explicit migration from JavaScript's shared close Promise; Rust futures do not preserve its object identity.

Session logging accepts `serde_json::Value`, which has no property getters, `undefined`, or nonfinite numbers. Native tests retain complete durable metadata and privacy checks for representable values; omitted JavaScript getters and nonfinite-input projections are not equivalent input operations. `SessionLogOptions::warn` is a synchronous callback. Ordinary unwinding warning panics are contained, with hook output suppressed only after the explicit privacy initialization. To adapt a warning callback that returns a future, copy the warning into an owned string and call `handle.submit(warning.len(), move || factory(warning))` from the synchronous bridge. Keep the `ObserverTasks` owner and explicitly close it at shutdown. The returned `Send` + `'static` future is polled independently; a payload-free `ObserverRejected` replaces JavaScript rejection details. Returning unit from the warning bridge alone does not prove asynchronous rejection handling, and Promise/Error object identities are not preserved.

`read_session_history()` validates every `HistoryLimits` field before starting its filesystem worker, including fields constructed directly by Rust callers. Zero values and values above the original reader ceilings return `Invalid session-history read limit.` before ID or path errors. Defaults and smaller positive limits retain their existing behavior. The pure parser APIs accept caller-owned data separately; they do not perform filesystem admission.

`remove_stale_status_directories` uses the Tokio filesystem rather than JavaScript's injected `io` object. An unavailable directory returns zero; failed metadata reads skip that entry without exposing its path or error. Tests exercise real owned permission and disappearance faults. The injected filesystem-provider and JavaScript Error object identities are not part of the Rust API.

`CleanupOptions::alive` receives a `u64` so it can observe every positive JavaScript safe-integer PID through `9_007_199_254_740_991`. Embeddings with an explicitly typed `u32` callback must update that argument type. The default callback checks the positive signed operating-system PID range before querying liveness; larger values return false without narrowing, wrapping or signalling a process group.

`SavingsTracker::update(&Value)` accepts owned JSON data, without executable
getters or nonfinite scalar numbers. JavaScript own properties with `undefined`
map to absent optional JSON fields. When starting from an authoritative
`JsDocument`, use its existing normalization projection so a nonfinite counter
remains invalid; do not serialize Infinity into null and call that the original
input. Host-object conversion happens outside the tracker. `pricing_facts()`
returns a fresh owned value that callers can change locally; those changes
cannot alter the private constant rates, later facts or estimates. This replaces
the JavaScript frozen-object write errors with detached-value ownership. The
[per-assertion review](../parity/assertion-mappings/savings-ingress-contracts.json)
records these distinctions and actual consumer controls.

Setup's `ConfigContext` receives `Environment`, a map of `OsString` keys and
values. `environment_json()` converts present entries to strings and preserves
absence. Raw JSON null, boolean or optional values cannot be inserted into that
map; caller code must supply real OS strings or omit the key. The separate
`read_config(&Value, ...)` JSON interface applies field-specific validation.
URL fields first use JavaScript primitive and array string conversion, then
the existing URL safety checks. `read_config_document()` reads those fields
from the original `JsDocument`, before its lossy metadata projection can turn
nonfinite numbers into null or replace lone UTF-16 surrogates. Inactive
evaluator URLs remain masked, and URL conversion preserves validation order.
This URL behavior does not qualify raw credential values or other malformed
configuration fields. [Setup migration evidence](../parity/assertion-mappings/onboarding-environment-contracts.json)
keeps these input domains separate from historical executable CLI checks.

`policy::load_policy()` and `load_policy_at()` return `PolicyError` on failure.
Its `code()` is `AUTOROUTER_CONFIG_ERROR`; `message()` and `Display` preserve the
safe policy diagnostic. `Debug` includes that code and message, with no underlying
I/O cause, path or rejected policy data. Callers previously matching a `String`
can use `message()` or `to_string()`. Rust typed errors replace JavaScript Error
objects; they preserve the category value, without JavaScript object identity.
`user_config::get_config_path()`, `load_user_config()` and `load_with_policy()`
return `UserConfigError` with the same code, a message-only `Display`, a
code/message-only `Debug`, and no `Error::source()`. The path/JSON/filesystem
checks and built-in Keychain/policy diagnostics retain their previous messages.
`From<UserConfigError> for String` keeps `?` at higher-level String-error CLI
boundaries compatible; direct Result returns and mixed-error branches must use
`.map_err(String::from)`. Callers inspecting a failure use `code()` and
`message()` rather than String methods. There is no public String-to-error
constructor. Save, validation and secret-removal APIs retain their existing
String errors in this narrow migration.

An injected `Keychain` still returns arbitrary String messages. Configuration
loading preserves those messages verbatim while assigning the native
configuration category; it does not sanitize adapter content or preserve a
foreign JavaScript error's identity or custom code. Trusted adapters must avoid
including secrets in errors. No nested cause or JavaScript stack/object identity
is retained. `load_with_policy()` remains the explicit injection boundary.
Product commands still use the fixed, root-owned system-policy location;
`load_policy_at()` is an explicit embedding/test boundary, with no environment
override added by this migration.
