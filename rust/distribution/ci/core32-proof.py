#!/usr/bin/env python3
"""CI evidence only: actual 32-bit core arithmetic, never platform qualification."""
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys
import tomllib

ROOT = Path(os.environ["GITHUB_WORKSPACE"])
OUT = ROOT / "artifacts/rust-rewrite/core32"
TARGET = "i686-unknown-linux-gnu"
CONTEXT_CONTROLS = [
    "consumer_compiler::tests::exact_dependency_selection_ignores_other_available_libraries_and_keeps_flag_tokens",
    "consumer_compiler::tests::explicit_target_replays_target_and_both_target_and_host_dependencies",
    "consumer_compiler::tests::unavailable_or_ambiguous_exact_dependency_is_rejected_instead_of_substituted",
]
REQUIRED = {
    "autorouter_core": [
        "router::tests::deferred_context_charge_matches_actual_source_above_u32_without_mutation",
        "router::tests::context_charge_preserves_source_number_operation_order_and_json_shape",
        "router::tests::unavailable_context_length_requires_capacity_check_without_fake_bytes",
    ],
    "typed_contracts": [
        "documented_producers_compile_and_each_invalid_contract_has_its_own_diagnostic",
        *CONTEXT_CONTROLS,
    ],
    "savings_ingress_contracts": [
        "native_consumer_controls_accept_plain_value_and_reject_host_getters_and_private_rates",
        *CONTEXT_CONTROLS,
    ],
}


def receipt(path, limit, contents=False, prefix_bytes=0):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
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
                raise ValueError("Evidence file grew beyond limit")
            digest.update(chunk)
            if len(prefix) < prefix_bytes:
                prefix.extend(chunk[:prefix_bytes - len(prefix)])
            if contents:
                chunks.append(chunk)
        after = os.fstat(fd)
        fields = lambda value: (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns)
        if fields(before) != fields(after) or total != before.st_size:
            raise ValueError("Evidence identity changed during read")
    finally:
        os.close(fd)
    row = {"path": str(path.relative_to(ROOT)), "bytes": total, "sha256": digest.hexdigest()}
    if contents:
        return row, b"".join(chunks)
    return (row, bytes(prefix)) if prefix_bytes else row


