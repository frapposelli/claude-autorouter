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

Configuration model sidecars preserve execution identity. Effective configuration
JSON-text serialization has its own
[82-case parser and serialization checkpoint](../parity/measurements/native-config-owned-values-summary.json),
which fixes the historically reproduced selector, timeout, keep-alive and UTF-16
output differences for that corpus. Typed display projections remain bounded;
`serde_json::Value` is not a lossless observer for unpaired strings or deep data.
Original nonfinite numbers serialize as null, matching JavaScript serialization,
while validation and request construction retain their effective original kind.

`AUTOROUTER_MIN_CONFIDENCE` is validated by `read_config()` and
`read_config_document()` as a finite number from zero through one. It applies to
Jev; an inactive Ollama configuration retains the default unless full validation
is requested. Ordinary decimal/type/range and conversion-error ordering remain
part of this parsed-setting contract.

The [individual confidence-setting review](../parity/measurements/native-min-confidence-configuration-key-summary.json)
records two native embedding boundaries. JSON-backed input cannot admit NaN;
invalid JSON rejection and a null projection do not execute the numeric guard
with NaN. `RouterConfig::min_confidence` is a public `f64` and can represent NaN,
infinity or an invalid range after direct mutation. Such edits bypass the reader;
this review gives them no parsed-setting validation guarantee. Parse settings
through the configuration reader before using them.

For rejected nonnumeric JSON values, the native conversion uses an iterative
walk. The reviewed migration permits host resource-exhaustion diagnostics to
differ from recursive JavaScript array conversion. It does not reproduce a
V8-specific stack threshold or promise equal allocation-failure behavior.
Ordinary conversion errors still precede numeric type errors where the source
requires that order, opaque object properties stay opaque, and arrays/objects
remain invalid numeric settings. No new supported-input depth ceiling is added.
The actual depth-64 and depth-256 controls are ordinary bounded observations,
not an execution of extreme-depth stack exhaustion. JavaScript getters, custom
prototypes, error identities and mutable object aliases remain outside this
JSON-backed input interface.

With the supported JSON features, native request representations cannot contain
NaN. Literal `NaN` is invalid JSON and is rejected during document admission;
that rejection is not execution of an Auto compatibility guard with a NaN
argument. Valid JSON numeric
overflow can retain Infinity as an original Number, which the output-cap guard
rejects before any projection to null. The
[reviewed Auto output-limit migration](../parity/measurements/native-callback-auto-output-summary.json)
covers this specific difference from the ESM object API, with separate actual
Infinity evidence. Callers must handle unrepresentable values when migrating;
this review does not accept arbitrary JavaScript getter, prototype or object
identity differences. An API that later admits NaN needs its own guard regression.

`JsDocument::sha256_with_fragments(before, after)` hashes the document's exact
canonical JSON between the supplied UTF-8 fragments and returns the 32-byte
SHA-256 digest. Fragments are written verbatim, with no added separators or
escaping. The classifier uses this to retain its existing cache-key preimage
without allocating the complete serialized request. The sink uses a fixed
4 KiB batch; the existing iterative traversal still allocates for pending nodes.

The convenience `route()`/`complete()` adapters use `serde_json::Value` and serve scalar-string embeddings and historical fixtures. The product gateway uses the exact APIs. A displayed replacement character cannot stand in for an escaped lone surrogate when deciding which request or model owns a continuation.

Native request preparation takes a borrowed `serde_json::Value` or an
authoritative `JsDocument` and returns an owned request. Use
`prepare_request_document_exact` with a `JsString` model when preserving UTF-16
identities and opaque request data. Only the selected model and a
catalog-approved thinking setting change; all other fields and the input
remain intact. These operations do not select a route or bypass compatibility
checks.

These JSON interfaces have no JavaScript `undefined` node. A JavaScript own
`thinking: undefined` property maps to an absent optional field, matching its
JSON wire representation; it does not map to `null`. Returned Rust values and
documents do not preserve JavaScript shallow object aliases. Their subtrees
retain complete values, while subsequent supported mutations affect only the
owned document being changed. Embeddings relying on shared JavaScript
`thinking`, signed-history, safeguards or other object identity must retain
that state separately. Document node IDs are meaningful only within their
own arena and are not cross-document object identities. This migration does
not permit dropping unfamiliar extensions, changing signed content or
inferring model compatibility from a display projection.

