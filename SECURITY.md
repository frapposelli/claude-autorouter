# Security policy

## Report privately

Use GitHub's private [Report a vulnerability](https://github.com/frapposelli/claude-autorouter/security/advisories/new) form. Private vulnerability reporting is enabled for this repository. If the form is unavailable, email maintainer Fabio Rapposelli at [fabio@rapposelli.org](mailto:fabio@rapposelli.org) with the subject `AutoRouter security report`.

Do not open a public issue or pull request with exploit details, credentials or sensitive request data. Include the affected AutoRouter and Claude Code versions, operating system, evaluator/client profile, expected security boundary, observed impact and a minimal synthetic reproduction where possible. Do not include real API keys, OAuth tokens, private source code, prompts or transcripts. The maintainer can coordinate any additional evidence privately.

Reports are handled on a best-effort basis; there is no guaranteed response time or bounty program. We will coordinate investigation, remediation and disclosure with the reporter before publishing details.

## Supported versions and scope

Security fixes target the latest stable release. Older releases are not maintained as separate security branches; users may need to upgrade. Reports against `main` are also welcome.

Relevant reports include credential exposure, unauthorized access to the local gateway or saved configuration, unintended disclosure through logs, and routing or request changes that weaken Claude's authentication or permission boundaries. Provider accounts, billing and vulnerabilities in Claude Code, TypeSafe or Ollama should also be reported to the responsible provider when applicable.

## Handling diagnostic data

AutoRouter's default evaluator is local Ollama, which keeps classification on loopback. The optional hosted Jev evaluator (`--evaluator jev`) receives bounded task/history excerpts, which may contain private code or tool results. Recognizable credentials and personal identifiers are redacted from those excerpts first; this pattern-based filter reduces, but does not eliminate, disclosure. Anthropic still receives the full inference request. See [data flow and authentication](docs/reference.md#data-flow-and-authentication).

New macOS setups keep saved keys in the login Keychain. Existing and non-macOS configurations keep plaintext keys in the private configuration file until you run `claude-autorouter config set AUTOROUTER_SECRET_STORE keychain` (macOS only). See [credential storage](docs/reference.md#credential-storage).

Session logging is optional. Enabling a log directory uses the default `metadata` mode, which omits prompt excerpts. Opting into `AUTOROUTER_SESSION_LOG_MODE=prompts` includes bounded human-task excerpts with recognizable credentials and personal identifiers redacted (pattern-based, so not exhaustive). Inspect even metadata-only output before sharing it; identifiers, paths or environment details can still be sensitive. Saved logs have no automatic deletion policy. See [history and privacy](docs/reference.md#session-decision-logs).
