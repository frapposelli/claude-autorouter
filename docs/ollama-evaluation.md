# Local evaluator measurements

Jev remains the default evaluator. Ollama is an optional local classifier: Claude still generates the answer, and the router's capability and continuation guards still apply. This evaluation uses synthetic prompts only and does not measure the quality of Claude's completed work.

## Candidates and method

Measurements were recorded on September 29, 2026, on an Apple M4 Mac with 16 GiB of unified memory, running Ollama 0.33.3 alongside other applications. Candidates were loaded one at a time with a 4,096-token context. Download size is not the same as resident memory.

| Candidate | Published download size | Purpose |
| --- | ---: | --- |
| `qwen3.5:0.8b` | Approximately 1.0 GB | Smallest candidate |
| `qwen3:1.7b` | Approximately 1.4 GB | Compact speed baseline |
| `qwen3.5:2b` | Approximately 2.7 GB | Additional compact candidate |
| `qwen3.5:4b` | Approximately 3.4 GB | Measured larger candidate; rejected for preset latency |
| `qwen3:4b` | Approximately 2.5 GB | Larger candidate selected for the `quality` option |

Sizes and quantization vary by tag. Use explicit tags: untagged `qwen3.5` currently selects the much larger 9B model. See the official [Qwen3.5 model catalog](https://ollama.com/library/qwen3.5) and [Qwen3 1.7B listing](https://ollama.com/library/qwen3:1.7b).

The `compact` option selects `qwen3:1.7b` for its smaller allocation and faster measured classification. The `quality` option selects `qwen3:4b`, which agreed more often with the held-out labels while using more memory and time. The latter's official listing specifies a roughly 2.5 GB download and Q4_K_M quantization. Additional memory headroom is useful when other applications are open, but more RAM alone does not guarantee better accuracy or meeting the evaluator deadline. See the [official Qwen3 4B listing](https://ollama.com/library/qwen3:4b).

The checked-in fixture contains 36 independently authored, balanced routing cases: 12 tuning cases and 24 held-out cases, with equal numbers of Haiku, Sonnet, and Opus labels. Cases cover mechanical edits, ordinary implementation, difficult correctness and security work, topic changes, tool results, short follow-ups, and misleading routing instructions. Expected labels are judgments under the routing rubric, not independently verified claims about which Claude model would succeed.

The rubric was refined using the tuning set. The final held-out run uses the frozen rubric and is reported separately. Repeating each held-out case three times gives 72 decisions per model, but still only 24 distinct workloads.

The harness calls the production `buildOllamaState` and `evaluateOllama` functions. It includes local model metadata checks in wall-clock latency and bypasses AutoRouter's decision cache. Ollama's normal shared-prefix caching remains enabled. Input excerpts have a 3,000-character/UTF-8-byte ceiling. Requests disable thinking, use temperature 0, seed 0, a 32-token output cap, and a JSON schema containing only the tier. A returned tier is not a calibrated confidence probability. See Ollama's [thinking controls](https://docs.ollama.com/capabilities/thinking) and [structured-output guidance](https://docs.ollama.com/capabilities/structured-outputs).

A cold measurement starts with the model unloaded from Ollama; operating-system file caches and compiled kernels may already be warm. Cold calls have a separate 60-second measurement deadline. Warm measurements use the production 1,500 ms deadline. Model allocation is read from `/api/ps`, not inferred from the download size. On unified-memory hardware, its GPU allocation is not additional independent RAM. See [Ollama's running-model API](https://docs.ollama.com/api/ps) and [context-memory guidance](https://docs.ollama.com/context-length).

## Results

The frozen-rubric tuning results below document model selection. They are not held-out performance. GB values use decimal bytes; allocation is what the local running-model API reported, rather than total process or system memory.

| Model | Tuning agreement | Timeouts | Warm p50 / p95 | Cold wall time | Disk size | Model allocation |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `qwen3:1.7b` | 20 / 24 (83.3%) | 0 / 24 | 245 / 280 ms | 2.50 s | 1.36 GB | 1.70 GB |
| `qwen3:4b` | 24 / 24 (100%) | 0 / 24 | 743 / 889 ms | 9.62 s | 2.50 GB | 3.18 GB |
| `qwen3.5:0.8b` | 10 / 24 (41.7%) | 0 / 24 | 409 / 437 ms | 2.78 s | 1.04 GB | 1.09 GB |
| `qwen3.5:2b` | 18 / 24 (75.0%) | 0 / 24 | 918 / 1,009 ms | 6.49 s | 2.74 GB | 2.36 GB |
| `qwen3.5:4b` | No valid warm results | 24 / 24 | Deadline reached | 7.26 s | 3.39 GB | 3.14 GB |

The smallest model was highly sensitive to rubric wording. It should not be selected solely because its download is small. The 2B candidate was slower and agreed less often than 1.7B on this tuning set. A separate diagnostic gave `qwen3.5:4b` a 5,000 ms deadline: all 12 tuning decisions agreed, but warm p50/p95 was 3,615/4,872 ms. **Qwen3.5 4B is rejected as a preset because it missed the production deadline on every warm tuning request.** Its diagnostic result demonstrates a latency tradeoff, not held-out quality. Extra RAM alone does not establish that this model will meet a 1,500 ms deadline on another machine.

An initial 4B run also exposed a metadata-response limit: its local `/api/show` response exceeded 64 KiB. That issue was fixed before the results above by bounding metadata separately at 1 MiB while retaining a 64 KiB classifier-output limit.

### Held-out results

Both selected candidates completed all 72 held-out decisions without a timeout. Qwen3 4B agreed more often than 1.7B, including every Opus-labeled workload. These measurements were taken later than tuning while other applications remained open; they do not represent an isolated hardware benchmark.

| Model | Agreement | Timeouts | Warm p50 / p95 | Cold wall time | Under-routes / over-routes |
| --- | ---: | ---: | ---: | ---: | ---: |
| `qwen3:1.7b` | 42 / 72 (58.3%) | 0 / 72 | 602 / 834 ms | 4.80 s | 18 / 12 |
| `qwen3:4b` | 66 / 72 (91.7%) | 0 / 72 | 889 / 1,242 ms | 5.92 s | 0 / 6 |

The 1.7B confusion matrix:

| Expected tier | Returned Haiku | Returned Sonnet | Returned Opus |
| --- | ---: | ---: | ---: |
| Haiku | 12 | 6 | 6 |
| Sonnet | 0 | 24 | 0 |
| Opus | 0 | 18 | 6 |

The 4B confusion matrix:

| Expected tier | Returned Haiku | Returned Sonnet | Returned Opus |
| --- | ---: | ---: | ---: |
| Haiku | 24 | 0 | 0 |
| Sonnet | 0 | 18 | 6 |
| Opus | 0 | 0 | 24 |

Each row represents eight unique workloads repeated three times. The wrong labels were consistent across repetitions. For 1.7B, six of eight Opus workloads were sent to Sonnet, and four of eight Haiku workloads were sent to a higher tier. For 4B, two of eight Sonnet workloads were sent to Opus. The compact model's gap between tuning and held-out agreement limits what can be claimed about it. Both local options remain experimental; these results do not establish parity with Jev, which was not evaluated on this fixture.

### Full-excerpt performance

An additional eight synthetic requests per model filled the entire 3,000-byte state budget. They kept a clearly mechanical current task and included long synthetic tool output. A different nonce at the beginning of each serialized state prevented reuse of the previous full user-state prefix while preserving the common rubric prefix. Both models exceeded the 1,500 ms deadline on all eight requests. These are separate performance checks, not additional held-out accuracy cases. Short-prompt latency must not be treated as a bound for full excerpts.

| Model | Timeouts at 1,500 ms | Wall p50 / p95 before cancellation |
| --- | ---: | ---: |
| `qwen3:1.7b` | 8 / 8 | 1,502 / 1,503 ms |
| `qwen3:4b` | 8 / 8 | 1,502 / 1,505 ms |

A diagnostic rerun of 1.7B with a 5,000 ms deadline completed all eight and returned Haiku, with p50/p95 of 3,753/3,991 ms. Qwen3 4B still timed out on all eight at that longer deadline, with p50/p95 cancellation times of 5,002/5,007 ms. Its actual completion times for these full excerpts were not measured. A larger model and startup priming do not remove the need for a bounded timeout and fallback during longer requests.

All final comparisons use fixture SHA-256 `1ef5111a6a36f0f4bc8d111d54c6985cac4c4a8b16357023a0958d285f7438ad` and rubric SHA-256 `42c3e18ddcf7c9d8756100b740f686b3c2f1bbdfbcbaf3cbc3021a9c9b4c6ee6`.

## Reproducing the evaluation

Use a source checkout; benchmark scripts and fixtures are development files and are not bundled in the npm package. Start local Ollama and explicitly download the models you intend to test. The harness never downloads or deletes models, and refuses to begin while another model is resident. It unloads each tested model after its measurements.

```sh
node scripts/evaluate-ollama.mjs --models qwen3:1.7b,qwen3:4b --split tuning --rounds 2 --output artifacts/ollama-tuning.json
node scripts/evaluate-ollama.mjs --models qwen3:1.7b,qwen3:4b --split heldout --rounds 3 --stress-rounds 8 --output artifacts/ollama-heldout.json
```

JSON reports contain fixture/rubric hashes, model digests, quantization, per-case labels, timing counters, failures, confusion matrices, and allocation measurements. They contain no private repository prompts or API credentials. Raw reports are written only when `--output` is supplied; `artifacts/` is ignored by Git.

## Limits

This is a small synthetic rubric-agreement benchmark, not a downstream task-quality, cost-savings, or security evaluation. Its prompts cannot represent every repository or long conversation. A model can agree with the labels and still miss important context outside the excerpt. The tests do not establish robust resistance to prompt injection.

Latency depends on hardware, current application load, model residency, and prompt length. Cold loading exceeds the normal evaluator deadline, which is why the launcher primes an installed local model with a synthetic classification using the production request settings before starting Claude. The warm benchmark measurements above already followed a full classification, so this startup improvement does not change those measurements. Timeouts and invalid responses use the router's existing conservative fallback policy. Re-run the evaluation before adopting different tags or changing the rubric; no result here guarantees accuracy on your workload.
