# OpenSSL stream transport: stage A and B1

This is a test-only feasibility experiment. Shipping `NativeHttpClient` still
uses Rustls. The runtime test module is absent from ordinary builds, and no CLI
flag selects this candidate.

The candidate retains Hyper HTTP/1, the raw/fetch parser extensions, streamed
`Incoming` bodies, gateway cancellation, the response observer, and downstream
completion instrumentation. Separate clients offer no ALPN for raw HTTPS and
`http/1.1` for fetch. Both disable request replay. Connections own their socket
and TLS stream, and the connector rejects destinations outside loopback.

Certificate trust uses the existing frozen Node 22.14 store and exact hostname
rules. The OpenSSL context explicitly selects Node's cipher list, level 1,
TLS1.2/1.3 default limits, and the classical default-provider group list from OpenSSL
3.0.15, including actual P-521 key exchange. It never loads operating-system
roots implicitly. OpenSSL 3.6.3 is still a different backend from the reference
3.0.15+quic. Failed verification never retries with another verifier or policy.

`hyper-openssl =0.10.2` is a development dependency with all optional features
and default features disabled. Only its stream API is used; its legacy connector
and session cache are not used. `tower-service =0.3.3` was already locked.
`openssl-spike-provenance.json` records the published archive checksum, source
commit, all 18 published file hashes, and both full license notices. The offline
verifier checks the Cargo lock, resolved features, dev-only declaration, archive,
extracted sources, and license inventory. This records reviewable provenance;
it is not an upstream endorsement of this experiment.

From `rust/`, build the runtime library test executable:

```sh
cargo test -p autorouter-runtime --lib --no-run --offline --locked
cargo test -p autorouter-runtime --lib tls_roots::openssl_spike --offline --locked
cargo clippy -p autorouter-runtime --all-targets --offline --locked -- -D warnings
```

Cargo prints the test executable path. Pass that exact path from the repository
root, rather than the shipping CLI:

```sh
node rust/parity/verify-openssl-spike.mjs
node rust/parity/check-openssl-transport-spike.mjs artifacts/rust-rewrite/reference rust/target/debug/deps/autorouter_runtime-EXACT_BUILD_HASH
```

The differential driver requires the frozen Node22.14.0/OpenSSL3.0.15+quic
reference, verifies the frozen source manifest, and copies the candidate to an
immutable temporary executable. Each case gets an isolated HOME and fresh
process. Both provider and evaluator endpoints are checked before spawn;
provider hostnames are numeric loopback or literal `localhost` resolving only
to loopback. Evaluator URLs always use numeric loopback. Certificates, prompts,
credentials, and peers are synthetic. There are no provider or model calls.

The shared runner keeps production case IDs and its 115-case result separate.
The original stage-A 125-case matrix contains those cases plus four P-521 TLS1.2 certificate cases,
two forced P-521 key-exchange cases, and four real gateway cases observing raw
versus fetch ALPN and SNI across TLS1.2/1.3. Peer counters reject extra connection
attempts and unexpected session reuse in the handshake cases. The focused
cancellation test abandons a stalled handshake, requires its socket to close,
and proves a sibling HTTP request succeeds. It does not stand in for cancellation
at every TLS flight or the pooled-connection race.

Reports include the exact executable, fixture corpus and source hashes. The
223-case B1 candidate report is `artifacts/rust-rewrite/parity-openssl-transport-spike.json`;
all candidate reports are also retained under content-addressed `evidence/`
paths before the convenience report is replaced. The three configuration option
rows remain in the separate `openssl-spike-unqualified.json` characterization.
Unexpected differences make the command fail. Shipping Rustls evidence remains
in `parity-tls-options.json` and `tls-policy-characterization.json`.

B1 adds 96 cases declared in `tls-b1-cases.mjs` before the first differential
run. Dedicated TLS1.0/1.1 peers distinguish actual negotiation from allowing a
lower minimum against a modern peer. Legacy protocols require an explicit
`@SECLEVEL=0` for these fixtures; selecting a lower protocol never lowers the
security level automatically. TLS1.2/1.3 peers cover cipher separation, empty
and invalid lists, string operands, last-value precedence and level0/1/2 with
weak-key and SHA1 certificates. Missing/invalid command operands retain startup
errors; invalid cipher expressions fail the connection without opening TCP or
retrying another policy. Node's TLS1.3 defaults remain when no `TLS_` terms are
provided, and explicit security levels are not reset after cipher selection.
Two later independent-review regressions retain the explicit operand and
underscore spelling in invalid-negation diagnostics, bringing the total to 223.

