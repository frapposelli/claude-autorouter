//! Synthetic unit/owned-filesystem evidence only. No SDK, process, env mutation or CI qualification.
use super::*;
use crate::audit_support::Scratch;
use std::cell::Cell;
use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::symlink;

fn commit() -> String {
    "a".repeat(40)
}
fn pairs(env: &BTreeMap<String, String>) -> Vec<(OsString, OsString)> {
    env.iter()
        .map(|(name, value)| {
            (
                OsString::from(name.as_str()),
                OsString::from(value.as_str()),
            )
        })
        .collect()
}
fn admitted_env(p: Profile) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for (name, value) in [
        ("PATH", "/opt/autorouter-rust/bin:/usr/bin:/bin"),
        ("CARGO_HOME", "/opt/autorouter-cargo"),
        ("CARGO_INCREMENTAL", "0"),
        ("CARGO_NET_RETRY", "3"),
        ("CC", "/opt/autorouter-c-compilers/cc"),
        ("CXX", "/opt/autorouter-c-compilers/cxx"),
        ("CFLAGS", "-O2"),
        ("CXXFLAGS", "-O2"),
        ("RUSTFLAGS", p.rustflags),
        ("OPENSSL_NO_VENDOR", "0"),
        ("OPENSSL_STATIC", "1"),
        ("OPENSSL_SRC_PERL", "/opt/autorouter-perl/bin/perl"),
        ("AUTOROUTER_BUILD_TARGET", p.target),
        ("AUTOROUTER_BUILD_MACHINE", p.machine),
        ("AUTOROUTER_BUILD_IMAGE", p.image),
        ("AUTOROUTER_RUST_ARCHIVE_SHA256", p.rust_archive),
        ("AUTOROUTER_PERL_VERSION", "5.42.3"),
        (
            "AUTOROUTER_PERL_ARCHIVE_SHA256",
            "c9387e1473a1866935cb047ece7c2e0a80767a3acdecb79d4a375f8a95970ddc",
        ),
        ("GITHUB_ACTIONS", "true"),
        ("GITHUB_REPOSITORY", REPOSITORY),
        ("GITHUB_SERVER_URL", "https://github.com"),
        ("GITHUB_JOB", "glibc228"),
        ("GITHUB_RUN_ID", "731"),
        ("GITHUB_RUN_ATTEMPT", "3"),
    ] {
        env.insert(name.into(), value.into());
    }
    env.insert("GITHUB_SHA".into(), commit());
    env.insert("GITHUB_WORKFLOW_SHA".into(), "b".repeat(40));
    env.insert(
        "GITHUB_WORKFLOW_REF".into(),
        format!("{REPOSITORY}/{WORKFLOW}@refs/pull/16/merge"),
    );
    env
}

fn metadata(p: Profile) -> (Value, Value) {
    let wrappers = json!({
        "CC":{"path":"/opt/autorouter-c-compilers/cc","contents":"synthetic reviewed cc bytes","sha256":"c".repeat(64),"effective_target_options":"synthetic cc target options","baseline_macros":"synthetic cc baseline macros"},
        "CXX":{"path":"/opt/autorouter-c-compilers/cxx","contents":"synthetic reviewed cxx bytes","sha256":"d".repeat(64),"effective_target_options":"synthetic cxx target options","baseline_macros":"synthetic cxx baseline macros"}
    });
    let mut state = json!({"schema_version":1,"kind":"native_binary_build_inputs","profile":"gnu-glibc228-v1","target":p.target,
        "source_commit":commit(),"source_dirty":false,"cargo_lock_sha256":"e".repeat(64),"source_date_epoch":"1792600000",
        "environment":admitted_env(p),"ci":{"workflow_sha":"b".repeat(40),"run_attempt":"3"},
        "files":{},"compiler":{"rustc_text":"synthetic rustc1.99.0","cargo_text":"synthetic cargo1.99.0","gcc_text":"synthetic gcc", "linker_text":"synthetic linker","perl_text":"synthetic perl5.42.3","kernel_text":"synthetic kernel","wrappers":wrappers},
        "outputs":{"build_report":"/synthetic/production.jsonl","feature_report":"/synthetic/features.json"}});
    state["files"][WORKFLOW] = json!({"sha256":"f".repeat(64)});
    let builder = json!({"schema_version":1,"kind":"gnu_glibc228_builder_provenance","source_dirty":false,
        "source_commit":state["source_commit"],"cargo_lock_sha256":state["cargo_lock_sha256"],"workflow_sha256":state["files"][WORKFLOW]["sha256"],
        "target":p.target,"image":p.image,"rust_archive_sha256":p.rust_archive,"rustc":state["compiler"]["rustc_text"],
        "cargo":state["compiler"]["cargo_text"],"gcc":state["compiler"]["gcc_text"],"linker":state["compiler"]["linker_text"],
        "libc":"glibc 2.28","rustflags":p.rustflags,"cflags":"-O2","openssl_no_vendor":"0","openssl_static":"1",
        "compiler_wrappers":state["compiler"]["wrappers"],"perl_archive_sha256":"c9387e1473a1866935cb047ece7c2e0a80767a3acdecb79d4a375f8a95970ddc",
        "perl_version":"5.42.3","perl":state["compiler"]["perl_text"],"source_date_epoch":state["source_date_epoch"],"kernel":state["compiler"]["kernel_text"]});
    (builder, state)
}

