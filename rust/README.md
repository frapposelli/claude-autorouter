# Native AutoRouter development

This workspace implements the [Rust rewrite plan](../docs/rust-rewrite-plan.md) alongside the shipping JavaScript package. The development executable contains the product commands and does not start Node. The npm release remains on JavaScript while compatibility, distribution, performance, and live qualification are completed.

Run all Cargo commands from this directory so `rust-toolchain.toml` selects the pinned compiler:

```sh
cargo build --release --locked --package claude-autorouter
./target/release/claude-autorouter --help
./target/release/claude-autorouter --version
```

Select an implementation for a complete launch. Running the native `claude`, `serve`, `setup`, or configuration commands uses the existing configuration and storage paths. Contributor tests use isolated synthetic homes. Do not run two launchers against one temporary status directory or switch an engine during an active Claude task.

## Workspace

| Crate | Responsibility |
| --- | --- |
| `autorouter-core` | JavaScript-compatible JSON semantics, configuration policy, catalog and compatibility, task state, redaction, routing policy, status/history, and pricing. |
| `autorouter-runtime` | HTTP/TLS, evaluation and count transports, independent cancellation, completion evidence, gateway, private storage, Keychain adapter, and local setup/diagnostics. |
| `claude-autorouter` | Native command dispatch, terminal input, Claude launch, signals, and resource cleanup. Help/version/status rendering avoid creating an async runtime. |
| `xtask` | Frozen-reference comparisons, evaluation/probe tools, native packaging, and synthetic benchmarks. |

Request and provider model identities use exact UTF-16 strings. The iterative JSON arena preserves opaque data and JavaScript serialization behavior; its explicitly lossy metadata projection must never decide compatibility, identity, or forwarded bytes. Routing selects a model, while continuation state is committed only after clean provider completion and a successful downstream writer flush. Selection and observed model metadata alone do not confirm execution.

The HTTP/1 adapter pins Hyper. Its completion registry and parser compatibility changes have dedicated tests; changes to that dependency require re-running the transport and raw-parser comparisons. The product uses one current-thread Tokio executor, pooled upstream clients, bounded shared classification, and separate asynchronous status/history writers.

The [library integration guide](docs/embedding.md) documents the Rust API, the selected/unconfirmed embedding behavior, and the explicit migration boundary for ESM consumers.

## Validation

```sh
cargo fetch --locked
cargo metadata --format-version 1 --locked --offline > /dev/null
cargo fmt --check
node vendor/verify-hyper.mjs
node vendor/verify-hyper-util.mjs
node vendor/verify-production-features.mjs --self-test
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo xtask freeze-reference
cargo xtask parity --report artifacts/rust-rewrite/parity.json
node parity/check-config-commands.mjs
node parity/check-gateway.mjs
node parity/check-http-parser.mjs
```