Direct `TurnState::select()` accepts every JSON value as pin metadata and copies
its enumerable properties using JavaScript object-spread semantics. Strings
spread by UTF-16 code unit. `get_metadata_exact()` returns the committed
`JsDocument`, preserving even surrogate units generated by spreading a valid
supplementary character. It applies the same expiry and ambiguous-alias checks
as `get_exact()`. The `get()` and `Pin::value` observations repair those units
to U+FFFD; they preserve opaque object members without a new depth limit.

Construct native pins with `Pin::new()` and set their public exact identity and
tool fields before passing ownership to `select_exact()`. Those fields control
native selection retention; display metadata cannot replace them. The Value
adapter preserves the original raw `toolModels.length` truthiness for selected
pins, including unusual JSON metadata. Neither selection path confirms execution.
`Pin::is_confirmed()` reports private state set by a validated completion;
editing display `confirmed` does not establish it. Failed or superseded attempts
preserve the previous exact metadata, confirmation and tool owner.

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

The three consuming functions `read_bounded_document`,
`read_response_document` and `read_bounded_json` return `Result<_, ReadError>` and consume an owned native
`Body`. Match `InvalidResponse`, `Oversized` or `InvalidJson` for invalid response
data, and `Cancelled` for caller cancellation. These fixed unit variants carry
no response text, arbitrary abort reason or JavaScript `Error` identity; their
`Debug` output is the variant name, not `classifier_invalid_response`. The
evaluator adapter maps the invalid-response variants to its existing public
classifier category. The direct reader does not expose a JavaScript
`Response`, `.body.locked` object or asynchronous `.cancel()` callback.

Supply a borrowed `CancellationToken`; cancellation does not consume the caller's
token or preserve a caller-supplied error object. Reader work begins when the
Rust future is polled. Pre-cancellation rejects without polling the body, while
in-flight cancellation interrupts a yielding pending read. Success and error
paths release the owned body before returning; the finite regression bodies
verify exactly one destruction and no retained body-state references. This is
native ownership evidence, not execution of JavaScript's unresolved cancel
Promise. No detached cleanup task is started by the reader.

An empty native body represents no parseable response bytes and returns
`InvalidJson`. It is not a distinct JavaScript `Response(null)` absent-body
object. Valid streamed bytes remain authoritative over missing or inaccurate
length hints; a valid oversized hint can reject before polling. Invalid limits
are checked before cancellation, and valid pre-cancellation precedes header
rejection. Synchronous blocking destructors, non-yielding injected code,
arbitrary JavaScript getters/prototypes and general transport shutdown remain
outside these finite asynchronous ownership controls.

This consuming-reader guarantee does not apply to the incremental
`DecodedResponseStream`. Its `read(&mut self, ...)` borrows an existing stream;
returning from or cancelling one read future does not consume that stream. The
stream keeps its owned body state until the stream itself is dropped.

For Jev body failures, `Classifier::classify` preserves the fallback category and
keeps its deadline active after response headers. Oversized, timed-out and
non-success responses release their owned body before the worker publishes its
result; failures do not populate the successful-result cache. Rust does not
expose a JavaScript `ReadableStream.cancel()` callback or await its potentially
never-settling Promise. Owned body/future destruction is the native replacement
for that observation. A normal completed `Classifier::close().await` drains its
tracked workers, not arbitrary background work owned by a custom transport.
These finite yielding-body controls do not bound blocking destructors, prove
TCP/TLS teardown or change the other cancellation and configuration API limits.

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

The native history metadata APIs retain exact byte lengths. `parse_session()`
and `parse_session_with_locale()` take caller-supplied `file_bytes: u64`;
`read_session_history()` obtains that value from native filesystem metadata.
Their returned `summary.coverage.file_bytes` values, including summaries in a
list, retain the exact unsigned integer. This is an explicit typed API
difference from JavaScript's ordinary `Stats.size` and `readSessionHistory()`,
which expose a binary64 Number. Exact native metadata above
`9_007_199_254_740_991` must not be described as the same JavaScript numeric
operation. A caller-supplied `u64::MAX` control does not establish that an
operating system can report a regular file of that length.

The `sessions list/show --json` CLI instead projects only the known
`summary.coverage.file_bytes` and `sessions[i].coverage.file_bytes` fields to
binary64 when they exceed that safe-integer bound, before rendering them with
the Source number formatter. This preserves the JavaScript outward Number
value and shortest decimal spelling. Even an exactly representable binary64
integer can have different shortest decimal text: native metadata
`4708356024711512064` renders as `4708356024711512000` at that CLI boundary.
Embeddings retaining the native Value can continue using the exact `u64`.
Embeddings intentionally producing Source-shaped JSON must make this same
field-specific projection; generic native JSON writers preserve integer
variants and do not silently convert every `u64`.

