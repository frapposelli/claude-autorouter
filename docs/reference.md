# Reference

AutoRouter is an independent gateway for Claude Code. Its existing `claude-autorouter` package, command, configuration paths, and repository identity remain unchanged. See [integration boundaries and provider policy](subscription-integration.md) for authentication ownership and the distinction between technical operation and provider authorization.

## Commands

| Command | Purpose |
| --- | --- |
| `claude-autorouter setup` | Save subscription-mode configuration and a Jev key |
| `claude-autorouter setup --auth-mode api-key` | Configure Jev and Anthropic API-key billing |
| `claude-autorouter setup --client-profile auto` | Save a Sonnet/Opus profile compatible with Claude's Auto permission mode |
| `claude-autorouter setup --stop-hook-block-cap 2` | Opt into a shorter native Stop-hook continuation cap during setup |
| `claude-autorouter setup --session-log-dir DIR` | Save an opt-in directory for per-session JSONL decisions and outcomes |
| `claude-autorouter setup --evaluator ollama --pull` | Configure the native local evaluator and download its selected model if missing |
| `claude-autorouter setup --evaluator ollama --ollama-timeout-ms 0 --force` | Save a disabled runtime evaluator deadline |
| `claude-autorouter setup --force` | Update an existing config while preserving unrelated saved settings |
| `claude-autorouter setup --replace` | Explicitly replace the saved configuration |
| `claude-autorouter config show --json` | Inspect effective settings and default/file/environment provenance; secrets are hidden |
| `claude-autorouter config set KEY VALUE` | Change one nonsecret saved setting |
| `claude-autorouter config unset KEY` | Remove one saved override |
| `claude-autorouter sessions list [--json]` | Inspect optional saved local history |
| `claude-autorouter sessions show ID [--json]` | Show correlated decisions, outcomes and pricing coverage |
| `claude-autorouter doctor --evaluate-local` | Run synthetic classifier checks on the installed local Ollama model |
| `claude-autorouter doctor` | Check config, Claude executable/login, and the selected local Ollama model without paid calls |
| `claude-autorouter claude [arguments]` | Start a local router and pass arguments through to Claude Code |
| `claude-autorouter serve` | Run the router for separately configured clients |
| `claude-autorouter --help` | Show command help |
| `claude-autorouter --version` | Print the package version |

The launcher binds an ephemeral port on `127.0.0.1`, creates a temporary local credential, starts Claude with the gateway address, and shuts down when Claude exits. It works from any project directory on macOS or Linux, including WSL. Native Windows is unsupported in this release; the status-line command uses a POSIX shell. Standalone `serve` uses the configured port and requires a local token.

## Configuration

First setup defaults to subscription mode unless `--auth-mode` or `AUTOROUTER_AUTH_MODE` selects another mode. Local Ollama is the default evaluator (changed from Jev in 0.5.0; configurations created by `setup` always record their evaluator, so existing ones are unchanged, but an environment-only launch with no `AUTOROUTER_EVALUATOR` now selects Ollama); `--evaluator jev` selects TypeSafe's hosted evaluator. An existing configuration updated with `--force` keeps its saved choices unless a command-line flag changes them; unrelated environment overrides remain temporary. Setup prompts for required secrets without echoing them and writes a private JSON file. On macOS, new and `--replace` setups keep keys in the login Keychain by default (see [credential storage](#credential-storage)); elsewhere, and in existing configurations, stored keys are plaintext in that file, so keep it private and out of source control. Supply keys through the environment when interactive input is unavailable. Subscription mode with Ollama requires no API keys. API-key authentication always requires `ANTHROPIC_API_KEY`, regardless of evaluator.

The config path is selected in this order:

1. `AUTOROUTER_CONFIG`, when set.
2. `$XDG_CONFIG_HOME/claude-autorouter/config.json`, when `XDG_CONFIG_HOME` is a nonempty absolute path.
3. `~/.config/claude-autorouter/config.json`.

The JSON file uses flat environment-style string keys, such as `AUTOROUTER_AUTH_MODE` and `TYPESAFE_API_KEY`. Environment values take precedence over the saved config. Use `config set` or `config unset` to edit a single saved setting, or `setup --force` to update an existing configuration while retaining unrelated settings. `setup --replace` explicitly rebuilds a readable supported saved configuration; malformed or unsupported files are left intact for manual repair. The launcher does not discover or load a project's `.env` file. From a source checkout, explicitly loading one still works:

```sh
node --env-file=.env bin/autorouter.mjs claude
```

### Credential storage

`AUTOROUTER_SECRET_STORE` selects where saved `ANTHROPIC_API_KEY`, `TYPESAFE_API_KEY` and `AUTOROUTER_TOKEN` values live. `file` stores them in the private JSON file; it is used on Linux and by configurations created before this option existed. `keychain`, available on macOS and chosen by default there for new setups, stores each one as a generic password in the login Keychain (service `claude-autorouter`, scoped to the configuration file path); the JSON file then contains only settings. Changing the setting moves saved keys:

```sh
claude-autorouter config set AUTOROUTER_SECRET_STORE keychain   # file → Keychain
claude-autorouter config set AUTOROUTER_SECRET_STORE file       # Keychain → file
```

If the Keychain cannot be used during a default setup (locked or headless), setup says so and saves keys to the file instead; an explicit `--secret-store keychain` fails rather than falling back. An existing plaintext configuration is never moved implicitly: `setup --force` and `doctor` flag plaintext keys on macOS and print the command above. Keys are written to the Keychain before the file is rewritten, so an interrupted move leaves a copy in both places rather than neither. Items are removed only after the file is saved. Values pass to the system `security` tool on standard input, never as process arguments, and each write is read back to confirm it. Keychain values must be printable single-line ASCII. Environment variables still take precedence: when the Keychain is locked (for example over SSH), a key supplied in the environment is used instead, but moving keys between stores waits until every saved key can be read. `sessions` never reads the Keychain.

