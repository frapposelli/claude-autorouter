# Security policy

## Report privately

Once this repository is public and private vulnerability reporting is enabled, use GitHub's [Report a vulnerability](https://github.com/frapposelli/claude-autorouter/security/advisories/new) form. While the repository is private, or if that form is unavailable, email maintainer Fabio Rapposelli at [fabio@rapposelli.org](mailto:fabio@rapposelli.org) with the subject `AutoRouter security report`.

Do not open a public issue or pull request with exploit details, credentials or sensitive request data. Include the affected AutoRouter and Claude Code versions, operating system, evaluator/client profile, expected security boundary, observed impact and a minimal synthetic reproduction where possible. Do not include real API keys, OAuth tokens, private source code, prompts or transcripts. The maintainer can coordinate any additional evidence privately.

Reports are handled on a best-effort basis; there is no guaranteed response time or bounty program. We will coordinate investigation, remediation and disclosure with the reporter before publishing details.

## Supported versions and scope

Security fixes target the latest stable release. Older releases are not maintained as separate security branches; users may need to upgrade. Reports against `main` are also welcome.

Relevant reports include credential exposure, unauthorized access to the local gateway or saved configuration, unintended disclosure through logs, and routing or request changes that weaken Claude's authentication or permission boundaries. Provider accounts, billing and vulnerabilities in Claude Code, TypeSafe or Ollama should also be reported to the responsible provider when applicable.

## Handling diagnostic data

AutoRouter's default Jev evaluator receives bounded task/history excerpts, which may contain private code or tool results. The local Ollama option keeps classification on loopback; Anthropic still receives the full inference request. See [data flow and authentication](docs/reference.md#data-flow-and-authentication).

Session logging is optional. Enabling only a log directory uses the default `prompts` mode, which includes bounded human-task excerpts. Select `AUTOROUTER_SESSION_LOG_MODE=metadata` before enabling a directory to omit those excerpts. Inspect even metadata-only output before sharing it; identifiers, paths or environment details can still be sensitive. Saved logs have no automatic deletion policy. See [history and privacy](docs/reference.md#session-decision-logs).
