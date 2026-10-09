# Native qualification and rollback

The shipping package remains JavaScript version 0.5.2. Building this workspace, passing a host smoke test, or producing a private feasibility archive does not authorize a release or a change to the default implementation. The [rewrite plan](../../docs/rust-rewrite-plan.md), [parity inventory](../parity/coverage.json), [performance protocol](../parity/performance-gates.json), and [platform matrix](../distribution/platforms.json) define the remaining qualification work.

Select an implementation for a complete Claude launch. Each launcher owns its local gateway, temporary authentication, settings overlay, and cleanup. Finish or cancel that launch before starting the other implementation. Both versions use the same configuration and history paths; copying credentials, renaming the config, or silently converting historical prices is not part of switching engines.

## Isolated upgrade and rollback rehearsal

Run this against an exact local native archive from the repository root's paths:

```sh
cd rust
cargo xtask upgrade-rollback \
  --native artifacts/rust-rewrite/package-NEW/claude-autorouter-0.5.2.tgz \
  --output artifacts/rust-rewrite/upgrade-rollback-NEW
```

The command first verifies the native archive and refuses an existing output directory. It constructs the baseline npm archive from the frozen Git commit's public file allowlist, checks every packed file against its Git content, then installs the baseline, native candidate, baseline rollback, and native candidate again into one private npm prefix. All installs are offline and use `--ignore-scripts`; each installation's files are checked against the exact archive before execution.

The rehearsal uses one synthetic configuration with file credentials, plus existing schema 1/2 history and recorded pricing provenance. It checks read-only inspection, edits in both directions, unrelated settings and credential retention, private file permissions, history interpretation and unchanged historical bytes. Native commands run without Node on `PATH`; npm and the frozen baseline still require Node. Temporary homes and the installation prefix are removed, while the input archives and a metadata-only report are retained under the chosen output directory.

This check does not touch real Keychain entries or call Claude, an evaluator, or a provider. Its result qualifies only the current host and the tested artifacts. Installed launcher/terminal cleanup, real Keychain migration, the other platform targets, and live canaries retain their own evidence requirements. Failed rehearsals cannot be counted as successful rollback evidence.

## Release evidence

Keep the tested tarball immutable. Record its SHA-256 and npm integrity, binary target inspections, compiler/lockfile identity, source commit, complete dependency licenses, and installed tests for every supported target. Production manifests require evidence tied to the same clean source commit; a private feasibility manifest cannot be made publishable by editing `private` alone.

Use the native release check and verifier described in [release tooling](../distribution/release-schema.md). Acceptance of a submission and public registry availability are separate states. An uncertain submission is investigated or verified again; it is not automatically republished. Registry metadata, tarball integrity, installed bytes, and command behavior must agree before a release is reported as verified.

A prerelease or cutover additionally needs reviewed parity, representative hardware measurements, explicitly invoked live canaries with recorded Claude/model versions, and the documented installation/rollback matrix. Keep the frozen oracle for the stabilization period specified by that release decision. Removing the JavaScript runtime or tools, changing required checks, versioning, tagging, or publishing belongs to that separately authorized cutover work.

For an actual rollback, finish active native launches, install the retained exact previous artifact, and check `--version`, redacted configuration output, and historical session output before launching Claude again. Restore an intentional configuration edit from its separately retained backup only when needed; reinstalling the executable does not require deleting credentials or session logs. The [library integration guide](embedding.md) covers the separate ESM embedding migration.