For an environment-only subscription launch, set `AUTOROUTER_AUTH_MODE=subscription` and either supply `TYPESAFE_API_KEY` or select `AUTOROUTER_EVALUATOR=ollama` with a running local model. For API-key mode, also supply `ANTHROPIC_API_KEY`. The shell variables are read by the router; Jev's key is removed from the Claude child environment.

| Variable | Default | Purpose |
| --- | --- | --- |
| `TYPESAFE_API_KEY` | required for Jev | Jev credential; unused by Ollama |
| `ANTHROPIC_API_KEY` | required in API-key mode | Upstream Anthropic credential |
| `AUTOROUTER_CONFIG` | see path order above | Explicit user config path |
| `AUTOROUTER_AUTH_MODE` | `api-key` without saved config; setup selects `subscription` | Authentication mode |
| `AUTOROUTER_EVALUATOR` | `ollama` | local `ollama` (default) or hosted `jev` classification |
| `AUTOROUTER_SECRET_STORE` | `keychain` for new macOS setups; otherwise `file` | Saved-config setting: `keychain` keeps saved keys in the macOS login Keychain; the environment cannot redirect it |
| `AUTOROUTER_CLIENT_PROFILE` | `compatible` | `native` retains client model/thinking settings; `auto` starts with Sonnet when no explicit model is set and excludes Haiku from task routing |
| `AUTOROUTER_STATUSLINE` | enabled | `0` retains your existing status line |
| `AUTOROUTER_DEBUG` | off | `1` enables launcher metadata logs on stderr |
| `AUTOROUTER_SESSION_LOG_DIR` | off | Write per-session JSONL decisions and outcomes into this directory; unset or empty disables it |
| `AUTOROUTER_SESSION_LOG_MODE` | `metadata` | `metadata` omits prompt excerpts; `prompts` includes bounded human-task excerpts; setting a mode alone does not enable logging |
| `CLAUDE_CODE_STOP_HOOK_BLOCK_CAP` | unset; Claude currently uses `8` | Optional cap on consecutive Stop/SubagentStop continuations without tool use; `0` disables the cap |
| `ENABLE_TOOL_SEARCH` | `true` in launcher when unset | Load MCP tool definitions on demand; explicit values are preserved |
| `AUTOROUTER_HAIKU_MODEL` | `claude-haiku-4-5-20251001` | Routine tier |
| `AUTOROUTER_SONNET_MODEL` | `claude-sonnet-5`; `claude-sonnet-5-5` in the `auto` profile | Standard tier |
| `AUTOROUTER_OPUS_MODEL` | `claude-opus-5-5` | Demanding tier and savings baseline |
| `AUTOROUTER_JEV_MODEL` | `jev-latest` | Classifier version |
| `AUTOROUTER_JEV_TIMEOUT_MS` | `1500` | Classifier deadline in milliseconds |
| `AUTOROUTER_OLLAMA_URL` | `http://127.0.0.1:11434` | Loopback Ollama base URL |
| `AUTOROUTER_OLLAMA_MODEL` | `nimble:9b-q4_K_M` | Installed local model tag or alias compatible with `/v1/systemone` |
| `AUTOROUTER_OLLAMA_TIMEOUT_MS` | model-dependent; see below | Runtime local classification deadline, `1`–`30000` ms; `0` disables it |
| `AUTOROUTER_OLLAMA_KEEP_ALIVE` | `5m` | How long Ollama retains the evaluator in memory |
| `AUTOROUTER_TOKEN_COUNT_TIMEOUT_MS` | `1500` | Context-check deadline; runs alongside classification |
| `AUTOROUTER_MIN_CONFIDENCE` | `0.75` | Jev confidence threshold; does not apply to Ollama |
| `AUTOROUTER_PORT` | `8787` | Standalone server port |
| `AUTOROUTER_TOKEN` | none | Standalone local credential, at least 16 characters |
| `AUTOROUTER_UPSTREAM_URL` | `https://api.anthropic.com` | Anthropic-compatible origin; fixed in subscription mode |
| `AUTOROUTER_JEV_URL` | `https://api.typesafe.ai/v1/systemone` | Jev endpoint |

