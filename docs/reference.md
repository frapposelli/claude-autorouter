# Reference

## Commands

| Command | Purpose |
| --- | --- |
| `claude-autorouter setup` | Save subscription-mode configuration and a Jev key |
| `claude-autorouter setup --auth-mode api-key` | Configure Jev and Anthropic API-key billing |
| `claude-autorouter setup --evaluator ollama --ollama-preset compact --pull` | Configure a local evaluator and download its selected model if missing |
| `claude-autorouter setup --force` | Replace an existing user config |
| `claude-autorouter doctor` | Check config, Claude executable/login, and the selected local Ollama model without paid calls |
| `claude-autorouter claude [arguments]` | Start a local router and pass arguments through to Claude Code |
| `claude-autorouter serve` | Run the router for separately configured clients |
| `claude-autorouter --help` | Show command help |
| `claude-autorouter --version` | Print the package version |

The launcher binds an ephemeral port on `127.0.0.1`, creates a temporary local credential, starts Claude with the gateway address, and shuts down when Claude exits. It works from any project directory on macOS or Linux, including WSL. Native Windows is unsupported in this release; the status-line command uses a POSIX shell. Standalone `serve` uses the configured port and requires a local token.

## Configuration

Setup defaults to subscription mode unless `--auth-mode` or `AUTOROUTER_AUTH_MODE` selects another mode. Jev remains the default evaluator; `--evaluator ollama` selects local classification. Setup prompts for required secrets without echoing them and writes a private JSON file. Stored keys are plaintext; keep the file private and out of source control. Supply keys through the environment when interactive input is unavailable. Subscription mode with Ollama requires no API keys. API-key authentication always requires `ANTHROPIC_API_KEY`, regardless of evaluator.

The config path is selected in this order:

1. `AUTOROUTER_CONFIG`, when set.
2. `$XDG_CONFIG_HOME/claude-autorouter/config.json`, when `XDG_CONFIG_HOME` is a nonempty absolute path.
3. `~/.config/claude-autorouter/config.json`.

The JSON file uses flat environment-style string keys, such as `AUTOROUTER_AUTH_MODE` and `TYPESAFE_API_KEY`. Environment values take precedence over the saved config. Use `setup --force` to replace existing configuration. The launcher does not discover or load a project's `.env` file. From a source checkout, explicitly loading one still works:

```sh
node --env-file=.env bin/autorouter.mjs claude
```

For an environment-only subscription launch, set `AUTOROUTER_AUTH_MODE=subscription` and either supply `TYPESAFE_API_KEY` or select `AUTOROUTER_EVALUATOR=ollama` with a running local model. For API-key mode, also supply `ANTHROPIC_API_KEY`. The shell variables are read by the router; Jev's key is removed from the Claude child environment.

| Variable | Default | Purpose |
| --- | --- | --- |
| `TYPESAFE_API_KEY` | required for Jev | Jev credential; unused by Ollama |
| `ANTHROPIC_API_KEY` | required in API-key mode | Upstream Anthropic credential |
| `AUTOROUTER_CONFIG` | see path order above | Explicit user config path |
| `AUTOROUTER_AUTH_MODE` | `api-key` without saved config; setup selects `subscription` | Authentication mode |
| `AUTOROUTER_EVALUATOR` | `jev` | `jev` or local `ollama` classification |
| `AUTOROUTER_CLIENT_PROFILE` | `compatible` | `native` retains Claude's own model and thinking settings |
| `AUTOROUTER_STATUSLINE` | enabled | `0` retains your existing status line |
| `AUTOROUTER_DEBUG` | off | `1` enables launcher metadata logs on stderr |
| `ENABLE_TOOL_SEARCH` | `true` in launcher when unset | Load MCP tool definitions on demand; explicit values are preserved |
| `AUTOROUTER_HAIKU_MODEL` | `claude-haiku-4-5-20251001` | Routine tier |
| `AUTOROUTER_SONNET_MODEL` | `claude-sonnet-5` | Standard tier |
| `AUTOROUTER_OPUS_MODEL` | `claude-opus-5-5` | Demanding tier and savings baseline |
| `AUTOROUTER_JEV_MODEL` | `jev-latest` | Classifier version |
| `AUTOROUTER_JEV_TIMEOUT_MS` | `1500` | Classifier deadline in milliseconds |
| `AUTOROUTER_OLLAMA_URL` | `http://127.0.0.1:11434` | Loopback Ollama base URL |
| `AUTOROUTER_OLLAMA_MODEL` | `qwen3:1.7b` | Installed local model tag; setup can choose a preset |
| `AUTOROUTER_OLLAMA_TIMEOUT_MS` | `1500` | Whole local classification deadline in milliseconds |
| `AUTOROUTER_OLLAMA_KEEP_ALIVE` | `5m` | How long Ollama retains the evaluator in memory |
| `AUTOROUTER_TOKEN_COUNT_TIMEOUT_MS` | `1500` | Context-check deadline; runs alongside classification |
| `AUTOROUTER_MIN_CONFIDENCE` | `0.75` | Jev confidence threshold; does not apply to Ollama |
| `AUTOROUTER_PORT` | `8787` | Standalone server port |
| `AUTOROUTER_TOKEN` | none | Standalone local credential, at least 16 characters |
| `AUTOROUTER_UPSTREAM_URL` | `https://api.anthropic.com` | Anthropic-compatible origin; fixed in subscription mode |
| `AUTOROUTER_JEV_URL` | `https://api.typesafe.ai/v1/systemone` | Jev endpoint |