Explicit `NODE_TLS_REJECT_UNAUTHORIZED=0` is read per new TLS connection and
sets that connection's certificate policy before its handshake. Other tested
values remain strict. It never retries after strict verification fails and
never disables TLS handshake signatures or authenticated encryption. Negative
fixtures corrupt TLS1.2 ServerKeyExchange signatures and TLS1.3 encrypted
handshake records under both policies. Gateway stderr observations assert no
warning at startup, exactly one message after TLS use across repeated requests
and raw/fetch profiles, and no warning for a local `/health` request. These
checks compare the warning message and count, not Node's PID/trace-hint wrapper
or generic runtime warning-suppression flags. Certificate failure flight timing
and a deliberately sub-1024-bit DH peer are still unqualified.

The first B1 run passed 221/221 with three separately retained configuration
characterizations; matching these fixtures does not establish all cipher
expressions or OpenSSL3.0/3.6 equivalence. Original stage-A reports remain under
content-addressed evidence paths, including
`openssl-spike-5a8a99915d3665ea2edaf88875f3255c393e5f95f910c2b4c7dbcc3d98c6c94d.json`.

The candidate is **not eligible for production**. The ordinary `stage-a`
runner keeps session reuse disabled so the original fresh-handshake corpus
remains unchanged. The separate `stage-b2` marker enables only the test-only
raw HTTPS cache. Production `NativeHttpClient` still uses Rustls. Both test
profiles now share one root-store snapshot per client instance; process-global
sharing across independently constructed clients remains unqualified.

