#!/usr/bin/env node
import { randomBytes } from 'node:crypto';
import { spawn } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { readConfig, requireKeys } from '../src/config.mjs';
import { createRouterServer, listen } from '../src/server.mjs';
import { buildClaudeEnv, conflictingProviders } from '../src/auth.mjs';
import { dirname } from 'node:path';
import { createStatusState } from '../src/status-state.mjs';
import { addStatusLineSettings } from '../src/status-settings.mjs';
import { loadUserConfig } from '../src/user-config.mjs';
import { setup, doctor } from '../src/onboarding.mjs';
import { setupOllama } from '../src/ollama-setup.mjs';

const [command = 'help', ...args] = process.argv.slice(2);
if (['--version', '-v', 'version'].includes(command)) {
  console.log(JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8')).version);
} else if (['help', '--help', '-h'].includes(command)
  || (['setup', 'doctor', 'serve'].includes(command) && args.some(arg => ['--help', '-h'].includes(arg)))) {
  console.log(`Claude AutoRouter — routing for Haiku, Sonnet, and Opus

Usage:
  claude-autorouter setup [--auth-mode subscription|api-key] [--force]
    [--evaluator jev|ollama]
    [--ollama-model MODEL] [--pull]
  claude-autorouter doctor
  claude-autorouter claude [Claude Code arguments]
  claude-autorouter serve
  claude-autorouter --version

Setup defaults to subscription authentication and prompts for keys without echoing.
For noninteractive setup, supply keys through environment variables.
User config: ~/.config/claude-autorouter/config.json (or XDG_CONFIG_HOME).
AUTOROUTER_CONFIG selects a different file; environment variables take precedence.
Project .env files are never loaded automatically.

Jev is the default evaluator and requires TYPESAFE_API_KEY.
Ollama evaluates locally and requires Ollama 0.35+ with /v1/systemone.
Use setup --evaluator ollama --pull to detect Ollama and download a missing model.
The local default is nimble:9b-q4_K_M; --ollama-model selects another compatible model.
Local routing is experimental; see docs/ollama-evaluation.md for measured limits.
AUTOROUTER_AUTH_MODE=subscription uses your saved Claude Code login.
Without setup, AUTOROUTER_AUTH_MODE defaults to api-key and also requires ANTHROPIC_API_KEY.
AUTOROUTER_CLIENT_PROFILE=compatible (default) enables all three routing tiers.
Use AUTOROUTER_CLIENT_PROFILE=native to retain Claude Code's own model/thinking settings.
Standalone serve also requires AUTOROUTER_TOKEN (at least 16 characters).
The claude launcher creates a temporary credential and an ephemeral port.
It enables an AutoRouter status line for this session (AUTOROUTER_STATUSLINE=0 to opt out).
Launcher logs are quiet by default; AUTOROUTER_DEBUG=1 enables diagnostic logs on stderr.
Jev sends prompt excerpts to TypeSafe; Ollama keeps classification on this machine.
Complete inference requests still go to Anthropic. See README.md.`);
} else if (command === 'setup' || command === 'doctor') {
  try {
    if (command === 'setup') await setup(args);
    else {
      if (args.length) throw new Error('Usage: claude-autorouter doctor');
      if (!await doctor()) process.exitCode = 1;
    }
  } catch (error) { console.error(error.message); process.exitCode = 1; }
} else if (!['claude', 'serve'].includes(command)) {
  console.error('Unknown command. Run claude-autorouter --help.');
  process.exitCode = 1;
} else {
  let server;
  let status;
  const stop = () => {
    if (server) { server.close(); server.closeAllConnections(); }
    status?.close();
  };
  try {
    if (command === 'serve' && args.length) throw new Error('Usage: claude-autorouter serve');
    const runtimeEnv = loadUserConfig().env;
    const config = readConfig(runtimeEnv);
    requireKeys(config);
    const diagnosticLogs = command === 'serve' || runtimeEnv.AUTOROUTER_DEBUG === '1';
    if (command === 'claude') {
      if (config.authMode === 'subscription' && args.includes('--bare')) {
        throw new Error('--bare disables Claude Code OAuth; omit it in subscription mode');
      }
      for (const key of conflictingProviders(runtimeEnv)) {
        throw new Error(`Unset ${key}; this router supports the Anthropic Messages API`);
      }
      config.localToken = randomBytes(32).toString('hex');
    }
    if (config.evaluator === 'ollama') {
      console.error(`Preparing local Ollama evaluator (${config.ollamaModel})…`);
      try { await setupOllama(config, { pull: false, warm: true, write: () => {} }); }
      catch {
        console.error('Ollama could not be prepared. Requests will use the conservative fallback while it is unavailable; run claude-autorouter doctor.');
      }
    }
    const statusEnabled = command === 'claude' && runtimeEnv.AUTOROUTER_STATUSLINE !== '0';
    let claudeArgs = args;
    if (statusEnabled) {
      status = createStatusState({ baselineModel: config.models.opus });
      if (status.path) {
        try { claudeArgs = addStatusLineSettings(args, dirname(status.path)); }
        catch {
          status.close(); status = undefined;
          console.error('AutoRouter status line unavailable: could not safely prepare session settings. Passing your original settings to Claude.');
        }
      } else console.error('AutoRouter status line unavailable: could not create local status storage.');
    }
    // Claude owns the terminal while its UI is running. Status updates use the
    // local snapshot independently; proxy JSON must not write over the UI.
    server = createRouterServer(config, {
      log: diagnosticLogs ? undefined : () => {},
      onStatus: event => status?.update(event),
    });
    const address = await listen(server, command === 'claude' ? 0 : config.port);
    const baseUrl = `http://127.0.0.1:${address.port}`;
    if (diagnosticLogs) console.error(`AutoRouter listening on ${baseUrl} (${config.authMode} authentication)`);
    if (diagnosticLogs && command === 'claude' && config.clientProfile === 'compatible') {
      console.error(`AutoRouter uses Haiku-compatible requests with client thinking disabled; ${config.evaluator === 'ollama' ? 'Ollama' : 'Jev'} selects the upstream model.`);
    }
    if (command === 'serve') {
      for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => {
        stop();
      });
    } else {
      const env = buildClaudeEnv(config, baseUrl, runtimeEnv);
      delete env.AUTOROUTER_CONFIG;
      delete env.AUTOROUTER_STATUS_FILE;
      if (status?.path) env.AUTOROUTER_STATUS_FILE = status.path;
      const child = spawn('claude', claudeArgs, { stdio: 'inherit', env });
      child.once('error', () => { console.error('Could not launch Claude Code. Ensure `claude` is installed and on PATH.'); process.exitCode = 1; stop(); });
      child.once('exit', (code, signal) => { process.exitCode = code ?? (signal === 'SIGINT' ? 130 : 1); stop(); });
      for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => child.kill(signal));
    }
  } catch (error) {
    console.error(error.message);
    stop();
    process.exitCode = 1;
  }
}