The projection does not change byte admission, read capacities, truncation,
limits, log/config contents or native metadata. It does not reinterpret other
numeric report fields, rewrite durable records or establish general CLI byte,
key-order or Unicode equivalence. Synthetic stat seams and caller-provided
metadata controls establish their declared API/producer behavior, not real
huge-file, filesystem, memory, performance or platform qualification.

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

The historical raw-document comparison reproduced invalid-number defaults,
changed first-error ordering, deep keep-alive rejection and scalar output
substitutions. The corrected shared parser reads original kinds directly, and
the separate exact JSON-text experiment matches all 82 declared inputs against
the frozen JavaScript implementation. This is parser/serialization evidence;
the historical Value-observer result remains separate and failed.

`RouterConfig::source_values` owns compact arena subtrees for accepted keep-alive
values and exact session-log paths. The public fields remain bounded typed
projections. `ollama_keep_alive_json()` serializes the effective complete value;
`ollama_keep_alive_is_string()` and `ollama_keep_alive_is_nullish()` inspect its
original kind. In particular, a nonfinite number can serialize as null without
becoming a nullish value for warmup fallback. Use `replace_ollama_keep_alive()`,
`replace_ollama_keep_alive_document()` or `clear_ollama_keep_alive()` for explicit
replacement. Consumed deep Value inputs are dismantled iteratively. A retained
document contains only the selected subtree and remains valid after its source
is dropped. Direct public projection changes take effect when they differ from
the captured projection; use replacement methods to express changes to an
identical lossy projection. `session_log_dir_exact()` and
`replace_session_log_dir()` provide the equivalent path boundary.

`replace_model()` and `replace_jev_model()` clear the corresponding explicit
exact-string override. Existing public `exact_models` and `exact_jev_model`
overrides retain authority until cleared, including after direct typed-field
edits. JSON-text serialization preserves those overrides and exact paths.
`serde_json::to_value()` can reject unpaired UTF-16 or deep data; it must not be
used as a lossless configuration observer. The unchanged older probe now exits
at the first unpaired-string serialization failure after 63 completed rows;
this is a native API migration, not a passing rerun of the old Value experiment.

Parsed keep-alive arrays use iterative primitive conversion and retain their
complete original JSON value. This is a resource-behavior migration from the
recursive JavaScript reader: a valid array can remain usable after JavaScript
exhausts its host stack. The [parsed-depth comparison](../parity/measurements/native-ollama-configuration-keys-summary.json)
records matching complete output at 63, 64, 80 and 2,048 array layers on pinned
Node 22.14.0. At 20,000 layers, JavaScript parses the input but `readConfig`
raises `RangeError`; the native reader retains and serializes the exact value
after dropping the source document. This does not reproduce a V8-specific
recursion threshold or promise equal allocation-failure diagnostics. Ordinary
grammar, primitive conversion errors, validation order and input bounds are
unchanged; no supported-input depth ceiling is added. The legacy Value adapters
can still reject values they cannot represent. This comparison makes no
evaluator/provider acceptance claim for nested array settings.

Native JSON configuration APIs convert URL arrays iteratively after the parser
admits the document. This permits a directly supplied array to normalize to a
valid endpoint when Source returns a URL validation error during its host
conversion. The accepted-input extension for `AUTOROUTER_UPSTREAM_URL`,
`AUTOROUTER_JEV_URL` and `AUTOROUTER_OLLAMA_URL` is separate from keep-alive
ownership and error-object identity. In the declared Node 22.14.0 controls, all
three keys accepted 63, 64, 80 and 2,048 singleton-array layers; at 20,000 layers
Source returned the existing safe URL error before config serialization. The
inner constructor exception was caught, so these observations do not expose a
RangeError or establish a fixed stack threshold. Native acceptance at 20,000
layers must be recorded separately from the Source failures.

Ordinary OS strings, nullish defaults, invalid-object conversion, validation
order, inactive-provider masking and every endpoint safety/account guard remain
unchanged. This extension changes no input bounds, adds no depth cap and makes
no promise of arbitrary-depth or resource acceptance.

