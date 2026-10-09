# Native release verification

The native source tools run from `rust/` using the pinned toolchain:

```sh
cargo xtask release-check source vVERSION
cargo xtask release-pack materials
cargo xtask release-pack vVERSION --inputs ../release-evidence/inputs.json --output ../dist
# Exercise these unchanged candidate archives on every qualified target.
cargo xtask release-pack authorize ../dist --inputs ../release-evidence/final-inputs.json
cargo xtask release-check archive vVERSION
cargo xtask release-verify preflight vVERSION --archive ../dist/claude-autorouter-VERSION.tgz --report ../release-evidence/preflight.json
cargo xtask release-verify submitted vVERSION --archive ../dist/claude-autorouter-VERSION.tgz --report ../release-evidence/submitted.json
cargo xtask release-verify verify vVERSION --archive ../dist/claude-autorouter-VERSION.tgz --report ../release-evidence/verified.json
```

Assembly and authorization are offline and never run builds, installers, measurements or provider calls. These commands never publish, move a dist-tag, create a version/tag, or edit user configuration. `preflight`, `verify`, and `artifact-source` make read-only registry/GitHub calls when explicitly invoked; `verify` also runs an isolated npm installation and the installed CLI's help/version. No such calls are part of the unit tests.

The source check requires an exact version-tag/HEAD match, ancestry from `origin/main`, and a clean checkout. The archive check retains the compressed and expanded 32 MiB bounds, rejects links, traversal, duplicate paths, altered checksums and undeclared files, and verifies the runtime documentation baseline. Package identity, repository and public-registry settings must match the established package.

`package` currently produces **private feasibility artifacts**, with build schema 1 and `release_approved: false`. Those artifacts deliberately fail the production release checks. Creating a public manifest or recomputing an archive checksum does not turn schema 1 into a qualified release. `release-pack` is the separate production candidate assembler. It fails closed until the rewrite's required platform, compatibility, performance, license and lifecycle evidence is available. Current pending matrices and host-only reports cannot satisfy it. Distribution cutover remains separate work.

## Production build manifest

A production archive contains `build-manifest.json` with:

- `schema_version: 2`, `kind: "native_npm_release"`, `qualification_approved: true`, `release_approved: false`, and the exact package `version`.
- `source`: `commit` (40 lowercase hexadecimal characters), `dirty: false`, `cargo_lock_sha256`, and `provenance: "ci-source-build"`.
- `qualification`: `compatibility`, `performance`, `platforms`, `licenses`, and `installed_lifecycle` entries, each with `passed: true`, the retained report's `report_sha256`, and the same `source_commit`.
- `approval`: the source qualification review's `record_sha256`, `approved: true` and matching `source_commit`. This is not final archive authorization.
- `files`: every file except the manifest itself, each with exact `path`, `sha256`, byte count in `bytes`, and permission bits in `mode`.
- `artifacts`: each native executable's `target`, `path`, `sha256`, matching `source_commit` and `cargo_lock_sha256`, the hash of retained CI binary provenance in `provenance_sha256`, and the executable-header `inspection` produced by the native packager.

The included `platforms.json` must declare `status: "release_matrix_approved"`, have no unresolved baseline architectures, and qualify every target. Every declared target needs a corresponding inspected executable. Current checked-in platform qualification remains pending.

The file allowlist includes the reviewed shell dispatcher, native executables, license material, public package documentation, and these manifests. JavaScript product runtime files, fixtures, private results, credentials, dependency caches, and agent instructions are excluded. Required public guides remain `docs/reference.md`, `docs/development.md`, `docs/releasing.md`, and `docs/ollama-evaluation.md`.

Embedded qualification cannot authorize publication: every candidate retains `release_approved: false`. Final authorization is external so its installed evidence can bind the final archive hash without a circular dependency. These fields are evidence declarations, not a signature or proof that measurements occurred. The trusted CI source build, retained qualification reports, exact archive hash, and original workflow artifact identity are the review boundary. Tests use clearly synthetic declarations and executable headers to verify rejection behavior; they do not establish a qualified production artifact.

## Candidate inputs and direct archives

