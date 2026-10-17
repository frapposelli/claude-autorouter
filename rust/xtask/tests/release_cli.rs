//! The installed xtask entrypoint must check the invoking checkout, not the
//! source tree embedded by Cargo when the tool was compiled.
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        for _ in 0..8 {
            let mut nonce = [0_u8; 16];
            getrandom::fill(&mut nonce).unwrap();
            let suffix = nonce
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let path = std::env::temp_dir().join(format!("autorouter-release-cli-{suffix}"));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("Cannot create private release fixture: {error}"),
            }
        }
        panic!("Cannot create unique release fixture")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn isolated(command: &mut Command, home: &Path) {
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C");
}

fn git(root: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    isolated(&mut command, root);
    let output = command
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn check(cwd: &Path, home: &Path, output_path: &Path, tag: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    isolated(&mut command, home);
    command
        .current_dir(cwd)
        .args(["release-check", "source", tag])
        .env("GITHUB_ACTIONS", "true")
        .env("GITHUB_REPOSITORY", "frapposelli/claude-autorouter")
        .env("GITHUB_REF_TYPE", "tag")
        .env("GITHUB_REF", "refs/tags/v1.2.3")
        .env("GITHUB_OUTPUT", output_path)
        .output()
        .unwrap()
}

#[test]
fn built_xtask_checks_the_invoking_checkout_and_only_appends_validated_outputs() {
    let scratch = Scratch::new();
    let parent_project = scratch.0.join("parent npm project with spaces");
    fs::create_dir(&parent_project).unwrap();
    fs::write(
        parent_project.join("package.json"),
        br#"{"name":"wrong-parent-project","version":"9.9.9"}"#,
    )
    .unwrap();
    // This root is itself named rust and has a Cargo.toml. Without preferring
    // its own package.json, checkout_root would incorrectly select the parent.
    let checkout = parent_project.join("rust");
    fs::create_dir(&checkout).unwrap();
    fs::create_dir(checkout.join("rust")).unwrap();
    fs::write(
        checkout.join("package.json"),
        br#"{"name":"claude-autorouter","version":"1.2.3","repository":{"url":"git+https://github.com/frapposelli/claude-autorouter.git"},"publishConfig":{"access":"public","registry":"https://registry.npmjs.org/"}}"#,
    )
    .unwrap();
    fs::write(checkout.join("Cargo.toml"), b"[workspace]\n").unwrap();
    fs::write(checkout.join("rust/Cargo.toml"), b"[workspace]\n").unwrap();
    fs::write(checkout.join("README.md"), b"Synthetic release fixture.\n").unwrap();
    git(&checkout, &["init", "--initial-branch=main"]);
    git(&checkout, &["config", "user.name", "Release Fixture"]);
    git(
        &checkout,
        &["config", "user.email", "release@example.invalid"],
    );
    git(&checkout, &["add", "."]);
    git(&checkout, &["commit", "-m", "Synthetic release"]);
    git(
        &checkout,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    git(&checkout, &["tag", "-a", "v1.2.3", "-m", "Synthetic tag"]);

    let output_path = scratch.0.join("github output");
    let expected = b"version=1.2.3\narchive=dist/claude-autorouter-1.2.3.tgz\ndist_tag=latest\n";
    for (index, cwd) in [&checkout, &checkout.join("rust")].into_iter().enumerate() {
        let result = check(cwd, &scratch.0, &output_path, "v1.2.3");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("Verified source release:"));
        assert_eq!(fs::read(&output_path).unwrap(), expected.repeat(index + 1));
    }

    let malicious = check(
        &checkout,
        &scratch.0,
        &output_path,
        "v1.2.3\narchive=malicious",
    );
    assert!(!malicious.status.success());
    assert!(String::from_utf8_lossy(&malicious.stderr).contains("strict SemVer"));
    assert_eq!(fs::read(&output_path).unwrap(), expected.repeat(2));
}
