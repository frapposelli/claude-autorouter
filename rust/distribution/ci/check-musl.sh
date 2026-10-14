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
cargo fmt --check
cargo clippy --workspace --all-targets --offline --locked -- -D warnings
cargo test --workspace --offline --locked
cargo build --release --offline --locked --package claude-autorouter --message-format=json > "$out/production-build.jsonl"
"$out/autorouter-audit-bootstrap" production-features --build-report "$out/production-build.jsonl" > "$out/production-features.json"
cp "$CARGO_TARGET_DIR/$CARGO_BUILD_TARGET/release/claude-autorouter" "$out/claude-autorouter-musl"
# Rebuild ordinary CLI from a second empty compiler-output tree. No normalization
# or exception is allowed: failure retains both identities and stops the job.
CARGO_TARGET_DIR="$out/target-repro" cargo build --release --offline --locked --package claude-autorouter --message-format=json > "$out/repro-build.jsonl"
cp "$out/target-repro/$CARGO_BUILD_TARGET/release/claude-autorouter" "$out/claude-autorouter-musl-repro"
sha256sum "$out/claude-autorouter-musl" "$out/claude-autorouter-musl-repro" > "$out/repro-sha256.txt"
cmp "$out/claude-autorouter-musl" "$out/claude-autorouter-musl-repro"
# Only after copying/auditing ordinary shipping builds may the development helper
# unify its probe features. It never qualifies the ordinary CLI by itself.
cargo build --release --offline --locked --package xtask --bin xtask
cp "$CARGO_TARGET_DIR/$CARGO_BUILD_TARGET/release/xtask" "$out/xtask-musl"
"$out/xtask-musl" package-inspect "$AUTOROUTER_BUILD_TARGET" "$out/claude-autorouter-musl" > "$out/static-inspection.json"
readelf --file-header --program-headers --dynamic "$out/claude-autorouter-musl" > "$out/readelf.txt"
node --input-type=module <<'NODE'
import {readFileSync}from'node:fs';
const {inspection}=JSON.parse(readFileSync('../artifacts/rust-rewrite/musl-portability/static-inspection.json'));
if(inspection.format!=='ELF'||inspection.target!==process.env.AUTOROUTER_BUILD_TARGET||inspection.interpreter!==null||inspection.dynamic_libraries.length!==0)throw Error('Musl artifact must have no interpreter or DT_NEEDED');
NODE
"$out/xtask-musl" parity --all --output artifacts/rust-rewrite/musl-parity
for check in check-config-commands check-node-options-startup check-gateway check-gateway-resources check-http-parser check-upstream-transport check-upstream-security check-tls-options check-localhost-transport; do
  node "parity/$check.mjs" ../artifacts/rust-rewrite/reference "$out/claude-autorouter-musl"
done
node parity/check-gateway.mjs ../artifacts/rust-rewrite/reference "$out/claude-autorouter-musl" --unicode
"$out/xtask-musl" package --binary "$AUTOROUTER_BUILD_TARGET=$out/claude-autorouter-musl" --output artifacts/rust-rewrite/musl-package --smoke
version=$("$out/claude-autorouter-musl" --version)
"$out/xtask-musl" package --direct-from "artifacts/rust-rewrite/musl-package/claude-autorouter-$version.tgz" --target "$AUTOROUTER_BUILD_TARGET" --output artifacts/rust-rewrite/musl-direct --smoke
