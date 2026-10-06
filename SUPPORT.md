# Support

AutoRouter is an independent, community-maintained project. It does not provide official support for Anthropic, TypeSafe or Ollama, and has no response-time guarantee.

For setup and usage, start with the [README](README.md) and [troubleshooting reference](docs/reference.md#troubleshooting). Run `claude-autorouter --version`, `claude --version` and `claude-autorouter doctor` to identify the installation and configuration involved. Ordinary `doctor` performs configuration and local service checks; it does not verify paid-provider access.

Use [GitHub issues](https://github.com/frapposelli/claude-autorouter/issues) for reproducible bugs, feature proposals and questions not answered by the documentation. Repository access is required while the repository is private; if you cannot access it, contact [fabio@rapposelli.org](mailto:fabio@rapposelli.org). For vulnerabilities, follow [SECURITY.md](SECURITY.md) instead of opening an issue. Community conduct reports follow the [code of conduct](CODE_OF_CONDUCT.md).

## Make a report useful

- Include AutoRouter, Claude Code, Node.js and operating-system versions; include the Ollama version and model tag for local evaluation.
- Identify the authentication mode, evaluator and client profile without sharing credentials or full environment/configuration dumps.
- Describe expected and actual behavior, and provide minimal steps using a synthetic prompt or fixture when possible. Distinguish the selected model from the provider-confirmed serving model.
- Share only the relevant, inspected diagnostic excerpt. Remove secrets, private prompts, responses, source code, personal paths and organization identifiers. Screenshots can disclose this information too.

Logging is disabled by default. When enabling optional session history for a reproduction, explicitly choose `AUTOROUTER_SESSION_LOG_MODE=metadata`; the default `prompts` mode includes task excerpts. Metadata-only output still needs review before sharing. Debug stderr can also contain Claude's own diagnostics. See [session logs](docs/reference.md#session-decision-logs) and [troubleshooting](docs/reference.md#troubleshooting).

Account access, subscription/model eligibility, billing and provider outages belong with the responsible provider. AutoRouter reports can investigate how the gateway handles those failures, but cannot change a provider's account policies. Local-model accuracy and performance depend on the workload and hardware; include those conditions when reporting unexpected classifications.
