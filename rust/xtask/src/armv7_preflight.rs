//! Fixed checkout-only ARMv7 metadata collector. No target/product execution.
//! Node remains optional: only this explicit command/ignored integration needs it.
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const SCRIPT: &str = "rust/distribution/ci/armv7-preflight.mjs";
const OUTPUT: &str = "artifacts/rust-rewrite/armv7-preflight/observations.json";
const OWNER_TIMEOUT: Duration = Duration::from_secs(190);
const SCRIPT_MAX: u64 = 8 * 1024 * 1024;
const REPORT_MAX: u64 = 2 * 1024 * 1024;

fn arguments(args: &[String]) -> Result<bool, String> {
    match args {
        [] => Ok(false),
        [arg] if arg == "--help" => Ok(true),
        _ => Err("ARMv7 preflight accepts no execution arguments; use --help".into()),
    }
}

// Preserve the invocation alias; canonical metadata admits the selected file.
// This is tool selection, not an immutable Node/OS supply-chain proof.
fn node_from_path(value: &OsStr) -> Result<PathBuf, String> {
    let value = value.to_str().ok_or("Invalid ARMv7 Node search path")?;
    if value.is_empty() || value.len() > 32768 || value.chars().any(char::is_control) {
        return Err("Invalid ARMv7 Node search path".into());
    }
    let paths: Vec<_> = std::env::split_paths(value).collect();
    if paths.len() > 128 || paths.iter().any(|p| !p.is_absolute()) {
        return Err("Invalid ARMv7 Node search path".into());
    }
    for path in paths {
        let invocation = path.join("node");
        match fs::symlink_metadata(&invocation) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("Cannot inspect ARMv7 Node invocation".into()),
            Ok(_) => {}
        }
        let canonical =
            fs::canonicalize(&invocation).map_err(|_| "Cannot resolve ARMv7 Node invocation")?;
        let file =
            fs::symlink_metadata(canonical).map_err(|_| "Cannot inspect ARMv7 Node executable")?;
        if !file.is_file()
            || file.len() == 0
            || file.len() > 512 * 1024 * 1024
            || file.permissions().mode() & 0o111 == 0
        {
            return Err("ARMv7 Node invocation is not a bounded executable".into());
        }
        return Ok(invocation);
    }
    Err("ARMv7 preflight requires the pinned Node22.14.0 on PATH".into())
}

fn real_components(root: &Path, relative: &str, directory: bool) -> Result<PathBuf, String> {
    let mut current = root.to_path_buf();
    let components: Vec<_> = Path::new(relative).components().collect();
    for (index, part) in components.iter().enumerate() {
        current.push(part.as_os_str());
        let info = fs::symlink_metadata(&current)
            .map_err(|_| "ARMv7 fixed collector path is unavailable")?;
        if info.file_type().is_symlink()
            || (index + 1 < components.len() || directory) && !info.is_dir()
            || index + 1 == components.len()
                && !directory
                && (!info.is_file() || info.len() > SCRIPT_MAX)
        {
            return Err("ARMv7 fixed collector path is not an admitted regular path".into());
        }
    }
    Ok(current)
}

fn fixed_paths(root: &Path) -> Result<(PathBuf, PathBuf), String> {
    let script = real_components(root, SCRIPT, false)?;
    real_components(root, "artifacts/rust-rewrite/armv7-preflight", true)?;
    for name in [
        OUTPUT,
        "artifacts/rust-rewrite/armv7-preflight/observations-incomplete.json",
    ] {
        match fs::symlink_metadata(root.join(name)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err("ARMv7 collector refuses an existing output".into()),
        }
    }
    Ok((script, root.join(OUTPUT)))
}

// Private test seam only. Production's caller always supplies fixed_paths().
// Every child inherits the native-owned group. Node never signals numeric PGIDs.
fn capture_collector(
    node: &Path,
    script: &Path,
    root: &Path,
    output: &Path,
    timeout: Duration,
) -> Result<crate::process::CapturedProcess, String> {
    let mut command = Command::new(node);
    command
        .arg(script)
        .arg("--root")
        .arg(root)
        .arg("--out")
        .arg(output)
        .current_dir(root.join("rust"))
        .env("AUTOROUTER_ARMV7_NATIVE_OWNER", "1");
    crate::process::capture_result(&mut command, b"", timeout)
        .map_err(|error| format!("ARMv7 native collector owner: {error}"))
}

