// Temporary Node-reference comparison only; the installed product does not use
// this driver. All configurations, credentials and homes are synthetic.
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { isDeepStrictEqual } from 'node:util';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const candidate = resolve(process.argv[3] ?? join(root, 'rust/target/debug/claude-autorouter'));
const baseline = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json'), 'utf8'));
await verifyBaseline(reference);
const evidenceDirectory = join(root, 'artifacts/rust-rewrite');
mkdirSync(evidenceDirectory, { recursive: true });
const evidencePath = join(evidenceDirectory, `parity-config-commands-${Date.now()}.json`);
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const provenance = { node: process.version, candidate_sha256: digest(readFileSync(candidate)), reference_integrity_verified: true };
const directory = mkdtempSync(join(tmpdir(), 'autorouter-native-config-parity-'));
const path = join(directory, 'config.json');
const base = { HOME: directory, XDG_CONFIG_HOME: directory, AUTOROUTER_CONFIG: path, PATH: directory };
writeFileSync(join(directory, 'claude'), '#!/bin/sh\nif [ "$1" = "--version" ]; then printf "2.1.285 (Claude Code)\\n"; else printf \'{"loggedIn":true,"authMethod":"claude.ai"}\\n\'; fi\n', { mode: 0o700 });
const cases = [];
for (const saved of [undefined, {}, { AUTOROUTER_PORT: '8123', TYPESAFE_API_KEY: 'synthetic-file-key' },
  { AUTOROUTER_AUTH_MODE: 'subscription', AUTOROUTER_SECRET_STORE: 'file', AUTOROUTER_STOP: 'unsupported-synthetic' }]) {
  for (const env of [{}, { AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: 'synthetic-env-key' },
    { AUTOROUTER_AUTH_MODE: 'subscription', AUTOROUTER_CLIENT_PROFILE: 'auto' },
    { AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_JEV_TIMEOUT_MS: 'bad' },
    { AUTOROUTER_PORT: '9000', AUTOROUTER_SECRET_STORE: 'keychain' },
    { AUTOROUTER_CLIENT_PROFILE: 'invalid' }]) {
    for (const flags of [[], ['--json'], ['--check-all'], ['--check-all', '--json']]) {
      cases.push({ saved, env, args: ['config', 'show', ...flags] });
    }
  }
}
for (const args of [
  ['set', 'AUTOROUTER_PORT', '8124'], ['set', 'AUTOROUTER_PORT', 'invalid'], ['unset', 'AUTOROUTER_PORT'],
  ['set', 'AUTOROUTER_JEV_TIMEOUT_MS', 'invalid'], ['set', 'AUTOROUTER_OLLAMA_TIMEOUT_MS', '0'],
  ['set', 'CLAUDE_CODE_STOP_HOOK_BLOCK_CAP', '0'], ['set', 'AUTOROUTER_SESSION_LOG_DIR', './logs'],
  ['set', 'TYPESAFE_API_KEY', 'synthetic-argument'], ['set', 'TYPESAFE_API_KEY', '--stdin'],
  ['unset', 'TYPESAFE_API_KEY'], ['set', 'AUTOROUTER_TOKEN', '--stdin'],
  ['set', 'synthetic-unknown-key', 'value'], ['show', '--invalid'], ['set', 'AUTOROUTER_SECRET_STORE', 'file'],
]) {
  for (const env of [{}, { AUTOROUTER_PORT: '9000', TYPESAFE_API_KEY: 'synthetic-environment' }]) {
    cases.push({ saved: { AUTOROUTER_PORT: '8123', TYPESAFE_API_KEY: 'synthetic-saved' }, env,
      args: ['config', ...args], stdin: ' synthetic-stdin-value\r\n' });
  }
}
for (const saved of [undefined, { AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_SECRET_STORE: 'file', AUTOROUTER_AUTH_MODE: 'subscription', TYPESAFE_API_KEY: 'synthetic-kept', AUTOROUTER_PORT: '8123' }]) {
  for (const args of [[], ['--force'], ['--replace'], ['--force', '--client-profile', 'auto'], ['--force', '--evaluator', 'jev'],
    ['--auth-mode', 'invalid'], ['--secret-store', 'invalid'], ['--ollama-timeout-ms', '30001'], ['--evaluator', 'jev', '--pull'],
    ['--force', '--stop-hook-block-cap', '0'], ['--force', '--session-log-dir', './logs'], ['--force', '--session-log-mode', 'prompts']]) {
    cases.push({ saved, env: { AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_SECRET_STORE: 'file', TYPESAFE_API_KEY: 'synthetic-new', AUTOROUTER_PORT: '9000' }, args: ['setup', ...args] });
  }
}
for (const saved of [undefined, { AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_AUTH_MODE: 'subscription' },
  { AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_AUTH_MODE: 'api-key', ANTHROPIC_API_KEY: 'synthetic-saved-anthropic' },
  { AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_AUTH_MODE: 'subscription', AUTOROUTER_CLIENT_PROFILE: 'auto', CLAUDE_CODE_STOP_HOOK_BLOCK_CAP: '2', AUTOROUTER_SESSION_LOG_DIR: './logs' }]) {
  cases.push({ saved, env: { TYPESAFE_API_KEY: 'synthetic-jev' }, args: ['doctor'] });
}
for (const key of ['AUTOROUTER_HAIKU_MODEL', 'AUTOROUTER_SONNET_MODEL', 'AUTOROUTER_OPUS_MODEL', 'AUTOROUTER_JEV_MODEL', 'ENABLE_TOOL_SEARCH', 'AUTOROUTER_SESSION_LOG_DIR', 'TYPESAFE_API_KEY']) {
  for (const suffix of ['\ud800', '\udfff', '\ufffd\ud800']) {
    const saved = { AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_AUTH_MODE: 'subscription', AUTOROUTER_SECRET_STORE: 'file', TYPESAFE_API_KEY: 'synthetic-key', [key]: `synthetic-${suffix}` };
    for (const args of [['config', 'show', '--json'], ['config', 'show'], ['config', 'set', 'AUTOROUTER_PORT', '8124'], ['setup', '--force'], ['config', 'unset', key]]) {
      cases.push({ saved, env: {}, args });
    }
    cases.push({ saved, env: { [key]: 'environment' }, args: ['config', 'show', '--json'] });
    if (key !== 'TYPESAFE_API_KEY') cases.push({ saved, env: {}, args: ['config', 'set', key, `synthetic-${suffix.replace(/[\ud800-\udfff]/g, '\ufffd')}`] });
  }
}
let activeCase, completedCases = 0;
function run(executable, prefix, testCase, index, implementation) {
  activeCase = { case: index, implementation };
  rmSync(path, { force: true });
  if (testCase.saved !== undefined) writeFileSync(path, JSON.stringify(testCase.saved), { mode: 0o600 });
  const output = spawnSync(executable, [...prefix, ...testCase.args], {
    cwd: directory, env: { ...base, ...testCase.env }, input: testCase.stdin ?? '', encoding: 'utf8',
    timeout: 15000, maxBuffer: 1024 * 1024,
  });
  if (output.error || output.signal) {
    activeCase.signal = output.signal ?? null;
    activeCase.error_code = typeof output.error?.code === 'string' ? output.error.code : null;
    throw new Error(`Synthetic configuration case ${index} (${implementation}) did not complete: ${activeCase.signal ?? activeCase.error_code ?? 'unknown failure'}.`);
  }
  let saved;
  try { saved = JSON.parse(readFileSync(path, 'utf8')); } catch { saved = null; }
  const stdout = testCase.args.includes('--json') ? JSON.parse(output.stdout)
    : testCase.args[0] === 'doctor' ? output.stdout.replace(/^OK  (?:Node\.js|AutoRouter) .*\n/, '') : output.stdout;
  return { status: output.status, stdout, stderr: output.stderr, saved };
}
try {
  const failures = [];
  for (const [index, testCase] of cases.entries()) {
    const expected = run(process.execPath, [join(reference, 'bin/autorouter.mjs')], testCase, index, 'node');
    const actual = run(candidate, [], testCase, index, 'rust');
    if (!isDeepStrictEqual(actual, expected)) failures.push({ case: index, args: testCase.args, fields: Object.keys(actual).filter(key => !isDeepStrictEqual(actual[key], expected[key])) });
    completedCases++;
  }
  const report = { schema_version: 1, baseline_commit: baseline.commit ?? baseline.baseline_commit, ...provenance, completed: true,
    cases: cases.length, matched: cases.length - failures.length, failures,
    deliberate_differences: ['Plain doctor replaces the Node.js prerequisite line with native version and target diagnostics, as required by the rewrite plan.'] };
  writeFileSync(evidencePath, `${JSON.stringify(report, null, 2)}\n`, { flag: 'wx' });
  console.log(JSON.stringify(report, null, 2));
  if (failures.length) process.exitCode = 1;
} catch (error) {
  writeFileSync(evidencePath, `${JSON.stringify({ schema_version: 1, ...provenance, completed: false, completed_cases: completedCases, active_case: activeCase, error: 'Synthetic command comparison could not complete.' }, null, 2)}\n`, { flag: 'wx' });
  throw error;
} finally { rmSync(directory, { recursive: true, force: true }); }
