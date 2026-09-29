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

The source checkout uses Ollama's native `/v1/systemone` decision API with `nimble:9b-q4_K_M` by default. **This local integration is unreleased; npm version 0.2.0 still has the older chat-model implementation.** Jev remains the default evaluator.

Install and start Ollama 0.35 or newer; [version 0.35.0](https://github.com/ollama/ollama/releases/tag/v0.35.0) is a prerelease as of September 29, 2026. Then run from this checkout:

```sh
node bin/autorouter.mjs setup --evaluator ollama --pull --force
node bin/autorouter.mjs doctor
node bin/autorouter.mjs claude
```

`--force` replaces existing AutoRouter configuration. `--pull` downloads the selected model only if missing. Setup does not install or start Ollama, or delete existing models. Select a native decision model explicitly with `--ollama-model`:

| Model | Approximate download | Selection |
| --- | ---: | --- |
| [Nimble 9B Q4_K_M](https://ollama.com/library/nimble) | 5.63 GB | Default: `nimble:9b-q4_K_M` |
| [Tev1 0.8B Q8](https://ollama.com/library/tev1) | 812 MB | `tev1:0.8b` |
| [Tev1 4B Q4_K_M](https://ollama.com/library/tev1) | 2.7 GB | `tev1:4b-q4_K_M` |

For example, select Tev1 0.8B with:

```sh
node bin/autorouter.mjs setup --evaluator ollama --ollama-model tev1:0.8b --pull --force
```

Use `--ollama-model tev1:4b-q4_K_M` for the listed 4B variant; on the tested Mac it also needed a longer deadline, such as `AUTOROUTER_OLLAMA_TIMEOUT_MS=10000`, at setup. `tev1:latest` and `tev1:4b` select the larger Q8 download. Model terms are linked in the listings above; download size does not measure resident memory or routing quality. Custom native model tags and aliases also work.

No Jev key is needed for local classification. The launcher primes the evaluator before opening Claude's UI, and evaluation failures fall back to Sonnet or retain Opus without contacting Jev. Claude still answers through Anthropic, with the same routing guards and subscription limits.

On the tested 16 GiB M4, Tev1 0.8B matched 18/24 held-out labels with 450 ms median latency and no timeouts at the default 1,500 ms deadline, including full-excerpt checks. Tev1 4B matched 22/24 with a 10-second diagnostic deadline and 3.15-second median latency. Nimble matched 23/24 with a 30-second deadline and 11.4-second median latency. Both larger models exceeded the normal deadline in their standard-deadline tests. See the [measurements and limits](docs/ollama-evaluation.md) and [Ollama reference](docs/reference.md#ollama-evaluator), including configuration migration and the latency tradeoff.

## Behavior and data

- The default client profile permits all three routing tiers. Tool continuations, thinking history, model-specific features, and context size can keep or upgrade a model even when the evaluator chooses a cheaper tier. [Routing policy](docs/reference.md#routing-policy).
- The selected evaluator receives bounded excerpts that can contain source code, system instructions, and tool results: TypeSafe with Jev, or the local service with Ollama. Anthropic receives the complete request. Images, document payloads, and private thinking are omitted from classifier input. [Data flow and authentication](docs/reference.md#data-flow-and-authentication).
- Subscription access and usage limits still apply. Model switches can reduce cache reuse; cheaper token prices do not guarantee cheaper completed tasks. Run ordinary `claude` to bypass routing.
- The launcher is quiet by default. Use `AUTOROUTER_DEBUG=1` for metadata diagnostics or `AUTOROUTER_STATUSLINE=0` to retain your existing status line. [Troubleshooting](docs/reference.md#troubleshooting).

[Reference](docs/reference.md) · [Development and validation](docs/development.md) · [CI and npm release setup](docs/releasing.md) · [Apache-2.0 license](LICENSE)
