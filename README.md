# Claude AutoRouter

Use Haiku, Sonnet, and Opus in one Claude Code session. A local gateway asks [TypeSafe Jev](https://typesafe.ai/blog/introducing-system-one-models-and-jev) to classify each inference request, applies compatibility and context checks, and streams the selected model's response back to Claude Code.

Requires Node.js 22+, macOS or Linux (including WSL), an installed `claude` command, a Claude subscription login or Anthropic API key, and a [TypeSafe API key](https://console.typesafe.ai). There are no runtime package dependencies. Native Windows is not supported in this release.

## Install and start

**npm publication is pending.** Install the release tarball now:

```sh
npm install -g ./claude-autorouter-0.1.0.tgz
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

Savings are an **API-equivalent estimate for the same token counts**, using Opus as the baseline. They do not measure subscription bill reductions or quota credits and exclude Jev charges. [Status line and savings details](docs/reference.md#status-line-and-savings).

## Behavior and data

- The default client profile permits all three routing tiers. Tool continuations, thinking history, model-specific features, and context size can keep or upgrade a model even when Jev chooses a cheaper tier. [Routing policy](docs/reference.md#routing-policy).
- Jev receives bounded excerpts of prompts that can contain source code, system instructions, and tool results. Anthropic receives the complete request. Images, document payloads, and private thinking are omitted from Jev's input. [Data flow and authentication](docs/reference.md#data-flow-and-authentication).
- Subscription access and usage limits still apply. Model switches can reduce cache reuse; cheaper token prices do not guarantee cheaper completed tasks. Run ordinary `claude` to bypass routing.
- The launcher is quiet by default. Use `AUTOROUTER_DEBUG=1` for metadata diagnostics or `AUTOROUTER_STATUSLINE=0` to retain your existing status line. [Troubleshooting](docs/reference.md#troubleshooting).

[Reference](docs/reference.md) · [Development and validation](docs/development.md) · [Preparing a release](docs/releasing.md) · [Apache-2.0 license](LICENSE)
