# Native distribution feasibility

This directory defines an experimental archive format. The shipping JavaScript npm package is unchanged. Candidate archives are private, use the existing version, and must not be published. Platform and performance gates remain pending.

The POSIX dispatcher resolves npm's symbolic links, selects a bundled executable and uses `exec` with the original arguments. It performs no downloads and has no Node fallback. Package verification rejects links, traversal, duplicate files, incorrect binary architecture, checksum drift and non-executable launchers. Both compressed and expanded archives retain the existing 32 MiB bounds.

The initial macOS ARM64/x86_64 and Linux ARM64/x86_64 candidates do not cover every platform where the existing unrestricted JavaScript package can run. Node 22 additionally lists Linux armv7, ppc64le and s390x, plus experimental architectures. These remain audit items in [platforms.json](platforms.json), alongside Claude executable availability. See the [Node 22 platform table](https://raw.githubusercontent.com/nodejs/node/v22.x/BUILDING.md).

Rust documents macOS 11.0 ARM64 and 10.12 x86_64 minima; its deployment target can raise those requirements. The produced binary and every dependency still need inspection and execution on the oldest claimed system. See [Apple target requirements](https://doc.rust-lang.org/rustc/platform-support/apple-darwin.html). GNU target defaults and musl target availability likewise do not establish the minimum libc/kernel of a linked product; the [Rust target table](https://doc.rust-lang.org/rustc/platform-support.html) is an input to qualification, not product support evidence.

npm's `bin` mapping provides Unix symbolic links. The isolated smoke check installs the exact archive offline with `--ignore-scripts`, then runs the symlink from an unrelated directory with Node absent from PATH. The dispatcher resolves standard utilities from `/usr/bin` or `/bin` without altering the child's PATH. The same native CLI integration suite then exercises the installed executable, including launcher signals, credentials, settings, configuration and cleanup. See [npm executable mappings](https://docs.npmjs.com/cli/v11/configuring-npm/package-json/#bin).

From the Rust workspace, assemble and verify an explicitly supplied host artifact:

```sh
cargo build --release --locked --package claude-autorouter
cargo xtask package --binary aarch64-apple-darwin=rust/target/release/claude-autorouter --output artifacts/rust-rewrite/package-NEW --smoke
cargo xtask package-verify artifacts/rust-rewrite/package-NEW/claude-autorouter-0.5.2.tgz
cargo xtask package-smoke artifacts/rust-rewrite/package-NEW/claude-autorouter-0.5.2.tgz
```

Choose the actual target of the supplied executable; filenames do not override architecture inspection. Paths resolve against the repository root and assembly refuses to overwrite an existing destination. Additional `--binary TARGET=PATH` arguments include other independently built artifacts. Smoke checks require the host artifact, npm for installation, and the Rust toolchain for the shared integration suite; the installed CLI itself needs no Node runtime. System certificate-store and loopback access are needed for launcher checks.

Local package assembly records binary and Cargo lock hashes, source revision/dirty state, license material and target inspection. These are artifact-integrity facts; a dirty-tree feasibility build is not release provenance. A complete release still requires every supported target, exact installed launcher lifecycle, license review, an immutable archive tested once, registry integrity verification, rollout and rollback qualification.

A source-only transfer bundle is also available:

```sh
cargo xtask benchmark-bundle --output artifacts/rust-rewrite/benchmark-source-NEW.tar.gz
```

It includes the native workspace, lockfile, synthetic fixtures, public hardware
notes, vendored parser source/licenses, and a per-file checksum manifest. It
excludes configuration, credentials, captured traffic, logs, build artifacts,
and JavaScript runtime sources. The destination still needs the pinned Rust
toolchain and locked Cargo dependencies; this is not a standalone executable
or an offline dependency vendor. Building it and requesting tool help does
not invoke an evaluator. Actual evaluator and Claude-startup commands retain
their explicit opt-in requirements.