Classifier snapshots preserve effective keep-alive values for cache identity
and remove captured session-log paths when clearing that unrelated field.
`build_evaluator_request_document()` accepts serialized state and returns an
owned request arena; runtime inference uses that path. `build_ollama_request()`
and `build_jev_request()` are checked `Result<Value, EvaluationError>` adapters
and refuse data their Value boundary cannot represent. `evaluate_answer()`
imports an existing Value state iteratively before serialization.
`JsDocument::from_serde_value()` is public and imports a borrowed tree without
recursive cloning; callers still own the input Value and its destruction.

The Ollama routing tool exposes `run_tests_document()` for exact report output.
Its original `run_tests()` is a checked Value adapter. The ordinary CLI still
uses the existing private report writer; its environment inputs are strings.
Configuration reports restore only their six selected nonsecret effective
fields rather than serializing the complete credential-bearing configuration.
These synthetic ownership/request/report regressions do not qualify real
provider handling, OS path materialization or numerical performance.

`RouterConfig::source_credentials` owns compact source subtrees for only
`anthropicKey`, `jevKey` and `localToken`. The public `Option<String>` fields
retain their typed transport/display projections. Serialization uses captured
source while that projection is unchanged, and emits each credential once.
Use `replace_credential(Credential, Option<String>)` to replace or remove both
representations. `replace_credential_value()` distinguishes `None` (remove)
from `Some(&Value::Null)` (retain null); `replace_credential_document()` retains
the selected node's original number types and UTF-16. An invalid node leaves
the previous credential intact. Direct public field writes cannot express
Some-to-None history or replacement with the same lossy display string, so use
these methods for credential mutations.

`serde_json::to_string()` preserves exact credential JSON, including escaped
lone surrogates, through a checked temporary `RawValue`. That output view is
not the stored authority: an overflowing number serializes as null but remains
a number for `require_keys()`. `serde_json::to_value()` is a fallible scalar
observation and rejects unpaired UTF-16. Required keys retain their source
activation and order: Jev first when selected, then Anthropic in API-key mode;
local tokens are never required by this helper. Null/absent/blank values use
the existing missing-key diagnostic. Other non-string values use the source
trim-call message through native `Result<(), String>`, without a JavaScript
TypeError object or identity. Parsing masks Anthropic only at initial
subscription capture. Later mode changes do not recapture or erase it.

Classifier worker snapshots clear both representations of unrelated Anthropic
and local credentials, and launcher/doctor replacements discard old source
state. Effective Jev presence and JSON feed private cache identity. These
configuration and snapshot contracts do not qualify malformed provider headers,
local-token authentication or raw JavaScript object-to-OS environment output.
`credential_text(Credential)` returns sensitive primitive text as
`Result<Option<Cow<'_, str>>, &'static str>`. Absence remains distinct from null;
arrays and objects use the source primitive-conversion rules. Conversion failure
returns a fixed message without the rejected value. This is a conversion API,
not permission to use its output as an OS string or HTTP header.

The native gateway accepts a local authentication token only through
`RouterConfig::local_token: Some(String)` and requires at least sixteen UTF-16
units. `read_config_document()` still retains original non-string credential
JSON for exact configuration observation, replacement and the separate child
OS-environment contract. That retained value does not make a non-string local
token admissible to `Gateway::new()` or `Gateway::with_router()`.

This is a deliberately narrower Rust embedding boundary than the JavaScript
constructor's runtime length check. The JavaScript helper can construct a server
for some truthy non-string values whose `.length` is absent or at least sixteen;
a later `Buffer.from(localToken)` may fail or may accept a numeric array. Native
callers must not infer string-token usability or raw-value authentication parity
from configuration-parser success. Choose an explicit string secret and call
`replace_credential(Credential::LocalToken, Some(secret))` before constructing
the gateway; this clears any earlier captured source. Do not treat primitive
string conversion as an emulation of JavaScript `Buffer.from(array)`.

The minimum check counts UTF-16 units rather than UTF-8 bytes. Constructor
admission is separate from an actual authentication result, transport header
encoding, saved-secret validation and the launcher's fresh-token replacement.
This boundary does not relax constant-time credential comparison, subscription
OAuth requirements, upstream credential isolation or any request limit. It does
not qualify other credential keys or all arbitrary JavaScript object APIs.

