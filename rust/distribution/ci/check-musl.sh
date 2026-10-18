#!/bin/bash
# Invoked only inside the immutable, build-only Alpine SDK.
set -euo pipefail
root=${AUTOROUTER_SOURCE_ROOT:?}
[[ "$root" == /* ]]
out="$root/artifacts/rust-rewrite/musl-portability"
mkdir -p "$out"
if [[ "$1" == fetch ]]; then
  cargo fetch --locked
  exit 0
fi
[[ "$1" == check ]]
[[ "$(uname -m)" == "$AUTOROUTER_BUILD_MACHINE" ]]
[[ "$(rustc -vV | awk '/^host:/ { print $2 }')" == "$AUTOROUTER_BUILD_TARGET" ]]
[[ "$(node --version)" == v22.14.0 ]]
git config --global --add safe.directory "$root"
[[ -z "$(git status --porcelain)" ]]
[[ "$(git rev-parse HEAD)" == "$AUTOROUTER_SOURCE_COMMIT" ]]
mkdir -p /tmp/autorouter-compilers
case "$AUTOROUTER_BUILD_MACHINE" in
  x86_64) cpu=(-march=x86-64 -mtune=generic); rust_cpu=x86-64 ;;
  aarch64) cpu=(-march=armv8-a); rust_cpu=generic ;;
  *) exit 1 ;;
esac
for language in cc cxx; do
  if [[ "$language" == cc ]]; then compiler=$(command -v gcc); else compiler=$(command -v g++); fi
  { printf '#!/bin/sh\nexec '; printf '%q ' "$compiler" "${cpu[@]}"; printf '"$@"\n'; } > "/tmp/autorouter-compilers/$language"
  chmod 755 "/tmp/autorouter-compilers/$language"
done
export CC=/tmp/autorouter-compilers/cc CXX=/tmp/autorouter-compilers/cxx
export CFLAGS=-O2 CXXFLAGS=-O2
export RUSTFLAGS="-C target-cpu=$rust_cpu -C target-feature=+crt-static --remap-path-prefix=$root=/autorouter-source"
export OPENSSL_NO_VENDOR=0 OPENSSL_STATIC=1
# Keep shipping flags off host proc macros, even on this native musl SDK.
export CARGO_BUILD_TARGET="$AUTOROUTER_BUILD_TARGET"
node --input-type=module <<'NODE'
import {execFileSync} from 'node:child_process';
import {readFileSync,writeFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
const run=(file,args)=>execFileSync(file,args,{encoding:'utf8'}).trim();
const hash=path=>createHash('sha256').update(readFileSync(path)).digest('hex');
for(const suffix of ['CC','CXX','CFLAGS','CXXFLAGS'])for(const name of [`HOST_${suffix}`,`TARGET_${suffix}`,`${suffix}_${process.env.AUTOROUTER_BUILD_TARGET}`,`${suffix}_${process.env.AUTOROUTER_BUILD_TARGET.replaceAll('-','_')}`])if(process.env[name]!==undefined)throw Error('Unexpected compiler override');
const report={schema_version:2,kind:'musl_ci_builder_inputs',source_commit:run('git',['rev-parse','HEAD']),source_dirty:run('git',['status','--porcelain'])!=='',cargo_lock_sha256:hash('Cargo.lock'),workflow_sha256:hash('../.github/workflows/rust-musl.yml'),sdk_dockerfile_sha256:hash('distribution/ci/musl.Dockerfile'),sdk_script_sha256:hash('distribution/ci/check-musl.sh'),pins_sha256:hash('distribution/musl-builder-pins.json'),node_image:process.env.AUTOROUTER_NODE_IMAGE,workflow_run:process.env.AUTOROUTER_WORKFLOW_RUN,target:process.env.AUTOROUTER_BUILD_TARGET,node:run('node',['--version']),rustc:run('rustc',['-vV']),cargo:run('cargo',['--version']),kernel:run('uname',['-srmo']),alpine:readFileSync('/etc/alpine-release','utf8').trim(),package_index_sha256:hash('/opt/autorouter-build-inputs/APKINDEX.tar.gz'),package_bytes_sha256:readFileSync('/opt/autorouter-build-inputs/apk-package-bytes.sha256','utf8'),installed_packages:readFileSync('/opt/autorouter-build-inputs/installed-packages.txt','utf8').split('\n'),rust_archive_sha256:readFileSync('/opt/autorouter-build-inputs/rust-archive.sha256','utf8'),gcc:run('gcc',['--version']),musl:run('apk',['info','-v','musl']),perl:run('perl',['-V']),tar:run('tar',['--version']),xz:run('/usr/bin/xz',['--version']),cc_wrapper:readFileSync(process.env.CC,'utf8'),cxx_wrapper:readFileSync(process.env.CXX,'utf8'),rustflags:process.env.RUSTFLAGS,cflags:process.env.CFLAGS,source_date_epoch:process.env.SOURCE_DATE_EPOCH,qualification:'Current Alpine3.21 container userland only; oldest Alpine/kernel/CPU/WSL/corporate deployment and full-matrix/release gates remain pending'};
if(report.source_dirty)throw Error('Dirty source cannot establish build identity');
writeFileSync('../artifacts/rust-rewrite/musl-portability/builder.json',JSON.stringify(report,null,2)+'\n',{flag:'wx'});
NODE
cargo metadata --format-version 1 --locked --offline > /dev/null
cargo build --package xtask --no-default-features --bin autorouter-audit --offline --locked --message-format=json > "$out/bootstrap-build.jsonl"
cp "$CARGO_TARGET_DIR/$CARGO_BUILD_TARGET/debug/autorouter-audit" "$out/autorouter-audit-bootstrap"
"$out/autorouter-audit-bootstrap" bootstrap --build-report "$out/bootstrap-build.jsonl" > "$out/bootstrap-proof.json"
"$out/autorouter-audit-bootstrap" vendor > "$out/vendor-audit.json"
"$out/autorouter-audit-bootstrap" production-features --self-test
sha256sum "$out/autorouter-audit-bootstrap" > "$out/bootstrap-binary.sha256"
# Keep auditor trust separate from the collecting workspace phase.
cargo clippy --package xtask --no-default-features --all-targets --offline --locked -- -D warnings
cargo test --package xtask --no-default-features --bin autorouter-audit --offline --locked
cargo fmt --check
cargo clippy --workspace --all-targets --offline --locked -- -D warnings

# Child interpreters enforce strict errors independently of parent capture.
autorouter_musl_phase() {
  local phase=$1
  shift
  local root=${AUTOROUTER_SOURCE_ROOT:?}
  local out="$root/artifacts/rust-rewrite/musl-portability"
  case "$phase" in
    workspace)
cargo test --workspace --offline --locked --no-fail-fast
      ;;
    production)
cargo build --release --offline --locked --package claude-autorouter --message-format=json > "$out/production-build.jsonl"
"$out/autorouter-audit-bootstrap" production-features --build-report "$out/production-build.jsonl" > "$out/production-features.json"
cp "$CARGO_TARGET_DIR/$CARGO_BUILD_TARGET/release/claude-autorouter" "$out/claude-autorouter-musl"
sha256sum "$out/claude-autorouter-musl" > "$out/production-binary.sha256"
      ;;
    helper)
# Only after copying/auditing the ordinary shipping build may the development helper
# unify its probe features. It never qualifies the ordinary CLI by itself.
cargo build --release --offline --locked --package xtask --bin xtask
cp "$CARGO_TARGET_DIR/$CARGO_BUILD_TARGET/release/xtask" "$out/xtask-musl"
sha256sum "$out/xtask-musl" > "$out/helper-binary.sha256"
      ;;
    inspection)
"$out/xtask-musl" package-inspect "$AUTOROUTER_BUILD_TARGET" "$out/claude-autorouter-musl" > "$out/static-inspection.json"
readelf --file-header --program-headers --dynamic "$out/claude-autorouter-musl" > "$out/readelf.txt"
node --input-type=module <<'NODE'
import {readFileSync}from'node:fs';
const {inspection}=JSON.parse(readFileSync('../artifacts/rust-rewrite/musl-portability/static-inspection.json'));
if(inspection.format!=='ELF'||inspection.target!==process.env.AUTOROUTER_BUILD_TARGET||inspection.interpreter!==null||inspection.dynamic_libraries.length!==0)throw Error('Musl artifact must have no interpreter or DT_NEEDED');
NODE
      ;;
    freeze-reference)
"$out/xtask-musl" freeze-reference > "$out/freeze-reference.json"
      ;;
    parity)
"$out/xtask-musl" parity --all --output artifacts/rust-rewrite/musl-parity
      ;;
    protocol)
check=$1
case "$check" in
  check-config-commands|check-node-options-startup|check-gateway|check-gateway-resources|check-http-parser|check-upstream-transport|check-upstream-security|check-tls-options|check-localhost-transport) ;;
  *) return 2 ;;
esac
node "parity/$check.mjs" ../artifacts/rust-rewrite/reference "$out/claude-autorouter-musl"
      ;;
    protocol-unicode)
node parity/check-gateway.mjs ../artifacts/rust-rewrite/reference "$out/claude-autorouter-musl" --unicode
      ;;
    npm)
"$out/xtask-musl" package --binary "$AUTOROUTER_BUILD_TARGET=$out/claude-autorouter-musl" --output artifacts/rust-rewrite/musl-package --smoke
      ;;
    direct)
version=$("$out/claude-autorouter-musl" --version)
"$out/xtask-musl" package --direct-from "artifacts/rust-rewrite/musl-package/claude-autorouter-$version.tgz" --target "$AUTOROUTER_BUILD_TARGET" --output artifacts/rust-rewrite/musl-direct --smoke
      ;;
    rollback)
version=$("$out/claude-autorouter-musl" --version)
"$out/xtask-musl" upgrade-rollback --native "artifacts/rust-rewrite/musl-package/claude-autorouter-$version.tgz" \
  --output artifacts/rust-rewrite/upgrade-rollback-musl
node --input-type=module <<'NODE'
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
const packed=JSON.parse(readFileSync('../artifacts/rust-rewrite/musl-package/package-report.json'));
const rehearsal=JSON.parse(readFileSync('../artifacts/rust-rewrite/upgrade-rollback-musl/report.json'));
assert.equal(rehearsal.passed,true);
assert.equal(rehearsal.native_archive_sha256,packed.sha256);
NODE
node distribution/ci/upgrade-rollback-proof.mjs --self-test
node distribution/ci/upgrade-rollback-proof.mjs ../artifacts/rust-rewrite/upgrade-rollback-musl/report.json ../artifacts/rust-rewrite/musl-package/package-report.json
      ;;
    repro)
# Preserve the already-built debug test artifacts until the installed
# lifecycles and rollback rehearsal finish. OpenSSL embeds its absolute MODULESDIR build-output path
# in the binary, so move the complete first compiler tree aside only now,
# then rebuild from an empty tree at that same path. Keep the full-byte check.
[[ "$CARGO_TARGET_DIR" == "$out/target-main" && -d "$CARGO_TARGET_DIR" && ! -L "$CARGO_TARGET_DIR" ]]
[[ ! -e "$out/target-primary-outputs" && ! -L "$out/target-primary-outputs" ]]
mv "$CARGO_TARGET_DIR" "$out/target-primary-outputs"
mkdir -m 700 "$CARGO_TARGET_DIR"
printf 'production_compiler_output_path=%s\nproduction_compiler_output_retained=%s\nreproduction_compiler_output_path=%s\n' \
  "$CARGO_TARGET_DIR" "$out/target-primary-outputs" "$CARGO_TARGET_DIR" > "$out/repro-output-roots.txt"
cargo build --release --offline --locked --package claude-autorouter --message-format=json > "$out/repro-build.jsonl"
cp "$CARGO_TARGET_DIR/$CARGO_BUILD_TARGET/release/claude-autorouter" "$out/claude-autorouter-musl-repro"
sha256sum "$out/claude-autorouter-musl" "$out/claude-autorouter-musl-repro" > "$out/repro-sha256.txt"
cmp "$out/claude-autorouter-musl" "$out/claude-autorouter-musl-repro"
      ;;
    *) return 2 ;;
  esac
}
export -f autorouter_musl_phase

# Do not start another phase if the controller itself receives cancellation.
# Container/runner teardown remains external; no process-tree guarantee here.
trap 'exit 130' INT
trap 'exit 143' TERM
: > "$out/phase-statuses.jsonl"
autorouter_first_failure=0
autorouter_last_status=0
autorouter_phase_count=0
autorouter_record_phase() {
  local name=$1
  shift
  local -a codes
  local metadata_status=0
  set +e
  /bin/bash -euo pipefail -c 'autorouter_musl_phase "$@"' -- "$@" 2>&1 | tee "$out/phase-$name.log"
  codes=("${PIPESTATUS[@]}")
  set -e
  printf '{"phase":"%s","command_exit_code":%s,"log_exit_code":%s}\n' \
    "$name" "${codes[0]}" "${codes[1]}" >> "$out/phase-statuses.jsonl" || metadata_status=$?
  autorouter_last_status=${codes[0]}
  if (( autorouter_last_status == 0 )); then autorouter_last_status=${codes[1]}; fi
  if (( autorouter_last_status == 0 )); then autorouter_last_status=$metadata_status; fi
  if (( autorouter_first_failure == 0 && autorouter_last_status != 0 )); then
    autorouter_first_failure=$autorouter_last_status
  fi
  autorouter_phase_count=$((autorouter_phase_count + 1))
  return 0
}
autorouter_skip_phase() {
  local name=$1 reason=$2 metadata_status=0
  printf '{"phase":"%s","skipped_for":"%s"}\n' "$name" "$reason" \
    >> "$out/phase-statuses.jsonl" || metadata_status=$?
  if (( autorouter_first_failure == 0 && metadata_status != 0 )); then
    autorouter_first_failure=$metadata_status
  fi
  autorouter_phase_count=$((autorouter_phase_count + 1))
  return 0
}
# Calls are ordinary commands/then-bodies, never tested shell functions.
# Each child separately enables strict errors; the parent only captures status.
autorouter_record_phase workspace workspace
autorouter_record_phase production production
autorouter_production_status=$autorouter_last_status
if (( autorouter_production_status == 0 )); then
  autorouter_record_phase helper helper
  autorouter_helper_status=$autorouter_last_status
else
  autorouter_skip_phase helper production
  autorouter_helper_status=1
fi
if (( autorouter_helper_status == 0 )); then
  autorouter_record_phase inspection inspection
  autorouter_inspection_status=$autorouter_last_status
else
  autorouter_skip_phase inspection helper
  autorouter_inspection_status=1
fi
if (( autorouter_inspection_status == 0 )); then
  autorouter_record_phase freeze-reference freeze-reference
  autorouter_reference_status=$autorouter_last_status
else
  autorouter_skip_phase freeze-reference inspection
  autorouter_reference_status=1
fi
if (( autorouter_reference_status == 0 )); then
  autorouter_record_phase parity parity
else
  autorouter_skip_phase parity freeze-reference
fi
for check in check-config-commands check-node-options-startup check-gateway check-gateway-resources check-http-parser check-upstream-transport check-upstream-security check-tls-options check-localhost-transport; do
  if (( autorouter_reference_status == 0 )); then
    autorouter_record_phase "protocol-$check" protocol "$check"
  else
    autorouter_skip_phase "protocol-$check" freeze-reference
  fi
done
if (( autorouter_reference_status == 0 )); then
  autorouter_record_phase protocol-unicode protocol-unicode
else
  autorouter_skip_phase protocol-unicode freeze-reference
fi
if (( autorouter_inspection_status == 0 )); then
  autorouter_record_phase npm npm
  autorouter_npm_status=$autorouter_last_status
else
  autorouter_skip_phase npm inspection
  autorouter_npm_status=1
fi
if (( autorouter_npm_status == 0 )); then
  autorouter_record_phase direct direct
  # Rollback consumes the verified npm archive, independently of direct success.
  autorouter_record_phase rollback rollback
else
  autorouter_skip_phase direct npm
  autorouter_skip_phase rollback npm
fi
# Every potential test/lifecycle consumer above has returned before moving the
# first compiler tree. The exact-path/no-overwrite repro body remains unchanged.
if (( autorouter_inspection_status == 0 )); then
  autorouter_record_phase repro repro
else
  autorouter_skip_phase repro inspection
fi
printf '{"schema_version":1,"kind":"musl_ci_phase_summary","phase_count":%s,"exit_code":%s}\n' \
  "$autorouter_phase_count" "$autorouter_first_failure" > "$out/phase-summary.json" \
  || { autorouter_metadata_status=$?; if (( autorouter_first_failure == 0 )); then autorouter_first_failure=$autorouter_metadata_status; fi; }
exit "$autorouter_first_failure"
