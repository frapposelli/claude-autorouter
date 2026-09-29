# Local decision evaluator measurements

AutoRouter supports `/v1/systemone` classification only: remote Jev with an API key, or local Ollama 0.35+ with a compatible decision model. Jev remains the default evaluator. The local default is `nimble:9b-q4_K_M`; the old Qwen chat adapter and compact/quality/auto presets have been removed. Existing downloaded models are not deleted.

## Method

Measurements were recorded on September 29, 2026, on an Apple M4 Mac with 16 GiB of unified memory alongside other applications. Nimble ran on an isolated Ollama 0.35.0 process; the installed Ollama 0.33.3 daemon was left unchanged during that test. Tev1 was tested later on the user's upgraded Ollama 0.35.0 service after its downloads finished. Version 0.35.0 was a prerelease at the time. These are observations under different application loads, not a controlled hardware comparison. See the [official release](https://github.com/ollama/ollama/releases/tag/v0.35.0), [Nimble catalog](https://ollama.com/library/nimble), and [Tev1 catalog](https://ollama.com/library/tev1).

The selected Nimble tag contains a 9B Q4_K_M model, approximately 5.63 GB to download, with an 8,194-token native context setting. `/v1/systemone` scores choices directly; it accepts no chat generation options or per-request context override. The model/server configuration determines allocation. The production excerpt remains bounded to 3,000 serialized characters and UTF-8 bytes, including for non-ASCII text. Native confidence measures the concentration of the choice distribution, not calibrated accuracy, and is not used as Jev's confidence threshold.

The fixture contains 36 balanced synthetic workloads: 12 tuning cases and 24 held-out cases, with equal numbers of Haiku, Sonnet, and Opus labels. Cases include mechanical edits, ordinary implementation, difficult correctness and security work, topic changes, tool results, short follow-ups, and misleading routing instructions. These labels are judgments under the routing policy, not proof of which Claude model would complete each task successfully. The native questions preserve the existing local routing policy and were frozen before Nimble testing; no changes were made from held-out results.

The harness uses the production state builder and evaluator, including local metadata checks in wall-clock latency. It bypasses AutoRouter's decision cache; Ollama's own caching remains enabled. A cold measurement starts with the model unloaded, but operating-system file caches and kernels may already be warm. Cold calls have a separate 60-second deadline. The normal evaluator deadline is 1,500 ms. Reported model allocation comes from `/api/ps`; it is not a measurement of total process or system memory, and its GPU allocation is not additional independent RAM on this unified-memory Mac. Aggregate runtime RSS includes all Ollama and llama-server processes, including the original idle daemon.

## Nimble results

The first production-deadline run returned Haiku correctly for its cold mechanical task in 17.64 seconds. **All 12 warm tuning requests timed out at 1,500 ms**, with cancellation p50/p95 of 1,503/1,513 ms. No warm classification accuracy can be inferred from that run. The model's reported allocation was 5.48 GB, with an 8,194-token context. This did not meet the desired fast-routing target on the tested Mac.

A separate first-load smoke call correctly classified `[].length` as Haiku in 25.23 seconds. It checks integration, not warm performance or classifier accuracy.

The separate held-out diagnostic used a 30,000 ms deadline and one pass over 24 distinct workloads. It does not change the production default or establish performance at 1,500 ms.

| Measurement | Result |
| --- | ---: |
| Held-out rubric agreement | 23 / 24 (95.8%) |
| Valid decisions / timeouts | 24 / 0 |
| Warm p50 / p95 wall time | 11,432 / 16,334 ms |
| Minimum / maximum wall time | 697 / 20,201 ms |
| Cold wall time | 15.47 s |
| Reported model allocation | 5.48 GB |
| Under-routes / over-routes | 1 / 0 |

All eight Haiku and eight Sonnet labels matched. Seven of eight Opus labels matched. The `held-o-injection` case, a difficult deadlock investigation containing an instruction to select Haiku, was incorrectly routed to Haiku. This is a concrete limitation against misleading routing instructions. These 24 decisions are a single pass, not repeated measurements or a claim of comparable performance to Jev.