Run the fetch step without `--target`: the offline license inventory reads dependencies for every target in the lockfile, including crates a host-only build does not download. Repeat it when the lockfile changes. Packaging keeps metadata collection offline and rejects missing license material; it does not fetch dependencies itself. [Cargo documents this all-target fetch behavior](https://doc.rust-lang.org/cargo/commands/cargo-fetch.html).

Source builds also need a C compiler, Make and Perl for vendored OpenSSL. The pinned crate builds and links its bundled native library; packaged executables do not need those build tools. Keep `OPENSSL_NO_VENDOR` unset when producing a candidate, and retain the artifact's linkage checks as part of platform qualification. Native OpenSSL and its bundled build-tool license texts are included separately from the Rust wrapper licenses.

The tests bind loopback sockets. They use fake credentials and local mocks, without a provider call or model download. The last three commands are temporary reference-test drivers; neither the native application nor its distribution invokes them. Reference files are verified against commit `ea930c247626ce2af5ccdad721b5121417bf4ad8` before comparison. See the [parity inventory](parity/coverage.json) and [reference protocol](parity/README.md).

Run the existing contributor checks from the repository root as well:

```sh
npm ci --ignore-scripts --no-audit --no-fund
npm run check
npm test
npm run test:package
```

The four existing Node/OS CI jobs remain required. Native CI adds compiler, contract, integration, and executable comparison checks; it does not invoke live tools or impose workstation timing thresholds.

The experimental connection lease in vendored hyper-util is enabled only by a
runtime test dependency. CI records the ordinary release build's Cargo JSON
messages and rejects an artifact compiled with `node-http1-request-lease`.
To repeat that check locally:

```sh
mkdir -p ../artifacts/rust-rewrite
cargo build --release --locked --package claude-autorouter --message-format=json > ../artifacts/rust-rewrite/production-build.jsonl
node vendor/verify-production-features.mjs --build-report ../artifacts/rust-rewrite/production-build.jsonl
```

The separate [sanitizer fuzz workspace](fuzz/README.md) exercises JSON edits,
redaction, observer framing and history parsing with synthetic seeds. Its pinned
nightly/tooling and bounded campaigns are opt-in; the production workspace and
ordinary CI do not include libFuzzer.

## Native tooling

`cargo xtask evaluate`, `evaluate-ollama`, `test-ollama-routing`, `live-validation`, and `context-probe` replace the corresponding source tools. Use each command's `--help` for its arguments. They retain separate availability, transport, classification, policy, and task-quality results. Explicit `--env-file=.env` and `--env-file-if-exists=.env` options load an environment file without shell expansion; ordinary product launches never implicitly load a project `.env`.

These evaluation/probe commands are opt-in. They can contact the selected evaluator, and live validation calls Claude. A context probe can execute configured MCP servers or startup customizations even when inference is stubbed. Existing evaluator quality failures remain visible. The [paired live comparison](parity/live-comparison.md) runs the same version 2 coding-repair fixture through either native or frozen Node using `--engine`; its outcome is not interchangeable with the original historical version 1 fixture. Implementing this mode does not establish live-provider results.

The synthetic runtime-neutral driver uses the same mock HTTP services and full CLI process boundary for Node and Rust:

```sh
cargo xtask benchmark --output artifacts/rust-rewrite/validation-run --validate
```

Omitting `--validate` runs the declared measurement protocol. Choose a fresh output directory for each run. The [local protocol](parity/local-benchmark-v1.json) freezes sample counts, ordering, uncertainty, and noise floors before measurement. Its initial workload coverage is incomplete; exploratory workstation results cannot approve the full [performance gates](parity/performance-gates.json). True peak RSS, allocations, the remaining workloads, and representative hardware require separate evidence.

Add `--protocol v2` to select the [streaming protocol](parity/local-benchmark-v2.json). It measures the first nonempty response-body frame separately from headers and adds burst and paced SSE workloads at concurrency 1/8/32/128. The driver checks complete response bytes, evaluator/upstream counts, and stream producer cleanup before reporting results. Observed data-frame gaps can reflect HTTP coalescing or fragmentation. Version 2 requires fresh matched runs; its samples must not be pooled with version 1. `--validate` exercises the fixtures without retaining numerical performance results.

The [v3 protocol](parity/local-benchmark-v3.json), selected with `--protocol v3`, adds one isolated resource collector per gateway. After cleanup it records the kernel's full-lifetime peak RSS and CPU usage, excluding the driver, mocks, collector and previous workloads. Waited descendants contribute CPU and the largest individual RSS; this is not summed process-tree memory. Lifetime measurements include startup, warmup and shutdown and remain separate from interval CPU and sampled RSS. Ready/cleanup times include collector overhead; startup commands are still launched directly. New v3 comparisons require five fresh matched rounds; `--validate` retains collection success without numerical resource values. Allocations, descriptor counts and the remaining performance gates still need qualification.

Use `cargo xtask benchmark --compare-legacy REPORT.json` to compare historical JavaScript router benchmark results without launching workloads. The input is a JSON object containing `baseline` and `candidate` reports, both with the original schema 1/type markers or both without markers. The command prints the original comparison gates and exits unsuccessfully when they fail; it leaves the input unchanged and creates no output files. Inputs must be regular files at most 16 MiB with finite metrics and at most 4,096 rows per report. Historical heap and event-loop gates remain separate from the native v1/v2/v3 protocols; native or mixed report formats are rejected.

The [measured comparisons and profiling notes](docs/performance.md) retain all three runs, including earlier failures. The third run meets measured CPU, throughput and latency nonregression targets on the developer workstation. Processing-only latency, true peak memory, additional workloads and representative hardware still keep the overall performance gate incomplete.

The [synthetic storage tool](docs/storage-benchmark.md) provides explicit native or frozen-reference validation of status snapshots and session logs. Numerical collection uses a separate opt-in mode and does not qualify the full performance gate.

## Distribution and remaining gates

Native feasibility archives contain a POSIX dispatcher, prebuilt binaries, checksums, dependency licenses, and a source/build manifest. They install offline with `--ignore-scripts`, perform no download, and run without Node on `PATH`. `cargo xtask package --binary TARGET=PATH --output NEW_DIRECTORY --smoke` creates and tests a private candidate. The [reviewed native limits](distribution/expanded-cap-proposal.md) are 32 MiB compressed, 64 MiB expanded, 32 MiB per file and 256 entries. Historical JavaScript archives and source bundles retain 32 MiB expansion; the complete qualified target matrix must still fit.

The [qualification and rollback guide](docs/rollout.md) describes the isolated `cargo xtask upgrade-rollback` rehearsal and the evidence needed for a later cutover.

`cargo xtask release-pack` assembles qualified candidates, verifies per-target native archives, and records separate final authorization after exact installed-archive checks. The [release input schema](distribution/release-schema.md) specifies its commands and required evidence. Embedded candidate declarations cannot authorize publication; the current pending platform/performance evidence is insufficient for a production release.

The [platform matrix](distribution/platforms.json) records unverified targets and OS/libc/TLS questions. A passing host archive does not qualify the full matrix. Exact installed CLI tests can also be run with `AUTOROUTER_TEST_EXECUTABLE` set to the absolute installed dispatcher path.

Full completion still requires assertion-level parity coverage, remaining JSON/configuration and locale edge cases, parser/timing qualification, all declared resource/performance workloads, the supported platform matrix, dependency/license and build-provenance review, upgrade/rollback rehearsal, and explicitly invoked live canaries. Release tools, release artifacts, and cutover are separate gates. No version bump, release tag, publication, or change to the default implementation is implied by building this workspace.