`release-pack vVERSION --inputs FILE --output FRESH_DIRECTORY` requires an existing output parent and refuses to replace any directory or symlink. It reads only explicit inputs and the public source allowlist. Paths in descriptors resolve relative to the input JSON file; absolute paths are permitted. Every material descriptor has `path` and its lowercase `sha256`. Input JSON is bounded to 2 MiB, individual retained evidence to 128 MiB and all evidence verification to 512 MiB. The compressed and expanded 32 MiB caps apply separately to every archive, including the complete npm target matrix; the assembler does not increase them when the matrix cannot fit.

The input document has `schema_version: 1`, `kind: "native_release_inputs"`, and:

| Field | Required material |
| --- | --- |
| `platforms` | Descriptor for the complete reviewed matrix |
| `licenses` | Descriptor for reviewed UTF-8 `THIRD-PARTY-LICENSES.txt` |
| `qualification` | One report descriptor for each of the five gates |
| `artifacts` | Rows with `target`, a `binary` descriptor and a `provenance` descriptor |
| `approval` | Descriptor for a separate source qualification review |

The supplied matrix preserves the checked-in `baseline`, archive caps and every checked-in target, including noncandidate baseline targets. It qualifies each target exactly once. Each formerly unresolved architecture needs a `baseline_resolutions` row containing `architecture`, a retained `evidence_sha256`, `review_reference`, and either `result: "qualified"` with a matrix `target`, or `result: "baseline_unavailable"` with a documented `reason`. The latter requires evidence that the baseline itself cannot support it; it does not permit removing a working baseline architecture to fit an archive. Merely emptying `unresolved_baseline_architectures` does not qualify the matrix.

Each report has `schema_version: 1`, `kind: "native_release_qualification"`, its `gate`, `source_commit`, `cargo_lock_sha256`, `passed: true`, `complete: true`, and nonempty `evidence` descriptors. Its `artifacts` object maps every target to exact `binary_sha256` and `provenance_sha256`. The following `checks` must all be true:

| Gate | Required checks |
| --- | --- |
| `compatibility` | `assertion_mapping_complete`, `differential_parity`, `protocol_bytes`, `privacy`, `ordinary_checks` |
| `performance` | `predeclared_thresholds`, `representative_hardware`, `matched_baseline`, `required_latency_memory_gates`, `paired_live_task_quality` |
| `platforms` | `oldest_os_libc`, `dynamic_dependencies_cpu`, `tls_custom_ca`, `claude_available`, `terminal_signals`, `wsl`, `aggregate_archive_caps` |
| `licenses` | `all_dependency_texts`, `vendored_materials`, `license_review` |
| `installed_lifecycle` | `offline_npm_no_scripts`, `no_node_runtime`, `direct_archive`, `launch_cleanup`, `config_history_migration`, `upgrade_rollback` |

The platform report also binds `platforms_sha256` and the exact `targets` list; the license report binds `licenses_sha256`; installed lifecycle covers the same target list. These are review envelopes around retained original evidence, not replacements for tests or measurements. In particular, mock timings cannot satisfy representative hardware or paired live task-quality checks. Preassembly lifecycle evidence qualifies the payload and distribution implementation; it is followed by exact final archive checks below.

Each binary provenance document has `schema_version: 1`, `kind: "native_binary_provenance"`, `provenance: "ci-source-build"`, exact `source_commit`, `cargo_lock_sha256`, `target`, `binary_sha256`, `repository: "frapposelli/claude-autorouter"`, `profile: "release"`, `portable_cpu: true`, plus nonempty `rustc`, `workflow_run`, and a `rustflags` array. Native CPU flags are rejected. Executable headers, architecture and dynamic library constraints are independently inspected; a filename or declaration alone cannot establish them. Header inspection still does not establish OS execution or CPU portability, which require retained platform evidence and trusted CI build provenance.

The separate qualification approval document has `schema_version: 1`, `kind: "native_release_approval"`, `approved: true`, `source_commit`, `cargo_lock_sha256`, `version`, `platforms_sha256`, `licenses_sha256`, `public_files_sha256`, `reviewer`, `review_reference`, the exact `artifacts` hash map, and `qualification` mapping gate names to report hashes. All `reviewed` entries `baseline_platform_scope`, `runtime_documentation`, `live_canaries` and `license_review` must be true. `release-pack materials` prints the public files and deterministic `public_files_sha256` for review without generating approval. The digest is SHA256 of the pretty JSON file declarations, ordered by path, with a final newline. It binds the original public documentation and source package manifest before the native manifest transformation.

