# Local-model hardware validation

Actual Tev1 and Nimble evaluators have now been measured on a 16 GiB M4 and a 64 GiB M2 Ultra; see [the comparison and limitations](hardware-comparison.md). These instructions reproduce the opt-in benchmark on another Mac. It uses only checked-in synthetic tasks; no Jev or Anthropic calls, user configuration, user prompts or model downloads are involved.

Transfer `autorouter-hardware-benchmark.tar.gz` and its checksum to the other Mac. The bundle contains an explicit source allowlist and hashes; it excludes credentials, configurations, histories, git metadata and dependencies. Extract it:

```sh
shasum -a 256 -c autorouter-hardware-benchmark.tar.gz.sha256
tar -xzf autorouter-hardware-benchmark.tar.gz
cd autorouter-benchmark
node --version
node scripts/evaluate-ollama.mjs --help
```

Node 22+ and a running Ollama 0.35+ are required. No npm install is needed. Use already-installed model tags. Start when Ollama has no resident models; the benchmark refuses to displace unrelated resident models. It loads each selected candidate for a separate cold observation and repeated warm evaluations, then unloads that candidate from memory. It never deletes downloaded models. Keep other applications running at a representative load; CPU load averages and free memory are recorded, without process names or arguments.

Use the same two model tags and 30-second warm deadline as the 16 GiB baseline:

```sh
node scripts/evaluate-ollama.mjs --models tev1:4b-q4_K_M,nimble:9b-q4_K_M --split heldout --rounds 3 --stress-rounds 8 --timeout-ms 30000 --cold-timeout-ms 60000 --output hardware-report.json
```

For an installed Tev1 model, substitute its exact tag, such as `tev1:4b-q4_K_M`. Multiple installed candidates can be comma-separated. Use `--timeout-ms 0` to measure without the runtime timer; initial loading still has the separate cold deadline. Measure finite-deadline reliability in a separate report as well. Do not change label-agreement thresholds after seeing results: the default requires complete expected-label agreement, no under-routing and all three classified tiers. A nonzero exit can indicate a quality-gate failure while the JSON still contains valid measurements.

For a controlled comparison, match model artifact digests as well as tags, quantization, context allocation and runtime versions. The existing cross-host reports have different model digests and other conditions, so they are observational measurements rather than a RAM-only comparison.

The report contains hardware/Node/Ollama versions, initial/final background load/free memory, fixture and rubric hashes, model identity/quantization/resident memory, cold and repeated warm latency, full-budget stress cases, deadline failures and classification gates. Classification quality is agreement with the frozen synthetic rubric; selected and confirmed Claude models remain unmeasured. It does not prove end-user task quality or net savings.

Return `hardware-report.json` for comparison with the same workload on the 16 GiB Mac. Keep the bundle's `source-manifest.json` with it. Record whether the machine was in normal use or deliberately idle. A missing model or quality failure is reported explicitly rather than hidden by choosing a passing model or relabeling cases.
