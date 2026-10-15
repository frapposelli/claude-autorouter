#!/usr/bin/env python3
"""Bounded CI receipts for real i686 runtime, CLI, and installed private archives.

This is a runner-specific execution check. It is not minimum-platform, task-quality,
production-release, or full Node-baseline architecture qualification.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys
import tomllib
import uuid

ROOT = Path(os.environ["GITHUB_WORKSPACE"])
OUT = ROOT / "artifacts/rust-rewrite/runtime32"
TARGET = "i686-unknown-linux-gnu"
TARGET_DIR = OUT / "target"
BASELINE_COMMIT = "ea930c247626ce2af5ccdad721b5121417bf4ad8"
MIB = 1024 * 1024
TEST_BINARY_CAP = 128 * MIB  # Streamed debug-test evidence only; shipping cap stays 32 MiB.
RELEASE_BINARY_CAP = 32 * MIB
PACKAGE_CAP = 32 * MIB
SOURCE_TOTAL_CAP = 64 * MIB
REQUIRED = {
    ("autorouter-runtime", "autorouter_runtime"): [
        "tls_roots::tests::backend_is_the_reviewed_vendored_release",
    ],
    ("autorouter-runtime", "bounded_json_contracts"): [
        "exact_65537_byte_overflow_releases_body_under_all_three_frozen_headers",
        "public_reader_invalid_limits_precede_cancellation_and_valid_cancel_precedes_headers",
    ],
    ("autorouter-runtime", "session_history_limits"): [
        "direct_invalid_fields_reject_before_runtime_worker_path_or_id_admission",
    ],
    ("autorouter-runtime", "routing_compatibility_contracts"): [
        "supplemental_diagnostic_number_comparison_rejects_lossy_and_saturating_integers",
    ],
    ("claude-autorouter", "commands"): [
        "direct_claude_help_forwards_literal_arguments_and_removes_router_secrets",
    ],
}
SUITES = [
    "commands", "edge_cases", "launcher_logging", "onboarding_local",
    "launcher_contracts", "startup_options", "config_contracts",
]


def receipt(path, limit, contents=False, prefix_bytes=0):
    path = Path(path)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    chunks = []
    prefix = bytearray()
    digest = hashlib.sha256()
    total = 0
    try:
        before = os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or not 0 <= before.st_size <= limit:
            raise ValueError("Evidence file type or byte limit rejected")
        while True:
            chunk = os.read(fd, min(65536, limit - total + 1))
            if not chunk:
                break
            total += len(chunk)
            if total > limit:
                raise ValueError("Evidence file grew beyond its declared bound")
            digest.update(chunk)
            if len(prefix) < prefix_bytes:
                prefix.extend(chunk[:prefix_bytes - len(prefix)])
            if contents:
                chunks.append(chunk)
        after = os.fstat(fd)
        identity = lambda item: (
            item.st_dev, item.st_ino, item.st_size, item.st_mtime_ns, item.st_ctime_ns
        )
        if identity(before) != identity(after) or total != before.st_size:
            raise ValueError("Evidence identity changed while being read")
    finally:
        os.close(fd)
    row = {
        "path": str(path.relative_to(ROOT)),
        "bytes": total,
        "sha256": digest.hexdigest(),
    }
    if contents:
        return row, b"".join(chunks)
    return (row, bytes(prefix)) if prefix_bytes else row


def read_json(path, limit=2 * MIB):
    row, raw = receipt(path, limit, contents=True)
    return row, json.loads(raw)


def write(name, value):
    data = (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()
    if len(data) > 2 * MIB:
        raise ValueError("Evidence report exceeds its byte bound")
    fd = os.open(OUT / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as output:
        output.write(data)


def elf32(path, limit, parent=None):
    path = Path(path)
    if parent is not None and path.parent != parent:
        raise ValueError("ELF32 executable is outside its declared target directory")
    if path.is_symlink() or not os.access(path, os.X_OK):
        raise ValueError("ELF32 executable is a link or is not executable")
    row, header = receipt(path, limit, prefix_bytes=64)
    if (
        header[:6] != b"\x7fELF\x01\x01"
        or header[16:18] not in (b"\x02\x00", b"\x03\x00")
        or header[18:20] != b"\x03\x00"
    ):
        raise ValueError("Expected genuine little-endian ELF32 EM_386 executable")
    row.update({"class": "ELF32", "machine": "EM_386", "target": TARGET})
    return row


def source_rows():
    fixed = [
        ".github/workflows/rust-runtime32.yml",
        "rust/distribution/ci/runtime32-proof.py",
        "rust/distribution/autorouter.sh",
        "rust/distribution/platforms.json",
        "rust/parity/baseline.json",
        "rust/Cargo.toml",
        "rust/Cargo.lock",
        "rust/rust-toolchain.toml",
        "rust/.cargo/config.toml",
    ]
    paths = {ROOT / name for name in fixed}
    for crate in ("autorouter-core", "autorouter-runtime", "claude-autorouter"):
        base = ROOT / "rust/crates" / crate
        paths.add(base / "Cargo.toml")
        paths.update(base.rglob("*.rs"))
    xtask = ROOT / "rust/xtask"
    paths.add(xtask / "Cargo.toml")
    paths.update(xtask.rglob("*.rs"))
    paths.update((ROOT / "rust/parity/cases").glob("*.json"))
    paths.update((ROOT / "rust/parity/cases").glob("*.jsonl"))
    if not 50 <= len(paths) <= 1024:
        raise ValueError("Source receipt inventory count rejected")
    rows = [receipt(path, 8 * MIB) for path in sorted(paths)]
    if sum(row["bytes"] for row in rows) > SOURCE_TOTAL_CAP:
        raise ValueError("Source receipt aggregate exceeds its declared bound")
    return rows


def expected_targets(package):
    base = ROOT / "rust/crates" / package
    _, manifest_bytes = receipt(base / "Cargo.toml", 64 * 1024, contents=True)
    manifest = tomllib.loads(manifest_bytes.decode())
    if (
        manifest["package"]["name"] != package
        or manifest["package"].get("autotests", True) is not True
        or manifest.get("test")
        or manifest.get("bin")
    ):
        raise ValueError("Unexpected explicit runtime or CLI test-target configuration")
    unit_name, unit_path, unit_kind = (
        ("autorouter_runtime", base / "src/lib.rs", ["lib"])
        if package == "autorouter-runtime"
        else ("claude-autorouter", base / "src/main.rs", ["bin"])
    )
    paths = list((base / "tests").glob("*.rs"))
    lower = 30 if package == "autorouter-runtime" else 7
    if not lower <= len(paths) <= 128 or not unit_path.is_file():
        raise ValueError("Incomplete or excessive runtime/CLI integration target inventory")
    names = {path.stem for path in paths}
    if len(names) != len(paths):
        raise ValueError("Duplicate integration test target name")
    return {unit_name: (unit_path, unit_kind), **{
        path.stem: (path, ["test"]) for path in paths
    }}


def build_tests(package):
    log = "runtime-build.jsonl" if package == "autorouter-runtime" else "cli-build.jsonl"
    report, raw = receipt(OUT / log, 16 * MIB, contents=True)
    expected = expected_targets(package)
    selected = {}
    completed = []
    base = ROOT / "rust/crates" / package
    for line in raw.decode().splitlines():
        row = json.loads(line)
        if row.get("reason") == "build-finished":
            completed.append(row.get("success"))
        if row.get("reason") != "compiler-artifact" or row.get("profile", {}).get("test") is not True:
            continue
        target = row.get("target", {})
        source = target.get("src_path")
        if not isinstance(source, str) or not Path(source).is_relative_to(base):
            continue
        name = target.get("name")
        if name not in expected or name in selected:
            raise ValueError("Unexpected or ambiguous package test executable")
        expected_source, expected_kind = expected[name]
        if Path(source) != expected_source or target.get("kind") != expected_kind:
            raise ValueError("Cargo test executable source or target kind differs")
        executable = row.get("executable")
        if not isinstance(executable, str):
            raise ValueError("Package test executable is missing")
        selected[name] = elf32(
            executable, TEST_BINARY_CAP, TARGET_DIR / TARGET / "debug/deps"
        )
    if completed != [True] or set(selected) != set(expected):
        raise ValueError("A successful build and every runtime/CLI test executable are required")
    return {"build": report, "binaries": dict(sorted(selected.items()))}


def runner_command():
    script = ROOT / "rust/distribution/ci/runtime32-proof.py"
    if " " in str(script):
        raise ValueError("Cargo runner source path contains a space")
    return f"python3 {script} run-test"


def environment(allow_runner=False):
    version = tomllib.loads(
        receipt(ROOT / "rust/rust-toolchain.toml", 64 * 1024, contents=True)[1].decode()
    )["toolchain"]["channel"]
    wanted = {
        "CARGO_INCREMENTAL": "0",
        "CARGO_PROFILE_DEV_DEBUG": "0",
        "CARGO_PROFILE_TEST_DEBUG": "0",
        "CARGO_BUILD_JOBS": "4",
        "CARGO_TARGET_DIR": str(TARGET_DIR),
        "CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_LINKER": "gcc",
        "CARGO_BUILD_TARGET": TARGET,
        "CARGO_NET_OFFLINE": "true",
        "RUSTUP_TOOLCHAIN": version + "-x86_64-unknown-linux-gnu",
        "OPENSSL_NO_VENDOR": "0",
        "RUSTFLAGS": "",
    }
    for key, value in wanted.items():
        if os.environ.get(key) != value:
            raise ValueError("Runtime32 target, compiler, offline or OpenSSL environment differs")
    for key in [
        "RUSTC", "RUSTDOC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_ENCODED_RUSTFLAGS",
        "OPENSSL_DIR", "OPENSSL_LIB_DIR", "OPENSSL_INCLUDE_DIR",
    ]:
        if os.environ.get(key):
            raise ValueError("Unexpected compiler, runner or TLS override")
    runner = os.environ.get("CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_RUNNER")
    if runner != (runner_command() if allow_runner else None):
        raise ValueError("i686 Cargo test runner differs from its declared phase")
    return wanted


def preflight():
    names = [
        "apt-policy.txt", "requested-root-versions.tsv", "installed-packages.tsv",
        "apt-index-targets.txt", "gcc-version.txt", "perl-ipc-cmd.txt",
        "make-version.txt", "python-version.txt", "os-release.txt", "kernel.txt",
        "rustc-version.txt", "cargo-version.txt", "rustup-version.txt",
        "target-cfg.txt", "host-uname.txt", "personality-uname.txt",
        "personality-rustc-version.txt", "source-commit.txt", "source-status.txt",
    ]
    rows = {name: receipt(OUT / name, 2 * MIB) for name in names}
    checks = {
        "host-uname.txt": b"x86_64\n",
        "personality-uname.txt": b"i686\n",
        "source-status.txt": b"",
    }
    for name, value in checks.items():
        if receipt(OUT / name, 64, contents=True)[1] != value:
            raise ValueError("Host, i686 personality or source-clean preflight differs")
    cfg = receipt(OUT / "target-cfg.txt", 64 * 1024, contents=True)[1]
    if b'target_pointer_width="32"' not in cfg.splitlines():
        raise ValueError("Rust target pointer width is not 32")
    rustc = receipt(OUT / "personality-rustc-version.txt", 64 * 1024, contents=True)[1]
    if b"host: x86_64-unknown-linux-gnu" not in rustc.splitlines():
        raise ValueError("i686 personality changed the pinned compiler host")
    if not receipt(OUT / "perl-ipc-cmd.txt", 1024, contents=True)[1].startswith(b"IPC::Cmd "):
        raise ValueError("Perl IPC::Cmd build module is unavailable")
    c = elf32(OUT / "preflight32", MIB, OUT)
    return {"materials": rows, "gcc_m32_executed_elf32": c}


def load(name):
    return read_json(OUT / name)[1]


def check_prepared():
    prepared = load("prepared.json")
    if (
        prepared["kind"] != "native_runtime32_prepared"
        or prepared["target"] != TARGET
        or prepared["sources"] != source_rows()
        or prepared["environment"] != environment(allow_runner=True)
    ):
        raise ValueError("Prepared i686 source or build environment changed")
    for package in ("autorouter-runtime", "claude-autorouter"):
        expected = expected_targets(package)
        binaries = prepared["packages"][package]["binaries"]
        if set(binaries) != set(expected):
            raise ValueError("Prepared test executable inventory changed")
        for name, row in binaries.items():
            if elf32(ROOT / row["path"], TEST_BINARY_CAP, TARGET_DIR / TARGET / "debug/deps") != row:
                raise ValueError("Prepared ELF32 test executable changed")
    return prepared


def inspect():
    prepared = {
        "schema_version": 1,
        "kind": "native_runtime32_prepared",
        "target": TARGET,
        "execution": "pending",
        "environment": environment(),
        "preflight": preflight(),
        "sources": source_rows(),
        "packages": {
            package: build_tests(package)
            for package in ("autorouter-runtime", "claude-autorouter")
        },
        "debug_test_binary_cap_bytes": TEST_BINARY_CAP,
        "shipping_native_file_cap_bytes": RELEASE_BINARY_CAP,
        "scope": "Every discovered runtime/CLI test executable is ELF32 EM_386; complete execution and installed artifacts pending",
    }
    write("prepared.json", prepared)


def run_test():
    """Cargo's target runner checks the real ELF32 binary, records it, then replaces itself."""
    if len(sys.argv) < 3:
        raise ValueError("Cargo runner omitted its test executable")
    environment(allow_runner=True)
    if os.uname().machine != "i686":
        raise ValueError("Cargo test child lacks genuine i686 uname personality")
    phase = os.environ.get("AUTOROUTER_RUNTIME32_PHASE")
    if phase not in ("default-runtime", "default-cli", "npm", "direct"):
        raise ValueError("Unexpected i686 test execution phase")
    raw_path = Path(sys.argv[2])
    path = raw_path if raw_path.is_absolute() else ROOT / "rust" / raw_path
    if phase == "default-runtime":
        packages = ("autorouter-runtime",)
    else:
        packages = ("claude-autorouter",)
    prepared = load("prepared.json")
    matches = [
        (package, name, row)
        for package in packages
        for name, row in prepared["packages"][package]["binaries"].items()
        if ROOT / row["path"] == path
    ]
    if len(matches) != 1:
        raise ValueError("Cargo runner executable is not one prepared package target")
    package, name, row = matches[0]
    if phase in ("npm", "direct") and name not in SUITES:
        raise ValueError("Installed lifecycle invoked an unexpected test target")
    if elf32(path, TEST_BINARY_CAP, TARGET_DIR / TARGET / "debug/deps") != row:
        raise ValueError("Cargo runner executable differs from prepared ELF32 bytes")
    receipts = OUT / "runner-receipts" / phase
    receipts.mkdir(parents=True, exist_ok=True)
    record = {
        "schema_version": 1,
        "phase": phase,
        "package": package,
        "target": name,
        "binary": row,
        "personality": "i686",
        "arg_count": len(sys.argv) - 3,
    }
    data = (json.dumps(record, sort_keys=True) + "\n").encode()
    if len(data) > 2048:
        raise ValueError("Cargo runner receipt exceeds bound")
    fd = os.open(receipts / (uuid.uuid4().hex + ".json"), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as output:
        output.write(data)
        output.flush()
        os.fsync(output.fileno())
    os.execv(path, [str(path), *sys.argv[3:]])


def build_executable(log_name, source, name, path, limit):
    log, raw = receipt(OUT / log_name, 16 * MIB, contents=True)
    artifacts = []
    completed = []
    for line in raw.decode().splitlines():
        row = json.loads(line)
        if row.get("reason") == "build-finished":
            completed.append(row.get("success"))
        if row.get("reason") != "compiler-artifact":
            continue
        target = row.get("target", {})
        if target.get("name") == name and target.get("kind") == ["bin"] and target.get("src_path") == str(source):
            artifacts.append(row.get("executable"))
    if completed != [True] or artifacts != [str(path)]:
        raise ValueError("Release build lacks one exact successful target executable")
    return {"build": log, "executable": elf32(path, limit, path.parent)}


def production():
    check_prepared()
    source = ROOT / "rust/crates/claude-autorouter/src/main.rs"
    path = TARGET_DIR / TARGET / "release/claude-autorouter"
    built = build_executable("production-build.jsonl", source, "claude-autorouter", path, RELEASE_BINARY_CAP)
    copied = elf32(OUT / "claude-autorouter-i686", RELEASE_BINARY_CAP, OUT)
    if copied["sha256"] != built["executable"]["sha256"] or copied["bytes"] != built["executable"]["bytes"]:
        raise ValueError("Frozen production i686 executable differs from audited release build")
    audit, result = read_json(OUT / "production-features.json")
    if (
        result.get("passed") is not True
        or result.get("development_probes_feature") is not False
        or result.get("lease_feature") is not False
        or result.get("raw_pool_feature") is not False
        or result.get("body_handoff_feature") is not False
        or result.get("binary_sha256") != copied["sha256"]
    ):
        raise ValueError("Independent production feature or binary audit failed")
    version = receipt(OUT / "cli-version.txt", 64, contents=True)[1].decode().strip()
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("Invalid actual i686 CLI version output")
    write("production.json", {
        "schema_version": 1, "kind": "native_runtime32_production",
        "target": TARGET, "release_approved": False, "cli": built,
        "frozen_cli": copied, "audit": audit, "version": version,
    })


def helper():
    check_prepared()
    production = load("production.json")
    if elf32(OUT / "claude-autorouter-i686", RELEASE_BINARY_CAP, OUT) != production["frozen_cli"]:
        raise ValueError("Production executable changed after runtime-tools compilation")
    source = ROOT / "rust/xtask/src/main.rs"
    path = TARGET_DIR / TARGET / "release/xtask"
    built = build_executable("xtask-build.jsonl", source, "xtask", path, RELEASE_BINARY_CAP)
    write("helper.json", {
        "schema_version": 1, "kind": "native_runtime32_helper",
        "target": TARGET, "xtask": built, "production_cli_sha256": production["frozen_cli"]["sha256"],
    })


def test_result(text, required, nonempty):
    footer = re.findall(
        r"^test result: ok\. ([0-9]+) passed; ([0-9]+) failed; ([0-9]+) ignored; ([0-9]+) measured; ([0-9]+) filtered out;",
        text, re.M,
    )
    if len(footer) != 1 or footer[0][1] != "0" or footer[0][4] != "0" or (nonempty and int(footer[0][0]) == 0):
        raise ValueError("Missing complete successful i686 test result")
    for test in required:
        if len(re.findall(r"^test " + re.escape(test) + r" \.\.\. ok$", text, re.M)) != 1:
            raise ValueError("Mandatory i686 runtime or CLI control did not pass")
    return {
        "passed": int(footer[0][0]),
        "ignored": int(footer[0][2]),
        "measured": int(footer[0][3]),
        "filtered": 0,
        "required_controls": required,
    }


def package_results(package, prepared):
    name = "runtime-tests.log" if package == "autorouter-runtime" else "cli-tests.log"
    log, raw = receipt(OUT / name, 8 * MIB, contents=True)
    text = raw.decode()
    headers = list(re.finditer(
        r"^\s+Running ([^()\n]+) \(([^()\n]+)\)$|^\s+Doc-tests ([A-Za-z_][A-Za-z0-9_]*)$",
        text, re.M,
    ))
    seen = {}
    docs = {}
    binaries = prepared["packages"][package]["binaries"]
    expected_doc = "autorouter_runtime" if package == "autorouter-runtime" else "claude_autorouter"
    for index, header in enumerate(headers):
        block = text[header.end():headers[index + 1].start() if index + 1 < len(headers) else len(text)]
        if header.group(3):
            doc = header.group(3)
            if doc != expected_doc or doc in docs:
                raise ValueError("Unexpected or duplicated package doc-test group")
            docs[doc] = test_result(block, [], False)
            continue
        descriptor = header.group(1).strip()
        if descriptor == "unittests src/lib.rs" and package == "autorouter-runtime":
            target_name = "autorouter_runtime"
        elif descriptor == "unittests src/main.rs" and package == "claude-autorouter":
            target_name = "claude-autorouter"
        else:
            match = re.fullmatch(r"tests/([A-Za-z_][A-Za-z0-9_]*)\.rs", descriptor)
            if not match:
                raise ValueError("Running test header has an unexpected package target")
            target_name = match.group(1)
        if target_name not in binaries or target_name in seen:
            raise ValueError("Running test header does not bind one prepared package target")
        # Cargo may print either the ELF path or its configured runner command
        # in parentheses. The target runner's exact ELF32 receipts below bind
        # this target to the executed bytes independently of that rendering.
        if not header.group(2).strip():
            raise ValueError("Running test header omitted its execution command")
        seen[target_name] = {
            "binary": binaries[target_name],
            **test_result(text=block, required=REQUIRED.get((package, target_name), []), nonempty=True),
        }
    if set(seen) != set(binaries) or (package == "autorouter-runtime" and set(docs) != {expected_doc}):
        raise ValueError("Every prepared package test binary and runtime doc tests must execute")
    return {
        "log": log,
        "groups": dict(sorted(seen.items())),
        "doc_tests": docs,
        "passed": sum(row["passed"] for row in seen.values()) + sum(row["passed"] for row in docs.values()),
        "ignored": sum(row["ignored"] for row in seen.values()) + sum(row["ignored"] for row in docs.values()),
    }


def archive(path, report_hash):
    row = receipt(path, PACKAGE_CAP)
    sidecar = receipt(Path(str(path) + ".sha256"), 1024, contents=True)[1]
    if sidecar != f"{row['sha256']}  {path.name}\n".encode() or row["sha256"] != report_hash:
        raise ValueError("Immutable native archive, sidecar or report checksum differs")
    return row


def lifecycle(value):
    if value.get("passed") is not True:
        raise ValueError("Installed launcher lifecycle did not pass")
    expected = [f"crates/claude-autorouter/tests/{name}.rs" for name in SUITES]
    if value.get("test_suites") != expected:
        raise ValueError("Installed lifecycle omitted a CLI test suite")
    summaries = value.get("summaries")
    if not isinstance(summaries, list) or len(summaries) != len(SUITES):
        raise ValueError("Installed lifecycle summary count differs")
    counts = []
    for summary in summaries:
        match = re.fullmatch(r"test result: ok\. ([0-9]+) passed; 0 failed;.*", summary)
        if not match or int(match.group(1)) == 0:
            raise ValueError("Installed lifecycle suite is empty or failed")
        counts.append(int(match.group(1)))
    return {"suite_count": len(SUITES), "passed": sum(counts)}


def packages(production):
    version = production["version"]
    npm_path = OUT / f"npm/claude-autorouter-{version}.tgz"
    direct_path = OUT / f"direct/claude-autorouter-{version}-{TARGET}.tar.gz"
    npm_report, npm = read_json(OUT / "npm/package-report.json")
    direct_report, direct = read_json(OUT / "direct/package-report.json")
    npm_command, npm_cmd = read_json(OUT / "npm-command.json")
    direct_command, direct_cmd = read_json(OUT / "direct-command.json")
    npm_archive = archive(npm_path, npm["sha256"])
    direct_archive = archive(direct_path, direct["sha256"])
    if npm_cmd != npm or direct_cmd != direct:
        raise ValueError("Package command output differs from immutable package report")
    frozen_sha = production["frozen_cli"]["sha256"]
    artifacts = npm.get("artifacts")
    if (
        npm.get("kind") != "native_npm_feasibility"
        or npm.get("verified") is not True
        or npm.get("release_approved") is not False
        or not isinstance(artifacts, list)
        or len(artifacts) != 1
        or artifacts[0].get("target") != TARGET
        or artifacts[0].get("sha256") != frozen_sha
        or npm.get("smoke", {}).get("node_absent_from_execution_path") is not True
        or npm.get("smoke", {}).get("offline_install") is not True
        or npm.get("smoke", {}).get("ignore_scripts") is not True
    ):
        raise ValueError("Verified i686 native npm archive or Node-free smoke differs")
    npm_lifecycle = lifecycle(npm["smoke"]["complete_launcher_lifecycle"])
    if (
        direct.get("kind") != "native_direct_feasibility"
        or direct.get("verified") is not True
        or direct.get("release_approved") is not False
        or direct.get("target") != TARGET
        or direct.get("version") != version
        or direct.get("source", {}).get("commit") != source_commit()
        or direct.get("source", {}).get("dirty") is not False
        or direct.get("source", {}).get("cargo_lock_sha256") != receipt(ROOT / "rust/Cargo.lock", 4 * MIB)["sha256"]
        or direct.get("npm", {}).get("sha256") != npm_archive["sha256"]
        or direct.get("smoke", {}).get("kind") != "native_direct_installed_smoke"
        or direct.get("smoke", {}).get("checks", {}).get("no_node_runtime") is not True
        or direct.get("smoke", {}).get("target") != TARGET
    ):
        raise ValueError("Verified i686 direct archive or Node-free smoke differs")
    direct_lifecycle = lifecycle(direct["smoke"]["complete_launcher_lifecycle"])
    return {
        "npm": {"archive": npm_archive, "report": npm_report, "command": npm_command, "lifecycle": npm_lifecycle},
        "direct": {"archive": direct_archive, "report": direct_report, "command": direct_command, "lifecycle": direct_lifecycle},
        "artifact_source_to_binary_provenance": "unverified; private feasibility archives only",
    }


def upgrade_rollback(package_evidence, production):
    version = production["version"]
    original_path = OUT / f"npm/claude-autorouter-{version}.tgz"
    original = archive(original_path, package_evidence["npm"]["archive"]["sha256"])
    if original != package_evidence["npm"]["archive"]:
        raise ValueError("Original verified i686 npm archive changed before rehearsal proof")
    original_sidecar, original_sidecar_bytes = receipt(
        Path(str(original_path) + ".sha256"), 1024, contents=True
    )
    if original_sidecar_bytes != f"{original['sha256']}  {original_path.name}\n".encode():
        raise ValueError("Original i686 npm sidecar changed before rehearsal proof")

    output = OUT / "upgrade-rollback"
    report_file, report = read_json(output / "report.json", 64 * 1024)
    command_file, command = read_json(OUT / "upgrade-rollback-command.json", 64 * 1024)
    if report_file["sha256"] != command_file["sha256"] or report != command:
        raise ValueError("Upgrade/rollback command output differs from its retained report")
    candidate = receipt(output / "candidate.tgz", PACKAGE_CAP)
    baseline_path = output / "baseline/claude-autorouter-0.5.2.tgz"
    baseline = archive(baseline_path, report.get("baseline_archive_sha256"))
    baseline_sidecar, baseline_sidecar_bytes = receipt(
        Path(str(baseline_path) + ".sha256"), 1024, contents=True
    )
    if baseline_sidecar_bytes != f"{baseline['sha256']}  {baseline_path.name}\n".encode():
        raise ValueError("Rehearsal baseline sidecar differs from its retained archive")
    baseline_manifest, manifest = read_json(ROOT / "rust/parity/baseline.json", 2 * MIB)

    npm_report_file, npm_report = read_json(OUT / "npm/package-report.json")
    if npm_report_file != package_evidence["npm"]["report"]:
        raise ValueError("Original i686 npm verification report changed")
    expected_verification = dict(npm_report)
    expected_verification.pop("archive", None)
    expected_verification.pop("smoke", None)
    verified = [
        "exact_installed_archive_bytes", "npm_command_symlink", "offline_ignore_scripts",
        "native_without_node_on_path", "same_config_path_without_migration",
        "existing_file_credentials_retained", "private_atomic_edit",
        "history_schema_1_and_2", "recorded_pricing", "history_bytes_unchanged",
        "isolated_cleanup",
    ]
    limits = [
        "local_host_only", "synthetic_file_credentials_only",
        "no_real_keychain_or_provider", "no_publication_or_cutover_approval",
    ]
    stages = ["baseline", "native", "rollback", "native_after_rollback"]
    if (
        manifest.get("baseline_commit") != BASELINE_COMMIT
        or manifest.get("baseline_version") != "0.5.2"
        or report.get("schema_version") != 1
        or report.get("kind") != "offline_upgrade_rollback_rehearsal"
        or report.get("passed") is not True
        or report.get("baseline_commit") != BASELINE_COMMIT
        or report.get("native_archive_sha256") != original["sha256"]
        or report.get("native_verification") != expected_verification
        or report.get("platform") != {"os": "linux", "arch": "x86"}
        or report.get("stages") != stages
        or report.get("verified") != verified
        or report.get("limits") != limits
        or candidate["sha256"] != original["sha256"]
        or candidate["bytes"] != original["bytes"]
        or baseline["sha256"] != report.get("baseline_archive_sha256")
    ):
        raise ValueError("Exact i686 offline upgrade/rollback evidence differs")
    return {
        "passed": True,
        "scope": "Synthetic same-prefix npm baseline/native/rollback/native rehearsal; no direct archive rollback, failed-startup or partial-write coverage",
        "report": report_file,
        "command": command_file,
        "original_npm": original,
        "original_npm_sidecar": original_sidecar,
        "candidate": candidate,
        "baseline": baseline,
        "baseline_sidecar": baseline_sidecar,
        "baseline_manifest": baseline_manifest,
        "stages": stages,
        "verified": verified,
    }


def source_commit():
    commit = receipt(OUT / "source-commit.txt", 64, contents=True)[1].decode().strip()
    after = receipt(OUT / "source-commit-after.txt", 64, contents=True)[1].decode().strip()
    if not re.fullmatch(r"[0-9a-f]{40}", commit) or after != commit:
        raise ValueError("Source commit changed or is not exact")
    for name in ("source-status.txt", "source-status-after.txt"):
        if receipt(OUT / name, 64, contents=True)[1]:
            raise ValueError("Source checkout was not clean")
    return commit


def runner_results(prepared):
    expected = {
        "default-runtime": {
            ("autorouter-runtime", name): row
            for name, row in prepared["packages"]["autorouter-runtime"]["binaries"].items()
        },
        "default-cli": {
            ("claude-autorouter", name): row
            for name, row in prepared["packages"]["claude-autorouter"]["binaries"].items()
        },
    }
    for phase in ("npm", "direct"):
        expected[phase] = {
            ("claude-autorouter", name): prepared["packages"]["claude-autorouter"]["binaries"][name]
            for name in SUITES
        }
    result = {}
    parent = OUT / "runner-receipts"
    if set(path.name for path in parent.iterdir()) != set(expected):
        raise ValueError("Runner receipt phases differ from the declared executions")
    for phase, inventory in expected.items():
        paths = list((parent / phase).iterdir())
        if len(paths) != len(inventory) or len(paths) > 128 or any(path.suffix != ".json" for path in paths):
            raise ValueError("Runner receipt count or file type differs")
        observed = {}
        for path in paths:
            file_row, value = read_json(path, 2048)
            key = (value.get("package"), value.get("target"))
            if (
                value.get("schema_version") != 1
                or value.get("phase") != phase
                or key not in inventory
                or key in observed
                or value.get("binary") != inventory[key]
                or value.get("personality") != "i686"
                or not isinstance(value.get("arg_count"), int)
                or not 0 <= value["arg_count"] <= 32
            ):
                raise ValueError("Runner receipt does not bind one exact ELF32 execution")
            observed[key] = file_row
        if set(observed) != set(inventory):
            raise ValueError("Runner did not launch every declared test executable")
        result[phase] = {
            "count": len(observed),
            "receipts": {f"{package}/{name}": observed[(package, name)] for package, name in sorted(observed)},
        }
    return result


def finish():
    prepared = check_prepared()
    production = load("production.json")
    helper_report = load("helper.json")
    if production["kind"] != "native_runtime32_production" or helper_report["kind"] != "native_runtime32_helper":
        raise ValueError("Missing prepared production or helper receipt")
    frozen = elf32(OUT / "claude-autorouter-i686", RELEASE_BINARY_CAP, OUT)
    xtask = elf32(TARGET_DIR / TARGET / "release/xtask", RELEASE_BINARY_CAP, TARGET_DIR / TARGET / "release")
    if frozen != production["frozen_cli"] or xtask != helper_report["xtask"]["executable"]:
        raise ValueError("Frozen production CLI or i686 package helper changed")
    tests = {
        package: package_results(package, prepared)
        for package in ("autorouter-runtime", "claude-autorouter")
    }
    clippy = receipt(OUT / "clippy.log", 8 * MIB)
    bootstrap, bootstrap_json = read_json(OUT / "auditor-bootstrap-proof.json")
    if bootstrap_json.get("passed") is not True or bootstrap_json.get("excluded_runtime_and_vendor_dependencies") is not True:
        raise ValueError("Independent bootstrap auditor proof did not pass")
    source = source_commit()
    receipt(OUT / "vendor-audit.json", 2 * MIB)
    materials = {name: receipt(OUT / name, 2 * MIB) for name in (
        "source-commit-after.txt", "source-status-after.txt", "production-self-test.txt",
    )}
    package_evidence = packages(production)
    rehearsal = upgrade_rollback(package_evidence, production)
    write("execution.json", {
        "schema_version": 1,
        "kind": "native_runtime32_and_installed_cli_execution",
        "passed": True,
        "target": TARGET,
        "source_commit": source,
        "prepared": receipt(OUT / "prepared.json", 2 * MIB),
        "production": receipt(OUT / "production.json", 2 * MIB),
        "helper": receipt(OUT / "helper.json", 2 * MIB),
        "tests": tests,
        "total_test_passed": sum(row["passed"] for row in tests.values()),
        "strict_i686_clippy": clippy,
        "independent_auditor": bootstrap,
        "packages": package_evidence,
        "upgrade_rollback": rehearsal,
        "runner_executions": runner_results(prepared),
        "final_environment": environment(allow_runner=True),
        "final_materials": materials,
        "platform_qualification": "pending",
        "release_approved": False,
        "package_pin_precision": "Exact selected gcc-multilib, libc6-dev-i386, make and perl roots from rolling signed runner indexes; transitive/preinstalled package versions recorded, immutable image and full package-byte provenance not claimed",
        "scope": "Real 32-bit runtime/CLI package and docs, strict target Clippy, pinned source/feature audit, exact private npm/direct installed lifecycle and synthetic npm upgrade/rollback on this 64-bit-kernel runner with a genuine i686 uname personality",
        "excluded": [
            "minimum supported GNU OS/libc/kernel/CPU",
            "WSL and system/corporate/custom CA qualification",
            "other Node 22 baseline architectures",
            "real Claude, Jev or Ollama provider/model calls",
            "performance and evaluator task quality",
            "aggregate release matrix and publication",
            "direct archive rollback, failed-startup and partial-write rollback",
        ],
    })


MODES = {
    "inspect": inspect,
    "production": production,
    "helper": helper,
    "finish": finish,
}
try:
    if len(sys.argv) >= 3 and sys.argv[1] == "run-test":
        run_test()
    elif len(sys.argv) == 2 and sys.argv[1] in MODES:
        MODES[sys.argv[1]]()
    else:
        raise ValueError("Expected inspect, production, helper, finish or run-test")
except (OSError, ValueError, KeyError, TypeError, UnicodeError) as error:
    raise SystemExit("32-bit runtime evidence rejected: " + type(error).__name__)
