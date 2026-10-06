# Repository rulesets

These JSON files are the intended policy for `frapposelli/claude-autorouter`. They are GitHub REST request bodies, not automatically applied by CI. Read back the live settings after importing or editing them. GitHub adds default fields to its responses.

| Definition | Policy |
| --- | --- |
| `main-integrity.json` | PRs, resolved review threads, all four Node/OS CI checks from GitHub Actions, up-to-date checks, no force push or deletion. No bypass. |
| `main-review.json` | One approval including the code owner, stale approvals dismissed, approval of the latest push. Owner can bypass only through a PR. |
| `release-tag-creation.json` | Only owner `frapposelli` can create `v*` tags. |
| `release-tag-immutability.json` | No updates, force pushes or deletion of `v*` tags, including by the owner. No bypass. |

Owner-specific bypasses use GitHub user ID `541832`; CI checks use GitHub Actions app ID `15368`. Review these values and `.github/CODEOWNERS` before transferring the repository. Keeping review and tag-creation exceptions in separate rulesets prevents them from bypassing CI or tag immutability. The owner review exception supports a single-maintainer project; it does not waive tests or allow direct pushes to `main`.

Existing rulesets should be updated using their IDs, rather than imported again to create duplicates. Repository administrators can change ruleset policy; protect the owner's account and review the ruleset audit trail. Tag ancestry checks in the publishing workflow complement these access restrictions and do not replace them.

See [repository administration](../../docs/open-source-readiness.md) for live rule IDs, verification commands and the remaining public-launch settings.