Model access depends on your account. The policy recognizes specific Claude model versions; arbitrary gateway aliases do not automatically inherit their capabilities or context windows. Compare overrides with the [Anthropic model catalog](https://platform.claude.com/docs/en/models/overview).

## Ollama evaluator

Ollama is an experimental local classifier. It chooses a Claude tier; Haiku, Sonnet, or Opus still completes the task through Anthropic. Jev remains the default, and selecting Ollama never silently switches back to Jev.

[Install Ollama](https://docs.ollama.com/quickstart) and start its local service first. Open the Ollama app on macOS, or use `ollama serve` if a server is not already running. Then:

```sh
claude-autorouter setup --evaluator ollama --ollama-preset compact --pull
claude-autorouter doctor
claude-autorouter claude
```

Use `--force` to replace existing configuration. Setup detects the running local API. `--pull` authorizes downloading the chosen model when it is missing; without it, install the model yourself before setup. AutoRouter does not install Ollama, start its daemon, or download models during ordinary launches or `doctor` checks.

| Preset | Model | Selection |
| --- | --- | --- |
| `compact` | `qwen3:1.7b` | Default prioritizes lower memory use; lower held-out rubric agreement |
| `quality` | `qwen3:4b` | Better measured rubric agreement, with higher memory use and latency |
| `auto` | One of the above | `quality` only when reported total system memory exceeds 24 GiB; otherwise `compact` |

The memory heuristic estimates headroom using total system RAM, not currently free memory or a CPU/GPU benchmark. Both presets were tested on a 16 GiB Mac; `quality` can be selected explicitly on that capacity when memory permits. The automatic threshold is a conservative headroom choice, not a speed or accuracy guarantee. The `quality` model has an approximately 2.5 GB download; download size differs from resident memory, which includes runtime and context allocations. Concurrent applications also need memory. The classifier uses a 4,096-token context to bound that allocation. See [Ollama's context-memory guidance](https://docs.ollama.com/context-length). Downloaded models have their own licenses and are not included in this package.

The 16 GiB M4 comparison used 24 distinct held-out synthetic workloads, eight per tier, repeated three times for each model:

| Model | Rubric agreement | Warm p50 / p95 | Cold call | Model allocation |
| --- | ---: | ---: | ---: | ---: |
| `qwen3:1.7b` | 42 / 72 (58.3%) | 602 / 834 ms | 4.80 s | 1.70 GB |
| `qwen3:4b` | 66 / 72 (91.7%) | 889 / 1,242 ms | 5.92 s | 3.18 GB |

Both completed every short held-out request without a timeout. Compact routed six of eight distinct Opus-labeled workloads to Sonnet. Quality had no under-routing in this fixture; its six errors were two distinct Sonnet workloads routed to Opus on each repetition. Each model exceeded the 1,500 ms deadline on all eight requests in a separate full-excerpt stress test. The [local evaluation report](ollama-evaluation.md) records the test conditions and rejected candidates. There was no Jev comparison, so these results do not establish parity with Jev. Rubric agreement on synthetic cases does not establish the quality or cost of completed Claude tasks.

Use `--ollama-model LOCAL_TAG` to override the preset, for example with an already installed local model. The endpoint must be loopback (`127.0.0.1`, `localhost`, or `::1`), without a path, credentials, query, or fragment. Cloud model tags and metadata identifying a remote model are rejected before sending task text. Claude and Jev credentials are never attached to Ollama requests.

Local classification caps serialized evaluator state at both 3,000 characters and 3,000 UTF-8 bytes, including for non-ASCII prompts. It sends that state to `/api/chat`, requests a strict JSON tier, disables thinking, and caps output at 32 tokens. It does not invent a confidence probability; `AUTOROUTER_MIN_CONFIDENCE` applies only to Jev. All capability, tool-continuation, thinking, and context guards still apply.

Before opening Claude's UI, the launcher loads an installed model and primes the actual classifier rubric with a synthetic task, using a separate deadline of up to 60 seconds. Each normal evaluation has a 1,500 ms deadline covering local model metadata checks and classification. Priming reduces first-request overhead but does not guarantee that longer excerpts finish in time. `AUTOROUTER_OLLAMA_KEEP_ALIVE` defaults to `5m`. After five idle minutes, the next request may need to reload the model, exceed that deadline, and use the fallback. A longer positive keep-alive can reduce reloads while retaining memory longer; `0` unloads immediately and can make every evaluation cold. Supported values are `0` or a positive duration such as `30s`, `5m`, or `1h`, following the [Ollama API's keep-alive setting](https://docs.ollama.com/api/chat).

If startup priming fails, the launcher warns and continues. A missing model, unavailable service, malformed answer, or evaluation timeout falls back to Sonnet or retains an incoming Opus, subject to the usual compatibility policy. No Jev request is made. The status line identifies `Ollama fallback` and its error category. Run `doctor` to inspect local API availability and installed model metadata; it does not download or generate.

## Data flow and authentication

```text
Claude Code → authenticated local gateway → Jev or local Ollama classification
                                        → routing policy and optional token check
                                        → selected Claude model → streamed response
```

AutoRouter uses Claude Code's [gateway integration](https://code.claude.com/docs/en/llm-gateway-protocol), so it sees inference requests and tool continuations. It does not rely on a user-prompt hook.

The selected evaluator receives a bounded state containing the latest human request and excerpts of the original task, system text, and recent messages: up to 12,000 serialized characters sent to TypeSafe for Jev, or 3,000 UTF-8 bytes sent to the local Ollama service. These excerpts can include private source code and tool results. Images, document payloads, and signed thinking are omitted. Full tool schemas and full conversation history are not sent to either classifier. Anthropic receives the complete request, including its tools and attachments. Large or multimodal requests may also go to Anthropic's token-count endpoint before inference, including when classification is local.

In subscription mode, Claude Code owns login and OAuth refresh. AutoRouter forwards the current request's authorization and beta headers to Anthropic. It does not read keychain or saved login files, persist subscription tokens, or send them to Jev. A separate temporary `X-Autorouter-Token` authenticates the local connection and is stripped upstream. Subscription forwarding is restricted to `https://api.anthropic.com`. See [subscriptions and gateways](https://code.claude.com/docs/en/llm-gateway#subscriptions-and-gateways).

In API-key mode, the upstream key stays in the proxy and Claude receives a temporary local credential. Requests are billed to the supplied API key. Subscription requests remain subject to the subscription's model access and usage limits. AutoRouter never falls back from subscription authentication to API billing.

Routine logs contain route, model, timing, usage, and error-category metadata, not prompts, raw responses, or credentials. Status snapshots contain routing metadata and token counts in a private temporary directory and are deleted on normal launcher exit. Classification, turn, and token-count caches are held in memory. Claude Code and the external providers have their own storage and logging behavior.

## Routing policy

Each `/v1/messages` request is evaluated. Exact repeated bodies reuse a classification for five minutes. Both evaluators use a starting rubric choosing Haiku for routine work, Sonnet for ordinary engineering, and Opus for demanding reasoning. These choices require evaluation on your tasks; they are not quality guarantees.

The evaluator prioritizes the actual human request before startup metadata. Complete Claude reminder and tool-list blocks are excluded from that task excerpt, and long text retains its beginning and end. The outbound Anthropic request remains complete. Complexity outside the bounded excerpt can still be missed.

The following policy applies after classification:

- Jev's 1,500 ms deadline covers the response body and has no retry. Successful calls return immediately. Timeouts, HTTP errors, and invalid responses fall back to Sonnet or retain an existing stronger model.
- Jev confidence below 0.75 prevents a downgrade below Sonnet or the requested tier. Ollama returns a tier without calibrated confidence; its failure handling and compatibility guards still apply.
- Tool continuations retain the model chosen at the start of the human turn. Session, agent, and prompt headers identify turns; normalized conversation content provides a fallback. Moving prompt-cache markers does not create a new turn.
- Thinking history, fixed-budget thinking, server tools, context management, and other recognized model-specific features preserve the current model. Adaptive thinking, effort, and output above 64K prevent a Haiku choice. Fields are never stripped to force a downgrade.
- Mid-conversation `system` messages preserve the requested model and pass through unchanged. They do not count as a tool continuation by themselves.
- Recognized compaction and auxiliary requests otherwise preserve their requested model. Token counting and model discovery pass through without classification.

The default `compatible` profile starts Claude with Haiku-compatible requests and client-requested thinking disabled. When an upgrade to Opus 5/5.5 requires adaptive thinking, AutoRouter enables it. `AUTOROUTER_CLIENT_PROFILE=native` preserves normal client settings, which can constrain routing. An explicit Claude `--model` argument overrides the starting model, but `/model` and `--model` are requested models, not locks on the routed result.

The launcher enables `ENABLE_TOOL_SEARCH=true` when unset. Claude can otherwise disable on-demand MCP discovery when using a custom API address, loading connected-tool schemas into even a fresh conversation. Explicit values, including `false` or `auto:5`, are preserved. Managed settings and always-loaded tools can still affect deferral. See [Claude Code tool search](https://code.claude.com/docs/en/mcp#configure-tool-search).

### Context capacity

Context-relevant JSON above 150KB, or image/document blocks including those inside tool results, trigger a token check for otherwise compatible small models. This byte threshold is a trigger, not a token estimate. The check includes system instructions and active tool schemas. Unused deferred schemas are excluded from the trigger; discovered references and historical tool calls add them back. Unknown shapes are counted conservatively.

The check uses [Anthropic's token-count endpoint](https://platform.claude.com/docs/en/build-with-claude/token-counting), the current request's authentication, and the target model's tokenizer. It runs alongside the selected evaluator with a separate 1,500 ms deadline. Counts are cached for five minutes with at most 100 hash-and-number entries. Input at or below 190K can remain on a 200K model, allowing a margin for estimation differences. Unsupported input, timeouts, and failures fall back to the conservative capacity guard.

If the small model cannot fit, the default mapping upgrades it to Sonnet 5's native 1M window. A known capable Opus can be used when a configured Sonnet lacks that capacity. Compatible Haiku tool turns can upgrade as they grow; subsequent calls retain the upgrade. Thinking history and feature constraints remain pinned. Unknown capacities are not guessed. See [Anthropic context windows](https://platform.claude.com/docs/en/build-with-claude/context-windows).

Requests exceeding the selected model's capacity still receive the provider's error. AutoRouter does not truncate context or retry a generation on another model after an error or partial stream.

## Status line and savings

The `claude` launcher automatically adds a temporary [status-line command](https://code.claude.com/docs/en/statusline). Its fields show:

| Field | Meaning |
| --- | --- |
| `Sonnet 5 selected` | Routing chose this model; Anthropic has not confirmed it yet |
| `Opus 5.5` / `last Opus 5.5` | Provider-confirmed streaming or most recent model |
| `Jev` / `Ollama`, with `cache` or `fallback` when applicable | Classification source; timing includes concurrent context checks |
| `Jev→Haiku` or `Ollama→Haiku` beside Sonnet | A policy guard overrode the evaluator's Haiku choice |
| `large context` | Token count exceeded the small-model input budget |
| `size unverified` | Token checking failed or was unavailable; conservative guard applied |
| `API ctx` | Latest main request's input divided by the actual model's known window |
| `CLI ctx` | Claude's client accounting, shown when its window differs or API capacity is unknown |
| `est saved … vs Opus` | Cumulative API-equivalent token-cost estimate |

Background agents and auxiliary requests cannot replace the foreground model. Errors, fallback, cancellation, and stale/offline state remain visible. The command reads a local snapshot and makes no network requests. It respects terminal width and `NO_COLOR`; lower-priority fields disappear on narrow terminals.

Context includes uncached input, cache reads, and cache writes, excluding output to match [Claude's percentage formula](https://code.claude.com/docs/en/statusline#context-window-fields). It is current context rather than cumulative usage. Historical usage is marked `last`; compaction resets stale readings. The compatible client's 200K window can reach 100% while a routed Sonnet request uses only part of its 1M window. Displaying both does not change Claude's compaction threshold.

Savings compare the actual models' API token prices with the configured Opus model's prices for the **same reported counts and cache profile**. The percentage is `(Opus cost − routed cost) / Opus cost`. Input, output, cache reads, and 5-minute/1-hour cache writes are priced separately using the bundled table based on [Anthropic's published USD pricing](https://platform.claude.com/docs/en/about-claude/pricing).

Totals include completed main, agent, and auxiliary calls for the current session observed by this router process. Streaming usage is counted once, and totals reset when the router or session restarts. Unrecognized prices or unsupported usage produce `partial` or `savings unavailable`. Higher routed costs show `est extra`. The arithmetic runs locally.

This estimate does not measure subscription bill savings or quota credits. It excludes Jev charges, local compute costs, tool fees, negotiated discounts, and unpriced requests. A real Opus run can produce different tokens and cache hits. Incomplete streams, unknown cache-write TTLs, unsupported pricing modifiers, and unrecognized model versions are excluded rather than guessed. The rate table requires updates when prices change.

Set `AUTOROUTER_STATUSLINE=0` to retain an existing status line. Other `--settings` values are retained in the temporary overlay; source-relative Read/Edit rules keep their anchors. Ambiguous relative sandbox paths cause the launcher to skip the overlay and pass original settings through with a notice. Safe mode disables custom status lines; print mode has no status-line UI. Standalone `serve` does not install one.

## Troubleshooting

Run `claude-autorouter doctor` first. It performs local checks without Claude generations or Jev calls, including local HTTP checks for the selected Ollama model. It cannot establish Anthropic/TypeSafe availability, current quota, or whether a key will be accepted remotely.

For metadata logs without terminal noise:

```sh
AUTOROUTER_DEBUG=1 claude-autorouter claude 2>autorouter-debug.log
```

The launcher is quiet by default. Standalone `serve` logs to stderr by default. Claude still displays API errors normally.

**Authentication conflicts:** subscription launches remove inherited `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, and `CLAUDE_CODE_OAUTH_TOKEN` from the child environment, plus authentication-related custom headers. Claude settings can still supply conflicting `apiKeyHelper`, `env`, base-URL, or custom-header overrides. Remove those conflicts from the relevant settings, run `claude auth login` if needed, and relaunch. Parent-shell and saved Claude settings remain unchanged. `--bare` is incompatible with subscription OAuth.

**A simple prompt selects Sonnet:** inspect the reason and evaluator choice in the status line. Background context can be large even in a new session. Model-specific settings, tool continuity, Jev confidence, classifier failures, and context limits can override the classified tier. A fresh conversation picks up tool-loading changes; resumed history can retain already-loaded schemas.

**Claude says Haiku while AutoRouter says Opus:** the built-in model label is Claude's starting/requested model. The AutoRouter confirmed-model label comes from Anthropic. Claude's own token-cost estimate can likewise be attributed to the requested model.

**After restarting mid-conversation:** turn state is in memory and expires after 30 minutes. Unknown continuations preserve the incoming model. Start a fresh conversation when restarting around signed thinking; AutoRouter cannot reconstruct the prior actual model from lost turn state.

Switching models can lose prompt-cache reuse. A cheaper price per token does not guarantee a cheaper or faster task. Only requests using Claude's configured base URL are visible to this proxy. Alternate provider modes such as Bedrock, Vertex, Foundry, Mantle, and `ANTHROPIC_AWS` are unsupported; unset their enable flags before launching. Use ordinary `claude` to bypass routing.

## Standalone server

The launcher handles local credentials and environment settings automatically. For separate clients, configure `AUTOROUTER_TOKEN` with a random value of at least 16 characters and start:

```sh
claude-autorouter serve
```

For subscription mode, configure the client's environment in another terminal:

```sh
unset ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN CLAUDE_CODE_OAUTH_TOKEN
export ANTHROPIC_BASE_URL=http://127.0.0.1:8787
export ANTHROPIC_CUSTOM_HEADERS='X-Autorouter-Token: YOUR_LOCAL_ROUTER_TOKEN'
export CLAUDE_CODE_GATEWAY_HINT_HEADERS=1
export ANTHROPIC_MODEL=claude-haiku-4-5-20251001
export MAX_THINKING_TOKENS=0
export ENABLE_TOOL_SEARCH=true
claude
```

Replace the placeholder with the server's local token, never a subscription credential. Include other custom headers on separate lines if needed. Omit model/thinking variables to retain native client settings.

For API-key mode, set the base URL and gateway-hint variable above, and set both client `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN` to the local router token. Keep the real upstream API key in the server environment/config.

The server binds only to `127.0.0.1` and requires local authentication. `/health` checks the local process, not upstream provider availability.