The Jev evaluator materializes that effective credential for each request, then
applies the existing Fetch-style byte conversion, edge whitespace trimming and
HTTP header validation. Explicit replacement or removal cannot restore an older
captured credential. Invalid conversion or header bytes return the safe
`EvaluationError::Network` before transport; pre-cancellation still wins and no
request is sent. Native `evaluate_serialized_answer()` validates its serialized
state before credential conversion, so competing invalid state/credential inputs
retain that native API error order rather than JavaScript options-object order.
These controls do not qualify malformed Anthropic gateway authorization.

Gateway forwarding and advisory token counting materialize Anthropic credentials
separately. `credential_node_http_values()` returns sensitive ordered field values:
absence is an error at the forwarding boundary, null becomes `"null"`, and a
top-level array supplies repeated fields (an empty array omits the field).
`upstream_headers()` validates those values as Latin1 HTTP bytes. `count_headers()`
instead applies Fetch scalar conversion and whitespace trimming; absence becomes
`"undefined"` and arrays join into one field. Both remove incoming local credentials
and cookies. Subscription mode uses the current request's OAuth headers without
reading the configured API key.

The gateway rejects invalid forwarding credentials after request validation and
inference routing/preparation. Unusable count headers supply an unavailable count
callback, preserving classification and ordinary context policy. The additive
`GatewayRouter::route_contract_exact_with_counter()` passes Fetch-form count
headers and an optional `RouteTokenCounter`. Its default delegates to the existing
route hook and ignores the override; adapters that own counting must override it
to honor that callback. The product `Router` does so. Forwarding failures retain
the generic safe 502 response and release staged attempts without confirmation.
JavaScript exception identity is not a native API guarantee. A gateway owns its
configuration; replacement methods apply before construction, without promising
live mutation through an external configuration alias.

For library-owned routing, `Router::reconfigure_evaluator(&mut self, &RouterConfig)`
applies evaluator settings between route borrows on the same instance. It retains
active task state, staged attempts, classifier and token caches, worker owners and
closed state. Only evaluator fields and their exact Jev/keep-alive values are
copied; authentication, inference models, resource limits and other settings stay
owned by the existing router. The input has the same trusted typed configuration
contract as construction; parse external settings with the existing configuration
reader first. This method does not reload an `Arc`-owned gateway configuration.

`auth::build_claude_env()` returns `Result<Environment, ChildEnvironmentError>`
and materializes effective local-token values into OS strings. Null becomes
`"null"`, arrays use primitive joining, and source numbers retain their original
type until conversion. Unpaired UTF-16 becomes replacement characters at this
OS boundary. Absence removes API authentication fields and becomes `undefined`
in subscription custom headers; an empty string remains present.

This typed OS map is distinct from the JavaScript helper's raw object return.
Conversion failure or a NUL in the converted token returns a fixed safe error;
its `Debug`, `Display` and absent `Error::source()` contain no rejected value.
Unrelated parent OS keys and values retain their original bytes. The adapter
does not reproduce JavaScript exception identity, builder-versus-spawn phases,
arbitrary parent-object insertion order or competing-error precedence. Other
malformed parent environment entries remain the process launcher's concern.
Callers must handle this result before acquiring resources or inside an owned
cleanup scope. The launcher awaits its existing cleanup; diagnostic tools
preflight the environment before creating transport or listeners.

`ENABLE_TOOL_SEARCH` uses this existing `OsString` environment interface. Its
absence defaults to the string `"true"` in every client profile; every explicit
string, including `""`, `"false"` and `"auto:5"`, is preserved without boolean
parsing. Embedders supply already materialized OS strings and preserve absence.
The JavaScript helper can also return raw null, booleans, numbers, arrays or
objects until Node converts the child environment. Those raw values and object
aliases have no corresponding native parent-map input. In particular, raw null
remains null in the JavaScript builder and reports as `"true"`, but reaches the
child as `"null"`; the native string `"null"` reports and launches as `"null"`.
A JavaScript object that cannot convert to a primitive raises a pre-spawn
`TypeError`; it is outside the native typed-map API, rather than an equivalent
native runtime rejection. Configuration reports use the typed string domain,
including explicit empty values and saved/environment provenance. These API
differences do not establish Claude's deferred tool invocation behavior.

`CLAUDE_CODE_STOP_HOOK_BLOCK_CAP` is parsed into `Option<u64>`: absence stays
unset, and every accepted value is a nonnegative JavaScript-safe integer through
`9_007_199_254_740_991`. Zero disables the cap when it is the effective child
setting; an explicit parent cap string still wins. The native JSON and Value
inputs cannot contain JavaScript `NaN`. Rejecting the invalid JSON token `NaN`
does not execute the cap guard. Separate numeric-overflow controls exercise
Infinity rejection; this specific input-domain migration does not claim native
execution of the original NaN assertion. A future input API that admits NaN
needs a direct parser regression.