Assembly preserves public documentation, removes Node engines/development dependencies/scripts from the output manifest, and supplies the reviewed shell dispatcher and all qualified binaries. The source manifest is never changed. The output contains `claude-autorouter-VERSION.tgz`, one `claude-autorouter-VERSION-TARGET.tar.gz` per target, `release-index.json`, and SHA256 sidecars. Encoding is deterministic gzip/ustar with fixed timestamps. Any encoding/size/verification failure leaves no completed output.

Direct archives contain a `claude-autorouter/` root with `bin/claude-autorouter`, public docs, full license material, the reviewed platform matrix, and schema 2 `kind: "native_direct_release"` manifest. They contain no npm wrapper or Node requirement. Their manifest binds the exact parent npm archive hash and the matching executable. `release-pack verify-direct ARCHIVE` checks a direct archive and sidecar without extracting or executing it. This verifies candidate integrity, not final authorization. Final validation compares every direct payload file against its npm counterpart.

## Final immutable archive authorization

Keep the entire candidate directory unchanged while installed checks run. Final reports must record each archive's actual SHA256; an earlier package with equivalent source does not count. `release-pack authorize DIRECTORY --inputs FILE` consumes reviewed results, creates only `release-authorization.json` and its sidecar, and refuses to overwrite them. It does not perform checks or manufacture approvals.

Final input JSON uses `schema_version: 1`, `kind: "native_release_final_inputs"`, and `report`/`approval` material descriptors. The report has `schema_version: 1`, `kind: "native_release_final_qualification"`, `passed: true`, `complete: true`, exact `source` copied from the index, `release_index_sha256`, and `instances`. There must be one instance for every target installed from npm and one for every direct target archive. Each instance has `filename`, `target`, `distribution` (`npm` or `direct`), exact `sha256`, `passed: true`, nonempty retained `evidence` descriptors, and these true `checks`: `byte_integrity`, `help_version`, `no_node_runtime`, `launch_cleanup`, `terminal_signals`, `tls_custom_ca`. npm instances additionally require `offline_install_no_scripts` and `upgrade_rollback`.

The final review has `schema_version: 1`, `kind: "native_release_final_approval"`, `approved: true`, matching `source`, `release_index_sha256`, `report_sha256`, `reviewer` and `review_reference`. Authorization retains only hashes and explicit metadata, excluding evidence paths and raw diagnostics. Both native `release-check archive` and `release-verify` require this external authorization and recheck the exact complete archive set and sidecars. Missing direct checks, stale approvals, changed bytes, private feasibility manifests, undeclared directory files and incomplete target coverage fail before registry access. Copy the complete immutable release directory when retaining workflow artifacts.

The external authorization is a review record, not a cryptographic signature. Trusted workflow identity and retained original evidence remain required. The tests use synthetic records solely to establish rejection and consistency behavior; no production authorization is supplied by this checkout.

## Submission and availability

Registry metadata and the full packument are checked independently with cache-bypassing requests. Conflicting immutable evidence fails. An identical existing version skips publication. A workflow retry with unknown submission state never guesses that another publication is safe. New stable versions must be newer than the current stable `latest`; prereleases target `next`.

Verification checks registry tarball length, SHA256 and SHA512 integrity, then compares every installed file before executing the installed command. It checks the command link, help and version from an unrelated directory with an isolated npm prefix, cache and config, disabled install scripts, and no inherited API credentials. A newer dist-tag can supersede the exact verified version without any tag mutation.

Unavailable metadata, delayed tags/tarballs and installation outages remain pending. Polling has a total deadline, capped backoff and a bounded timeline. A pending report preserves the original archive identity and instructs the operator to verify again rather than submit a duplicate release.

`artifact-source TAG --run-id N --artifact-id N` checks the original tag-triggered publishing workflow, repository, commit, artifact identity and expiry. It requires `GH_TOKEN` only for that explicit read-only operation.

## Historical evidence

`release-verify inspect-legacy TAG --archive PATH` explicitly reads supported historical JavaScript package archives without executing them. `verify-historical` explicitly performs registry and isolated-install verification for those archives. Historical inspection cannot authorize native publication; the native preflight requires production schema 2. This preserves older package evidence without giving the new product a Node runtime fallback.

`release-verify notes TAG --archive PATH --report PATH` writes release notes from the checked source and immutable archive identity. GitHub output values are bounded to single-line validated metadata. Reports contain states, identifiers, hashes and metadata, never response bodies, credentials or command diagnostics.
