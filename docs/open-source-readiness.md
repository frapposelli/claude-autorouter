# Open-source readiness and repository administration

The repository remains **private**. This preparation does not authorize changing its visibility or publishing a new npm version. Author names and email addresses remain in the existing history. The project uses Apache-2.0 and is branded AutoRouter, with existing package/CLI identifiers retained for compatibility.

## Safeguards applied on October 6, 2026

| Active ruleset | GitHub ID |
| --- | --- |
| [Main integrity and CI](https://github.com/frapposelli/claude-autorouter/rules/24562842) | `24562842` |
| [Main review](https://github.com/frapposelli/claude-autorouter/rules/24562877) | `24562877` |
| [Owner-only release tag creation](https://github.com/frapposelli/claude-autorouter/rules/24562888) | `24562888` |
| [Immutable release tags](https://github.com/frapposelli/claude-autorouter/rules/24562899) | `24562899` |

Definitions and bypass details are in [.github/rulesets](../.github/rulesets/README.md). The CI ruleset has no owner bypass: a runner outage or billing restriction blocks merges until checks run successfully. Code-owner review takes effect for designated paths once `.github/CODEOWNERS` reaches the target branch.

Actions requires full commit-SHA pinning. Its default token is read-only and cannot approve pull requests. Private-fork workflows are disabled, and write tokens/secrets are not sent to them. Dependency vulnerability alerts are enabled. Dependabot is configured to propose weekly development-tool and pinned Action updates through PRs once its configuration reaches `main`. npm trusted publishing remains scoped to `frapposelli/claude-autorouter` and `publish.yml`; releases still require owner-created version tags. The publisher enables npm provenance automatically for future releases from public source, and disables it while source is private. Existing npm releases cannot gain provenance retroactively.

[SECURITY.md](../SECURITY.md) establishes private email reporting now, with GitHub private vulnerability reporting preferred when it becomes available. Support, conduct, contribution and issue/PR guidance are included in the repository and relevant policy documents ship with the CLI.

## Pre-publication review

The preparation audit checked all 17 reachable commits in its baseline, all 25 retained Actions log archives and all 10 retained Actions artifacts. Gitleaks found no secrets with its default detection rules. All nine packaged artifact archives matched their corresponding Git tags and checksum sidecars. The review found no tracked private configuration, session captures, employer URLs or personal filesystem paths. Existing author names and email addresses are intentionally retained.

These are pattern-based and retained-inventory checks, not proof that every possible secret or private item is absent. Review material added after the audit and confirm ownership of contributed work before opening the repository. The audit does not establish provider authorization for subscription forwarding.

## At the separately authorized public launch

Merge the preparation PR after all required checks pass. Review any newly added history, Actions logs and retained artifacts before making the repository public: changing visibility exposes past material as well as the current tree. The integration note explains the remaining [provider-policy scope](subscription-integration.md#provider-guidance-and-unresolved-scope); technical success does not establish provider authorization.

After the owner separately changes visibility to public, enable private vulnerability reporting and require maintainer approval for workflows from all external contributors. These settings are unavailable while the repository is private, so they must be activated after that change. Do not treat a private-repository 404 as proof that reporting is disabled.

```sh
gh api repos/frapposelli/claude-autorouter --jq '.visibility'
# Continue with these two settings only after the output is "public".
gh api -X PUT repos/frapposelli/claude-autorouter/private-vulnerability-reporting
gh api -X PUT repos/frapposelli/claude-autorouter/actions/permissions/fork-pr-contributor-approval \
  -f approval_policy=all_external_contributors
```

Check the public repository's Security settings for secret scanning and push protection, and enable them when available. Confirm npm's trusted publisher still matches the owner, repository, workflow filename and environment setting. The workflow currently uses no GitHub environment. Review any account spending restriction before relying on Actions for releases.

## Verify live policy

```sh
gh api repos/frapposelli/claude-autorouter/rulesets
gh api repos/frapposelli/claude-autorouter/rules/branches/main
gh api repos/frapposelli/claude-autorouter/actions/permissions
gh api repos/frapposelli/claude-autorouter/actions/permissions/workflow
gh api repos/frapposelli/claude-autorouter/private-vulnerability-reporting
gh api repos/frapposelli/claude-autorouter/actions/permissions/fork-pr-contributor-approval
```

Read individual rulesets by ID to inspect bypass actors; the summary listing does not show their full policy. Required CI contexts must match the workflow job names exactly. Review the rules again after any repository transfer, CI job rename, maintainer change or publishing-path change.