`build_claude_env()` does not revalidate direct edits to the public cap field.
Use `parse_stop_hook_block_cap()` or the configuration readers for external
values. Its parent map accepts OS strings and omission; JavaScript raw null,
objects and own `undefined` values are separate API inputs. The reviewed cap
contract preserves explicit parent strings, parsed configuration, and parent
nonmutation; it does not reproduce those raw-object values or aliases.

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

## Status and telemetry host boundaries

`StatusStore` options and updates are typed values: `StatusOptions` contains an owned path and explicit I/O/clock adapters, while `StatusStore` updates consume borrowed `Value` data and telemetry normalizers consume `Value` or `JsDocument`. They do not accept JavaScript `Error` instances, executable properties, cyclic host objects, or a top-level `undefined` value. An own optional property whose value is `undefined` maps to an absent JSON field, never to null; normalizer `None` is the absent-result observation. Hosts convert their own objects before this boundary. Source getter/cycle/Error tests remain original Source evidence, not native executions or a promise that arbitrary custom I/O/clock panics are caught.

`StatusStore::close().await` stops update admission when its future is first polled, drains the latest accepted state through its single writer, and awaits the owned worker and readiness task before returning. Separate close futures join the same drain/removal; they do not preserve JavaScript's shared Promise identity. Cancelling a close waiter retains those task handles for a later close. An unpolled future or dropping the store is not an awaited drain. Ordinary I/O errors are fail-soft; successful removal and no-recreation observations apply to the tested owned filesystem schedules, not to denied removal or a panicking custom adapter.

## Diagnostic, setup and session-log input boundaries

For the individually reviewed local-diagnostic tests 6, 12 and 13, `run_local_diagnostic` and `run_local_diagnostic_with_progress` use a typed `RouterConfig` and a borrowed `CancellationToken`. A `u64` timeout cannot contain JavaScript `NaN`: rejecting configuration text `NaN` is admission evidence, not execution of the original fifth `invalid_configuration` report. Cancellation returns `EvaluationError::Cancelled`; hosts retain arbitrary abort reasons and Error identities separately.

For Ollama setup tests 9, 10 and 14, `setup_ollama` and `warm_ollama` retain their own deadlines when the evaluator runtime timeout is zero. Tokens carry cancellation state, and `SetupError` returns the safe `OLLAMA_CANCELLED` or `OLLAMA_TIMEOUT` code and message without a caller reason or error cause. `HttpTransport` does not expose a JavaScript `AbortSignal` object. Tested owned request/body release replaces the original signal-property and stream-cancel callback observations; it does not execute those JavaScript operations or await an arbitrary cancellation Promise. The six native regression schedules use paused Tokio time and also check a separate wall-clock bound; they do not establish provider performance or real-clock native deadlines.

For session-log tests 3 and 6, `SessionLog::record` borrows `serde_json::Value`. Hosts supply inert metadata; the API has no property getters, host Error objects or nonfinite numeric inputs. Existing tests prove detached durable metadata and privacy for that data domain. Omitting an unknown accessor is not native getter non-invocation, and the selected-model getter operation remains one skipped input with two unexecuted native assertion sites. The old Infinity-to-null fixture projection proves finite-data latency omission only; it does not prove that native logging accepts Infinity. These are specific library migrations, not a general waiver for raw `JsDocument` consumers, callback behavior or other original tests. Existing filesystem, byte limits and owned-close obligations remain in force.

## Gateway receive timers and response preparation

`Gateway::listen` owns one 30,000 ms HTTP receive scanner. The first scan is scheduled from listener creation; later scans are scheduled from the actual callback start, without replaying missed ticks. The 10,000 ms header and 30,000 ms total-message limits are strict age thresholds evaluated by that scanner, rather than separate wall timers. A newly accepted blank connection is eligible. The first byte other than CR or LF starts that message's age, including an invalid method byte; leading CRLF alone does not reset it. Header completion clears header eligibility and complete body/trailer receipt clears total-message eligibility. Parser rejection clears receipt eligibility without claiming that a generic parse-error response is fully flushed or resource-bounded.

