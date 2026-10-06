# Public launch and repository administration

The repository became **public on October 6, 2026**, after [preparation PR #1](https://github.com/frapposelli/claude-autorouter/pull/1) merged at commit `e2bea3397bbcd673b8760105931021b3dfc2ae16`. The public launch created no release tag and published no new npm version. Author names and email addresses remain in the existing history. The project uses Apache-2.0 and is branded AutoRouter, with existing package/CLI identifiers retained for compatibility.

## Safeguards applied on October 6, 2026

| Active ruleset | GitHub ID |
| --- | --- |
| [Main integrity and CI](https://github.com/frapposelli/claude-autorouter/rules/24562842) | `24562842` |
| [Main review](https://github.com/frapposelli/claude-autorouter/rules/24562877) | `24562877` |
| [Owner-only release tag creation](https://github.com/frapposelli/claude-autorouter/rules/24562888) | `24562888` |
| [Immutable release tags](https://github.com/frapposelli/claude-autorouter/rules/24562899) | `24562899` |

Definitions and bypass details are in [.github/rulesets](../.github/rulesets/README.md). The CI ruleset has no owner bypass: a runner outage or billing restriction blocks merges until checks run successfully. `.github/CODEOWNERS` is on `main`, so its designated paths participate in the review ruleset; the documented owner exception still applies.

Actions requires full commit-SHA pinning. Its default token is read-only and cannot approve pull requests. Workflows from all external contributors require maintainer approval, and forked PRs do not receive write tokens or secrets. Dependency vulnerability alerts and Dependabot security-update PRs are enabled. Dependabot's configuration is on `main` and schedules weekly development-tool and pinned Action update PRs. npm trusted publishing remains scoped to `frapposelli/claude-autorouter` and `publish.yml`; releases still require owner-created version tags. The publisher requests npm provenance for future releases from the now-public source. No new provenance statement was created by changing visibility, and existing npm releases cannot gain provenance retroactively.

GitHub private vulnerability reporting, secret scanning and push protection are enabled. [SECURITY.md](../SECURITY.md) directs private reports to GitHub's reporting form, with email as a fallback. Support, conduct, contribution and issue/PR guidance are on `main`; relevant policy documents are included in the package allowlist for subsequent releases.

## Pre-publication review

The preparation audit checked all 17 reachable commits in its baseline, all 25 retained Actions log archives and all 10 retained Actions artifacts. The preparation checkpoint expanded the history review to 18 reachable commits. Gitleaks found no secrets with its default detection rules. All nine packaged artifact archives matched their corresponding Git tags and checksum sidecars. The review found no tracked private configuration, session captures, employer URLs or personal filesystem paths. Existing author names and email addresses are intentionally retained.

These are pattern-based and retained-inventory checks of the recorded snapshots, not proof that every possible secret or private item is absent. Continue reviewing new commits, logs and artifacts for private material, and confirm ownership of contributed work. The audit does not establish provider authorization for subscription forwarding.

## Public-launch settings

After the preparation PR merged and the owner authorized public visibility, GitHub's API confirmed `private: false`. Private vulnerability reporting and the `all_external_contributors` workflow-approval policy were then enabled and read back successfully. Secret scanning and push protection also report `enabled`. All four rulesets remain active, and the merged commit passed [CI run 37432784492](https://github.com/frapposelli/claude-autorouter/actions/runs/37432784492). These public-launch settings supplement the existing rulesets and Actions restrictions.

The following commands can reapply the reporting and approval settings after an authorized configuration change. Check visibility first; do not treat a private-repository 404 as proof that reporting is disabled.

```sh
gh api repos/frapposelli/claude-autorouter --jq '.visibility'
# Continue with these two settings only after the output is "public".
gh api -X PUT repos/frapposelli/claude-autorouter/private-vulnerability-reporting
gh api -X PUT repos/frapposelli/claude-autorouter/actions/permissions/fork-pr-contributor-approval \
  -f approval_policy=all_external_contributors
```

Confirm npm's trusted publisher still matches the owner, repository, workflow filename and environment setting before a release. The workflow currently uses no GitHub environment. Review any account spending restriction before relying on Actions for releases. The integration note explains the remaining [provider-policy scope](subscription-integration.md#provider-guidance-and-unresolved-scope); technical success does not establish provider authorization.

## Verify live policy

```sh
gh api repos/frapposelli/claude-autorouter --jq '{visibility, security_and_analysis}'
gh api repos/frapposelli/claude-autorouter/rulesets
gh api repos/frapposelli/claude-autorouter/rules/branches/main
gh api repos/frapposelli/claude-autorouter/actions/permissions
gh api repos/frapposelli/claude-autorouter/actions/permissions/workflow
gh api repos/frapposelli/claude-autorouter/private-vulnerability-reporting
gh api repos/frapposelli/claude-autorouter/actions/permissions/fork-pr-contributor-approval
```

Read individual rulesets by ID to inspect bypass actors; the summary listing does not show their full policy. Required CI contexts must match the workflow job names exactly. Review the rules again after any repository transfer, CI job rename, maintainer change or publishing-path change.
