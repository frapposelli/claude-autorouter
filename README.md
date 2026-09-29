# Claude AutoRouter

Use Haiku, Sonnet, and Opus in one Claude Code session. A local gateway classifies each inference request with the selected evaluator, applies compatibility and context checks, and streams the selected model's response back to Claude Code. [TypeSafe Jev](https://typesafe.ai/blog/introducing-system-one-models-and-jev) is the default; an experimental Ollama backend evaluates requests locally.

Requires Node.js 22+, macOS or Linux (including WSL), an installed `claude` command, and a Claude subscription login or Anthropic API key. The default evaluator also requires a [TypeSafe API key](https://console.typesafe.ai). There are no runtime package dependencies. Native Windows is not supported in this release.

## Install and start

Install from [npm](https://www.npmjs.com/package/claude-autorouter):

```sh
npm install -g claude-autorouter
```

Set up once, then launch from any project directory:

```sh
claude-autorouter setup
claude-autorouter doctor
cd /path/to/project
claude-autorouter claude
```

Setup defaults to your Claude subscription and prompts for your Jev key without echoing it. If Claude is not already signed in, run `claude auth login`. No Anthropic API key or exported subscription token is needed for subscription mode. Jev has separate credentials and billing.

Setup saves a private JSON config at `~/.config/claude-autorouter/config.json`; `XDG_CONFIG_HOME` and `AUTOROUTER_CONFIG` can change its location. Environment variables override saved configuration. Project `.env` files are not loaded automatically.

Claude Code arguments pass through:

```sh
claude-autorouter claude -p "Fix the typo in README.md"
claude-autorouter --help
claude-autorouter --version
```

For API billing, use `claude-autorouter setup --auth-mode api-key`. Use `--force` to replace an existing config. Automation can supply `TYPESAFE_API_KEY` and, in API-key mode, `ANTHROPIC_API_KEY` through the environment; keys are never command-line arguments. `doctor` checks local configuration and Claude installation/login state without paid requests. See the [configuration reference](docs/reference.md#configuration).

## What you see

The launcher adds a temporary status line and leaves saved Claude Code settings unchanged:

```text
● AutoRouter · last Haiku 4.5 · ready · Jev 210ms · est saved $0.04 (75%) vs Opus
● AutoRouter · Sonnet 5 selected · connecting · Jev→Haiku 290ms · large context
```

The confirmed model comes from Anthropic's response. Claude's own model label can still show its Haiku starting model. `API ctx` measures input against the actual model's known window; a different client limit remains visible as `CLI ctx`.

Savings are an **API-equivalent estimate for the same token counts**, using Opus as the baseline. They do not measure subscription bill reductions or quota credits and exclude Jev and local compute costs. [Status line and savings details](docs/reference.md#status-line-and-savings).

## Experimental local evaluator

The local setup below requires AutoRouter 0.3.2 or newer. It uses Ollama's native `/v1/systemone` decision API with `nimble:9b-q4_K_M` by default. Jev remains the default evaluator. If upgrading from 0.2.0, replace the old Qwen model configuration using the [migration steps](docs/reference.md#migrating-an-older-ollama-config).

Version 0.3.2 excludes Claude's executor system instructions from the local classifier excerpt, retaining task and conversation excerpts. Runtime deadlines default to 1,500 ms for Tev1 0.8B/custom models, 15,000 ms for official Tev1 4B tags, and 30,000 ms for official Nimble tags. Explicit timeout settings, including a `1500` saved with 0.3.1, still override these defaults. Jev is unchanged.

Set `0` to disable AutoRouter's runtime evaluator deadline for one launch using your existing configuration:

```sh
AUTOROUTER_OLLAMA_TIMEOUT_MS=0 claude-autorouter claude
```

To save that setting for an installed Tev1 4B model:

```sh
claude-autorouter setup --evaluator ollama --ollama-model tev1:4b --ollama-timeout-ms 0 --force
```

The setup flag overrides the timeout environment value and saves it. Cancellation and disconnected clients still stop evaluation, normal errors still use fallback, and startup priming keeps its separate 60-second deadline.

Install and start Ollama 0.35 or newer; [version 0.35.0](https://github.com/ollama/ollama/releases/tag/v0.35.0) is a prerelease as of September 29, 2026. Then run:

```sh
claude-autorouter setup --evaluator ollama --pull --force
claude-autorouter doctor
claude-autorouter claude
```

`--force` replaces existing AutoRouter configuration. `--pull` downloads the selected model only if missing. Setup does not install or start Ollama, or delete existing models. Select a native decision model explicitly with `--ollama-model`:

| Model | Approximate download | Selection |
| --- | ---: | --- |
| [Nimble 9B Q4_K_M](https://ollama.com/library/nimble) | 5.63 GB | Default: `nimble:9b-q4_K_M` |
| [Tev1 0.8B Q8](https://ollama.com/library/tev1) | 812 MB | `tev1:0.8b` |
| [Tev1 4B Q4_K_M](https://ollama.com/library/tev1) | 2.7 GB | `tev1:4b-q4_K_M` |

For example, select Tev1 0.8B with:

```sh
claude-autorouter setup --evaluator ollama --ollama-model tev1:0.8b --pull --force
```

Use `--ollama-model tev1:4b-q4_K_M` for the listed 4B variant; `tev1:latest` and `tev1:4b` select the larger Q8 download. Model terms are linked in the listings above; download size does not measure resident memory or routing quality. Custom native model tags and aliases also work.

No Jev key is needed for local classification. The launcher primes the evaluator before opening Claude's UI, and evaluation failures fall back to Sonnet or retain Opus without contacting Jev. Claude still answers through Anthropic, with the same routing guards and subscription limits.

Setup, doctor, and startup show the effective model and deadline. Warmup and doctor do not certify classification speed or accuracy. `Ollama fallback: timeout` means no valid decision arrived in time; it is different from the evaluator choosing Sonnet. Source users can run the [local routing regression](docs/development.md#local-routing-regression) to check all three tiers without Claude or Jev calls.

Historical measurements before 0.3.2, on a 16 GiB M4: Tev1 0.8B matched 18/24 held-out labels with 450 ms median latency and no timeouts at 1,500 ms, including full-excerpt checks. Tev1 4B matched 22/24 with a 10-second diagnostic deadline and 3.15-second median latency. Nimble matched 23/24 with a 30-second deadline and 11.4-second median latency. Both larger models exceeded the then-default 1,500 ms. A separate six-case regression with the 0.3.2 fixes passed for both tested Tev1 4B variants and Nimble; Tev1 0.8B matched only three cases. These small tests do not establish general accuracy or Jev parity. See the [measurements and limits](docs/ollama-evaluation.md) and [Ollama reference](docs/reference.md#ollama-evaluator).

## Behavior and data

- The default client profile permits all three routing tiers. Tool continuations, thinking history, model-specific features, and context size can keep or upgrade a model even when the evaluator chooses a cheaper tier. [Routing policy](docs/reference.md#routing-policy).
- The selected evaluator receives bounded excerpts that can contain source code and tool results: TypeSafe with Jev, or the local service with Ollama. Jev also receives system-text excerpts; the local path excludes Claude's executor system instructions. Anthropic receives the complete request. Images, document payloads, and private thinking are omitted from classifier input. [Data flow and authentication](docs/reference.md#data-flow-and-authentication).
- Subscription access and usage limits still apply. Model switches can reduce cache reuse; cheaper token prices do not guarantee cheaper completed tasks. Run ordinary `claude` to bypass routing.
- The launcher is quiet by default. Use `AUTOROUTER_DEBUG=1` for metadata diagnostics or `AUTOROUTER_STATUSLINE=0` to retain your existing status line. [Troubleshooting](docs/reference.md#troubleshooting).

[Reference](docs/reference.md) · [Development and validation](docs/development.md) · [CI and npm release setup](docs/releasing.md) · [Apache-2.0 license](LICENSE)
