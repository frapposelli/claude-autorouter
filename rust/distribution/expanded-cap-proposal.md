# Proposed native archive bounds

Status: historical proposal accepted for the Rust native readers. The implemented native policy is **32 MiB compressed / 64 MiB expanded / 32 MiB per file / 256 entries**; historical JavaScript and source policies retain 32 MiB expansion. The measurements below describe the unchanged isolated experiment, not the later streaming implementation. Neither approves a release or narrows the supported platform baseline.

Prefer a self-contained npm archive with a **32 MiB compressed limit, 64 MiB expanded tar limit, and 32 MiB individual-file limit**, subject to the implementation and full-matrix checks below. This preserves the existing offline, `--ignore-scripts`, dependency-free installation contract. It is sufficient for the four measured binaries; additional baseline platforms remain unresolved.

The [retained measurement](../parity/measurements/expanded-cap-8e94-summary.json) binds the source, isolated patch, tool, archive, binary digests and logs. A detached `8e94a9b` worktree changed only its experimental archive helper, private assembler and embedded experimental platform metadata. The historical size-profile experiment and main working-tree caps were untouched.

| Measurement | Result |
| --- | ---: |
| GNU glibc 2.28 Linux x64/ARM64 and macOS x64/ARM64 binary bytes | 57,183,424 |
| Actual four-target `.tgz` | 23,943,704 bytes |
| Actual expanded tar | 58,821,120 bytes |
| Margin under 32 MiB compressed | 9,610,728 bytes |
| Margin under 64 MiB expanded | 8,287,744 bytes |
| macOS ARM installed lifecycle smoke | 7 suites, 66 passing tests |
| Isolated archive safety tests | 5 passing tests |

Archive SHA256: `65e12e5a681a59858a4e8d839db6c1cb5f87d9a89bf53dc41951631f54711f16`. All four bundled binaries match their immutable CI bytes. Only the host ARM binary ran. Installation was offline with scripts disabled, a private prefix containing spaces/quote/dollar, unrelated working directory, npm command symlink and a runtime PATH without Node. The matching `8e94a9b` tests ran against the installed binary; later tests are outside this evidence.

The unchanged verifier rejects this exact archive with `Expanded archive exceeds 32 MiB`. The isolated verifier accepts it while retaining the compressed limit and rejecting a file above 32 MiB. Traversal, links, duplicate paths, invalid checksums and truncation still fail. The archive remains private and unapproved.

This is an 11-file feasibility package. A production schema-2 archive must also contain the complete public documentation and reviewed provenance materials. The isolated public-file inventory contains 22 files totaling 468,641 bytes; that inventory is useful context, not a measurement of a qualified final archive. Every final npm/direct archive must be measured and authorized by its exact hash. The remaining 8.3 MB of expanded margin does not establish room for the complete unresolved platform matrix or future crypto/runtime growth.

## Distribution alternatives

| Strategy | Installation and verification implications |
| --- | --- |
| One self-contained archive | One immutable artifact, existing POSIX dispatcher and offline installation remain usable without dependency resolution. Every install carries the four binaries. Current bytes fit the proposed limits; the complete supported matrix must still fit. |
| Platform optional packages | Reduces each platform's downloaded payload, but adds dependency resolution, multiple immutable artifacts/version identities, platform/libc selection, missing-dependency behavior and publication/rollback ordering. Empty-cache offline installation of only the parent tarball no longer establishes executable availability. |

npm supports `os`, `cpu` and Linux `libc` selectors, but optional dependencies may be absent without failing the parent installation, including when explicitly omitted. Handling their absence remains the application's responsibility. [npm package manifest documentation](https://docs.npmjs.com/cli/v11/configuring-npm/package-json/#optionaldependencies)