#[test]
fn only_reviewed_gnu_profiles_can_admit_synthetic_ci_metadata() {
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let p = profile(target).unwrap();
        let context = validate_env(&admitted_env(p), p, &commit()).unwrap();
        assert_eq!(context["checkout_commit"], commit());
        assert_ne!(context["workflow_sha"], context["checkout_commit"]);
        assert_eq!(
            context["workflow_run"],
            format!("https://github.com/{REPOSITORY}/actions/runs/731/attempts/3")
        );
        let (builder, state) = metadata(p);
        validate_builder(&builder, &state, p, Path::new("/synthetic")).unwrap();
    }
    for target in [
        "aarch64-apple-darwin",
        "x86_64-unknown-linux-musl",
        "i686-unknown-linux-gnu",
        "x86_64-unknown-linux-gnu-extra",
        "",
    ] {
        assert!(profile(target).is_err(), "{target}");
    }
}

#[test]
fn local_context_wrong_checkout_and_unreviewed_workflow_identity_are_refused() {
    let p = profile("x86_64-unknown-linux-gnu").unwrap();
    for (name, value) in [
        ("GITHUB_ACTIONS", "false"),
        ("GITHUB_REPOSITORY", "synthetic/fork"),
        ("GITHUB_SERVER_URL", "https://example.invalid"),
        ("GITHUB_SHA", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        ("GITHUB_JOB", "unreviewed"),
        ("GITHUB_WORKFLOW_SHA", "short"),
        ("GITHUB_RUN_ID", "0"),
        ("GITHUB_RUN_ID", "01"),
        ("GITHUB_RUN_ATTEMPT", "-1"),
        ("GITHUB_RUN_ATTEMPT", "1\n2"),
        ("GITHUB_RUN_ID", "123456789012345678901"),
    ] {
        let mut env = admitted_env(p);
        env.insert(name.into(), value.into());
        assert!(validate_env(&env, p, &commit()).is_err(), "{name}");
    }
    for suffix in [
        "refs/heads/",
        "refs/heads/../main",
        "refs/heads/main//branch",
        "refs/heads/main?token=fake",
        "refs/unreviewed/main",
    ] {
        let mut env = admitted_env(p);
        env.insert(
            "GITHUB_WORKFLOW_REF".into(),
            format!("{REPOSITORY}/{WORKFLOW}@{suffix}"),
        );
        assert!(validate_env(&env, p, &commit()).is_err());
    }
    let mut env = admitted_env(p);
    env.remove("GITHUB_ACTIONS");
    assert!(validate_env(&env, p, &commit()).is_err());
    env = admitted_env(p);
    env.insert(
        "GITHUB_WORKFLOW_REF".into(),
        format!("{REPOSITORY}/.github/workflows/ci.yml@refs/heads/main"),
    );
    assert!(validate_env(&env, p, &commit()).is_err());
}

#[test]
fn effective_rust_native_compiler_shell_and_sdk_overrides_fail_closed() {
    let p = profile("x86_64-unknown-linux-gnu").unwrap();
    for name in [
        "RUSTC",
        "RUSTDOC",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "RUSTUP_TOOLCHAIN",
        "RUST_TARGET_PATH",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_BUILD_RUSTC",
        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER",
        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER",
        "CARGO_PROFILE_RELEASE_OPT_LEVEL",
        "HOST_CC",
        "TARGET_CFLAGS",
        "CC_x86_64_unknown_linux_gnu",
        "CXX_x86_64-unknown-linux-gnu",
        "AR",
        "ARFLAGS",
        "RANLIB",
        "CPPFLAGS",
        "LDFLAGS",
        "CRATE_CC_NO_DEFAULTS",
        "CMAKE_TOOLCHAIN_FILE",
        "PKG_CONFIG_PATH",
        "LIBRARY_PATH",
        "CPATH",
        "C_INCLUDE_PATH",
        "CPLUS_INCLUDE_PATH",
        "GCC_EXEC_PREFIX",
        "COMPILER_PATH",
        "MAKEFLAGS",
        "PERL5LIB",
        "PERL5OPT",
        "BASH_ENV",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "DYLD_INSERT_LIBRARIES",
        "OPENSSL_DIR",
        "X86_64_UNKNOWN_LINUX_GNU_OPENSSL_DIR",
    ] {
        let mut env = admitted_env(p);
        env.insert(name.into(), "synthetic-private-override".into());
        let selected = select_env(pairs(&env)).unwrap();
        assert!(
            selected.contains_key(name),
            "override escaped selection: {name}"
        );
        let error = validate_env(&selected, p, &commit()).unwrap_err();
        assert!(!error.contains("synthetic-private-override"));
    }
    for (name, value) in [
        ("RUSTFLAGS", "-C target-cpu=native"),
        ("CFLAGS", "-march=native"),
        ("CXXFLAGS", "-O3 -march=x86-64-v4"),
        ("CC", "/synthetic/unreviewed-wrapper"),
        ("OPENSSL_NO_VENDOR", "1"),
        ("OPENSSL_STATIC", "0"),
        ("AUTOROUTER_BUILD_TARGET", "i686-unknown-linux-gnu"),
        ("AUTOROUTER_BUILD_IMAGE", "synthetic:mutable"),
        ("CARGO_TARGET_DIR", "relative"),
        ("CARGO_NET_OFFLINE", "false"),
        ("CARGO_BUILD_JOBS", "-2"),
    ] {
        let mut env = admitted_env(p);
        env.insert(name.into(), value.into());
        assert!(validate_env(&env, p, &commit()).is_err(), "{name}");
    }
}

#[test]
fn complete_path_is_checked_even_after_all_tools_would_resolve() {
    let p = profile("x86_64-unknown-linux-gnu").unwrap();
    for value in [
        "relative:/usr/bin",
        "/usr/bin:relative",
        "/usr/bin::/bin",
        "/usr/bin:",
        ":/usr/bin",
        "",
    ] {
        let mut env = admitted_env(p);
        env.insert("PATH".into(), value.into());
        assert!(validate_env(&env, p, &commit()).is_err(), "{value}");
    }
    let mut env = admitted_env(p);
    env.insert("PATH".into(), "/first/absolute:/second/absolute".into());
    validate_env(&env, p, &commit()).unwrap();
}

#[test]
fn environment_selection_omits_fake_credentials_and_enforces_encoded_bounds() {
    let p = profile("x86_64-unknown-linux-gnu").unwrap();
    let env = admitted_env(p);
    let mut input = pairs(&env);
    for name in [
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "ANTHROPIC_API_KEY",
        "JEV_API_KEY",
        "PRIVATE_PROMPT",
    ] {
        input.push((name.into(), "synthetic-secret-never-log".into()));
    }
    assert_eq!(select_env(input).unwrap(), env);
    let overflow = (0..65)
        .map(|n| (format!("CARGO_SYNTHETIC_{n}").into(), OsString::from("x")))
        .collect::<Vec<_>>();
    assert_eq!(
        select_env(overflow[..64].iter().cloned()).unwrap().len(),
        64
    );
    assert!(select_env(overflow).is_err());
    assert!(
        select_env(vec![(
            OsString::from("RUSTFLAGS"),
            OsString::from("x".repeat(8193))
        )])
        .is_err()
    );
    assert!(
        select_env(vec![(
            OsString::from(format!("RUST_{}", "x".repeat(124))),
            OsString::from("x")
        )])
        .is_err()
    );
    let aggregate = (0..9)
        .map(|n| {
            (
                format!("RUST_SYNTHETIC_{n}").into(),
                OsString::from("x".repeat(8192)),
            )
        })
        .collect::<Vec<_>>();
    assert!(select_env(aggregate).is_err());
    for bytes in [
        vec![255],
        b"-C target-cpu=generic\nprivate".to_vec(),
        b"bad\0value".to_vec(),
    ] {
        assert!(select_env(vec![("RUSTFLAGS".into(), OsString::from_vec(bytes))]).is_err());
    }
}

#[test]
fn each_builder_domain_is_bound_to_actual_admitted_state() {
    let p = profile("x86_64-unknown-linux-gnu").unwrap();
    let (builder, state) = metadata(p);
    for key in [
        "schema_version",
        "kind",
        "source_dirty",
        "source_commit",
        "cargo_lock_sha256",
        "workflow_sha256",
        "target",
        "image",
        "rust_archive_sha256",
        "rustc",
        "cargo",
        "gcc",
        "linker",
        "libc",
        "rustflags",
        "cflags",
        "openssl_no_vendor",
        "openssl_static",
        "perl_archive_sha256",
        "perl_version",
        "perl",
        "source_date_epoch",
        "kernel",
    ] {
        let mut changed = builder.clone();
        changed[key] = Value::Null;
        assert!(
            validate_builder(&changed, &state, p, Path::new("/synthetic")).is_err(),
            "{key}"
        );
    }
    for compiler in ["CC", "CXX"] {
        for field in [
            "path",
            "contents",
            "sha256",
            "effective_target_options",
            "baseline_macros",
        ] {
            let mut changed = builder.clone();
            changed["compiler_wrappers"][compiler][field] = json!("wrong synthetic observation");
            assert!(
                validate_builder(&changed, &state, p, Path::new("/synthetic")).is_err(),
                "{compiler}/{field}"
            );
        }
    }
    let mut changed = state.clone();
    changed["files"][WORKFLOW]["sha256"] = json!("0".repeat(64));
    assert!(validate_builder(&builder, &changed, p, Path::new("/synthetic")).is_err());
}

#[test]
fn begin_state_cannot_be_mixed_with_changed_source_compiler_or_output_reports() {
    let p = profile("x86_64-unknown-linux-gnu").unwrap();
    let (_, before) = metadata(p);
    let outputs = declared_outputs(
        &before,
        "/synthetic/production.jsonl",
        "/synthetic/features.json",
    )
    .unwrap();
    let mut after = before.clone();
    after.as_object_mut().unwrap().remove("outputs");
    bound_state(&before, after.clone(), &outputs).unwrap();
    for pointer in [
        "/source_commit",
        "/cargo_lock_sha256",
        "/source_date_epoch",
        "/target",
        "/ci/workflow_sha",
        "/ci/run_attempt",
        "/environment/RUSTFLAGS",
        "/compiler/rustc_text",
        "/compiler/wrappers/CC/contents",
    ] {
        let mut changed = after.clone();
        *changed.pointer_mut(pointer).unwrap() = json!("synthetic drift");
        assert!(
            bound_state(&before, changed, &outputs).is_err(),
            "{pointer}"
        );
    }
    assert!(
        declared_outputs(
            &before,
            "/synthetic/old-production.jsonl",
            "/synthetic/features.json"
        )
        .is_err()
    );
    assert!(
        declared_outputs(
            &before,
            "/synthetic/production.jsonl",
            "/synthetic/other-features.json"
        )
        .is_err()
    );
    after["extra_unreviewed_field"] = json!(true);
    assert!(bound_state(&before, after, &outputs).is_err());
}

#[test]
fn inputs_must_be_canonical_owned_json_with_reviewed_schema_profile_and_target() {
    let scratch = Scratch::new("provenance-inputs").unwrap();
    let p = profile("x86_64-unknown-linux-gnu").unwrap();
    let (_, state) = metadata(p);
    let valid = scratch
        .file("valid.json", &json_bytes(&state).unwrap())
        .unwrap();
    let (bytes, actual) = read_build_inputs(&valid).unwrap();
    assert_eq!(actual, state);
    assert_eq!(bytes, fs::read(&valid).unwrap());
    let compact = scratch
        .file("compact.json", &serde_json::to_vec(&state).unwrap())
        .unwrap();
    assert!(read_build_inputs(&compact).is_err());
    for (index, field) in ["schema_version", "kind", "profile", "target"]
        .into_iter()
        .enumerate()
    {
        let mut changed = state.clone();
        changed[field] = json!("unreviewed");
        let path = scratch
            .file(
                &format!("wrong-{index}.json"),
                &json_bytes(&changed).unwrap(),
            )
            .unwrap();
        assert!(read_build_inputs(&path).is_err());
    }
    let alias = scratch.0.join("alias.json");
    fs::hard_link(&valid, &alias).unwrap();
    assert!(read_build_inputs(&valid).is_err());
}

#[test]
fn cached_or_unspecified_dependency_artifacts_cannot_gain_current_compiler_attribution() {
    let rows = vec![
        json!({"reason":"compiler-artifact","target":{"name":"synthetic-dependency"},"fresh":false}),
        json!({"reason":"compiler-artifact","target":{"name":"claude-autorouter"},"fresh":false}),
        json!({"reason":"build-finished","success":true}),
    ];
    let encode = |rows: &[Value]| {
        rows.iter()
            .map(|row| serde_json::to_string(row).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes()
    };
    require_fresh_artifacts(&encode(&rows)).unwrap();
    for index in [0, 1] {
        for value in [json!(true), Value::Null, json!("false"), json!(0)] {
            let mut changed = rows.clone();
            changed[index]["fresh"] = value;
            assert!(require_fresh_artifacts(&encode(&changed)).is_err());
        }
    }
    let mut changed = rows;
    changed[0].as_object_mut().unwrap().remove("fresh");
    assert!(require_fresh_artifacts(&encode(&changed)).is_err());
    assert!(require_fresh_artifacts(b"{bad").is_err());
    assert!(require_fresh_artifacts(&[255]).is_err());
    // Unique successful shipping inventory/profile validation remains the
    // separate existing production_features::inspect prerequisite, not this helper.
}

#[test]
fn prospective_release_tree_refuses_existing_caches_without_deleting_them() {
    let scratch = Scratch::new("provenance-existing-release").unwrap();
    let state = json!({"environment":{}});
    let parent = scratch.0.join("rust/target");
    fs::create_dir_all(&parent).unwrap();
    let release = parent.join("release");
    fresh_release_tree(&scratch.0, &state).unwrap();
    fs::create_dir(&release).unwrap();
    fs::write(release.join("sentinel"), b"old cache").unwrap();
    assert!(fresh_release_tree(&scratch.0, &state).is_err());
    assert_eq!(fs::read(release.join("sentinel")).unwrap(), b"old cache");
    fs::remove_file(release.join("sentinel")).unwrap();
    fs::remove_dir(&release).unwrap();
    fs::write(&release, b"old cache file").unwrap();
    assert!(fresh_release_tree(&scratch.0, &state).is_err());
    assert_eq!(fs::read(&release).unwrap(), b"old cache file");
    fs::remove_file(&release).unwrap();
    let target = scratch.file("symlink-target", b"old cache target").unwrap();
    symlink(&target, &release).unwrap();
    assert!(fresh_release_tree(&scratch.0, &state).is_err());
    assert!(
        fs::symlink_metadata(&release)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(target).unwrap(), b"old cache target");
    let custom = scratch.0.join("custom-target");
    fs::create_dir(&custom).unwrap();
    let custom_state = json!({"environment":{"CARGO_TARGET_DIR":custom}});
    fresh_release_tree(&scratch.0, &custom_state).unwrap();
    fs::create_dir(custom.join("release")).unwrap();
    assert!(fresh_release_tree(&scratch.0, &custom_state).is_err());
}

#[test]
fn feature_report_bytes_cannot_be_reformatted_or_mixed_between_artifacts() {
    let report = json!({"passed":true,"report_sha256":"c".repeat(64),"binary_sha256":"d".repeat(64),"development_probes_feature":false});
    let bytes = json_bytes(&report).unwrap();
    matching_feature_report(&bytes, &report).unwrap();
    assert!(matching_feature_report(&serde_json::to_vec(&report).unwrap(), &report).is_err());
    for key in [
        "passed",
        "report_sha256",
        "binary_sha256",
        "development_probes_feature",
    ] {
        let mut changed = report.clone();
        changed[key] = json!("synthetic-private-report-value");
        let error = matching_feature_report(&json_bytes(&changed).unwrap(), &report).unwrap_err();
        assert!(!error.contains("synthetic-private-report-value"));
    }
}

#[test]
fn executable_must_be_the_actual_admitted_release_path_without_symlink_aliases() {
    let scratch = Scratch::new("provenance-executable").unwrap();
    let directory = scratch.0.join("rust/target/release");
    fs::create_dir_all(&directory).unwrap();
    let executable = directory.join("claude-autorouter");
    fs::write(&executable, b"synthetic not executable").unwrap();
    let before = json!({"environment":{}});
    let inspected = json!({"binary":{"executable":executable}});
    assert_eq!(
        admitted_executable(&scratch.0, &before, &inspected).unwrap(),
        executable
    );
    let custom = scratch.0.join("custom-target");
    fs::create_dir_all(custom.join("release")).unwrap();
    let custom_exe = custom.join("release/claude-autorouter");
    fs::write(&custom_exe, b"different synthetic bytes").unwrap();
    let custom_before = json!({"environment":{"CARGO_TARGET_DIR":custom}});
    let custom_inspected = json!({"binary":{"executable":custom_exe}});
    admitted_executable(&scratch.0, &custom_before, &custom_inspected).unwrap();
    assert!(admitted_executable(&scratch.0, &before, &custom_inspected).is_err());
    let alias = directory.join("alias");
    symlink(&executable, &alias).unwrap();
    assert!(
        admitted_executable(&scratch.0, &before, &json!({"binary":{"executable":alias}})).is_err()
    );
    assert!(admitted_executable(&scratch.0, &before, &json!({"binary":{}})).is_err());
}

fn put(bytes: &mut [u8], offset: usize, width: usize, value: u64) {
    for index in 0..width {
        bytes[offset + index] = (value >> (index * 8)) as u8;
    }
}
// Minimal data-only ELF, never loaded or executed: one mapped dynamic table,
// libc version need, and optional forbidden dependency/search path.
fn elf(machine: u64, glibc: &str, extra: Option<&str>, search: Option<u64>) -> Vec<u8> {
    let mut bytes = vec![0; 0x800];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    put(&mut bytes, 16, 2, 3);
    put(&mut bytes, 18, 2, machine);
    put(&mut bytes, 32, 8, 64);
    put(&mut bytes, 54, 2, 56);
    put(&mut bytes, 56, 2, 3);
    let mut strings = vec![0];
    let mut offset = |value: &str| {
        let at = strings.len() as u64;
        strings.extend_from_slice(value.as_bytes());
        strings.push(0);
        at
    };
    let libc = offset("libc.so.6");
    let version = offset(glibc);
    let extra = extra.map(&mut offset);
    let path = offset("/synthetic/builder/lib");
    bytes[0x480..0x480 + strings.len()].copy_from_slice(&strings);
    let mut tags = vec![
        (1, libc),
        (5, 0x8280),
        (10, strings.len() as u64),
        (0x6ffffffe, 0x8400),
        (0x6fffffff, 1),
    ];
    if let Some(extra) = extra {
        tags.push((1, extra));
    }
    if let Some(tag) = search {
        tags.push((tag, path));
    }
    tags.push((0, 0));
    for (index, (kind, start, size)) in [
        (1, 0x200, 0x600),
        (3, 0x210, 12),
        (2, 0x280, tags.len() as u64 * 16),
    ]
    .into_iter()
    .enumerate()
    {
        let at = 64 + index * 56;
        put(&mut bytes, at, 4, kind);
        put(&mut bytes, at + 8, 8, start);
        put(&mut bytes, at + 16, 8, 0x8000 + start - 0x200);
        put(&mut bytes, at + 32, 8, size);
        put(&mut bytes, at + 40, 8, size);
    }
    bytes[0x210..0x21c].copy_from_slice(b"/ld-test.so\0");
    for (index, (tag, value)) in tags.into_iter().enumerate() {
        put(&mut bytes, 0x280 + index * 16, 8, tag);
        put(&mut bytes, 0x288 + index * 16, 8, value);
    }
    put(&mut bytes, 0x600, 2, 1);
    put(&mut bytes, 0x602, 2, 1);
    put(&mut bytes, 0x604, 4, libc);
    put(&mut bytes, 0x608, 4, 16);
    put(&mut bytes, 0x618, 4, version);
    bytes
}

#[test]
fn real_header_and_dynamic_metadata_require_gnu_target_baseline_and_no_host_tls_paths() {
    for (target, machine) in [
        ("x86_64-unknown-linux-gnu", 62),
        ("aarch64-unknown-linux-gnu", 183),
    ] {
        let p = profile(target).unwrap();
        let good = elf(machine, "GLIBC_2.28", None, None);
        let inspected = inspected_gnu_binary(p, &good).unwrap();
        assert_eq!(inspected["target"], target);
        assert_eq!(inspected["minimum_glibc"], "2.28");
        assert_eq!(inspected["execution_qualification"], "pending");
        assert!(inspected_gnu_binary(p, &good[..63]).is_err());
        assert!(
            inspected_gnu_binary(
                p,
                &elf(
                    if machine == 62 { 183 } else { 62 },
                    "GLIBC_2.28",
                    None,
                    None
                )
            )
            .is_err()
        );
        for version in ["GLIBC_2.29", "GLIBC_ABI_DT_RELR"] {
            let bytes = elf(machine, version, None, None);
            let raw = crate::provenance_binary::inspect(target, &bytes).unwrap();
            assert!(
                raw["glibc_version_requirements"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(version))
            );
            assert!(inspected_gnu_binary(p, &bytes).is_err());
        }
        for dependency in [
            "libssl.so.3",
            "libcrypto.so.3",
            "/synthetic/lib/libssl-test.so",
        ] {
            let bytes = elf(machine, "GLIBC_2.28", Some(dependency), None);
            let raw = crate::provenance_binary::inspect(target, &bytes).unwrap();
            assert!(
                raw["dynamic_libraries"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(dependency))
            );
            assert!(inspected_gnu_binary(p, &bytes).is_err());
        }
        for search in [15, 29] {
            let bytes = elf(machine, "GLIBC_2.28", None, Some(search));
            let raw = crate::provenance_binary::inspect(target, &bytes).unwrap();
            assert_eq!(
                raw[if search == 15 { "rpath" } else { "runpath" }],
                "/synthetic/builder/lib"
            );
            assert!(inspected_gnu_binary(p, &bytes).is_err());
        }
    }
}

#[test]
fn stable_reads_allow_sdk_hardlinks_but_owned_reports_reject_them() {
    let scratch = Scratch::new("provenance-hardlinks").unwrap();
    let file = scratch.file("tool", b"synthetic SDK bytes").unwrap();
    let alias = scratch.0.join("tool-alias");
    fs::hard_link(&file, &alias).unwrap();
    assert_eq!(stable(&file, 19).unwrap(), b"synthetic SDK bytes");
    assert!(single_report(&file, 19).is_err());
    let before = descriptor(&file, 19).unwrap();
    assert_eq!(before["identity"]["links"], 2);
    fs::remove_file(alias).unwrap();
    single_report(&file, 19).unwrap();
    assert_ne!(descriptor(&file, 19).unwrap(), before);
}

#[test]
fn input_bounds_symlinks_fifo_and_directories_fail_without_reading_payloads() {
    let scratch = Scratch::new("provenance-file-bounds").unwrap();
    let file = scratch.file("plain", b"123456789").unwrap();
    assert_eq!(stable(&file, 9).unwrap(), b"123456789");
    assert!(stable(&file, 8).is_err());
    let alias = scratch.0.join("symlink");
    symlink(&file, &alias).unwrap();
    assert!(stable(&alias, 9).is_err());
    assert!(canonical(&alias).is_err());
    let fifo = scratch.0.join("fifo");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR).unwrap();
    assert!(stable(&fifo, 9).is_err());
    assert!(stable(&scratch.0, 9).is_err());
    let sparse = scratch.file("sparse", b"").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&sparse)
        .unwrap()
        .set_len(INPUT_MAX + 1)
        .unwrap();
    assert!(single_report(&sparse, INPUT_MAX).is_err());
    assert!(json_bytes(&json!({"synthetic":"x".repeat(OUTPUT_MAX)})).is_err());
}