The Gateway's opt-in Hyper control commits parser transitions before exposing an admitted request or final body frame. At timeout it first claims failure for pending delivery records and cancels request work, then makes at most one best-effort raw 408 write when the current final response has not been prepared. A blocked, partial or failed write is not retried or flushed; the connection ends. A prepared marker records the Gateway's equivalent of first send/end/flushHeaders, rather than response construction by writeHead alone or successful downstream forwarding. Prepared current responses suppress this raw 408. Earlier completed responses and later queued responses keep their own preparation markers. The existing first-positive-frame gate checks cancellation and deadline state before replaying staged provider bytes, preserving their contents and signed history.

After a response finishes with no queued outgoing response, the separate 6,000 ms keep-alive timer may coexist with an incomplete next header. Positive parser input clears that idle timer; pending reads and EOF are separate events. The lower-level public `transport_completion::serve_http1` and its `RequestReceiveDeadline` retain their existing per-connection helper contract. They do not opt an arbitrary embedded server into the Gateway's shared listener scanner.

The retained three-socket real-clock comparison qualifies the blank, late-header and late-body schedules. Paused-clock registry/Gateway tests and controlled parser/writer tests qualify their declared boundary and ordering controls separately. They do not establish a complete HTTP/TLS/platform or blocked generic-error cleanup contract, nor measure a delayed second actual listener callback or arbitrary callback overrun.

## Cleanup bounds and release inspection errors

The Gateway launcher retains only the lexical first 200 status-cleanup candidates while enumerating the directory. The same selection precedes ownership and permission checks; skipping an unsafe candidate does not admit an extra one. This bounds retained candidate names, without adding a scan, syscall or RSS ceiling. Snapshot limits count UTF-8 file bytes. Missing, malformed or invalid-PID snapshots require directory age strictly greater than ten minutes; a live PID is stale only after its heartbeat age strictly exceeds one day.

For the three original release-check negative contracts covering checksum tampering, checksum filename identity and archive symlinks, the Rust tool uses its native archive fixture and explicit fixture checkout. The shared early-rejection rules preserve the exact filename-before-digest precedence, the regular-file gate and the original safe diagnostic text. A typed error and a failing command replace JavaScript Promise rejection. This scoped migration does not make historical JavaScript archives eligible for native release checks, approve synthetic authorization as release evidence, or change final qualification, platform, archive-size or publication gates. Native final authorization continues to require its canonical LF-terminated sidecars.

## Native release material completeness

The native npm and direct archive validators require the complete named public document set selected by the packager, alongside the existing generated/runtime files and integrity checks. Native npm includes `package.json`; direct archives retain their own manifest representation. Qualification and final authorization remain mandatory.

Release-check preserves fixed safe diagnostics for extra release-directory artifacts and unexpected package members. Arbitrary error text and paths remain omitted. That material-completeness checkpoint individually accepted original release-check definitions 10 and 15. The separate migrations below cover definitions 11–14 and parent 7; stronger material admission alone did not establish those contracts. An incomplete authorization/checksum pair is an invalid artifact set; absence of the whole pair still requires final authorization.

## Native archive integrity API

The [individual archive mappings](../parity/assertion-mappings/release-layered-integrity-native-api-migrations.json) replace the historical JavaScript archive domain with a separately valid native release set. The complete positive result still contains exactly `version`, `archive` and `dist_tag`, with each fixture's version-derived filename. Archived manifest comparison uses the native manifest projection and retains the safe `package.json differs` diagnostic.

Runtime integrity binds the selected executable through file declarations, artifact inspection/provenance and the independently retained candidate index and final authorization. It does not compare executable bytes with Rust source text. Re-declaring a modified executable or forging its inner artifact digest still fails; the native typed reasons and fixed safe command failure replace the JavaScript source-path diagnostic. The exact selected target matrix replaces npm wildcard inclusion followed by exact pack-list comparison. A known, well-formed but unselected target remains inadmissible. Mandatory named native materials replace npm optional-file list semantics, including `LICENSE`; the original file-list diagnostic is a distinct native API migration. Synthetic binary headers and approval records in these tests are parser fixtures, not runnable target binaries or real release authorization.

## Default inspection budget

`setup_ollama` gives the entire version/tags/show inspection one default 5,000 ms budget. It does not restart that budget at each endpoint. Caller overrides and the separate pull and warmup deadlines retain their own contracts. [Boundary checks](../parity/measurements/native-inspection-default-deadline-summary.json) hold early or late response heads and bodies, keep the caller token active after timeout, and then complete a fresh three-request inspection with that same caller.