fn safe_stage(stderr: &[u8]) -> Option<String> {
    if stderr.len() > 16384 {
        return None;
    }
    let row: serde_json::Value = serde_json::from_slice(stderr).ok()?;
    if row["kind"] != "ARMv7_preflight_refusal" {
        return None;
    }
    let stage = row["stage"].as_str()?;
    let fixed = [
        "git",
        "rustup-which-rustc",
        "rustup-which-cargo",
        "rustc-version",
        "cargo-version",
        "target-cfg",
        "target-cpus",
        "rust-sysroot",
        "dpkg-installed",
        "dpkg-installed-after",
        "gcc-target",
        "gcc-version",
        "gcc-sysroot",
        "gcc-search-dirs",
        "gcc-default-options",
        "gcc-default-macros",
        "gcc-source-cc-macros",
        "cross-ar-version",
        "cross-ranlib-version",
        "cross-ld-version",
        "cross-readelf-version",
    ];
    if fixed.contains(&stage)
        || [
            "crt1.o",
            "crti.o",
            "crtn.o",
            "libgcc.a",
            "libc.so",
            "libgcc_s.so.1",
            "libatomic.so.1",
            "ld-linux-armhf.so.3",
        ]
        .iter()
        .any(|name| stage == format!("gcc-file-{name}"))
        || ["cc1", "as", "collect2", "ld"]
            .iter()
            .any(|name| stage == format!("gcc-program-{name}"))
    {
        Some(stage.to_owned())
    } else {
        None
    }
}

fn accept_capture(result: &crate::process::CapturedProcess) -> Result<(), String> {
    if !result.status.success() {
        return Err(match safe_stage(&result.stderr) {
            Some(stage) => format!(
                "ARMv7 collector failed ({}); fixed metadata stage: {stage}",
                result.status
            ),
            None => format!("ARMv7 collector failed ({})", result.status),
        });
    }
    if !result.stdout.is_empty() || !result.stderr.is_empty() {
        return Err("ARMv7 collector returned unexpected console output".into());
    }
    Ok(())
}

fn accept_report(output: &Path) -> Result<(), String> {
    let info = fs::symlink_metadata(output).map_err(|_| "ARMv7 collector has no success report")?;
    if !info.is_file() || info.file_type().is_symlink() || info.len() > REPORT_MAX {
        return Err("ARMv7 collector success report is not a bounded regular file".into());
    }
    let bytes = crate::audit_support::regular(output, REPORT_MAX)
        .map_err(|_| "ARMv7 collector success report is not a bounded regular file")?;
    let report: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| "ARMv7 collector success report is invalid JSON")?;
    if report["kind"] != "ARMv7_hosted_cross_tool_preflight_observation"
        || report["passed"] != true
        || report["target"] != "armv7-unknown-linux-gnueabihf"
        || report["portable_cpu"] != false
        || report["production_release_approved"] != false
    {
        return Err("ARMv7 collector success report has no matching observational result".into());
    }
    Ok(())
}

