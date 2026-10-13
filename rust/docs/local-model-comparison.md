# Paired local evaluator comparison

This development tool runs the frozen Node evaluator and native evaluator on the
same installed model. It needs no Git checkout, private wrapper, Claude login,
Jev key or model download. It preserves the original v1/v2 protocols and their
failed results. A prepare-time declaration for another installed model or Ollama
version is a separate experiment, with the same thresholds and samples.

Build the extracted source bundle with its pinned Rust toolchain, prerequisites
and Cargo.lock. Keep the complete111-file frozen reference at
`artifacts/rust-rewrite/reference` and provide a Node22.14.0 executable. Read the
immutable `rust/parity/local-model-orchestration-v1.json` before running.

From `rust/`, first build without performing any evaluator calls:

```sh
cargo build --release --locked -p xtask
```

Prepare an explicitly installed decision-capable model. This phase verifies the
reference, fixture, rubric, protocols, allowlisted native sources and exact
Node/native executables. It only queries bounded local metadata; it does not
load, unload, download or evaluate a model. Choose the warm deadline before any
calls. The output directory must not exist.

```sh
./target/release/xtask compare-local-evaluators prepare \
  --model nimble:9b-q4_K_M \
  --node /absolute/path/to/node22.14.0 \
  --warm-timeout-ms 1500 \
  --conditions normal-use-exclusive-ollama-builds-paused \
  --output results/nimble-finite-declaration
```

The model tag is an example; select an exact tag already present on your
instance. The tool requires observed local decision capability and a digest,
and refuses unsupported/remote models or any currently resident model. It
copies the executables into a new0700 directory and creates0600 declaration
evidence. Any source, fixture, reference, binary, model or runtime change
requires a new declaration. Do not rebuild or edit the source during a run.

Use an idle Ollama instance with no competing requests, and pause agent builds
and tests. Usual applications may remain open. The conditions argument is an
operator declaration; power/load/RAM/swap/pressure/residency are collected
separately as observations. These conditions never establish controlled
hardware acceptance. The API cannot distinguish another client's concurrent
request to the same tag; use an instance exclusively for this trial.

Run explicitly performs local inference. It does not contact Claude or Jev.
There are five alternating pairs, each with one separate cold call, three
held-out rounds of30 cases and eight stress calls. A complete experiment has
495 calls per engine,990 total. Cold failures remain skipped/incomplete warm
matrices rather than being replaced. Reports may exit1 for failed model quality;
the driver retains those reports and continues the remaining paired trial.

```sh
./target/release/xtask compare-local-evaluators run \
  --declaration results/nimble-finite-declaration \
  --conditions normal-use-exclusive-ollama-builds-paused \
  --output results/nimble-finite-run-1
```

All request rows, original reports, private subprocess logs, before/after
conditions, cleanup observations and categorical comparisons are retained. Each
child has a900-second process deadline, a5-second TERM-to-KILL grace and a2MiB
aggregate stdout/stderr retention quota. The parent reads each model report at a2MiB ceiling; orchestrator evidence and
declarations have a2MiB write/read ceiling. The unchanged evaluator children
write their precreated report directly without a file-growth ceiling. The underlying frozen evaluator's metadata reader
behavior remains part of that source; these limits do not claim to bound its
entire heap, kernel buffers or Ollama's memory. Cold means initially unloaded
model residency, not cleared operating-system file caches.

The original evaluators unload only their admitted candidate. After an
interrupted child, the driver verifies the model/runtime and exact resident
digest before requesting candidate-only unload. Unknown or different models
are left untouched. No spawned child means no unload admission. A changed
candidate digest, unrelated resident model, failed unload or still-resident
candidate keeps the run incomplete. Daemon processes remain user-owned;
cleanup proof covers the joined child/group and observed candidate residency.

Process ownership keeps the leader unreaped until its group is terminated, so
cleanup cannot signal a reused numeric PID/group after the leader exits. This
reuses the safe process owner rather than the earlier tool-process loop, which
waited/reaped first and sent a final group signal from Drop. One blocking worker
owns nonblocking pipes, exit observation, signals and reap. Dropping a join
waiter requests cancellation while keeping its join handle in the owner.
Explicit `close().await` joins cleanup; Drop itself is nonblocking and cancels
the same bounded worker. Ordinary CLI cancellation follows the joined barrier
before model cleanup. SIGKILL, power loss or killing the Ollama daemon cannot
execute asynchronous cleanup; preserve that interrupted directory, inspect
the instance and leave the run incomplete.

Strict quality remains100% agreement and zero under-routing, with all three
tiers required. Each report's acceptance is recomputed without changing labels.
Case/round/expected/actual/errors/state bytes and every available provider token
metric are compared exactly, including missing fields. Only per-request wall
time is excluded from categorical equality; it remains in the original reports.
JSON numeric1 and1.0 are the same value when checking serialized metadata and
acceptance, with no tolerance. Unknown context or missing successful-call token
metrics remain unverified, even when labels match. Cold and final resident
context/quantization observations must be present, stable and matched.

A separately declared zero-deadline investigation uses
`--warm-timeout-ms 0` and a new declaration/run directory. Only the warm
evaluator timer is disabled; cold60000ms, cancellation, response and subprocess
bounds remain. Never replace or pool finite-deadline results with this trial.
Every bidirectional1500ms availability mismatch remains a failed finite result.

Outputs are create-only. After interruption, retain the existing directory and
use a new run directory against the still-valid declaration. No in-place repair,
append or automatic retry hides failed rows. Matching evaluator results does
not prove completed Claude tasks, controlled hardware performance, shipping
artifact provenance, platform compatibility, package/release readiness or
cutover approval.