Model access depends on your account. The policy recognizes specific Claude model versions; arbitrary gateway aliases do not automatically inherit their capabilities or context windows. Compare overrides with the [Anthropic model catalog](https://platform.claude.com/docs/en/models/overview).

### Inspect and change settings

```sh
claude-autorouter config show
claude-autorouter config show --json --check-all
claude-autorouter config set AUTOROUTER_OLLAMA_TIMEOUT_MS 0
claude-autorouter config unset AUTOROUTER_OLLAMA_TIMEOUT_MS
claude-autorouter config set TYPESAFE_API_KEY
```

`show` reports whether each setting comes from a default, the saved file, or an environment override. Secret values are never displayed. The last command uses a hidden prompt; scripts can pipe a secret to `config set TYPESAFE_API_KEY --stdin`. Secret values are not accepted as command arguments. Edits validate and atomically update only the named saved setting. Environment overrides still apply after a saved change. An unset saved deadline returns to the model default unless an environment value overrides it.

Normal startup validates the selected evaluator; stale settings for the inactive evaluator do not prevent it from starting. `show --check-all` explicitly checks both. Blank numeric settings fail with their setting name; zero retains its documented meaning. `claude-autorouter help COMMAND` gives focused command help. `claude-autorouter claude --help` and `--version` call Claude directly without router setup or credentials.

### Organization policy

An administrator can restrict what users may configure with a policy file at a fixed system path: `/Library/Application Support/claude-autorouter/policy.json` on macOS and `/etc/claude-autorouter/policy.json` on Linux. No environment variable changes this path. The file and its directory must be regular, owned by root, and not writable by group or others; otherwise AutoRouter refuses to start. An invalid or unreadable file also stops it, so a broken policy never silently turns off.

```json
{
  "allowed_evaluators": ["ollama"],
  "allowed_auth_modes": ["subscription"],
  "session_log_mode": "metadata",
  "upstream_url": "https://api.anthropic.com",
  "jev_url": "https://api.typesafe.ai/v1/systemone"
}
```

All keys are optional. `allowed_evaluators` and `allowed_auth_modes` reject any other choice, including an unset default, with an error that names the setting. `session_log_mode`, `upstream_url` and `jev_url` replace whatever the saved file or environment supplies; `config show` reports them with source `policy`. `doctor` prints the policy path and the locked settings. `setup` and `config set` refuse to save a disallowed value, but still let a user correct a setting that the policy now forbids.

The policy guards against configuration drift and environment-driven changes such as direnv, devcontainer or CI variables. It does not stop someone who can run modified code, or run Claude Code without AutoRouter. Deploy it with device management, and use it with an allowlist of approved package versions.

## Ollama evaluator

The local configuration documented here requires AutoRouter 0.3.2 or newer and remains experimental. It uses Ollama's native `/v1/systemone` decision endpoint for every model, replacing the chat backend from 0.2.0. Jev is the optional hosted evaluator, using TypeSafe's `/v1/systemone` endpoint and a TypeSafe API key. Selecting Ollama never silently switches back to Jev. Haiku, Sonnet, or Opus still completes the task through Anthropic.

Version 0.3.2 excludes Claude's executor system instructions from local excerpts, uses model-specific runtime deadlines, and accepts `0` to disable that deadline. Setup, doctor, and startup show the effective model and deadline; setup accepts `--ollama-timeout-ms`. Jev is unchanged.

All local models require Ollama 0.35 or newer. Version 0.35.0 is a prerelease as of September 29, 2026; it introduces the native decision API. See the [Ollama release notes](https://github.com/ollama/ollama/releases/tag/v0.35.0). Install and start a compatible local service, then run:

```sh
claude-autorouter setup --evaluator ollama --pull --force
claude-autorouter doctor
claude-autorouter claude
```

`--force` updates an existing user config and preserves its other settings and selected model unless explicitly changed. Setup detects the running local API. `--pull` authorizes downloading the chosen model when it is missing; without it, install the model yourself before setup. AutoRouter does not install Ollama, start its daemon, delete models, or download models during ordinary launches or `doctor` checks.

### Local model selection

The default is `nimble:9b-q4_K_M`. Other tags can be selected with `--ollama-model LOCAL_TAG_OR_ALIAS` or `AUTOROUTER_OLLAMA_MODEL`. Every selected model must support `/v1/systemone`; a model name or alias does not change the endpoint. There are no model presets or automatic choices based on system RAM.

| Explicit tag | Parameters / quantization | Approximate download | Model details and terms |
| --- | --- | ---: | --- |
| `nimble:9b-q4_K_M` | 9B / Q4_K_M | 5.63 GB | [Nimble](https://ollama.com/library/nimble); local default |
| `tev1:0.8b` | 0.8B / Q8 | 812 MB | [Tev1](https://ollama.com/library/tev1) |
| `tev1:4b-q4_K_M` | 4B / Q4_K_M | 2.7 GB | [Tev1](https://ollama.com/library/tev1) |

To select Tev1, run one of these setup commands, then run `doctor` and `claude` as above:

```sh
# Tev1 0.8B Q8
claude-autorouter setup --evaluator ollama --ollama-model tev1:0.8b --pull --force
```

```sh
# Tev1 4B Q4_K_M, with a 15-second default deadline
claude-autorouter setup --evaluator ollama --ollama-model tev1:4b-q4_K_M --pull --force
```

For Nimble, the explicit Q4_K_M tag avoids `nimble:latest`, which currently selects an approximately 9.5 GB Q8 model. For Tev1, `tev1:latest` and `tev1:4b` select approximately 4.5 GB Q8 weights; the explicit `tev1:4b-q4_K_M` tag selects the smaller 4B download. Download size is not resident memory: runtime and context allocations add to it, and other applications need memory too. Downloaded models have their own licenses and are not bundled in this package. In historical tests before 0.3.2 on a 16 GiB M4, Tev1 0.8B matched 18/24 held-out labels at 450 ms median latency within 1,500 ms; 4B matched 22/24 at 3.15 seconds with a separate 10-second deadline. See the [local measurements](ollama-evaluation.md) before choosing a latency deadline.

The endpoint must be loopback (`127.0.0.1`, `localhost`, or `::1`); `localhost` is converted to `127.0.0.1` so the connection does not depend on name resolution, without a path, credentials, query, or fragment. Cloud model tags and metadata identifying a remote model are rejected before sending task text. Claude and Jev credentials are never attached to Ollama requests.

### Classification and fallback

Local classification caps serialized evaluator state at both 3,000 characters and 3,000 UTF-8 bytes, including for non-ASCII prompts. Claude's top-level executor system instructions are excluded before budgeting; the current task, original task, and recent conversation excerpts remain. `/v1/systemone` receives the bounded state and routing criteria and returns a tier directly. The router retains each model's native context setting: 8,194 tokens for the default Nimble tag and 2,050 for the listed Tev1 tags. Tev1's smaller window includes the routing criteria and template as well as the excerpt; the byte limit does not guarantee every possible input fits. Context errors use the normal fallback. Returned confidence scores summarize choice-distribution entropy; they are not calibrated accuracy probabilities. `AUTOROUTER_MIN_CONFIDENCE` applies only to Jev. All capability, tool-continuation, thinking, and context guards still apply.

The deadline covering local checks and classification defaults to 1,500 ms for Tev1 0.8B and custom/unrecognized tags, 15,000 ms for official Tev1 4B variants (including bare `tev1` and `latest`), and 30,000 ms for official Nimble variants. Official `library/` and `registry.ollama.ai/` aliases are recognized; a custom namespace such as `team/nimble` keeps the short default. An explicit timeout overrides the model default, including an old saved `1500`. Environment values override saved values on launch. Defaults are not written into the user config. To update a saved deadline, use `config set AUTOROUTER_OLLAMA_TIMEOUT_MS N` or `setup --ollama-timeout-ms N --force`; unrelated environment overrides remain temporary. Environment-provided Ollama settings are saved during first setup, replacement, or explicit `--evaluator ollama` selection. The command-line timeout flag takes precedence over the environment.

Set `AUTOROUTER_OLLAMA_TIMEOUT_MS=0` to remove AutoRouter's runtime evaluator timer while keeping the existing configuration:

```sh
AUTOROUTER_OLLAMA_TIMEOUT_MS=0 claude-autorouter claude
```

To persist it for an already installed Tev1 4B model, run:

```sh
claude-autorouter setup --evaluator ollama --ollama-model tev1:4b --ollama-timeout-ms 0 --force
```

Only the runtime evaluator deadline is disabled. User cancellation and client disconnection still abort evaluation; ordinary service, HTTP, and response errors still use fallback. Startup priming retains its separate 60-second limit, and lifecycle checks retain their own limits. Positive values from `1` to `30000` keep a finite deadline: for example, `--ollama-timeout-ms 2500` permits 2.5 seconds and can still time out on decisions near that cutoff.

Before opening Claude's UI, the launcher loads an installed model and primes the classifier rubric with a synthetic task, using a separate deadline of up to 60 seconds. Successful priming does not establish that real excerpts finish within an enabled runtime deadline or classify correctly. `doctor` checks version and model availability without inference; it does not certify speed or accuracy either. `AUTOROUTER_OLLAMA_KEEP_ALIVE` defaults to `5m`. After five idle minutes, the next request may need to reload the model and, when a runtime deadline is enabled, exceed it and use the fallback. A longer positive keep-alive reduces some reloads while retaining memory longer; keep-alive `0` unloads immediately and can make every evaluation cold. Supported keep-alive values are `0` or a positive duration such as `30s`, `5m`, or `1h`.

If startup priming fails, the launcher warns and continues. An incompatible model or Ollama version, missing model, unavailable service, malformed answer, or evaluation timeout falls back to Sonnet or retains an incoming Opus, subject to the usual compatibility policy. No Jev request is made. `Ollama fallback` with `timeout` means no valid classification completed in time; it is not a Sonnet prediction. In metadata, a valid Sonnet decision has `source: "ollama"` and `classified_tier: "sonnet"`; a timeout has `source: "fallback"` and `classifier_error: "timeout"`. Later policy guards can still change the selected Claude model. Use the source-only [local routing regression](development.md#local-routing-regression) to test classification and all three selected tiers without external provider calls.

Historical measurements before 0.3.2: Tev1 4B timed out on all eight full-excerpt checks even with a 10-second diagnostic allowance; its short-task results did not establish a full-excerpt latency bound. Tev1 0.8B completed all eight within 1,500 ms. On the tested 16 GiB M4, Nimble timed out on all 12 tuning requests at 1,500 ms. A separate 30-second diagnostic completed 24 held-out classifications with 23 matching labels, but median routing took 11.4 seconds. The one error followed a misleading tier instruction. These historical results precede the 0.3.2 excerpt changes and do not establish guarantees for the longer defaults. See the [measurements and limitations](ollama-evaluation.md).

### Test the installed local evaluator

```sh
claude-autorouter doctor --evaluate-local
claude-autorouter doctor --evaluate-local --json
```

This explicit diagnostic requires an installed local evaluator selected with `AUTOROUTER_EVALUATOR=ollama`. It uses synthetic tasks only, needs no Jev or Anthropic key, and makes no Claude inference calls. It checks local availability, measures a separate initial preparation call, then runs six uncached cases through the production classifier using the configured runtime deadline. A disabled runtime deadline remains disabled; Ctrl-C cancels the diagnostic.

The report separates availability, expected-label agreement, all-three-tier coverage, and latency. Residency is observed before calls, so it does not claim a controlled cold/warm benchmark. A pass establishes these six examples only. Incorrect predictions, fallback, or missing Haiku/Opus coverage fail even if Ollama answered successfully. Auto mode still checks all three raw evaluator labels; actual Auto routing applies its Sonnet floor separately.

The diagnostic does not download, unload, restart, or edit configuration. It refuses to run while unrelated models are resident and requires a positive keep-alive; normal routing still supports keep-alive `0`. Ordinary `doctor` remains a metadata check. A missing-model repair command uses the exact configured tag and preserves other settings.

### Migrating an older Ollama config

Version 0.3.1 used a 1,500 ms deadline for every local model. After upgrading to 0.3.2, an explicitly saved or exported `AUTOROUTER_OLLAMA_TIMEOUT_MS=1500` still wins over the new model-specific defaults. Remove that override to use the defaults, or rerun setup with the desired model and `--ollama-timeout-ms N --force`. The `0` value and setup timeout flag require 0.3.2 or newer.

Version 0.3.1 removed the Qwen chat backend and presets from 0.2.0. Existing downloaded models remain on disk, but an old Qwen model selection needs to be replaced with a native decision model. Run `claude-autorouter setup --evaluator ollama --ollama-model nimble:9b-q4_K_M --force`, or explicitly choose a Tev1 tag; merging with `--force` alone preserves the saved model. Remove or update any old `AUTOROUTER_OLLAMA_MODEL` environment value too, because environment variables override saved configuration. Update scripts to use `--ollama-model` when selecting a custom model.

## Data flow and authentication

```text
Claude Code → authenticated local gateway → Jev or local Ollama classification
                                        → routing policy and optional token check
                                        → selected Claude model → streamed response
```

AutoRouter launches the user's installed official Claude Code binary without patching it and uses Claude Code's [gateway integration](https://code.claude.com/docs/en/llm-gateway-protocol), so it sees inference requests and tool continuations. It does not rely on a user-prompt hook. Each user uses their own provider credentials; AutoRouter does not provide a Claude sign-in service or a shared provider account.

The selected evaluator receives a bounded state containing the latest human request and excerpts of the original task and recent messages: up to 12,000 serialized characters sent to TypeSafe for Jev, or 3,000 UTF-8 bytes sent to the local Ollama service. Jev also receives system-text excerpts. The local path excludes Claude's top-level executor system instructions. These excerpts can include private source code and tool results. Before excerpting, recognizable sensitive values are replaced (the same filter applies to opt-in session-log prompt excerpts, including when old logs are read back) with markers such as `[REDACTED:secret]`: private keys, common provider token formats (Anthropic, OpenAI-style `sk-`, AWS, GitHub, GitLab, Slack, Google, Stripe, npm), JWTs, authorization headers, URL credentials, values assigned to password/secret/token/key-like names, email addresses, and checksum-valid IBANs and payment card numbers. Setting names stay visible. Redaction is pattern-based: unrecognized formats can remain, code resembling an assignment can be over-redacted, and it does not make arbitrary private source code safe to share. Images, document payloads, and signed thinking are omitted. Full tool schemas and full conversation history are not sent to either classifier. Anthropic receives the complete request, including its tools and attachments. Large or multimodal requests may also go to Anthropic's token-count endpoint before inference, including when classification is local.

In subscription mode, Claude Code owns login and OAuth refresh. AutoRouter forwards the current request's authorization and beta headers to Anthropic. It does not read keychain or saved login files, persist subscription tokens, or send them to Jev. A separate temporary `X-Autorouter-Token` authenticates the local connection and is stripped upstream. Subscription forwarding is restricted to `https://api.anthropic.com`. See [subscriptions and gateways](https://code.claude.com/docs/en/llm-gateway#subscriptions-and-gateways).

In API-key mode, the upstream key stays in the proxy and Claude receives a temporary local credential. Requests are billed to the supplied API key. Subscription requests remain subject to the subscription's model access and usage limits. AutoRouter never falls back from subscription authentication to API billing.

The proxy processes authenticated requests in memory, including their authorization headers. Preserving Claude's login flow does not by itself establish that every deployment is permitted. The [provider-policy note](subscription-integration.md#provider-guidance-and-unresolved-scope) records the current documentation and the unresolved scope of model-rewriting subscription forwarding. Jev requires its own TypeSafe credentials and billing, separate from Anthropic authentication.

Routine diagnostic logs contain route, model, timing, usage, and error-category metadata, not prompts, raw responses, or credentials. Opt-in session history is separate and includes task excerpts only in `prompts` mode; the default is `metadata` (changed from `prompts` after 0.5.0; set `AUTOROUTER_SESSION_LOG_MODE=prompts` to keep excerpts). Status snapshots contain routing metadata and token counts in a private temporary directory and are deleted on normal launcher exit. After a hard kill, the next launch removes directories whose process is gone (only your own, owner-only `autorouter-status-*` directories in the temporary directory). Classification, turn, and token-count caches are held in memory. Claude Code and the external providers have their own storage and logging behavior.

## Routing policy

Each eligible `/v1/messages` request is evaluated. Exact repeated bodies reuse a classification for five minutes; concurrent identical evaluations share one request. Cache identity includes evaluator configuration, rubric and requested model floor. Internal permission classifiers and other documented pass-through paths skip evaluation. Both evaluators use a starting rubric choosing Haiku for routine work, Sonnet for ordinary engineering, and Opus for demanding reasoning. These choices require evaluation on your tasks; they are not quality guarantees.

The evaluator prioritizes the actual human request before startup metadata. Complete Claude reminder and tool-list blocks are excluded from that task excerpt, and long text retains its beginning and end. The outbound Anthropic request remains complete. Complexity outside the bounded excerpt can still be missed.

The following policy applies after classification:

- Jev's 1,500 ms deadline covers the response body and has no retry. Successful calls return immediately. Timeouts, HTTP errors, and invalid responses fall back to Sonnet or retain an existing stronger model.
- Jev confidence below 0.75 prevents a downgrade below Sonnet or the requested tier. Ollama returns a tier without calibrated confidence; its failure handling and compatibility guards still apply.
- Tool continuations retain the execution model confirmed by a successfully forwarded response. Active tasks and pending tools survive classification-cache expiry; retired task records expire separately. After restart, missing continuity is explicitly unknown. A selected model alone remains unconfirmed. Session, agent, and prompt headers identify turns; normalized conversation content provides a fallback. Text feedback from a Stop hook also retains the model when it serves the same gateway prompt ID and the client has not changed its requested model, subject to capability and context checks. Moving prompt-cache markers does not create a new turn.
- Claude's local `/goal` command can omit the prompt-ID header. For that path, an exact feedback label matching a preceding expanded `/goal` command keeps the original task and conversation anchor. This narrow text fallback also recognizes Claude's repeated-goal truncation format; arbitrary hook text is not treated as a goal. Feedback remains in the evaluator's recent conversation and the full API request. A new human message becomes the current task normally. The status line shows `prompt pinned` or `goal pinned` when either text-continuation rule applies.
- Thinking history, fixed-budget thinking, server tools, context management, and other recognized model-specific features preserve the current model except for the verified shared capabilities of the modern Auto-mode Sonnet/Opus pair described below. Adaptive thinking, effort, and output above 64K prevent a Haiku choice. Fields are never stripped to force a downgrade.
- Mid-conversation `system` messages preserve the requested model unless both Auto-mode models support them; they always pass through unchanged. They do not count as a tool continuation by themselves.
- Auxiliary requests, including Claude's Auto permission classifier, pass through on their requested model without Jev/Ollama evaluation, token checks, or turn-state changes. Compaction retains its existing model and context-capacity policy. Recognized server-reviewed execution requests can route between compatible Sonnet/Opus models while retaining `safeguards` and all verdicts unchanged. Unknown safeguards contracts pass through. Token counting and model discovery pass through without classification.

The default `compatible` profile starts Claude with Haiku-compatible requests and client-requested thinking disabled. AutoRouter uses adaptive thinking when upgrading these requests to Opus 5/5.5. Starting with 0.3.3, routing to exact `claude-sonnet-5-5` translates disabled thinking to `between_tools`, which skips up-front thinking but permits progress updates between tool calls. At `xhigh`/`max` effort, or when per-message effort differs from the top-level setting (default `high`), it uses adaptive thinking while preserving the effort settings. Token counting uses the same adaptation. Sonnet 5 still accepts disabled thinking and is unchanged. See [Sonnet 5.5 thinking requirements](https://platform.claude.com/docs/en/models/sonnet-5-5/migration-guide).

Explicit native `between_tools` and unknown thinking modes retain the incoming model on new human turns outside Auto routing. In Auto routing, a known Sonnet 5.5 `between_tools` request can upgrade to Opus with adaptive thinking. Signed thinking blocks pass through unchanged and existing tool turns retain their model pin. `AUTOROUTER_CLIENT_PROFILE=native` preserves normal client settings, which can constrain routing. An explicit Claude `--model` argument overrides the starting model, but `/model` and `--model` are requested models, not locks on the routed result. Native same-model requests and unknown model aliases are not rewritten; clients must use settings supported by that model.

The launcher enables `ENABLE_TOOL_SEARCH=true` when unset. Claude can otherwise disable on-demand MCP discovery when using a custom API address, loading connected-tool schemas into even a fresh conversation. Explicit values, including `false` or `auto:5`, are preserved. Managed settings and always-loaded tools can still affect deferral. See [Claude Code tool search](https://code.claude.com/docs/en/mcp#configure-tool-search).

### Auto permission mode

The default `compatible` profile starts Claude as Haiku to permit three-tier routing. Claude's Auto permission mode does not support Haiku, even if AutoRouter routes an API request to Sonnet. Eligibility is based on Claude's selected client model. Gateways themselves are supported. See [Claude's Auto-mode requirements](https://code.claude.com/docs/en/permission-modes#eliminate-permission-prompts-with-auto-mode).

AutoRouter 0.3.6 introduced an `auto` client profile but bypassed evaluation for server-reviewed execution. Version 0.3.7 adds automatic switching on those requests. Launch with:

```sh
claude-autorouter claude --permission-mode auto
```

An explicit `--permission-mode auto` (or `--permission-mode=auto`) selects the profile for that launch, including when your saved profile is `native`. All arguments still go to Claude unchanged. For Auto chosen from Claude's UI or existing settings instead, use:

```sh
env AUTOROUTER_CLIENT_PROFILE=auto claude-autorouter claude
```

The profile defaults to Sonnet 5.5 and Opus 5.5, preserving explicit configured model IDs. The evaluator chooses Sonnet or Opus for each new human task; a routine Haiku verdict uses Sonnet and shows `Auto mode floor`. Tool and `/goal` continuations stay on the selected execution model. Claude's initial client model remains separate from the routed model. An explicit client `--model` or `ANTHROPIC_MODEL` can still make Auto unavailable if it selects Haiku or another unsupported model; choose a supported Sonnet or Opus instead.

Claude remains responsible for enabling the permission mode and enforcing organization settings, account availability, and tool rules. The profile does not enable Auto by itself or override `disableAutoMode`. AutoRouter does not reproduce Claude's settings precedence to infer a mode from settings files. `setup --client-profile auto --force` updates an existing configuration while retaining its other settings. `config set AUTOROUTER_CLIENT_PROFILE auto` changes just that setting.

**Safety review:** Claude's permission-classifier requests retain their exact requested model and skip AutoRouter's evaluator. Ordinary execution requests with the known `dangerous_tool_use` version-1 review contract are evaluated and routed, retaining the complete `safeguards` object, beta headers, and streamed safety verdicts. This also detects server review when Auto was selected in Claude's UI rather than through the launch flag. Unknown or malformed review contracts and safeguarded compaction pass through with `Auto safety`; a target that cannot accept the request shows `Auto model guard`. AutoRouter never turns off server review or converts denied actions to approvals. See [server-side classifier review](https://code.claude.com/docs/en/permission-modes#server-side-classifier-review).

**Shared execution capabilities:** automatic Auto routing supports exact Sonnet 5/5.5 and Opus 5/5.5 IDs. The default 5.5 pair shares a native 1M context window, adaptive thinking, native context-editing strategies, and mid-conversation system updates. These fields and existing signed thinking no longer pin every future human task. Sonnet 5 cannot accept mid-conversation system messages, per-message effort, or task budgets; use Sonnet 5.5 for those sessions. Unknown context-editing strategies, specialized server tools, fixed thinking budgets, fast mode, older/custom targets, and other incompatible requests still retain a compatible model. A Sonnet 5.5 `between_tools` request uses adaptive thinking when upgraded to Opus; effort and conversation history remain unchanged.

Thinking blocks stay verbatim in the conversation. Anthropic may drop blocks the selected model cannot read, so switching models does not preserve access to every model's private reasoning on every turn. User text, tool results, and prior answers remain available. Returning to a model can make its preserved thinking readable again. Model changes can also miss the prior model's prompt cache. See [preserved thinking and model switching](https://platform.claude.com/docs/en/build-with-claude/preserved-thinking).

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
| `Jev` / `Ollama`, with `cache` or `fallback` when applicable | Classification source; displayed routing time includes evaluator waiting and context checks |
| `Jev→Haiku` or `Ollama→Haiku` beside Sonnet | A policy guard overrode the evaluator's Haiku choice |
| `large context` | Token count exceeded the small-model input budget |
| `size unverified` | Token checking failed or was unavailable; conservative guard applied |
| `API ctx` | Latest main request's input divided by the actual model's known window |
| `CLI ctx` | Claude's client accounting, shown when its window differs or API capacity is unknown |
| `est saved … vs Opus` | Cumulative API-equivalent token-cost estimate |

Background agents and auxiliary requests cannot replace the foreground model. Errors, fallback, cancellation, incomplete-response evidence and stale/offline state remain visible. The command reads a local snapshot and makes no network requests. It respects terminal width and `NO_COLOR`; lower-priority fields disappear on narrow terminals.

Context includes uncached input, cache reads, and cache writes, excluding output to match [Claude's percentage formula](https://code.claude.com/docs/en/statusline#context-window-fields). It is current context rather than cumulative usage. Historical usage is marked `last`; compaction resets stale readings. The compatible client's 200K window can reach 100% while a routed Sonnet request uses only part of its 1M window. Displaying both does not change Claude's compaction threshold.

Savings compare the actual models' API token prices with the configured Opus model's prices for the **same reported counts and cache profile**. The percentage is `(Opus cost − routed cost) / Opus cost`. Input, output, cache reads, and 5-minute/1-hour cache writes are priced separately using the bundled table based on [Anthropic's published USD pricing](https://platform.claude.com/docs/en/about-claude/pricing).

Totals include completed main, agent, and auxiliary calls for the current session observed by this router process. Streaming usage is counted once, and totals reset when the router or session restarts. Unrecognized prices or unsupported usage produce `partial`, `unpriced N` or `savings unavailable`. Saved history identifies pricing table `2026-09-29.1` (reviewed September 29, 2026) and unpriced reason counts; unknown historical table versions are not repriced. Higher routed costs show `est extra`. The arithmetic runs locally.

This estimate does not measure subscription bill savings or quota credits. It excludes Jev charges, local compute costs, tool fees, negotiated discounts, and unpriced requests. A real Opus run can produce different tokens and cache hits. Incomplete streams, unknown cache-write TTLs, unsupported pricing modifiers, and unrecognized model versions are excluded rather than guessed. The rate table requires updates when prices change.

Historical integration observations cover Claude Code 2.1.284 and 2.1.285. The source-only versioned corpus in `test/fixtures/claude-protocol-v1.json` separates newly authored synthetic contracts from those dated observations and their artifact hashes. Gateway tests exercise Auto safeguards, thinking, deferred tools, compaction, goal scoping, fallback ownership and usage while preserving response bytes. They do not establish current-source live compatibility, evaluator accuracy or downstream task quality. Doctor reports the installed executable version separately; discovering a binary is not evidence that its protocol or Auto eligibility has been tested. Opt-in live validation supplements the synthetic fixtures.

Set `AUTOROUTER_STATUSLINE=0` to retain an existing status line. Other `--settings` values are retained in the temporary overlay; source-relative Read/Edit rules keep their anchors. Ambiguous relative sandbox paths cause the launcher to skip the overlay and pass original settings through with a notice. Safe mode disables custom status lines; print mode has no status-line UI. Standalone `serve` does not install one.

## Session decision logs

Logging is optional and disabled by default. Enable it for one launch:

```sh
env AUTOROUTER_SESSION_LOG_DIR="$HOME/.local/state/claude-autorouter/sessions" \
  claude-autorouter claude
```

Or save metadata-only history, without prompt excerpts:

```sh
claude-autorouter config set AUTOROUTER_SESSION_LOG_MODE metadata
claude-autorouter config set AUTOROUTER_SESSION_LOG_DIR "$HOME/.local/state/claude-autorouter/sessions"
claude-autorouter sessions list
claude-autorouter sessions show autorouter-session-EXAMPLE
claude-autorouter sessions show autorouter-session-EXAMPLE --json
```

Use the exact `id` printed by `sessions list`. Commands need no evaluator credentials and do not contact providers. Human summaries distinguish selected models, observed serving models, confirmed completions, failures, cancellations, and pending/unconfirmed requests. They report routing latency, fallback and override counts, and API-equivalent savings coverage. A selected model or an HTTP 200 alone does not prove successful inference. Old schema-1 decision logs remain readable and explicitly lack outcome evidence.

`prompts` mode (opt-in) records excerpts when a log directory is enabled. Main requests retain at most 500 Unicode characters of the human task; recognized tool and goal continuations retain the originating task. Auxiliary, subagent, compaction, workflow, and attachment-only requests have empty excerpts. Metadata mode omits the excerpt fields entirely. Neither mode logs authentication headers, provider replies, full transcripts, or tool payloads. Text entered directly in a prompt can appear in an enabled prompt excerpt.

The settings also work with `serve` and every client profile. `setup --session-log-dir DIR --session-log-mode metadata --force` updates an existing configuration. Setup resolves relative directories at setup time; environment-only paths resolve from the launch directory. `AUTOROUTER_SESSION_LOG_DIR=''` disables a saved directory for one launch. Setting only the mode never enables logging. `doctor` reports preferences without creating files.

Files are named `autorouter-session-*.jsonl`, one per observed Claude session in each router launch. Resuming a session in a new launch creates a new file. Requests without a session header share an anonymous file; subagents retain their agent IDs. Every line is a bounded schema-2 JSON record:

- `decision`: requested and selected models, evaluator verdict/source, policy reason, compatibility and continuity detail. `evaluation_latency_ms` measures evaluation/cache waiting; `routing_latency_ms` (also `decision_latency_ms`) includes compatibility and context checks.
- `outcome`: the same `request_id`, observed `confirmed_model` and bounded model transitions, safe error category, `completed`, `error` or `cancelled` status, and separate `completion_confirmed` evidence. It includes usage when available, `first_response_ms` from upstream forwarding to response headers, and total request latency. Early errors can have an outcome without a decision.

Outcomes record the configured Opus baseline and pricing-table version. History prices only successfully completed, supported usage with a recognized recorded table version and baseline. Unknown prices, missing or partial streams, ambiguous/mixed-model usage, and legacy records remain unpriced with a reason. Estimates never represent subscription charges. Status savings identify partial coverage; history exposes the version and counts.

New directories use `0700`, files use `0600`; existing directory permissions are unchanged. Asynchronous writes use a bounded 1 MiB queue and at most 128 session files per process. Normal shutdown drains accepted records. Storage failure or queue limits disable further logging with one generic warning while routing continues. Abrupt termination can lose unwritten records.

History reads at most 100 files, 4 MiB per file, 16 MiB total and 5,000 records per file. Limits, malformed records and partial tails are reported as partial coverage. Files are never automatically rotated or deleted; manage retention in your chosen directory. Log filenames are ignored by this repository and excluded from the npm package.

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

**`/goal` repeats a question or says it is blocked:** Claude Code runs its own completion checker after each turn, separately from AutoRouter's Jev/Ollama classifier. AutoRouter preserves the requested model for that auxiliary check and forwards its verdict unchanged. The check cannot authorize GitHub SAML, approve a tool, or resolve an external dependency. A tool's authentication error is also different from a Claude API authentication failure.

If the worker reports a blocker but the checker keeps returning “not yet met,” Claude can repeat its answer until its no-progress guard pauses the goal. Repeated tool calls can keep the loop running longer. Use `/goal clear` to end the loop, resolve the external blocker, and set the goal again. For tasks that may require human action, explicitly allow reporting a blocker as an alternative end condition, for example: `/goal Verify the discrepancy against upstream main and run the relevant tests, or report an external authorization blocker and stop.` This changes what counts as completion; AutoRouter does not declare blocked work successful or rewrite goal instructions. See [Claude Code goal evaluation](https://code.claude.com/docs/en/goal#how-evaluation-works).

### Shorter Stop-hook loops (opt-in)

To return control sooner when a goal keeps reporting the same unmet condition, set Claude's native continuation cap for one launch:

```sh
env CLAUDE_CODE_STOP_HOOK_BLOCK_CAP=2 claude-autorouter claude
```

This permits two consecutive continuations without tool use; the third blocking verdict ends the turn. The goal remains set and unmet, and a new message can resume it. Tool activity resets the counter, so this is not a total turn or request limit and cannot bound repeated failed tool calls. It applies to **all Stop and SubagentStop hooks**, including `/goal`. A smaller cap can pause useful work sooner. Unset preserves Claude's default (currently `8`); **`0` disables the guard**. AutoRouter does not install a Stop hook or change completion verdicts. See [Claude's environment-variable reference](https://code.claude.com/docs/en/env-vars) and [Stop-hook loop behavior](https://code.claude.com/docs/en/hooks#stop).

The environment-only command also works on AutoRouter 0.3.4. Saved configuration and the setup flag require AutoRouter 0.3.5 or newer. To save the preference, add the following property to your existing AutoRouter config JSON, preserving its other values:

```json
"CLAUDE_CODE_STOP_HOOK_BLOCK_CAP": "2"
```

For a new configuration, use `claude-autorouter setup --stop-hook-block-cap 2`; the flag works with either evaluator and overrides the environment during setup. Runtime environment values override saved configuration. To change only a saved cap, use `config set CLAUDE_CODE_STOP_HOOK_BLOCK_CAP 2` or `setup --stop-hook-block-cap 2 --force`; both preserve unrelated saved settings. `doctor` reports the cap when configured. AutoRouter accepts nonnegative safe integers and leaves the setting absent unless you opt in.

### Other session issues

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