pub(crate) fn run(args: &[String], root: &Path) -> Result<bool, String> {
    if arguments(args)? {
        println!(
            "Usage: autorouter-audit armv7-preflight\n\nExplicit checkout-only metadata/C-preprocess collector. Requires setup-pinned Node22.14.0 and prepared ARMv7 tools/std. Fixed script/output; no execution overrides. Native owner190s, collector180s cooperative; existing subprocess byte caps unchanged. No ARM object/link/run, immutable SDK or release qualification. Separate host auditor bootstrap is a Cargo build."
        );
        return Ok(true);
    }
    let (script, output) = fixed_paths(root)?;
    let node = node_from_path(&std::env::var_os("PATH").ok_or("ARMv7 Node search path absent")?)?;
    // The reviewed fixed Node CLI checks actual process.version v22.14.0 and
    // the fixed owner marker before starting any metadata command. Marker is
    // misuse prevention only, not authentication or standalone ownership proof.
    let result = capture_collector(&node, &script, root, &output, OWNER_TIMEOUT)?;
    accept_capture(&result)?;
    accept_report(&output)?;
    println!("ARMv7 fixed metadata collector completed under native process ownership");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
    use std::os::unix::process::ExitStatusExt;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let name = crate::audit_support::random_hex::<16>().unwrap();
            let path = std::env::temp_dir()
                .join(format!("armv7-native-bridge-{}-{name}", std::process::id()));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn fixed_command_rejects_execution_overrides_before_paths() {
        assert!(!arguments(&[]).unwrap());
        assert!(arguments(&["--help".into()]).unwrap());
        for option in [
            "--root",
            "--script",
            "--node",
            "--out",
            "--timeout",
            "--self-test",
        ] {
            assert!(arguments(&[option.into(), "synthetic".into()]).is_err());
        }
    }

    #[test]
    fn node_invocation_preserves_alias_and_refuses_unbounded_or_relative_path() {
        let root = Scratch::new();
        let file = root.0.join("node-real");
        fs::write(&file, b"synthetic executable").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap();
        let alias = root.0.join("node");
        symlink(&file, &alias).unwrap();
        assert_eq!(node_from_path(root.0.as_os_str()).unwrap(), alias);
        for bad in ["", "relative", ":/usr/bin", "/usr/bin:", "/usr/bin\n"] {
            assert!(node_from_path(OsStr::new(bad)).is_err());
        }
        fs::remove_file(&alias).unwrap();
        fs::create_dir(&alias).unwrap();
        assert!(node_from_path(root.0.as_os_str()).is_err());
    }

    #[test]
    fn fixed_paths_reject_source_symlink_and_preserve_existing_report() {
        let root = Scratch::new();
        fs::create_dir_all(root.0.join("rust/distribution/ci")).unwrap();
        fs::create_dir_all(root.0.join("artifacts/rust-rewrite/armv7-preflight")).unwrap();
        let script = root.0.join(SCRIPT);
        fs::write(&script, b"synthetic script").unwrap();
        assert!(fixed_paths(&root.0).is_ok());
        fs::write(root.0.join(OUTPUT), b"existing owner").unwrap();
        assert!(fixed_paths(&root.0).is_err());
        assert_eq!(fs::read(root.0.join(OUTPUT)).unwrap(), b"existing owner");
        fs::remove_file(root.0.join(OUTPUT)).unwrap();
        fs::remove_file(&script).unwrap();
        let real = root.0.join("script-real");
        fs::write(&real, b"synthetic script").unwrap();
        symlink(real, script).unwrap();
        assert!(fixed_paths(&root.0).is_err());
    }

    #[test]
    fn failed_collector_exposes_only_closed_stage_not_raw_payload() {
        let mut result = crate::process::CapturedProcess { status: std::process::ExitStatus::from_raw(7 << 8),
            stdout: b"synthetic-private-stdout".to_vec(),
            stderr: br#"{"kind":"ARMv7_preflight_refusal","stage":"gcc-version","reason":"synthetic-private-reason"}"#.to_vec() };
        let error = accept_capture(&result).unwrap_err();
        assert!(error.contains("gcc-version") && error.contains("7"));
        assert!(!error.contains("synthetic-private"));
        result.stderr =
            br#"{"kind":"ARMv7_preflight_refusal","stage":"synthetic-private-path"}"#.to_vec();
        assert!(
            !accept_capture(&result)
                .unwrap_err()
                .contains("synthetic-private")
        );
        result.status = std::process::ExitStatus::from_raw(0);
        assert!(accept_capture(&result).is_err());
    }

    #[test]
    fn success_report_gate_requires_owned_bounded_observational_result() {
        let scratch = Scratch::new();
        let path = scratch.0.join("report.json");
        assert!(accept_report(&path).is_err());
        let valid = serde_json::json!({
            "kind":"ARMv7_hosted_cross_tool_preflight_observation",
            "passed":true,"target":"armv7-unknown-linux-gnueabihf",
            "portable_cpu":false,"production_release_approved":false
        });
        let original = serde_json::to_vec(&valid).unwrap();
        fs::write(&path, &original).unwrap();
        accept_report(&path).unwrap();
        for (field, value) in [
            ("kind", serde_json::json!("unrelated report")),
            ("passed", serde_json::json!(false)),
            ("target", serde_json::json!("x86_64-unknown-linux-gnu")),
            ("portable_cpu", serde_json::json!(true)),
            ("production_release_approved", serde_json::json!(true)),
        ] {
            let mut bad = valid.clone();
            bad[field] = value;
            fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
            assert!(accept_report(&path).is_err(), "{field}");
        }
        fs::File::create(&path)
            .unwrap()
            .set_len(REPORT_MAX + 1)
            .unwrap();
        assert!(accept_report(&path).is_err());
        fs::remove_file(&path).unwrap();
        let target = scratch.0.join("original.json");
        fs::write(&target, &original).unwrap();
        symlink(&target, &path).unwrap();
        assert!(accept_report(&path).is_err());
        assert_eq!(fs::read(&target).unwrap(), original);
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(accept_report(&path).is_err());
    }

    #[test]
    #[ignore = "Opt-in Node22.14 integration; no Node required by ordinary native test/bundle runs"]
    fn real_node_collector_group_cleanup() {
        let scratch = Scratch::new();
        let root = &scratch.0;
        fs::create_dir(root.join("rust")).unwrap();
        let node = node_from_path(&std::env::var_os("PATH").unwrap()).unwrap();
        // Fixed synthetic source only, not a production CLI script/root override.
        // Same capture_collector() seam, real Node+grandchild in inherited PGID.
        let script = root.join("fixture.mjs");
        let helper = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../rust/distribution/ci/armv7-preflight.mjs")
            .canonicalize()
            .unwrap();
        fs::write(root.join("helper-path"), helper.to_str().unwrap()).unwrap();
        fs::write(&script, r#"
import { spawn } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
if (process.version !== 'v22.14.0' || process.env.AUTOROUTER_ARMV7_NATIVE_OWNER !== '1') process.exit(90);
const root = process.argv[3], mode = readFileSync(join(root, 'mode'), 'utf8');
const childSource = `const fs=require('node:fs'),p=require('node:path'); const root=process.argv[1]; fs.writeFileSync(p.join(root,'ready'),'ready'); const t=setInterval(()=>{if(fs.existsSync(p.join(root,'release'))){fs.writeFileSync(p.join(root,'escaped'),'escaped');clearInterval(t);}},10);`;
if (mode === 'inner-fault') {
  const { capture } = await import(pathToFileURL(readFileSync(join(root,'helper-path'),'utf8')));
  try {
    await capture(process.execPath, ['-e', childSource, root], {
      cwd:root, env:{ PATH:'/usr/bin:/bin', HOME:root }, timeoutMs:5000, maxBytes:65536 });
    throw new Error('synthetic stalled capture unexpectedly completed');
  } catch (error) {
    if (error.message !== 'ARMv7 preflight: metadata timeout') throw error;
    process.exitCode=24;
  }
} else {
  spawn(process.execPath, ['-e', childSource, root], { detached:false, stdio:['ignore','pipe','pipe'] });
  const timer = setInterval(()=>{
    if (!existsSync(join(root,'ready'))) return; clearInterval(timer);
    if (mode === 'success') process.exit(0);
    if (mode === 'failure') process.exit(23);
    if (mode === 'overflow') process.stderr.write(Buffer.alloc(65537));
  },10);
}
"#).unwrap();
        for mode in ["success", "failure", "inner-fault", "overflow", "timeout"] {
            for name in ["ready", "release", "escaped"] {
                let _ = fs::remove_file(root.join(name));
            }
            fs::write(root.join("mode"), mode).unwrap();
            let result = capture_collector(
                &node,
                &script,
                root,
                &root.join("unused-report"),
                Duration::from_secs(10),
            );
            // Always release after the native call, including failed assertions.
            fs::write(root.join("release"), b"release").unwrap();
            std::thread::sleep(Duration::from_millis(250));
            assert_eq!(
                fs::read(root.join("ready")).unwrap(),
                b"ready",
                "{mode}: producer admission"
            );
            assert!(
                !root.join("escaped").exists(),
                "{mode}: descendant survived native return"
            );
            match mode {
                "success" => {
                    let output = result.unwrap();
                    assert!(output.status.success());
                }
                "failure" => assert_eq!(result.unwrap().status.code(), Some(23)),
                "inner-fault" => assert_eq!(result.unwrap().status.code(), Some(24)),
                "overflow" => assert_eq!(
                    result.err().unwrap(),
                    "ARMv7 native collector owner: Fixture executable exceeded output limit"
                ),
                "timeout" => assert_eq!(
                    result.err().unwrap(),
                    "ARMv7 native collector owner: Fixture executable timed out"
                ),
                _ => unreachable!(),
            }
        }
    }
}