#[test]
fn owned_file_drop_removes_only_its_created_path_and_keep_retains_exact_mode() {
    let scratch = Scratch::new("provenance-owned-file").unwrap();
    let path = scratch.0.join("created");
    let owned = fresh_file(&path, b"owned bytes", 0o755).unwrap();
    assert!(owned.matches());
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o755);
    drop(owned);
    assert!(!path.exists());
    let mut kept = fresh_file(&path, b"kept bytes", 0o600).unwrap();
    kept.keep();
    drop(kept);
    assert_eq!(fs::read(&path).unwrap(), b"kept bytes");
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
}

#[test]
fn file_and_symlink_collisions_preserve_unowned_bytes_and_never_call_writer() {
    let scratch = Scratch::new("provenance-collision").unwrap();
    let path = scratch.file("sentinel", b"unowned sentinel").unwrap();
    let before = fs::symlink_metadata(&path).unwrap();
    let calls = Cell::new(0);
    assert!(
        persist_owned_file(&path, 0o600, |_| {
            calls.set(calls.get() + 1);
            Ok(())
        })
        .is_err()
    );
    assert_eq!(calls.get(), 0);
    assert_eq!(fs::read(&path).unwrap(), b"unowned sentinel");
    assert_eq!(fs::metadata(&path).unwrap().ino(), before.ino());
    let link = scratch.0.join("link");
    symlink(&path, &link).unwrap();
    assert!(fresh_file(&link, b"replacement", 0o600).is_err());
    assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
    assert_eq!(fs::read(path).unwrap(), b"unowned sentinel");
}

