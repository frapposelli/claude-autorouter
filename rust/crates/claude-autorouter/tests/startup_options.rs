#![cfg(unix)]
mod support;
use std::fs::File;
use std::io::Read;
use std::os::unix::process::CommandExt;
use support::{Home, output, success};

fn runtime_prefix() -> String {
    let Some(executable) = std::env::var_os("AUTOROUTER_TEST_EXECUTABLE") else {
        return "synthetic-runtime-name".into();
    };
    let mut prefix = [0; 2];
    File::open(executable)
        .unwrap()
        .read_exact(&mut prefix)
        .unwrap();
    if prefix != *b"#!" {
        return "synthetic-runtime-name".into();
    }
    // An npm dispatcher execs its selected artifact with a new argv[0]. The
    // package verifier supplies that exact artifact as an independent oracle;
    // a direct native invocation still checks the custom argv[0] unchanged.
    let native = std::env::var_os("AUTOROUTER_TEST_NATIVE_EXECUTABLE")
        .expect("Installed dispatcher tests require the verified native artifact path");
    let native = std::fs::canonicalize(native).unwrap();
    File::open(&native)
        .unwrap()
        .read_exact(&mut prefix)
        .unwrap();
    assert_ne!(prefix, *b"#!", "Expected the verified native binary");
    native.to_string_lossy().into_owned()
}

#[test]
fn invalid_ca_selectors_fail_before_every_cli_dispatch_with_frozen_node_diagnostics() {
    let home = Home::new();
    let prefix = runtime_prefix();
    home.write("config.json", b"PRIVATE malformed configuration");
    home.claude("#!/bin/sh\nprintf 'CHILD_MUST_NOT_START\\n'\nexit 77\n");
    let before = home.names();
    let cases = [
        (
            "--use-openssl-ca --use-bundled-ca",
            "either --use-openssl-ca or --use-bundled-ca can be used, not both",
        ),
        (
            "--use-bundled-ca --use-openssl-ca",
            "either --use-openssl-ca or --use-bundled-ca can be used, not both",
        ),
        (
            "--use-openssl-ca=true --use-bundled-ca=false",
            "either --use-openssl-ca or --use-bundled-ca can be used, not both",
        ),
        (
            "--use-system-ca",
            "--use-system-ca is not allowed in NODE_OPTIONS",
        ),
        (
            "--use_system_ca",
            "--use_system_ca is not allowed in NODE_OPTIONS",
        ),
        (
            "--use-system-ca=false",
            "--use-system-ca= is not allowed in NODE_OPTIONS",
        ),
        (
            "--use-system-ca=0",
            "--use-system-ca= is not allowed in NODE_OPTIONS",
        ),
        (
            "--no-use-system-ca",
            "--no-use-system-ca is not allowed in NODE_OPTIONS",
        ),
        (
            "--no_use_system_ca",
            "--no_use_system_ca is not allowed in NODE_OPTIONS",
        ),
        (
            "\"--use-system-ca\"",
            "--use-system-ca is not allowed in NODE_OPTIONS",
        ),
        (
            "--use-system-ca --use-openssl-ca --use-bundled-ca",
            "--use-system-ca is not allowed in NODE_OPTIONS",
        ),
    ];
    for (options, diagnostic) in cases {
        for args in [
            vec!["--help"],
            vec!["--version"],
            vec!["statusline"],
            vec!["config", "show"],
            vec!["setup"],
            vec!["doctor"],
            vec!["serve"],
            vec!["claude", "--help"],
            vec!["claude"],
        ] {
            let result = output(
                home.command()
                    .args(args)
                    .arg0("synthetic-runtime-name")
                    .env("NODE_OPTIONS", options)
                    .env("NODE_EXTRA_CA_CERTS", home.0.join("missing-roots.pem")),
            );
            assert_eq!(result.status.code(), Some(9), "{options}");
            assert!(result.stdout.is_empty());
            assert_eq!(
                result.stderr,
                format!("{prefix}: {diagnostic}\n").as_bytes()
            );
            assert_eq!(home.names(), before);
        }
    }
}

#[test]
fn negated_ca_selectors_leave_help_independent_of_runtime_trust_loading() {
    let home = Home::new();
    for options in [
        "--use-openssl-ca --no-use-openssl-ca",
        "--use-openssl-ca --use-bundled-ca --no-use-bundled-ca",
        "--use-bundled-ca --use-openssl-ca --no-use-openssl-ca",
        "--no-use-bundled-ca",
        "--use-openssl-ca --no-use-bundled-ca",
    ] {
        let result = output(home.command().arg("--help").env("NODE_OPTIONS", options));
        assert!(success(&result).contains("claude-autorouter"));
        assert!(home.names().is_empty());
    }
}