def write(name, value):
    data = (json.dumps(value, indent=2) + "\n").encode()
    if len(data) > 2 * 1024 * 1024:
        raise ValueError("Evidence report exceeds byte limit")
    fd = os.open(OUT / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(fd, "wb") as output:
            output.write(data)
    except BaseException:
        raise


def binary(path):
    path = Path(path)
    prefix = OUT / "target" / TARGET / "debug" / "deps"
    if path.parent != prefix or path.is_symlink():
        raise ValueError("Test executable outside declared target output")
    row, header = receipt(path, 32 * 1024 * 1024, prefix_bytes=20)
    if header[:6] != b"\x7fELF\x01\x01" or header[18:20] != b"\x03\x00":
        raise ValueError("Expected actual little-endian ELF32/EM_386 test executable")
    row.update({"class": "ELF32", "machine": "EM_386", "target": TARGET})
    return row


def sources():
    # Hash code and the complete synthetic case directory without retaining raw
    # corpus text in this receipt. This exceeds the minimal arithmetic inputs;
    # it is still not an attestation of every workspace/build-script input.
    fixed = [
        "rust/Cargo.toml", "rust/Cargo.lock", "rust/rust-toolchain.toml", "rust/.cargo/config.toml",
        "rust/crates/autorouter-core/Cargo.toml",
        ".github/workflows/rust-core32.yml", "rust/distribution/ci/core32-proof.py",
    ]
    paths = {ROOT / path for path in fixed}
    paths.update((ROOT / "rust/crates/autorouter-core").rglob("*.rs"))
    paths.update((ROOT / "rust/parity/cases").glob("*.json"))
    paths.update((ROOT / "rust/parity/cases").glob("*.jsonl"))
    if not 1 <= len(paths) <= 512:
        raise ValueError("Source receipt count exceeds the declared bound")
    rows = [receipt(path, 8 * 1024 * 1024) for path in sorted(paths)]
    if sum(row["bytes"] for row in rows) > 48 * 1024 * 1024:
        raise ValueError("Source receipt aggregate exceeds its bound")
    return rows


def expected_targets():
    core = ROOT / "rust/crates/autorouter-core"
    _, raw = receipt(core / "Cargo.toml", 64 * 1024, True)
    manifest = tomllib.loads(raw.decode())
    if manifest["package"]["name"] != "autorouter-core" or manifest["package"].get("autotests", True) is not True or manifest.get("test"):
        raise ValueError("Unexpected explicit core test-target configuration")
    names = {path.stem for path in (core / "tests").glob("*.rs")}
    if not {"router_contracts", "typed_contracts", "savings_ingress_contracts"} <= names or len(names) > 64:
        raise ValueError("Declared full core test target set rejected")
    return {"autorouter_core": (core / "src/lib.rs", ["lib"]), **{
        name: (core / "tests" / (name + ".rs"), ["test"]) for name in names
    }}


def inspect():
    build, raw = receipt(OUT / "build.jsonl", 2 * 1024 * 1024, True)
    expected = expected_targets()
    selected = {}
    completed = []
    for line in raw.decode().splitlines():
        row = json.loads(line)
        if row.get("reason") == "build-finished":
            completed.append(row.get("success"))
        if row.get("reason") != "compiler-artifact" or not row.get("profile", {}).get("test"):
            continue
        target = row.get("target", {})
        name = target.get("name")
        source = target.get("src_path")
        if not isinstance(source, str) or not Path(source).is_relative_to(ROOT / "rust/crates/autorouter-core"):
            continue
        if name not in expected:
            raise ValueError("Unexpected executable test target in the full core build")
        source_path, kind = expected[name]
        if Path(source) != source_path or target.get("kind") != kind:
            raise ValueError("Cargo core test target source or kind rejected")
        path = row.get("executable")
        if not isinstance(path, str) or name in selected:
            raise ValueError("Missing or ambiguous core test executable")
        selected[name] = binary(path)
    if completed != [True] or set(selected) != set(expected):
        raise ValueError("A successful build and every full core test executable are required")
    cfg, contents = receipt(OUT / "target-cfg.txt", 64 * 1024, True)
    if b'target_pointer_width="32"' not in contents.splitlines():
        raise ValueError("Compiler target pointer width is not 32")
    prepared = {"schema_version": 2, "kind": "native_core32_prepared", "target": TARGET,
                "execution": "pending", "build": build, "target_cfg": cfg,
                "sources": sources(), "binaries": dict(sorted(selected.items())),
                "required_successful_callbacks": REQUIRED,
                "scope": "Every core unit/integration executable is ELF32; full package/default doc-test execution pending. No product/minimum-platform qualification"}
    write("prepared.json", prepared)
    marker = "AUTOROUTER_CORE32_EXECUTABLES"
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write("test_binaries<<" + marker + "\n")
        for row in prepared["binaries"].values():
            path = str(ROOT / row["path"])
            if any(char in path for char in "\r\n\x00") or path == marker:
                raise ValueError("Invalid action output path")
            output.write(path + "\n")
        output.write(marker + "\n")


def test_result(text, required, nonempty):
    footer = re.findall(r"^test result: ok\. ([0-9]+) passed; 0 failed; ([0-9]+) ignored; ([0-9]+) measured; ([0-9]+) filtered out;", text, re.M)
    if len(footer) != 1 or (nonempty and int(footer[0][0]) == 0) or footer[0][3] != "0":
        raise ValueError("Missing complete successful test result")
    for test in required:
        if len(re.findall(r"^test " + re.escape(test) + r" \.\.\. ok$", text, re.M)) != 1:
            raise ValueError("Mandatory 32-bit control did not execute successfully")
    return {"passed": int(footer[0][0]), "ignored": int(footer[0][1]), "measured": int(footer[0][2]), "filtered": 0, "required_controls": required}


def full_package_results(declaration):
    log, raw = receipt(OUT / "package-tests.log", 4 * 1024 * 1024, True)
    text = raw.decode()
    headers = list(re.finditer(r"^\s+Running [^\n]+ \(([^\n]+)\)$|^\s+Doc-tests (autorouter_core)$", text, re.M))
    seen = {}
    doc_tests = None
    for index, header in enumerate(headers):
        block = text[header.end():headers[index + 1].start() if index + 1 < len(headers) else len(text)]
        if header.group(2):
            if doc_tests is not None:
                raise ValueError("Duplicate core doc-test group")
            doc_tests = test_result(block, [], False)
            continue
        path = Path(header.group(1))
        if not path.is_absolute():
            path = ROOT / "rust" / path
        matches = [name for name, row in declaration["binaries"].items() if ROOT / row["path"] == path]
        if len(matches) != 1 or matches[0] in seen:
            raise ValueError("Actual Running header must bind one prepared ELF32 test binary")
        name = matches[0]
        seen[name] = {"binary": declaration["binaries"][name], **test_result(block, REQUIRED.get(name, []), True)}
    if set(seen) != set(declaration["binaries"]) or doc_tests is None:
        raise ValueError("Every prepared executable and the full-package doc-test group must execute")
    return {"log": log, "groups": dict(sorted(seen.items())), "doc_tests": doc_tests,
            "passed": sum(row["passed"] for row in seen.values()) + doc_tests["passed"],
            "ignored": sum(row["ignored"] for row in seen.values()) + doc_tests["ignored"]}


def finish():
    prepared, data = receipt(OUT / "prepared.json", 2 * 1024 * 1024, True)
    declaration = json.loads(data)
    if declaration["sources"] != sources():
        raise ValueError("Source identity changed during execution")
    for row in declaration["binaries"].values():
        if binary(ROOT / row["path"]) != row:
            raise ValueError("Executed test binary changed")
    results = full_package_results(declaration)
    commit = receipt(OUT / "source-commit.txt", 64, True)[1].decode().strip()
    after_commit = receipt(OUT / "source-commit-after.txt", 64, True)[1].decode().strip()
    if not re.fullmatch(r"[0-9a-f]{40}", commit) or after_commit != commit:
        raise ValueError("Source commit changed or is invalid")
    for name in ["source-status.txt", "source-status-after.txt"]:
        if receipt(OUT / name, 64, True)[1]:
            raise ValueError("Source original/final clean status rejected")
    materials = [receipt(OUT / name, 2 * 1024 * 1024) for name in [
        "apt-policy.txt", "requested-root-versions.txt", "installed-packages.tsv", "apt-index-targets.txt",
        "gcc-version.txt", "python-version.txt", "os-release.txt", "kernel.txt",
        "rustc-version.txt", "cargo-version.txt", "rustup-version.txt", "source-commit.txt", "source-status.txt",
        "source-commit-after.txt", "source-status-after.txt",
    ]]
    write("execution.json", {"schema_version": 2, "kind": "native_core32_arithmetic_execution", "passed": True,
         "target": TARGET, "source_commit": commit, "prepared": prepared, "results": results, "environment": materials,
         "package_pin_precision": "Two exact root versions selected at CI admission from rolling signed runner indexes; transitive/preinstalled package versions recorded, no immutable image or full package-byte provenance claim",
         "qualification": "Actual 32-bit core arithmetic/API and full default core package tests on this runner only; standalone consumers emit metadata and are not themselves executed foreign binaries",
         "excluded": ["armv7/other32-bit architectures", "Whole CLI/runtime/gateway", "Minimum GNU OS/libc/kernel/CPU", "WSL/CA/DNS",
                      "Provider/model calls", "Performance", "Release/default cutover"]})


if len(sys.argv) != 2 or sys.argv[1] not in {"inspect", "finish"}:
    raise SystemExit("Expected inspect or finish")
try:
    (inspect if sys.argv[1] == "inspect" else finish)()
except (OSError, ValueError, KeyError, UnicodeError) as error:
    raise SystemExit("32-bit core evidence rejected: " + type(error).__name__)
