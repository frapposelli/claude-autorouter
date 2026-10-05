const commands = {
  setup: `Usage: claude-autorouter setup [options]

Configure Jev (default) or an installed local Ollama evaluator.
  --auth-mode subscription|api-key   Default: subscription
  --client-profile compatible|native|auto
  --evaluator jev|ollama
  --ollama-model TAG                 Select a /v1/systemone model
  --ollama-timeout-ms N              0 disables the routing deadline
  --pull                            Download the selected missing Ollama model
  --stop-hook-block-cap N            Optional Claude Stop-hook retry limit
  --session-log-dir DIR              Opt in to private logs with prompt excerpts
  --session-log-mode metadata|prompts Choose whether excerpts are included
  --force                           Update an existing configuration
  --replace                         Explicitly rebuild the saved configuration

First setup reads environment settings and keys, or prompts for missing keys.
--force retains saved defaults and applies explicit options; unrelated runtime
overrides stay temporary. Explicit evaluator/auth selection accepts its supplied key.
Example: claude-autorouter setup --evaluator ollama --ollama-model tev1:0.8b --pull`,
  doctor: `Usage: claude-autorouter doctor [--evaluate-local] [--json]

Check configuration, installed Claude, and local model availability.
--evaluate-local explicitly tests synthetic routing cases on the existing
Ollama model. No downloads, Anthropic/Jev calls, or configuration changes.
--json returns the local evaluation report (requires --evaluate-local).

Example: claude-autorouter doctor --evaluate-local`,
  config: `Usage: claude-autorouter config show [--json] [--check-all]
       claude-autorouter config set KEY VALUE
       claude-autorouter config set KEY --stdin
       claude-autorouter config unset KEY

Show effective settings and their source; secrets are always redacted.
--check-all also validates settings for the inactive evaluator.
Set/unset changes only the named saved setting. Environment values still win.
Secret keys require --stdin or a hidden prompt, never a command-line value.

Example: claude-autorouter config set AUTOROUTER_OLLAMA_TIMEOUT_MS 0`,
  serve: `Usage: claude-autorouter serve

Run a local Messages API gateway. Requires configured evaluator credentials,
authentication, and AUTOROUTER_TOKEN (at least 16 characters).
Use claude-autorouter claude for managed startup and cleanup.`,
  sessions: `Usage: claude-autorouter sessions list [--json]
       claude-autorouter sessions show ID [--json]

Read optional local session logs from AUTOROUTER_SESSION_LOG_DIR.
List prints the IDs used by show. History includes routing choices, observed
models, request outcomes, latency, and API-equivalent savings coverage.
Logging is off by default. To enable metadata-only history:
  claude-autorouter config set AUTOROUTER_SESSION_LOG_MODE metadata
  claude-autorouter config set AUTOROUTER_SESSION_LOG_DIR /path/to/private/logs`,
  claude: `Usage: claude-autorouter claude [Claude Code arguments]

Launch Claude with automatic routing and an AutoRouter status line.
Example: claude-autorouter claude --permission-mode auto
Auto mode switches between Sonnet and Opus on new human tasks.
Claude owns permission checks and subscription authentication.
--help and --version pass directly to Claude without starting the router.`,
};

export function helpText(command = 'help') {
  return commands[command] ?? `Claude AutoRouter — automatic Claude model routing

Usage: claude-autorouter <command> [options]

  setup      Configure Jev or local Ollama; prompt for keys privately
  doctor     Check configuration and local dependencies
  config     Inspect or change saved settings without replacing them
  sessions   Read optional local decision and outcome history
  claude     Launch Claude Code with automatic routing
  serve      Run the local gateway separately
  --version  Print the AutoRouter version

Start: claude-autorouter setup
       claude-autorouter doctor
       claude-autorouter claude --permission-mode auto

Run claude-autorouter help <command> for options and examples.
Jev is the default; it receives bounded prompt excerpts. Ollama uses only the
local /v1/systemone evaluator. Complete requests go to Anthropic.
Session logging is off unless AUTOROUTER_SESSION_LOG_DIR is configured.
Environment variables override ~/.config/claude-autorouter/config.json.
AUTOROUTER_CONFIG selects another file. Project .env files are not auto-loaded.
Reference: docs/reference.md`;
}
