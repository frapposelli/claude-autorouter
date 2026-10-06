# Claude Code integration and provider policy

AutoRouter is an independent, user-operated local gateway for Claude Code. Anthropic does not sponsor or endorse this project. The project name is **AutoRouter**; references to Claude Code describe the software it runs. Existing npm, command, configuration, and repository identifiers remain `claude-autorouter` for compatibility. Those identifiers do not represent provider approval or trademark clearance. Anthropic's names and marks remain subject to its [trademark guidelines](https://www.anthropic.com/legal/trademark-guidelines).

## What the integration does

The launcher starts the official `claude` executable already installed on the user's machine. AutoRouter neither bundles nor patches that binary. It sets a local gateway address and temporary session settings, evaluates eligible inference requests, selects a compatible Claude model, and forwards the provider's response stream. Model-specific request adjustments and continuity checks are described in the [routing reference](reference.md#routing-policy). It does not change Claude's permission verdicts or grant access to unavailable models.

The gateway listens on `127.0.0.1` and authenticates local requests. Standalone `serve` retains that local boundary; it is not a hosted account-sharing service. This project does not supply a shared Anthropic account, resell inference, or pay provider charges on a user's behalf.

## Authentication and data ownership

- **Subscription mode:** each user signs into their own account through Claude Code's official login flow. Claude owns OAuth refresh. AutoRouter receives the authorization header with each proxied request and forwards it to `https://api.anthropic.com`, together with the OAuth capability header. It does not extract login files or keychain entries, persist subscription tokens, or send those tokens to an evaluator. A separate temporary local token is removed before forwarding.
- **API-key mode:** the user supplies their own Anthropic API key. The proxy forwards that key upstream and gives Claude a separate local credential. API usage is billed to the key owner's account; the router does not switch subscription traffic to API billing after an error.
- **Evaluation:** the default Ollama evaluator receives bounded, redacted task/history excerpts through its loopback service without Claude or Jev credentials. The optional Jev evaluator uses a separate TypeSafe key and separate billing, and receives the same bounded, redacted excerpts, which can still include private code. Anthropic still receives the complete inference request. See [data flow and authentication](reference.md#data-flow-and-authentication).

Claude Code, Anthropic, TypeSafe, and any downloaded local model have their own terms and data handling. Optional AutoRouter history persists locally when enabled; prompt mode can retain text entered by the user. See [history and privacy](reference.md#session-decision-logs).

## Provider guidance and unresolved scope

Documentation reviewed **October 6, 2026**:

Anthropic's [Claude Code legal guidance](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use) allows end users to sign into an unmodified Claude Code binary with their own credentials. It also restricts third-party credential collection or intermediation and certain subscription routing. Its [gateway documentation](https://code.claude.com/docs/en/llm-gateway#subscriptions-and-gateways) describes retaining a saved subscription login when configuring a gateway address without replacing authentication, including forwarding the OAuth capability header.

These statements are relevant technical and policy context; they do not specifically approve AutoRouter's model-rewriting OAuth proxy. This project has not established that every use of subscription forwarding meets the applicable restrictions. Successful authentication or a passing test establishes technical behavior, not provider authorization. Enterprise access does not establish a blanket exception.

Review your account agreement and organization requirements before deployment, and seek clarification from Anthropic about this integration when needed. Anthropic identifies [Commercial Terms](https://www.anthropic.com/legal/commercial-terms) for Team, Enterprise, and API use and [Consumer Terms](https://www.anthropic.com/legal/consumer-terms) for consumer plans in its [license guidance](https://code.claude.com/docs/en/legal-and-compliance#license). API-key mode is available with separate billing and remains subject to its applicable terms. No policy-compliance guarantee is made by this project.
