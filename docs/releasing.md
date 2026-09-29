# CI and npm releases

The package is `claude-autorouter`, licensed under [Apache-2.0](../LICENSE). Version `0.2.0` was published manually to [npm](https://www.npmjs.com/package/claude-autorouter) on September 29, 2026. npm trusted publishing is configured for this repository's `publish.yml`, including direct publication permission. Subsequent releases use [version tags](#3-release-subsequent-versions-by-tag). Preparing a tarball or merging a pull request does not publish it.

Version `0.3.1` replaces the old Ollama chat evaluator and Qwen presets with the native `/v1/systemone` endpoint on Ollama 0.35+. It supports Nimble, Tev1, and other compatible local models through `--ollama-model`; Jev remains the default remote evaluator. Existing local users should rerun setup with a supported model, as described in the [reference](reference.md#ollama-evaluator). The [evaluation report](ollama-evaluation.md) records local model latency, accuracy, and timeout limitations.

The GitHub repository is private. Publishing to npm makes the tarball's runtime source, README, configuration example, license, and shipped documentation public. Model weights, user configuration, credentials, transcripts, local artifacts, and test fixtures are excluded. Review the archive before the first publication and whenever the package allowlist changes.

## What runs automatically

| Workflow | Trigger | Behavior |
| --- | --- | --- |
| [ci.yml](https://github.com/frapposelli/claude-autorouter/blob/main/.github/workflows/ci.yml) | Pull requests, pushes to `main`, manual runs, and calls from the release workflow | Syntax checks, tests, and package smoke tests on Ubuntu/macOS with Node 22/24 |
| [publish.yml](https://github.com/frapposelli/claude-autorouter/blob/main/.github/workflows/publish.yml) | Push of a tag matching `v*` | Validate release, run CI, pack and test the candidate, then publish the verified archive |

A release tag must exactly equal `v` plus the version in `package.json`, and its commit must be reachable from `origin/main`. Package name and repository metadata must match `claude-autorouter` and `frapposelli/claude-autorouter`. Stable versions use npm's `latest` tag; prereleases such as `0.3.1-beta.1` use `next`.

The release workflow packs its candidate once and smoke-tests that exact `.tgz`. It uploads the archive and SHA-256 checksum as an Actions artifact. A separate publishing job downloads that artifact by its immutable ID, checks the checksum and every packaged file against the release checkout, then runs `npm publish` with scripts disabled. The publish job uses a GitHub-hosted Ubuntu runner, Node 24, and npm 11.19.1. Only that job has `id-token: write`; there is no `NPM_TOKEN` secret or required GitHub environment. Failed checks prevent publication.

The project has no package dependencies or lockfile, so CI runs its scripts directly without `npm ci`. Live Claude/Jev calls, Ollama downloads, and private repository probes are not CI checks.

## 1. Publish the first version interactively

This bootstrap was completed for `claude-autorouter@0.2.0`. Do not repeat it for this package; continue with [trusted publishing](#2-authorize-this-workflow-on-npm). The instructions below are retained as the bootstrap procedure for a new package name.

Merge the release workflows and package metadata to `main`, push to GitHub, and ensure GitHub Actions is enabled for the repository. Its Actions policy must permit the pinned official GitHub actions and the reusable CI workflow in this repository. Use a clean checkout of that commit, with Node 24 and npm 11.19.1 to match the publisher. No release tag is needed for this bootstrap. First check the registry and account:

```sh
npm ping --registry https://registry.npmjs.org/
npm view claude-autorouter name version --registry https://registry.npmjs.org/
npm whoami --registry https://registry.npmjs.org/
```

For a new package name, verify availability and ownership before continuing. A network or authentication failure is not evidence that a name is available. If another owner has claimed the name, choose an available name and update package metadata, release validation, documentation, and trust settings together.

If `whoami` reports `ENEEDAUTH`, sign in interactively and complete npm's browser/2FA prompts:

```sh
npm login --registry https://registry.npmjs.org/
npm whoami --registry https://registry.npmjs.org/
```

Use your own npm account with publishing permission, and keep credentials out of the repository and workflow secrets. See [npm login](https://docs.npmjs.com/cli/v11/commands/npm-login/) and [publishing a public package](https://docs.npmjs.com/creating-and-publishing-unscoped-public-packages/) for account requirements.

For the initial `0.2.0` release, run:

```sh
npm run check
npm test
npm run release:pack
npm run test:package -- --archive ./dist/claude-autorouter-0.2.0.tgz
tar -tzf ./dist/claude-autorouter-0.2.0.tgz
```

`release:pack` checks the allowed files and writes `dist/claude-autorouter-0.2.0.tgz` and its `.sha256` file. The `--archive` smoke test installs and exercises those bytes without repacking. Review the listed contents and retain both files. If anything changes, rebuild and repeat the exact-archive smoke test.

Publish that reviewed candidate, completing npm's interactive authentication challenge when requested:

```sh
npm publish ./dist/claude-autorouter-0.2.0.tgz --ignore-scripts --access public --tag latest --registry https://registry.npmjs.org/ --provenance=false
npm view claude-autorouter@0.2.0 version dist.integrity --registry https://registry.npmjs.org/
```

On a separate machine or disposable environment, verify the registry installation:

```sh
npm install -g claude-autorouter@0.2.0
claude-autorouter --version
claude-autorouter --help
```

Then run `setup`, `doctor`, and a launch from outside the source checkout as appropriate for that machine. `doctor` is local-only; a live prompt separately verifies provider access. Keep the README's installation instructions aligned with the verified registry release.

Do not push `v0.2.0` to test automation after this bootstrap: it would attempt to publish an existing version. npm name/version pairs cannot be reused, including after unpublishing. See the [npm publish reference](https://docs.npmjs.com/cli/v11/commands/npm-publish/).

## 2. Authorize this workflow on npm

Trusted publishing requires the package to exist first, which is why the initial version is published manually. The account configuring trust needs package write access and 2FA. See [npm trust prerequisites](https://docs.npmjs.com/cli/v11/commands/npm-trust/#prerequisites).

On npmjs.com, open the `claude-autorouter` package's **Settings → Trusted Publisher**, select **GitHub Actions**, and enter:

| Setting | Value |
| --- | --- |
| Organization or user | `frapposelli` |
| Repository | `claude-autorouter` |
| Workflow filename | `publish.yml` |
| Environment name | Leave blank |
| Allowed actions | Enable direct `npm publish` |

Use the filename only, not `.github/workflows/publish.yml`. No GitHub environment or npm token secret needs to be created. The owner, repository, and workflow must match exactly. New trust configurations default to permitting staged publication; **enable direct `npm publish`** for this workflow. See [npm trusted publishers](https://docs.npmjs.com/trusted-publishers/) and [staged publishing](https://docs.npmjs.com/staged-publishing/).

The workflow explicitly disables provenance because npm provenance is unsupported for private source repositories, even when the npm package is public. OIDC authentication still works. If the repository becomes public, review the workflow and metadata before enabling provenance. See [npm provenance requirements](https://docs.npmjs.com/generating-provenance-statements/).

After a successful trusted release, npm recommends the optional **Publishing access → Require two-factor authentication and disallow tokens** setting. It does not disable OIDC publishing. See [restricting token access](https://docs.npmjs.com/trusted-publishers/#recommended-restrict-token-access-when-using-trusted-publishers).

## 3. Release subsequent versions by tag

For the System One release, prepare `0.3.1` on `main` or through a pull request. For later releases, substitute the next unused version throughout:

```sh
git switch main
git pull --ff-only origin main
npm version 0.3.1 --no-git-tag-version
```

Review the version change and update any version-specific install examples or release notes. Check the candidate using the new filename:

```sh
npm run check
npm test
npm run release:pack
npm run test:package -- --archive ./dist/claude-autorouter-0.3.1.tgz
git diff --check
```

Commit the intended release changes and get that commit onto `main`, either through a pull request or a direct push allowed by the repository's branch rules. For a direct push with only the version changed:

```sh
git add package.json
git commit -m "Release 0.3.1"
git push origin main
```

Include any intentional documentation or release-note edits in that commit too. There is no publication from a branch push or PR merge. Wait for CI to pass, then tag that exact release commit:

```sh
git switch main
git pull --ff-only origin main
git tag -a v0.3.1 -m "Release 0.3.1"
git push origin v0.3.1
```

Before pushing, confirm `package.json` contains `0.3.1` and the tag points to the intended commit. For a prerelease, use a matching version/tag such as `0.4.0-beta.1` / `v0.4.0-beta.1`; it will publish under `next`, leaving `latest` unchanged.

Release stable versions in increasing version order, one tag at a time, and wait for each run to finish before pushing the next stable tag. The workflow queues releases without canceling an active run, but queue order does not sort semantic versions. Publishing an older stable version afterward could move `latest` backward; there is no registry version-order gate.

Open the tag's run under [GitHub Actions](https://github.com/frapposelli/claude-autorouter/actions). Under **Artifacts**, download `npm-package-<run-id>-<run-attempt>`, which contains the `.tgz` and checksum used for publication. Artifacts expire after 30 days, so retain them with the release record. After the publish job succeeds, verify the registry version and tags:

```sh
npm view claude-autorouter@0.3.1 version dist.integrity --registry https://registry.npmjs.org/
npm view claude-autorouter dist-tags --json --registry https://registry.npmjs.org/
```

Repeat the independent installation check for the released version. A GitHub Release page is optional; pushing the version tag is the publication trigger.

For local release diagnostics after the tag exists, `node scripts/release-check.mjs source v0.3.1` checks the tag, clean checkout, metadata, and ancestry. `node scripts/release-check.mjs archive v0.3.1` checks the candidate checksum and contents; `dist/` must contain only that version's archive and checksum, so retain older artifacts elsewhere first. These helpers are run automatically in the release workflow; the first untagged bootstrap uses the checks in step 1 instead.

## Recovering a failed release

- **Checks or archive validation failed:** nothing is published. Fix the cause and repeat validation before making a new release tag. Do not move a tag that already identifies a published version.
- **npm rejected OIDC authentication:** verify the npm trust fields, direct-publish permission, GitHub-hosted runner, and the publish job's OIDC permission. After correcting npm configuration, rerun the failed job if that version is still unpublished.
- **A publish timed out or the run was interrupted:** check `npm view` for the exact version before retrying. The registry may have accepted it before the connection failed.
- **The version already exists:** inspect the registry release; do not overwrite or unpublish to reuse it. Code or documentation corrections need a new version.
- **Local bootstrap authentication failed:** complete `npm login` and the account's 2FA flow in your terminal. CI trust cannot create the first package or substitute for that account step.

If release code or workflow changes are needed, commit the fix to `main` and prepare a new version/tag. Authentication-only corrections on npm can be retried against the unchanged, unpublished candidate.