#[test]
fn partial_write_and_sync_style_errors_remove_only_the_created_inode() {
    let scratch = Scratch::new("provenance-write-failure").unwrap();
    let path = scratch.0.join("partial");
    let calls = Cell::new(0);
    let result = persist_owned_file(&path, 0o600, |file| {
        calls.set(calls.get() + 1);
        file.write_all(b"partial bytes")?;
        Err(io::Error::other("synthetic sync failure"))
    });
    assert!(result.is_err());
    assert_eq!(calls.get(), 1);
    assert!(!path.exists());
    let moved = scratch.0.join("moved-owned");
    let result = persist_owned_file(&path, 0o600, |file| {
        file.write_all(b"owned partial")?;
        fs::rename(&path, &moved)?;
        fs::write(&path, b"unowned replacement")?;
        Err(io::Error::other("synthetic write failure"))
    });
    assert!(result.is_err());
    assert_eq!(fs::read(&path).unwrap(), b"unowned replacement");
    assert_eq!(fs::read(moved).unwrap(), b"owned partial");
}

#[test]
fn replaced_path_survives_guard_drop_and_other_hardlink_remains_owned_by_caller() {
    let scratch = Scratch::new("provenance-replacement").unwrap();
    let path = scratch.0.join("file");
    let moved = scratch.0.join("moved");
    let owned = fresh_file(&path, b"original", 0o600).unwrap();
    fs::rename(&path, &moved).unwrap();
    fs::write(&path, b"replacement").unwrap();
    assert!(!owned.matches());
    drop(owned);
    assert_eq!(fs::read(&path).unwrap(), b"replacement");
    assert_eq!(fs::read(&moved).unwrap(), b"original");
    let fresh = scratch.0.join("fresh");
    let alias = scratch.0.join("retained-alias");
    let owned = fresh_file(&fresh, b"aliased owned", 0o600).unwrap();
    fs::hard_link(&fresh, &alias).unwrap();
    drop(owned);
    assert!(!fresh.exists());
    assert_eq!(fs::read(alias).unwrap(), b"aliased owned");
}

