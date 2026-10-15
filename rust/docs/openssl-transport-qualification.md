# OpenSSL transport qualification work

The [rewrite plan](../../docs/rust-rewrite-plan.md) requires transparent provider bytes, matching trust policy, cancellation and task continuity through successful downstream forwarding. The ordinary native client still uses Rustls. The OpenSSL constructors, raw pool, bounded buffering and encoder-handoff bridge remain test-only; passing their current subset does not authorize a production switch.

Use the current [encoder-handoff summary](../parity/measurements/openssl-buffered-handoff-summary.json) when choosing work. It retains 162 matching comparisons and 324 executions, including the four byte-loss cases that failed in the older [buffered-response report](../parity/measurements/openssl-buffered-response-summary.json). Do not repeat the old defect as a current finding or replace the historical report. Encoder admission, write attempts, source EOF, pool return and final downstream flush remain separate observations.

## Required lifecycle families

Each family needs the actual candidate constructor and gateway, explicit stimulus/observation barriers, bounded cleanup and retained source/executable identities. Primitive state-machine or buffer tests supplement these schedules. They cannot replace the gateway evidence below.

| Family | Required evidence and ownership boundary |
| --- | --- |
| HEAD, no-body statuses and final data | Exercise HEAD, 204, 304, zero-length responses, final-data `is_end_stream`, trailers and clean EOF through the candidate. Preserve method/status framing and provider bytes; only complete consumer ownership may return a raw connection. No-body submission still needs successful header flush. |
| Blocked final flush and continuity | Deliver a complete valid provider tool response, then hold the downstream flush after all framing bytes are accepted. Confirm neither successful delivery nor a replacement model while flush is pending. Release successfully, fail the flush, cancel and expire the request in separate schedules. Only the successful control may replace the previously confirmed tool owner. |
| Attachment and failure races | Cover failure before headers, acquired headers before attachment, a response queued for encoding, attachment failure and cancellation at each boundary. A latched body failure must preserve acquired status while preventing a false success; dropping a stale response must not affect another request. |
| Pipelined A/B ownership | Execute actual concurrent service futures on one keep-alive connection, including B becoming ready before A. Bind submission, failure, EOF and flush to their exact response sequence. Retired A notifications and deadlines must not close or confirm B. |
| Buffered sibling and raw/fetch separation | Exercise two simultaneous same-origin responses, a different origin and fetch alongside raw forwarding. Cancellation, parser errors, session invalidation and pool retirement must affect only the owning request/connection. Raw-pool-only evidence does not qualify the buffered constructor. |
| Eight saturated responses and repeated cancellation | Hold eight real downstream consumers, reach declared buffer pressure, then cancel in independent orders and repeat. Account for retained aliases, producer tasks, sockets, timers and pool/session owners through cleanup. A per-buffer payload limit alone is insufficient. |
| Long legal streaming and aggregate allocation | Forward a generated legal stream with exact incremental byte count/digest and complete framing. Account for application blocks, Hyper frame backing, TLS state, encoder buffers, retained headers/aliases and metadata across all live owners. Keep total memory qualification pending until every term and recovery is justified. |

The [final-flush regressions](../crates/autorouter-runtime/src/server_failed_flush_contracts.rs) now exercise 24 synthetic schedules through the actual buffered constructor: HTTP, negotiated TLS 1.2 and TLS 1.3; ordinary completion and attached request-terminal ownership; successful flushing, flush error, cancellation and absolute deadline. They hold an instrumented downstream writer after the complete JSON tool response and HTTP framing have been accepted. Clean buffered EOF leaves the completion registry and replacement attempt pending. Only successful flushing replaces the previously confirmed Opus tool owner with Sonnet; the other outcomes preserve it. The tests verify callback, writer, task, socket, buffer and cache cleanup.

These controls use one bounded JSON response with `Connection: close`, an isolated fixture certificate for TLS and a scripted downstream writer. They do not establish all response shapes, real downstream socket failures, certificate-policy parity, pipeline ownership or the complete family matrix. The ordinary failed-framing control remains alongside them, and the [deadline regression](../crates/autorouter-runtime/src/server_deadline_contracts.rs) independently covers a held writer. Keep [`CompletionRegistry`](../crates/autorouter-runtime/src/transport_completion.rs) as the final delivery authority. Repeat the focused module from `rust/` with:

```sh
cargo test --locked --package autorouter-runtime server::failed_flush_contracts
```

## Remaining policy and session work

Retain the exact old session/idle comparison outcomes: original sessions 13/18, intent sessions 15/18, idle-close 4/10 and raw-pool original sessions 15/18. These refer to distinct retained constructors and executables. Fresh comparisons must explicitly select the candidate being evaluated and preserve the frozen Node inputs, event/call assertions and complete cleanup. A controlled race can explain an ownership boundary, but cannot turn an unresolved unbarriered comparison into a passing one.

The fresh-handshake experiment's 223 matching cases still excludes three OpenSSL configuration modes. `OPENSSL_CONF`, `--openssl-config` and `--openssl-shared-config` need fresh-process configuration/loading/error/privacy evidence before support is claimed. Do not weaken certificate or handshake verification, retry requests, infer socket ownership from a weak open flag, or change the oracle to obtain a match.

Promotion additionally requires the production feature/dependency and vendor-provenance review, full protocol and installed-archive regressions, supported-platform TLS execution, resource/performance gates and explicitly invoked provider canaries from the full plan. Historical experimental binaries do not qualify a rebuilt shipping binary. Keep source/build identity, exact package bytes and rollback evidence bound to the final candidate.
