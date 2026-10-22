# Native Musl build provenance

The experimental collector admits only the two reviewed native Alpine 3.21
SDK profiles: `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`.
It records build provenance; platform, TLS, performance, installed lifecycle,
release and default-cutover approval remain separate requirements.

The reviewed GitHub workflow builds the SDK from a pinned per-platform Node
image and the closed package manifest in `musl-builder-pins.json`. Every APK
download must match its filename and architecture-specific SHA256 before
offline installation, with APK signature verification enabled. The SDK retains
the package manifests, installed inventory, Rust archive digest and extraction
tool metadata. A removed package revision fails the build. Refreshing a pin
requires reviewing the replacement bytes and dependency closure; the build
does not resolve newer versions from a mutable index.

`binary-provenance begin` requires the original public GitHub workflow context,
a clean matching checkout, the builder report and the reviewed environment.
It checks the source and SDK pin copies, compiler wrappers, standalone Rust
tools, actual installed package inventory and relevant Cargo configuration.
Tool witnesses retain both invocation paths and canonical executable identities
so multicall programs keep their intended mode. Supplier executable hashes are
streamed under a separate 512 MiB bound; product and report limits are unchanged.
Metadata children receive the admitted `PATH`, `HOME` and `CARGO_HOME`, plus
fixed locale and Git controls. Other environment values remain excluded, so
numeric container users use the reviewed build homes without relying on a
passwd entry.

Both the host and target release directories must be absent before the first
ordinary release build. Existing files, directories and symlinks cause refusal;
the collector does not clear a cache to manufacture a cold build. The workflow
then captures Cargo's JSON build report and the production feature audit before
calling `binary-provenance finish`. Only the executable at the reviewed target
release path is admitted. The collector requires a static ELF without an
interpreter, shared-library dependency, search path or version requirement.

The finished pair contains the exact executable and its provenance document.
Before upload, the workflow checks the pair against the retained input,
builder, Cargo and feature reports, checkout and workflow/run identities, and
the executable used by the later checks. The twenty existing validation phases
retain their order and prerequisite rules. A later phase failure remains a
failure even when independently verified build provenance can be retained.

Local synthetic controls verify refusal and orchestration behavior. Actual
SDK compilation and inspection require the corresponding hosted runner and
original uploaded artifact; synthetic fixtures do not qualify either target.
The current Alpine userland does not establish oldest-supported kernel,
Alpine, CPU, WSL or corporate deployment compatibility.
