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

## Native tooling

`cargo xtask evaluate`, `evaluate-ollama`, `test-ollama-routing`, `live-validation`, and `context-probe` replace the corresponding source tools. Use each command's `--help` for its arguments. They retain separate availability, transport, classification, policy, and task-quality results. Explicit `--env-file=.env` and `--env-file-if-exists=.env` options load an environment file without shell expansion; ordinary product launches never implicitly load a project `.env`.

These evaluation/probe commands are opt-in. They can contact the selected evaluator, and live validation calls Claude. A context probe can execute configured MCP servers or startup customizations even when inference is stubbed. Existing evaluator quality failures remain visible. The [paired live comparison](parity/live-comparison.md) runs the same version 2 coding-repair fixture through either native or frozen Node using `--engine`; its outcome is not interchangeable with the original historical version 1 fixture. Implementing this mode does not establish live-provider results.

The synthetic runtime-neutral driver uses the same mock HTTP services and full CLI process boundary for Node and Rust:

```sh
cargo xtask benchmark --output artifacts/rust-rewrite/validation-run --validate
```

Omitting `--validate` runs the declared measurement protocol. Choose a fresh output directory for each run. The [local protocol](parity/local-benchmark-v1.json) freezes sample counts, ordering, uncertainty, and noise floors before measurement. Its initial workload coverage is incomplete; exploratory workstation results cannot approve the full [performance gates](parity/performance-gates.json). True peak RSS, allocations, the remaining workloads, and representative hardware require separate evidence.

The [measured comparisons and profiling notes](docs/performance.md) retain all three runs, including earlier failures. The third run meets measured CPU, throughput and latency nonregression targets on the developer workstation. Processing-only latency, true peak memory, additional workloads and representative hardware still keep the overall performance gate incomplete.

## Distribution and remaining gates

Native feasibility archives contain a POSIX dispatcher, prebuilt binaries, checksums, dependency licenses, and a source/build manifest. They install offline with `--ignore-scripts`, perform no download, and run without Node on `PATH`. `cargo xtask package --binary TARGET=PATH --output NEW_DIRECTORY --smoke` creates and tests a private candidate. The existing compressed and expanded 32 MiB archive caps remain unchanged.

The [qualification and rollback guide](docs/rollout.md) describes the isolated `cargo xtask upgrade-rollback` rehearsal and the evidence needed for a later cutover.

`cargo xtask release-pack` assembles qualified candidates, verifies per-target native archives, and records separate final authorization after exact installed-archive checks. The [release input schema](distribution/release-schema.md) specifies its commands and required evidence. Embedded candidate declarations cannot authorize publication; the current pending platform/performance evidence is insufficient for a production release.

The [platform matrix](distribution/platforms.json) records unverified targets and OS/libc/TLS questions. A passing host archive does not qualify the full matrix. Exact installed CLI tests can also be run with `AUTOROUTER_TEST_EXECUTABLE` set to the absolute installed dispatcher path.

Full completion still requires assertion-level parity coverage, remaining JSON/configuration and locale edge cases, parser/timing qualification, all declared resource/performance workloads, the supported platform matrix, dependency/license and build-provenance review, upgrade/rollback rehearsal, and explicitly invoked live canaries. Release tools, release artifacts, and cutover are separate gates. No version bump, release tag, publication, or change to the default implementation is implied by building this workspace.
