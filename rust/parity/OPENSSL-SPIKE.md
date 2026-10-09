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