#[test]
fn second_output_collision_cleans_created_first_file_and_preserves_collision() {
    let scratch = Scratch::new("provenance-second-collision").unwrap();
    let first = scratch.0.join("binary");
    let second = scratch
        .file("provenance.json", b"unowned provenance sentinel")
        .unwrap();
    let second_identity = Identity::of(&fs::metadata(&second).unwrap());
    let owned = vec![fresh_file(&first, b"new binary", 0o755).unwrap()];
    assert!(fresh_file(&second, b"new report", 0o600).is_err());
    drop(owned);
    assert!(!first.exists());
    assert!(second_identity.matches(&second, false));
    assert_eq!(fs::read(second).unwrap(), b"unowned provenance sentinel");
}

#[test]
fn replacing_output_directory_cannot_make_cleanup_remove_another_directorys_files() {
    let scratch = Scratch::new("provenance-directory-replacement").unwrap();
    let out = scratch.0.join("output");
    fs::create_dir(&out).unwrap();
    let directory = Identity::of(&fs::metadata(&out).unwrap());
    let owned = fresh_file(&out.join("binary"), b"owned bytes", 0o755).unwrap();
    let moved = scratch.0.join("moved-output");
    fs::rename(&out, &moved).unwrap();
    fs::create_dir(&out).unwrap();
    fs::write(out.join("binary"), b"unowned replacement").unwrap();
    assert!(!directory.matches(&out, true));
    assert!(!owned.matches());
    drop(owned);
    assert_eq!(
        fs::read(out.join("binary")).unwrap(),
        b"unowned replacement"
    );
    assert_eq!(fs::read(moved.join("binary")).unwrap(), b"owned bytes");
}