Native tests use paused time and assert pending ownership at 4,999 ms, timeout at 5,000 ms and request/body Drop before return. Frozen JavaScript observations use the real clock, cooperative abort-aware pending requests and stream cancellation that settles. These are separate cleanup and timing contracts; they do not establish prompt return from an arbitrary abort-ignoring fetch or a cancellation promise that never settles, or measure an Ollama model's speed.

Pull progress counts every decoded response byte toward its 16 MiB total. The 65,536-byte checks apply to a pending buffer and a nonblank update line, excluding its LF delimiter. Completed whitespace-only lines are ignored before the line-length check; an oversized whitespace prefix split before its LF can still fail the pending-buffer check. This preserves the Source behavior. See the [individual default-limit evidence](../parity/measurements/native-count-and-pull-default-limits-summary.json).


## Native contributor-tool API boundaries

The [four individual tooling mappings](../parity/assertion-mappings/tooling-policy-preflight-native-api-migrations.json) document development-tool interfaces separately from runtime routing. A completed nonzero installed-lifecycle capture returns a safe error containing status and a recoverable private JSON report path. The report retains the synthetic stdout and stderr; it has mode 0600. This replaces the JavaScript helper's resolved result object. Incomplete timeout or overflow capture does not produce a completed report. Consumer injection and separate real process-capture tests compose; the injected consumer test does not execute the installed main command.

Release preflight permits only native archives to become ready for new publication. Metadata checks, an identical existing version and an uncertain retry preserve their safe read-only behavior before that decision. Typed `HttpError::Network` has no arbitrary JavaScript Error message to sanitize; the original open-error input is an explicit interface migration, while safe diagnostics and all original status failures retain their tests.

Evaluation policies are copied owned values and may be edited before transfer. `run_evaluation` takes ownership and revalidates before creating a router or admitting evaluator requests. The public `f64` fields can contain NaN or positive infinity, and actual tests reject both before work. Owned policy and immutable input borrows replace JavaScript freeze exceptions; zero invalid-policy transport calls and static constructor ordering replace injected factory identity. The valid three-request control returns the original complete outage report (`passed: false`, three fallbacks). It proves admission and policy isolation, without claiming a successful evaluator response or arbitrary mutable-alias protection.


For the [individual stalled-child/missing-executable migration](../parity/assertion-mappings/package-lifecycle-spawn-cause-native-api-migration.json), the internal direct subprocess runner returns deadline failure metadata instead of a rejected JavaScript Promise. Failed spawn reports now include the closed `spawn_error_kind`: `not_found`, `permission_denied` or `other`. `not_found` categorizes the OS error without asserting which path or interpreter failed; invalid NUL arguments exercise the grouped `other` branch. Raw OS messages, paths, arguments and numeric spawn errno remain omitted. The live-report consumer removes this internal field, keeping its strict report schema unchanged.

The configured 100 ms control observes direct termination and return, while the raw report retains `cleanup_confirmed: false` and its process-group signal diagnostic. This is a scoped deadline/cause contract; it does not establish successful whole-group cleanup, the cause of errno 1, arbitrary blocking-spawn bounds or equivalent APIs for the separate synchronous/owned capture helpers. Existing cancellation, output bounds and cleanup policy remain unchanged.

### Contributor readiness and independent startup budgets

`tool_process::capture_lifecycle` uses the same owned subprocess loop as ordinary captures. Its Source lifecycle mode retains complete readiness JSON and sends a caller-selected INT or TERM to its owned, unreaped leader at most once. Readiness output supplies no PID authority. The ordinary live-tool capture adapter retains its 100,000-byte aggregate ceiling and whole-buffer decoding; Source mode counts decoded UTF-16 per observed chunk. Tokio chunks need not match Node data-event boundaries. Structured native failure values and TERM plus grace differ explicitly from Source rejected Promises and immediate KILL. Observed child exit and pipe EOF do not prove successful group cleanup.

The [default setup-budget evidence](../parity/measurements/native-default-setup-budget-limits-summary.json) accepts only the 900,000 ms pull and 60,000 ms warmup policy defaults. Eight controlled-clock schedules per engine cover held heads/bodies, late metadata, disabled routing deadlines and fresh retries. They measure policy boundaries, not model or wall-clock latency; Node's separate internal timeout primitive remains real and its private timers are not claimed joined. All broader qualification gates remain open.
