# Claude AutoRouter

Use Haiku, Sonnet and Opus in one Claude Code session. AutoRouter evaluates each coding request, checks model compatibility and context capacity, and forwards it through a local gateway. [TypeSafe Jev](https://typesafe.ai/blog/introducing-system-one-models-and-jev) is the default evaluator; native Ollama `/v1/systemone` models provide an experimental local option. Claude owns authentication, tool permissions and safety review.

Requires Node.js 22+, macOS or Linux (including WSL), an installed `claude` command, and a Claude subscription login or Anthropic API key. The default evaluator also needs a [TypeSafe API key](https://console.typesafe.ai). The installed CLI has no runtime dependencies.

Version 0.4.0 adds `config`, `sessions` and `doctor --evaluate-local`, durable task continuity, and clearer model outcomes. Upgrade from 0.3.x to use these commands. The [contributor guide](CONTRIBUTING.md) explains local verification, and the [release guide](docs/releasing.md) covers the changes and verified publication.

## Install and start

```sh
npm install -g claude-autorouter
claude-autorouter setup
claude-autorouter doctor
cd /path/to/project
claude-autorouter claude
```

Setup defaults to your Claude subscription and prompts privately for the Jev key. Run `claude auth login` if needed. Jev has separate credentials and billing; subscription mode needs no Anthropic API key. For API billing, use `setup --auth-mode api-key`.

Configuration is saved privately at `~/.config/claude-autorouter/config.json`. Environment variables override it; project `.env` files are not loaded automatically. `setup --force` updates an existing configuration while preserving other settings. Use focused commands for later edits:

```sh
claude-autorouter config show
claude-autorouter config set AUTOROUTER_JEV_TIMEOUT_MS 2000
claude-autorouter config unset AUTOROUTER_JEV_TIMEOUT_MS
claude-autorouter help config
```

Secret updates use a hidden prompt or `--stdin`, never a command-line value. Claude arguments pass through, including `claude-autorouter claude --help`. [Configuration reference](docs/reference.md#configuration).

## Auto permission mode

```sh
claude-autorouter claude --permission-mode auto
```

This profile automatically switches between Sonnet 5.5 and Opus 5.5 for new human tasks. A Haiku verdict uses Sonnet. Tool and `/goal` continuations retain the task's execution model; a new task can switch up or down. Claude's native safety review and organization policies still apply. For Auto selected through Claude's UI, save `AUTOROUTER_CLIENT_PROFILE=auto` with `config set`. [Auto support and limitations](docs/reference.md#auto-permission-mode).

## Inspect decisions

The launcher adds a temporary status line, preserving saved Claude settings:

```text
● AutoRouter · Opus 5.5 · ready · Jev 210ms
● AutoRouter · Sonnet 5.5 selected · Auto floor from Haiku · Jev 220ms
```

`selected` means Anthropic has not reported the serving model yet. Guard reasons and errors stay visible before optional savings. Claude's own model label may show its starting model. [Status details](docs/reference.md#status-line-and-savings).

Persistent history is optional and disabled by default. Enable metadata-only records without prompt excerpts:

```sh
claude-autorouter config set AUTOROUTER_SESSION_LOG_MODE metadata
claude-autorouter config set AUTOROUTER_SESSION_LOG_DIR "$HOME/.local/state/claude-autorouter/sessions"
claude-autorouter claude
claude-autorouter sessions list
claude-autorouter sessions show ID --json
```

Copy an `id` from `list`. History separates model decisions from outcomes and reports latency, fallbacks, failures and savings coverage. Choose `prompts` mode for bounded human-task excerpts. Files persist locally; no automatic deletion occurs. [History and privacy](docs/reference.md#session-decision-logs).

Savings are **API-equivalent estimates using the recorded Opus baseline and token counts**. They do not measure subscription bill reductions or quota credits, and exclude evaluator and local compute costs. Missing or unsupported usage stays unpriced.

## Local Ollama evaluator

Start Ollama 0.35+ with a model supporting its native decision endpoint, then configure it explicitly:

```sh
claude-autorouter setup --evaluator ollama --ollama-model tev1:4b-q4_K_M --pull --force
claude-autorouter doctor --evaluate-local
claude-autorouter claude
```

`--pull` authorizes downloading the chosen model if missing. Setup keeps existing models and settings; ordinary launches download nothing. The default local model is `nimble:9b-q4_K_M`; `tev1:0.8b` is smaller and requires checking its accuracy on your tasks. Local classification needs no Jev key. Claude still answers through Anthropic. [Model choices, deadlines and historical measurements](docs/reference.md#ollama-evaluator).

To allow a slower local model to finish without AutoRouter's runtime deadline:

```sh
claude-autorouter config set AUTOROUTER_OLLAMA_TIMEOUT_MS 0
```

Cancellation and response-size limits still apply. The local diagnostic uses synthetic prompts and reports observed latency, classification and fallback reasons; it makes no Anthropic/Jev calls or downloads and preserves unrelated resident models.

## Troubleshoot and upgrade

Inspect `config show` for environment overrides, and `doctor` for setup health. A valid evaluator verdict can be overridden by continuity, context or model compatibility. `Ollama fallback: timeout` means evaluation failed to finish, rather than predicting Sonnet. Status errors and saved history explain these paths.

```sh
npm install -g claude-autorouter@latest
claude-autorouter --version
claude-autorouter doctor
```

Historical integration observations cover Claude Code 2.1.284–2.1.285. The versioned synthetic protocol fixtures test reviewed request/response contracts; they do not certify the current checkout against a live Claude version. Real-provider checks remain explicitly invoked. [Troubleshooting](docs/reference.md#troubleshooting) covers context use, blocked goals and logging. Run ordinary `claude` to bypass routing.

The evaluator receives bounded task/history excerpts that may contain code and tool results: TypeSafe for Jev, or your loopback Ollama service. Anthropic receives the complete request. Model switching can reduce cache reuse. [Data flow and authentication](docs/reference.md#data-flow-and-authentication).

[Reference](docs/reference.md) · [Contributing](CONTRIBUTING.md) · [Development](docs/development.md) · [Releases](docs/releasing.md) · [Apache-2.0](LICENSE)
