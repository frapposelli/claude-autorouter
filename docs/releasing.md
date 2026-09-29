# Preparing a release

The package is named `claude-autorouter` and uses the [Apache-2.0 license](../LICENSE). Preparing or installing a tarball does not publish it to npm. npm publication is currently pending.

## Prepare the candidate

Confirm the package name, npm ownership, and release version before publication. Add accurate repository metadata when a public repository exists; do not invent a repository URL. Check current name/version availability with `npm view` and the publishing identity with `npm whoami`. If npm reports `ENEEDAUTH`, run `npm login` in your terminal. A lookup error alone does not distinguish an unavailable registry from a free name.

Update `package.json` to the intended version. For an existing release, `npm version patch --no-git-tag-version` is one way to prepare a patch increment without creating a Git tag. Review the resulting metadata and run:

```sh
npm run check
npm test
npm run test:package
npm run release:pack
```

`release:pack` creates the versioned `.tgz` and checksum in `dist/`. Review the manifest and tarball contents. The runtime package should contain its executable files, source modules, license, and shipped documentation; it must not contain `.env`, user config, credentials, local artifacts, diagnostic reports, transcripts, or test fixtures. The package's file allowlist and package test enforce the intended contents. [npm's packing rules](https://docs.npmjs.com/cli/v11/commands/npm-pack/) describe how the distributable is assembled.

For version 0.2.0, an independent machine can install the reviewed candidate with:

```sh
npm install -g ./claude-autorouter-0.2.0.tgz
claude-autorouter --version
claude-autorouter --help
claude-autorouter setup
claude-autorouter doctor
```

Test launch from a directory outside the source checkout. Setup should create only the user's chosen config, and the launcher should use the installed status-line path. `doctor` is local-only; a separate live test is needed to verify provider access.

## Publish the reviewed tarball

Once publication is authorized and npm authentication is configured, publish the exact candidate rather than rebuilding an unreviewed directory:

```sh
npm publish ./dist/claude-autorouter-0.2.0.tgz --access public
```

Replace the version with the candidate being released. npm does not allow reusing a published name/version, so corrections require a new version. See the [npm publish reference](https://docs.npmjs.com/cli/v11/commands/npm-publish/).

After publication, verify the registry version and install it in a fresh environment. Update README's pending-publication notice only after the registry release exists, and retain the reviewed tarball and checksum with the release record.

Provenance is optional and requires an appropriately configured supported CI publisher. Set up the actual repository and publishing workflow before adding provenance flags; do not claim provenance for a local pack. See [npm provenance guidance](https://docs.npmjs.com/generating-provenance-statements/).
