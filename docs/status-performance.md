# Status persistence benchmark

Status snapshots now use asynchronous atomic replacement with one writer and one dirty flag. Slow storage can delay the status display without blocking request handling or response streaming. Events received during a write are folded into the latest bounded session state; the writer does not queue a snapshot for every event.

The [recorded results](status-performance.json) compare the original synchronous writer, captured before implementation, with the asynchronous writer on an Apple M4, 16 GiB RAM, macOS Darwin 25.6.0, Node 26.10.0. Other workspace activity continued during both runs; load averages and peak process RSS are recorded. These are local synthetic observations, not provider latency or a cross-machine performance claim.

Each storage scenario has three runs of 60 bursts, six telemetry events per burst, 20 rotating sessions, and a 5 ms producer interval. A separate 5 ms timer feeds a local mock stream. The slow-storage case injects 20 ms into each snapshot write: a blocking wait for the original writer and an asynchronous wait for its replacement. It does not delay filesystem metadata operations. The stream measurement is lateness beyond its expected timer interval, not network throughput.

| Measurement, milliseconds unless stated | Normal synchronous | Normal asynchronous | Slow synchronous | Slow asynchronous |
| --- | ---: | ---: | ---: | ---: |
| Update burst p50 / p95 | 0.090 / 0.189 | 0.121 / 0.185 | 0.219 / 0.304 | 0.125 / 0.193 |
| `flush()` caller p50 / p95 | 1.070 / 1.955 | 0.004 / 0.006 | 26.068 / 27.621 | 0.004 / 0.006 |
| Mock stream lateness p50 / p95 | 0.996 / 2.360 | 0.215 / 0.955 | 26.445 / 28.508 | 0.065 / 1.042 |
| Snapshot writes per run, including initial file | 61 | 61 | 61 | 17 |
| Peak process RSS, MiB | 21.11 | 22.21 | 21.63 | 22.77 |

Under injected slow storage, stream lateness p95 fell by 96.3%. The asynchronous `flush()` promise still waits for persistence: its slow-storage completion p95 was 342.723 ms because it drains updates arriving during that write sequence. Request handling calls `update()` and does not await this drain. The JSON includes completion latency, initialization, shutdown, event-loop histograms, and maximum values, including scheduler outliers omitted from the compact table.

## Local regression gates

The harness derives these gates from the recorded synchronous baseline. All four passed on the measured machine:

| Gate | Maximum | Measured |
| --- | ---: | ---: |
| Slow-storage stream lateness p95: at least 75% lower | 7.127 ms | 1.042 ms |
| Slow-storage synchronous flush cost p95: at least 90% lower | 2.762 ms | 0.006 ms |
| Normal-storage stream lateness p95: baseline plus 25% | 2.950 ms | 0.955 ms |
| Normal-storage update p95: twice baseline | 0.378 ms | 0.185 ms |

These numerical checks are opt-in and sensitive to host load; they do not run as ordinary CI timing tests. Repeat locally with:

```sh
node scripts/benchmark-status.mjs --label local-check --check
```

This appends or replaces that label in `docs/status-performance.json`. `--output PATH` selects a separate report; `--module PATH` can benchmark another checkout's `src/status-state.mjs`. `--check` requires an existing `synchronous-baseline` entry in the selected report. No sockets, inference, credentials, or user session data are used.

## Persistence contract

`createStatusState()` returns immediately. Callers await `ready` before reading `path`; the path becomes available only after the first complete snapshot exists. Initial storage failure or a one-second readiness deadline disables the optional status display. Accepted snapshots use a private directory with mode `0700`, exclusive temporary files with mode `0600`, and atomic rename. Filesystem errors do not reject `ready`, `flush()`, or `close()`.

`close()` stops new updates, waits for the active write and latest accepted state, then removes the private directory. Native filesystem operations cannot be cancelled, so shutdown retains ownership of an outstanding operation even if startup readiness has already timed out. It never removes the directory while a detached writer could recreate a file.

Deterministic tests hold a write unresolved while an entire mock inference stream finishes; 2,000 update bursts produce only one subsequent snapshot. Other tests cover concurrent close calls, late updates, readiness timeout, temporary-file cleanup, recovery after a failed rename, permissions, and foreground/session isolation.
