# Claude AutoRouter

Use Haiku, Sonnet, and Opus in one Claude Code session. A local gateway classifies each inference request with the selected evaluator, applies compatibility and context checks, and streams the selected model's response back to Claude Code. [TypeSafe Jev](https://typesafe.ai/blog/introducing-system-one-models-and-jev) is the default; an experimental Ollama backend evaluates requests locally.

Requires Node.js 22+, macOS or Linux (including WSL), an installed `claude` command, and a Claude subscription login or Anthropic API key. The default evaluator also requires a [TypeSafe API key](https://console.typesafe.ai). There are no runtime package dependencies. Native Windows is not supported in this release.

## Install and start

**npm publication is pending.** Install the release tarball now:

```sh
npm install -g ./claude-autorouter-0.2.0.tgz
```

Once the package is published, install it from the registry with:

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

[Install and start Ollama](https://docs.ollama.com/quickstart), then select a preset. The default `compact` uses less memory; `quality` showed better agreement with the routing rubric:

```sh
claude-autorouter setup --evaluator ollama --ollama-preset compact --pull
claude-autorouter doctor
claude-autorouter claude
```

Add `--force` when replacing an existing config. Setup downloads a missing selected model only with `--pull`; it does not install or start Ollama. No Jev key is needed. Claude still answers through Anthropic, with the same routing guards and subscription limits.

The launcher primes the local classifier before opening Claude's UI. Failed evaluations fall back to Sonnet or retain Opus without contacting Jev. See the [Ollama reference](docs/reference.md#ollama-evaluator) for setup options.

Both presets were tested on a 16 GiB M4 Mac using 24 distinct held-out synthetic workloads repeated three times:

| Preset | Rubric agreement | Warm p50 / p95 | Model allocation |
| --- | ---: | ---: | ---: |
| `compact` (`qwen3:1.7b`) | 58.3% | 602 / 834 ms | 1.70 GB |
| `quality` (`qwen3:4b`) | 91.7% | 889 / 1,242 ms | 3.18 GB |

Use `--ollama-preset quality` to choose the larger model when memory permits, including on a 16 GiB machine. Each model timed out on all eight full-excerpt stress requests at the 1,500 ms deadline. These results do not establish parity with Jev or completed-task quality. Read the [measurements and limits](docs/ollama-evaluation.md) before choosing local classification.

## Behavior and data

- The default client profile permits all three routing tiers. Tool continuations, thinking history, model-specific features, and context size can keep or upgrade a model even when the evaluator chooses a cheaper tier. [Routing policy](docs/reference.md#routing-policy).
- The selected evaluator receives bounded excerpts that can contain source code, system instructions, and tool results: TypeSafe with Jev, or the local service with Ollama. Anthropic receives the complete request. Images, document payloads, and private thinking are omitted from classifier input. [Data flow and authentication](docs/reference.md#data-flow-and-authentication).
- Subscription access and usage limits still apply. Model switches can reduce cache reuse; cheaper token prices do not guarantee cheaper completed tasks. Run ordinary `claude` to bypass routing.
- The launcher is quiet by default. Use `AUTOROUTER_DEBUG=1` for metadata diagnostics or `AUTOROUTER_STATUSLINE=0` to retain your existing status line. [Troubleshooting](docs/reference.md#troubleshooting).

[Reference](docs/reference.md) · [Development and validation](docs/development.md) · [Preparing a release](docs/releasing.md) · [Apache-2.0 license](LICENSE)
