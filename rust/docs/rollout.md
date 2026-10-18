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

CI runs this rehearsal after the exact native archive checks on the four native
hosts, both pinned GNU userlands, both native musl SDKs, and i686. The input is
read and verified once; the retained candidate and report use those same owned
bytes. CI checks the rehearsal's archive identity against the previously tested
package and retains the candidate, frozen baseline archive and report. i686
also binds those files and their checksums in its bounded execution evidence.

The rehearsal also checks two installed launch failures before rollback. An
owned synthetic Claude child copies the live status snapshot and settings
overlay, then exits with a nonzero status; a second launch uses an empty PATH
and cannot find Claude. Both schedules check gateway closure, return-time
absence of temporary resources, unchanged complete configuration/history bytes
and metadata, and unchanged history directory entries. A positively checked
loopback sentinel receives no queued or accepted evaluator/upstream connection.
The live copies witness allocation only for the synthetic-child schedule.

This covers successful npm upgrade, rollback and re-upgrade, plus those two
failure schedules, with synthetic file credentials and history. Other startup
failures, partial storage writes, concurrent status ownership, direct-archive
rollback and real Keychain migration remain pending. The baseline archive is
reconstructed and checked against frozen Git sources; it is not the originally
published registry archive. No real Claude task, provider, model or user
configuration is involved.

## Release evidence

Keep the tested tarball immutable. Record its SHA-256 and npm integrity, binary target inspections, compiler/lockfile identity, source commit, complete dependency licenses, and installed tests for every supported target. Production manifests require evidence tied to the same clean source commit; a private feasibility manifest cannot be made publishable by editing `private` alone.

Use the native release check and verifier described in [release tooling](../distribution/release-schema.md). Acceptance of a submission and public registry availability are separate states. An uncertain submission is investigated or verified again; it is not automatically republished. Registry metadata, tarball integrity, installed bytes, and command behavior must agree before a release is reported as verified.

A prerelease or cutover additionally needs reviewed parity, representative hardware measurements, explicitly invoked live canaries with recorded Claude/model versions, and the documented installation/rollback matrix. Keep the frozen oracle for the stabilization period specified by that release decision. Removing the JavaScript runtime or tools, changing required checks, versioning, tagging, or publishing belongs to that separately authorized cutover work.

For an actual rollback, finish active native launches, install the retained exact previous artifact, and check `--version`, redacted configuration output, and historical session output before launching Claude again. Restore an intentional configuration edit from its separately retained backup only when needed; reinstalling the executable does not require deleting credentials or session logs. The [library integration guide](embedding.md) covers the separate ESM embedding migration.

## Explicit direct-artifact recovery rehearsal

`cargo xtask upgrade-rollback --direct CANDIDATE_DIRECT_ARCHIVE --direct-baseline PREVIOUS_DIRECT_ARCHIVE --output FRESH_DIRECTORY`
adds an offline direct-to-direct rehearsal. Both archives and sidecars must verify,
target the current host and contain different executable bytes. It installs only
inside one private scratch path and checks the complete payload tree, bytes and
full file modes after each replacement, before that stage's first command.
Later commands rely on the private installation having no concurrent owner.
The rehearsal exercises the two synthetic failed-startup schedules, then rolls
back and re-upgrades while retaining the same synthetic configuration and history. Direct mode does not invoke npm, Node or Git; full installed lifecycle
smoke remains a separate check. The distinct direct report is not automatically
admitted by existing npm CI consumers.

The retained archives can have the same private package version: this rehearses
physical artifact rollback, not published-version downgrade. It does not qualify
partial storage writes, concurrent ownership, real Keychain/provider behavior,
other platforms, release provenance or cutover. Retained archives and failure
reports stay in the new output directory; owned temporary installations are
removed by the existing scratch cleanup policy, without a process-tree or crash
guarantee. No user installation or saved user configuration is edited.
