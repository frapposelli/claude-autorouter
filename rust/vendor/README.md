# Pinned Hyper compatibility patch

`hyper/` is the crates.io Hyper 1.12.0 source archive (MIT license), limited to its
source, Cargo manifests, license and README. The original license is retained.
`hyper-provenance.json` records the archive SHA-256, every retained upstream file,
every candidate file, and the exact patch SHA-256. `hyper-node-http1-compat.patch`
is the complete modification. Run `node rust/vendor/verify-hyper.mjs` from any
working directory to reconstruct and verify the upstream tree offline.

The explicit `node-http1-compat` feature changes server HTTP/1 request-header
handling and permits bounded concurrent service futures with ordered response writes. Hyper still owns HTTP parsing, message framing, body decoding and
connection driving. The patch adds Node-compatible URL/header semantic byte
accounting at 16 KiB (including partial reads), rejects duplicate Content-Length
and Content-Length plus Transfer-Encoding, rejects methods outside Node's list
and bare-LF request lines, and retains the first 1000 raw header fields for the
application while validating every field for framing. Tiny duplicate headers
remain accepted up to the semantic byte bound. Ignored leading value whitespace and repeated request-line spaces are removed
incrementally before parsing, matching Node while keeping retained buffers bounded.
Normal requests retain Hyper's
100-entry stack storage; larger valid heads allocate bounded temporary slots.

Raw provider requests opt in separately with the `NodeHttpResponsePolicy` request
extension. That response parser enforces Node's status/header semantic byte bound,
rejects ambiguous framing and bare LF, validates all framing fields, and retains
only the first 1000 header fields. It resets accounting for informational responses.
Fetch-based evaluator/count transports instead set `NodeFetchResponsePolicy`: the
status text is excluded from the semantic header bound, bounded header fields are
retained without the native API truncation, and unsolicited 100/101 responses are
rejected as in Node's bundled Undici parser. Chunked trailers use the same bounded
semantic header accounting. Chunk extensions retain no extension text and validate
Node's name/value grammar; native HTTP applies its 16 KiB per-chunk extension bound,
while fetch matches Undici's absence of that extension byte counter.

The server keeps at most 15 queued service futures plus the current response writer.
Request bodies remain streamed with backpressure. Per-request method, version,
keep-alive and trailer state stays attached to its ordered response. Ready later
responses cannot overtake an earlier response. `NodeHttpResponseSubmission` fires
at response head encoding, allowing the runtime completion registry to distinguish
queued responses from responses actually submitted to the writer. Each completed
response must flush before the next response begins. Read errors, service errors,
write failures and connection drop cancel queued futures and release their bodies.
Header receipt timers start when message bytes are available, not during idle time
while an earlier pipelined service is still running.

The source contract is Node 22.14.0's `src/node_http_parser.cc` (`TrackHeader`,
`on_url`, `on_status`, `on_header_field`, `on_header_value`) and `lib/_http_common.js`
(`maxHeaderPairs`). Retained executable differentials check the frozen Node CLI
against the native CLI. Header/body deadlines, write completion, TLS and application routing have
separate tests and acceptance conditions. The upstream executable differential also
covers chunked trailers, extension grammar and their exact bounds, including
malformed trailers after payload delivery. Finite adversarial cases do not
establish full Node HTTP parser equivalence.

Sources:
- https://crates.io/crates/hyper/1.12.0
- https://github.com/hyperium/hyper/tree/v1.12.0
- https://github.com/nodejs/node/blob/v22.14.0/src/node_http_parser.cc
- https://github.com/nodejs/node/blob/v22.14.0/lib/_http_common.js
- https://github.com/nodejs/node/blob/v22.14.0/deps/undici/src/lib/dispatcher/client-h1.js

Updating Hyper requires regenerating the exact patch and hashes, reviewing
upstream dispatch/flush ordering, rerunning deterministic transport tests and
raw HTTP differentials, and reviewing the client parser separately. Do not
silently drop this feature or convert parser differences into normalized passes.