All eight full-excerpt diagnostic requests completed at the 30,000 ms deadline and returned the expected Haiku tier. Their p50/p95 latency was 25,711/27,271 ms. These synthetic requests fill the 3,000-byte state budget with clearly mechanical tasks and unrelated tool output. They are separate performance checks, not eight additional held-out workloads. Longer excerpts can consume almost the entire diagnostic deadline on this machine.

An isolated live Claude Code test also passed using the saved Enterprise subscription login, a temporary configuration with a 30,000 ms evaluator deadline, and no Jev key. Nimble selected Haiku in 12.69 seconds; Anthropic returned HTTP 200 with the expected literal response and confirmed `claude-haiku-4-5-20251001`. Only a synthetic prompt was used, with no repository files or tools. This verifies the authentication and routing integration, not general classifier accuracy. The installed Ollama service and user configuration were left unchanged.

## Tev1 results

Both [Tev1 variants](https://ollama.com/library/tev1) use the same production adapter and frozen questions, selected through `--ollama-model`. The 0.8B tag uses Q8_0 quantization and downloads approximately 812 MB; `tev1:4b-q4_K_M` downloads approximately 2.71 GB. The unqualified `tev1` tag selects the larger 4B Q8 model, which was not tested. Jev remains the evaluator default and Nimble remains the local-model default.

Each measured tag ships `num_ctx:2050`. The window includes the template, routing criteria, and excerpt. The 3,000-byte state cap is not a guarantee that every possible input fits this smaller token window. The reported stress cases used 1,664–1,752 input tokens on 0.8B. Requests exceeding model limits use the usual fallback; AutoRouter does not switch protocols or silently truncate additional content for Tev1.

These measurements use one pass over the same 24 held-out cases and eight separate full-excerpt cases, after both downloads finished. An exploratory 0.8B run during the 4B download gave the same labels; its timings are excluded here. Neither questions nor expected labels were changed in response to Tev1 outputs.

| Model | Deadline | Held-out agreement | Timeouts | Warm p50 / p95 | Reported allocation |
| --- | ---: | ---: | ---: | ---: | ---: |
| `tev1:0.8b` | 1,500 ms | 18 / 24 (75.0%) | 0 / 24 | 450 / 488 ms | 0.89 GB |
| `tev1:4b-q4_K_M` | 1,500 ms | No valid warm decisions | 24 / 24 | Deadline reached | 2.91 GB |
| `tev1:4b-q4_K_M` diagnostic | 10,000 ms | 22 / 24 (91.7%) | 0 / 24 | 3,149 / 4,169 ms | 2.91 GB |

The 0.8B model matched four of eight Haiku labels, all eight Sonnet labels, and six of eight Opus labels. It over-routed four mechanical tasks and under-routed two difficult tasks to Sonnet, including a case with a misleading tier instruction. Cold wall time was 2.08 seconds for 0.8B and 6.04 seconds for 4B. Model size and fast responses do not establish sufficient accuracy for an engineering workload.

With a separate 10-second deadline, 4B matched seven of eight Haiku labels, all eight Sonnet labels, and seven of eight Opus labels. It over-routed one mechanical case to Opus and under-routed one difficult case to Sonnet. Its cold diagnostic request took 3.69 seconds. The improved agreement comes with several seconds of classification latency; it is not performance at the default deadline.

All eight 0.8B full-excerpt requests completed within 1,500 ms and returned Haiku, with p50/p95 of 1,079/1,150 ms. The 4B model timed out on all eight at 1,500 ms and again on all eight at 10,000 ms. Its 10-second cancellation p50/p95 was 10,006/10,081 ms; completed full-excerpt latency was not measured. The longer deadline therefore allows the reported short held-out decisions but does not guarantee completion for full excerpts.

The native API is the supported Ollama integration. Together's [publisher interface](https://huggingface.co/togethercomputer/Tev1-4B-experimental#intended-interface) describes a different training prompt layout from the schema rendered by [Ollama 0.35's compiler](https://github.com/ollama/ollama/blob/v0.35.0/decision/systemone.go). These results measure the actual Ollama native path, not a reproduction of Together's training-format evaluation or its published accuracy figures.

Both tags passed isolated live Claude Enterprise integration checks on the user's Ollama 0.35 service, with no Jev key and no repository tools or files. Claude returned the correct `0` for a synthetic `[].length` query and Anthropic returned HTTP 200. The 0.8B evaluator took 584 ms under the default deadline but selected Sonnet, over-routing this mechanical task. The 4B evaluator selected Haiku in 3.78 seconds using a temporary 10-second deadline. These checks verify setup, authentication, native evaluation, and generation; they do not imply that every routing decision is correct. Temporary configurations were removed, tested models were unloaded from memory, and the user's Ollama service and model files were retained.

Model digests:

- `tev1:0.8b`: `c0099a86fcbd81bc5876a0d1f94998d2b038f7f1b5f7329a3dba43a36903c652`
- `tev1:4b-q4_K_M`: `3509ac7180e86e5fa8efc7b5745e32d55dd9d4e0a86bc9a88aba5323a5d29bc6`

## Reproducing the evaluation

Use a source checkout; benchmark scripts and fixtures are not included in the npm package. Install Ollama 0.35+, start it, and explicitly download the model:

```sh
ollama pull nimble:9b-q4_K_M
node scripts/evaluate-ollama.mjs --models nimble:9b-q4_K_M --split tuning --rounds 1 --output artifacts/nimble-tuning.json
node scripts/evaluate-ollama.mjs --models nimble:9b-q4_K_M --split heldout --rounds 1 --stress-rounds 8 --timeout-ms 30000 --output artifacts/nimble-diagnostic.json
ollama pull tev1:0.8b
ollama pull tev1:4b-q4_K_M
node scripts/evaluate-ollama.mjs --models tev1:0.8b,tev1:4b-q4_K_M --split heldout --rounds 1 --stress-rounds 8 --output artifacts/tev1-default.json
node scripts/evaluate-ollama.mjs --models tev1:4b-q4_K_M --split heldout --rounds 1 --stress-rounds 8 --timeout-ms 10000 --output artifacts/tev1-diagnostic.json
```

Use `--endpoint http://127.0.0.1:PORT` for another local instance. The benchmark requires no resident models at startup, never downloads or deletes models, and unloads each tested model afterward. It sends only checked-in synthetic cases and does not contact Claude or Jev. Reports include model identity, protocol, fixture/question hashes, token counts, per-case results, timeouts, confusion matrices, latency, and reported allocation. The eight separate stress requests fill the excerpt budget and vary an early nonce to prevent reuse of the previous full state; they are performance checks, not held-out accuracy cases.

Reproducibility identifiers:

- Model digest: `3776806da5587387a996e28e75d5d07fbebe7410d47879e1f71782f36c896ce3`
- Fixture SHA-256: `1ef5111a6a36f0f4bc8d111d54c6985cac4c4a8b16357023a0958d285f7438ad`
- Native questions SHA-256: `be151cedb4de4b7ef3f7162d751f70ce7d9dd14efc66fae1835f73ffd04027be`

## Limits and previous measurements

This is a small synthetic rubric-agreement benchmark, not a downstream task-quality, savings, Jev-parity, or security evaluation. Classification can miss context outside the excerpt. The cases do not establish robust resistance to prompt injection. Performance depends on hardware, memory pressure, prompt length, and residency; a successful setup or simple request does not guarantee the runtime deadline.

The launcher primes the model before opening Claude, allowing up to 60 seconds for that synthetic classification. Idle unloading can still make later requests cold. Runtime timeouts and invalid responses use the existing conservative fallback, without contacting Jev. A longer `AUTOROUTER_OLLAMA_TIMEOUT_MS` trades added prompt latency for more completed local classifications; it does not make the evaluator faster.

Earlier Qwen results used a different `/api/chat` implementation and are not measurements of this native backend. They remain available in the [historical evaluation document](https://github.com/frapposelli/claude-autorouter/blob/548a175/docs/ollama-evaluation.md). Reproduce those results from that revision, not the current native-only harness.