The reviewed B2 bindings provide context-bound immutable session snapshots and
shared-store ownership, with seven ownership tests and three compile-fail
checks. See [the binding contract](../vendor/TLS-TRUST.md#context-bound-session-and-store-apis)
for the weak-cache capture and ordinary private DER heap limits. No additional
binding, Cargo, gateway or production transport changes were made for runtime B2.

### B2 raw session increment

`session.rs` implements raw HTTPS's 100-entry FIFO cache. Replacing an existing
key retains its position; a later observed error-close evicts by key, including
a newer ticket installed by another connection. One close cannot evict twice.
Typed keys separate origin, SNI, trust/policy generation and verification mode;
fetch receives no session cache and has separate contexts. SSL ex-data uses one
process-wide index, avoiding an index allocation for each new client.

A connection retains only its latest pending ticket until the handshake's peer
and hostname checks pass. Rejected/abandoned attempts never publish it. Later
TLS1.3 tickets update accepted entries. The callback captures no strong cache
reference; connection state points weakly to the cache. Neither keys nor
session snapshots have Debug output. Only entry/byte counts appear in synthetic
cleanup diagnostics; DER and keys are never emitted.

Actual connect/IO errors provide error-close evidence. HTTP parser errors,
short bodies and ordinary request/body Drop do not. Frozen Node observations
and `_http_client.js` show that parsing/EOF failures can destroy a socket
without the error argument used by the raw agent's eviction hook. The candidate
therefore forwards `Incoming` directly and does not infer cancellation from
Drop. The current Gateway transport interface carries no cancellation cause;
matching active downstream cancellation needs a separately reviewed seam.

The final frozen B2 matrix is **13/18 exact matches and remains failed**. Its
five retained differences are:

- TLS1.2 and TLS1.3 immediate reuse after an idle peer's abrupt close: Node can
  assign the stale socket and fail that request; the candidate opens another
  connection. Separate cases with a server socket-close barrier and two gateway
  `/health` round trips match, while the original schedules remain failures.
- TLS1.2 and TLS1.3 downstream body cancellation: Node evicts the session;
  the candidate lacks explicit cancellation intent and retains it.
- TLS1.3 rejected hostname: application rejection, absence of a rejected cached
  session, and subsequent full/resumed requests match; the server's
  `secureConnection` event occurs in a different handshake/close order.

Exact matches include fresh/resumed TLS1.2/1.3, keepalive, server ticket-key
rotation declining a ticket without replay, parser-error and short-body session
retention, the two additional close-barrier cases, and TLS1.2 rejected-host
publication control. All 18 candidate cleanup checks reach zero active
connections and release the cache. Ten focused Rust tests pass, including shared
store identity, FIFO/replacement/byte accounting, pending/accepted/rejected
transitions, weak ownership, ordinary Drop and idempotent late-error handling.
Scoped runtime Clippy passes with warnings denied. The unchanged frozen
regressions pass 223/223 candidate and 115/115 shipping cases.

The source-controlled [B2 summary](measurements/openssl-b2-runtime-summary.json)
binds immutable source/binary snapshots, the final and earlier failed matrices,
unit/Clippy evidence, and all five gaps. The driver is
`check-openssl-sessions.mjs <frozen-reference> <runtime-lib-test-executable>`;
`--reference-only` retains characterization without claiming parity. It
verifies the frozen baseline first, uses only isolated synthetic CAs and
loopback endpoints, validates CA/leaf identity and signatures, and performs an
actual TLS fixture preflight. Original fixture-generation failures are retained:
macOS LibreSSL reused the CA's configured subject despite a `-subj` operand;
separate explicit leaf CSR configuration fixes the fixture without weakening
certificate checks.

Fetch's weak-cache/GC behavior, B3 configuration initialization, complete pool
and cancellation semantics, ticket-flood/retained-byte bounds, global trust-store
lifetime and backend-wide cipher/provider equivalence remain promotion gates.
The finite B1/B2 corpora do not prove all OpenSSL3.0.15/3.6.3 behavior. Dynamic
in-process changes to the rejection environment also remain unqualified; the
candidate isolates strict and explicitly disabled verification contexts.
The existing `NO_LOAD_CONFIG` initializer is unchanged.

### Selected-connection observation experiment (seventh increment)

Before adding any abort authority, this increment declares these tests:

1. A captured connection carries a private weak identity. Copying metadata does
   not keep the transport alive, and distinct transports have distinct identities.
2. An unsent request and a held TLS handshake have no selected-connection
   metadata. Dropping the waiter must close its owned socket without creating a
   permanent capture watcher.
3. For HTTP, TLS1.2 and TLS1.3 loopback peers, request B reuses A's connection
   while A still owns an unpolled, nonempty `Incoming`. Both captures and response
   extensions must identify that exact connection. Holding or later reading A's
   capture is not evidence that A still owns the connection.
4. Delaying an observer until after B is sent must expose the assignment-versus-
   observation ordering explicitly. No generation recorded by that observer is
   allowed to authorize an old-request abort. Plain capture/body Drop must not
   acquire socket-close or session-eviction authority.
5. Simultaneous same-origin requests with one response held open must identify
   separate selected connections. All fixture tasks, sockets and weak identities
   must be released under bounded cleanup, including a failed fixture assertion.

These are native ownership probes, not a replacement for the original failed
18-row Node differential. `capture_connection` in hyper-util0.1.21 publishes
metadata after pool checkout and before request dispatch, but the watcher runs
later. Its `Connected::extra` value is also copied for response and error
metadata; copying it is not a uniquely identified checkout callback. If these
public hooks cannot prove request retirement, this increment must report that
gap and propose a narrow synchronous hook before connecting user cancellation.
No gateway seam, production transport, session eviction rule, or B3 initializer
is changed by these observation tests.

The declared experiment passes **22 focused tests**, including the prior ten
spike tests and twelve new observation/cleanup tests. The held-handshake test
also checks that capture metadata never appears and that its waiter terminates
after request cancellation. The test executor counts every Hyper task separately
from IO leases; the new fixtures require both counts to reach zero before peer
shutdown, then join every fixture task and check weak cache/connection release.
An injected assertion failure exercises that cleanup path independently.

HTTP, TLS1.2 and TLS1.3 all demonstrate the same counterexample: A retains its
entire unpolled four-byte `Incoming`, but the peer has already received B on the
same connection. A's capture remains unchanged and live. B's capture and
response extensions identify that same connection. Additional cases safely drop
A's old buffered body while B is active, and concurrent held responses prove
same-origin connections remain distinct. These are bounded native ownership
observations, not timing benchmarks or Node cancellation parity.

**Request retirement and explicit abort authority remain unqualified.** A
capture watcher cannot revoke A synchronously before B is dispatched. The
observation type deliberately exposes no abort, poison or eviction method.
Using metadata `Clone` as checkout would conflate other metadata copies;
`Connected::poison` would not protect an already selected B. A separately
reviewed synchronous assignment/retirement guard in the pool is required before
wiring cancellation. The original B2 report stays failed at **13/18**, with all
five differences intact. [The lifecycle summary](measurements/openssl-b2-lifecycle-summary.json)
binds the immutable test executable/source snapshot, passing checks and retained
registration-race and sandbox-binding failures. No production transport switch
or coverage promotion is implied.

B2 lifecycle sources: [raw HTTPS session eviction](https://github.com/nodejs/node/blob/v22.14.0/lib/https.js#L130-L173),
[HTTP parser and socket-end destruction](https://github.com/nodejs/node/blob/v22.14.0/lib/_http_client.js#L435-L533),
and [TLS acceptance before session publication](https://github.com/nodejs/node/blob/v22.14.0/lib/_tls_wrap.js#L1562-L1615).

B1 sources: [Node 22.14 option defaults](https://github.com/nodejs/node/blob/v22.14.0/lib/tls.js),
[cipher expression processing](https://github.com/nodejs/node/blob/v22.14.0/lib/internal/tls/secure-context.js),
[connection acceptance](https://github.com/nodejs/node/blob/v22.14.0/lib/_tls_wrap.js),
and [connection-time rejection setting](https://github.com/nodejs/node/blob/v22.14.0/lib/internal/options.js).

Primary policy sources: [Node 22.14 cipher defaults](https://github.com/nodejs/node/blob/v22.14.0/src/node_constants.h),
[Node TLS context policy](https://github.com/nodejs/node/blob/v22.14.0/src/crypto/crypto_context.cc),
[OpenSSL 3.0.15 default groups](https://github.com/openssl/openssl/blob/openssl-3.0.15/ssl/t1_lib.c),
[raw HTTPS agent](https://github.com/nodejs/node/blob/v22.14.0/lib/https.js),
[fetch connector](https://github.com/nodejs/node/blob/v22.14.0/deps/undici/src/lib/core/connect.js),
and the [published stream API](https://docs.rs/hyper-openssl/0.10.2/hyper_openssl/struct.SslStream.html).

### B2 reservation-lease foundation (eighth increment, test builds only)

A local, feature-gated hyper-util 0.1.21 patch now makes the reservation boundary
explicit. `capture_http1_assignment` creates a request-owned pending guard before
submission; the first submission consumes its single-use slot. `Assignment`
observes the selected reservation, and `claim_abort` checks that it is still
active and poisons that exact connection under the same short mutex used for
retirement. `Guarded<Pooled>` retires before returning the connection to the pool,
including Hyper's immediate return, deferred readiness task, cancellation, error,
and unwinding paths. Metadata cloning, destruction, watch wakeups, public tracing,
and pool return happen outside that mutex. Connector-provided extras retain their
own ownership semantics; this spike stores weak connection identities.

The claim is **only a once-only pool-poisoning authorization foundation**. It does
not close IO, evict a session, infer cancellation from `Incoming` Drop, or change
the shipping transport. Attached requests reject retry-enabled/pool-disabled
clients, HTTP2, CONNECT and upgrade requests. A cloned request cannot consume the
same attachment twice. A selected connection marked as negotiated HTTP2 is also
rejected before assignment; unsolicited 101 responses retire before returning.

Fourteen bounded native tests exercise actual Hyper scheduling, including its
immediate and deferred branches, executor rejection, pending/unpolled/error
cleanup, and a pool winner while a losing connection attempt remains held.
HTTP, TLS1.2 and TLS1.3 loopback peers prove that A's unconsumed old body cannot
claim the connection after B has selected it. A valid B claim leaves B's response
readable and prevents the connection from serving C. Thirteen private ownership
tests cover once-only claims, concurrent and forced lock orders, metadata
Clone/Drop and watch-wake panics, weak ownership, and negotiated-HTTP2 rejection
using an in-memory connector. This is not an actual TLS ALPN qualification.

The published upstream tests and dev dependencies are retained. The isolated
test graph explicitly selects the current vendored Hyper1.12 and Tokio1.53.2;
its full-feature library suite passes 115 tests with one upstream ignored network
test, and its legacy-client HTTP1/HTTP2 integration suite passes 21. These reports precede
the final private metadata-Drop assertion refinement; the final thirteen private
tests and three mutation controls bind the corrected source separately. Early reports
using the published upstream lock's Hyper1.9 are retained separately. Mutation
controls deliberately remove early retirement and the phase check; both must
be detected. The first metadata-Drop negative exposed an overly broad panic
catch; its failed-control report is preserved, and the test now requires the
exact intended panic payload as well as retired state.

`node-http1-request-lease` is absent from `default` and `full` and is enabled only
by the runtime's dev dependency. The ordinary release build is audited from
Cargo compiler-artifact JSON, with exact package identity, a release CLI artifact,
successful build completion, and absence of the lease feature required. Its
self-test rejects 21 malformed, missing, unsuccessful, or feature-enabled inputs.
The [source verifier](../vendor/verify-hyper-util.mjs) checks all 62 published
members, the one added module, and exact forward/reverse patch hashes. This is a
local MIT-licensed dependency patch, not an upstream-approved API.

The original B2 differential remains failed at **13/18**. Actual destructive
IO cancellation, gateway cancellation-intent ownership, raw-session eviction
integration, fetch-session caching, B3 configuration initialization, full pooling
qualification, and the original five mismatches remain separate gates. Neither
this foundation nor its native tests promote the OpenSSL spike into production.

The [foundation summary](measurements/hyper-util-lease-foundation-summary.json)
binds the final private-test executable, exact isolated graph, source/patch
hashes, positive and mutation results, and every retained failure. The shipping
feature-off release proof remains separate, parent-owned final validation.