A retained synthetic npm 10.9.2 control used an empty cache and a parent tarball declaring one unavailable optional native package. `npm install --offline --ignore-scripts` exited successfully and installed the parent without the native dependency. It declared no scripts and executed no runtime. This is a missing-dependency control, not a complete optional-package prototype. Offline mode prohibits network requests; ignoring scripts does not provide missing dependency bytes. [npm configuration documentation](https://docs.npmjs.com/cli/v11/using-npm/config/#offline)

An optional-package design therefore needs an explicit revision of the dependency/offline contract, pinned hashes for every package, failure diagnostics when the host artifact is absent, separate install fixtures for omitted/missing/corrupt/wrong-platform dependencies, and atomic-enough release/rollback procedures across packages. Bundling all optional payloads back into the parent restores offline availability but does not remove the aggregate byte cost. A postinstall downloader is outside the preserved contract.

## Required reader and assembler changes

Use distinct named limits rather than raising `MAX_ARCHIVE` everywhere. Compressed local/network inputs remain 32 MiB; native npm/direct tar expansion becomes 64 MiB; a single file or supplied binary remains 32 MiB. Keep existing tighter JSON, checksum, evidence and request-body limits. Historical JavaScript archive inspection and source-only benchmark bundles retain their existing 32 MiB expanded policy through explicit reader options.

| Source | Required treatment |
| --- | --- |
| `xtask/src/package_archive.rs` | Explicit bounds for native versus historical/source archives; apply expanded bound to multi-member gzip decoding and ustar encoding; reject oversized individual entries before copying them; retain path/type/mode/checksum/end-marker rules. Add a native entry-count bound before accumulating metadata. |
| `xtask/src/package.rs` | Compare minimum expanded size against 64 MiB; retain 32 MiB binary/compressed reads; report compressed/expanded/per-file limits separately. Private assembly, verification, smoke and `package-inspect` must use the correct limit. |
| `xtask/src/release_pack.rs` | Change aggregate binary preflight to the native expanded budget; keep binary/license descriptor reads at 32 MiB. Final encoded archive including documentation/manifests must independently pass both archive limits. Preserve 2 MiB descriptors and 128/512 MiB evidence budgets. |
| `xtask/src/release.rs` | Select native versus explicit historical inspection policy for archive decoding; keep compressed and source-file reads bounded to 32 MiB. Public-file allowlists and native manifest verification remain mandatory. |
| `xtask/src/release_direct.rs` | Use the native expanded policy for direct encoding and verification while preserving 32 MiB compressed/path reads and exact npm-to-direct payload correspondence. |
| `xtask/src/release_authorization.rs` | Apply the same policy to every final npm/direct archive; retain compressed sidecar reads and the 512 MiB release-set expanded budget. Exact final lifecycle evidence and external authorization stay mandatory. |
| `xtask/src/release_install.rs` | Decode verified native archives with the native policy; retain 32 MiB compressed input, installed-file equality and existing command deadlines/output limits. |
| `xtask/src/release_verify.rs` | Registry/GitHub tarball responses remain bounded to 32 MiB, including decoded HTTP transfer content. Metadata retains its 16 MiB bound; never use the tar expansion limit as a network-body limit. |
| `xtask/src/upgrade_rollback.rs` | Native verification/install/replay accepts the new native policy; construction and inspection of frozen JavaScript archives remains at 32 MiB expanded. Verify exact installed bytes at each transition. |
| `xtask/src/bundle.rs` | Explicitly retain the 32 MiB source-only gzip/tar policy despite sharing archive helpers. |
| `distribution/platforms.json`, `distribution/README.md`, `distribution/release-schema.md`, `docs/rust-rewrite-plan.md` | Record separately reviewed caps and remaining matrix blockers. Matrix validation currently compares cap objects exactly; qualification fixtures and hashes must change consistently. Historical reports stay immutable. |

The JavaScript release scripts remain the frozen baseline during migration. Their compressed reads remain 32 MiB and `scripts/release-pack.mjs` retains its historical 32 MiB expansion limit. If they are later asked to verify a native artifact, that is a separate explicit reader change, not implicit compatibility with the Rust tool. Runtime HTTP request bodies, observer/evaluator bounds and benchmark request limits are unrelated and must remain unchanged.

## Memory and acceptance checks

The decoder used by this historical experiment holds the expanded tar and copies every file into an owned map; its caller also holds compressed bytes. Private assembly may additionally retain its original input map during verification. At the proposed ceilings, these logical buffers can total roughly **160 MiB for verification** and **224 MiB while an assembler retains another full file map**, before allocator capacity, metadata and parsed JSON. These are buffer-accounting estimates, not RSS bounds. `Vec` growth may reserve beyond its current length.

One isolated debug-tool verification of the real archive reported **154,632,192 bytes maximum RSS** and **148,144,704 bytes peak memory footprint** on this Mac. The first assembly timing wrapper failed on sandbox-restricted `kern.clockrate` after producing the archive; no assembly RSS result is claimed. Neither observation is a hardware acceptance benchmark or a concurrent process-tree memory bound.

Before adopting the cap, bound native archive entry count (256 accommodates the current schema's at-most-64 targets plus public files and directory records), enforce the 32 MiB file bound before copying, and prevent geometric buffer growth from defeating the intended allocation budget. Keep malformed-archive processing sequential and retain checked integer arithmetic. Avoid retaining a second decoded archive unnecessarily, and assess release-set verification, which can retain multiple direct payload maps under its existing 512 MiB budget. The subsequent implementation streams decoding and encoding, bounds compressed-writer growth and reuses decoded maps. Those later changes are not evidence supplied by this historical experiment.

Required acceptance tests:

Independent review adds three concrete implementation requirements. Historical
inspection currently permits both formats, so its boolean flag cannot directly
select a legacy-only decoder: select the format under explicit bounds and
validate the corresponding manifest. Release-set validation must clamp each
decode to its remaining expanded-byte budget before allocation and reuse the
validated map; the current path decodes direct archives twice and checks its
aggregate budget after accumulation. Encoding must use a bounded compressed
writer, since rejecting an oversized `GzEncoder<Vec>` only after `finish()` can
already allocate the forbidden output.

1. Valid native tar content above 32 MiB and at the permitted boundary succeeds; expanded 64 MiB plus one byte fails, including concatenated gzip members and forged tar lengths. Compressed 32 MiB plus one byte still fails before inflation.
2. Files of 32 MiB are permitted where the schema permits them; 32 MiB plus one byte and excessive native entry counts fail before payload/metadata accumulation. Preserve traversal, links, duplicate names, checksums, truncation, modes and integer-overflow cases.
3. Historical JavaScript archives and source bundles retain their separate limits. Native binaries, HTTP bodies, JSON descriptors and registry responses do not accidentally inherit 64 MiB.
4. Public assembly, release check/verify, direct archives, external final authorization, installed verification and upgrade/rollback agree on the exact limits and hashes. Missing target/approval evidence still rejects the candidate.
5. Assemble the complete reviewed target matrix, then run exact-archive installation/lifecycle checks on each target. Add oversized/adversarial decode memory checks and measure peak allocation/RSS across verify/assembly/release-set paths. Do not infer these results from the single host feasibility smoke.

The four-target experiment supports reviewing this narrow cap change. It does not resolve the full matrix, the recorded `8e94a9b` TLS policy gaps, reproducible builds, license review, real-provider/task-quality/performance gates or release authorization.
