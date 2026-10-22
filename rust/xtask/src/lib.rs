//! Contributor tool entrypoint and narrow canonical-archive test seams.
//! Empty for the standalone auditor's no-default-feature build.
#![cfg(feature = "runtime-tools")]

include!("cli_root.rs");

pub mod release_fixture_api {
    pub use super::release::{Artifact, ReleaseError, inspect_artifact};
    pub use super::release_install::{CommandRunner, Invocation, NativeCommand};
    use std::path::Path;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    pub fn binary_inspection(target: &str, bytes: &[u8]) -> Result<serde_json::Value, String> {
        if !super::package::known_target(target) {
            return Err("Unknown fixture executable target".into());
        }
        super::package::binary::inspect(target, bytes)
    }

    pub async fn install_with_runner<R: CommandRunner>(
        artifact: &Artifact,
        runner: &R,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<serde_json::Value, ReleaseError> {
        super::release_install::install_canonical_with(artifact, runner, timeout, cancel).await
    }

    pub fn compare_installed_archive(artifact: &Artifact, root: &Path) -> Result<(), ReleaseError> {
        let files = super::release_install::canonical_files(artifact)?;
        super::release_install::compare_installed(root, &files)
    }
}
