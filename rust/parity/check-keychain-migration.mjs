// Opt-in real Keychain comparison; synthetic accounts only, never ordinary CI.
import { spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync, statSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
if (process.argv.length !== 6 || process.argv[2] !== '--real-keychain') throw new Error('Usage: check-keychain-migration.mjs --real-keychain REFERENCE NATIVE_EXECUTABLE NEW_OUTPUT_DIRECTORY');
const reference = resolve(process.argv[3]);
await verifyBaseline(reference);
const { CONFIG_KEYS } = await import(pathToFileURL(join(reference, 'src/user-config.mjs')).href);
const output = resolve(process.argv[5]);
mkdirSync(output, { mode: 0o700 });
const scratch = mkdtempSync(join(tmpdir(), 'autorouter-keychain-parity-'));
const config = join(scratch, "settings quote' dollar$ brackets[].json");
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const suffix = sha(Buffer.from(config)).slice(0, 16);
const keys = ['ANTHROPIC_API_KEY', 'TYPESAFE_API_KEY', 'AUTOROUTER_TOKEN'];
const accounts = keys.map(key => `${key}:${suffix}`);
const values = ['synthetic_keychain_alpha', 'synthetic_keychain_beta'];
const native = resolve(process.argv[4]);
const baseline = join(reference, 'bin/autorouter.mjs');
const env = { ...process.env };
for (const key of CONFIG_KEYS) delete env[key];
env.AUTOROUTER_CONFIG = config;
const report = { schema_version: 1, kind: 'isolated_real_keychain_language_port_comparison',
  started_at: new Date().toISOString(), protocol_sha256: sha(readFileSync(join(root, 'rust/parity/keychain-comparison-protocol-v1.json'))),
  native_sha256: sha(readFileSync(native)), node: process.version,
  baseline_commit: 'ea930c247626ce2af5ccdad721b5121417bf4ad8',
  account_scope: 'Previously absent synthetic temporary-path accounts', checks: [], cleanup: [], passed: false };
const absent = new Set();
function run(executable, args, input = '') {
  const result = spawnSync(executable, args, { env, input, encoding: 'utf8',
    timeout: 30000, maxBuffer: 65536 });
  if (result.error) throw new Error('Bounded synthetic command could not complete');
  return result;
}
function check(name, condition) {
  report.checks.push({ name, passed: Boolean(condition) });
  if (!condition) throw new Error(`Synthetic check failed: ${name}`);
}
function lookup(account) {
  return run('/usr/bin/security', ['find-generic-password', '-s', 'claude-autorouter', '-a', account, '-w']);
}
function cli(engine, args, input = '') {
  const result = engine === 'native' ? run(native, args, input) : run(process.execPath, [baseline, ...args], input);
  check(`${engine} ${args.slice(0, 3).join(' ')} exit`, result.status === 0);
  check(`${engine} output privacy`, values.every(value => !`${result.stdout}${result.stderr}`.includes(value)));
  return result;
}
try {
  if (process.platform !== 'darwin') throw new Error('Real Keychain requires macOS');
  for (const account of accounts) {
    const result = lookup(account);
    check('Synthetic account initially absent', result.status === 44);
    absent.add(account);
  }
  writeFileSync(config, JSON.stringify({ AUTOROUTER_SECRET_STORE: 'file', AUTOROUTER_EVALUATOR: 'jev', AUTOROUTER_AUTH_MODE: 'subscription', TYPESAFE_API_KEY: values[0] }) + '\n', { mode: 0o600 });
  cli('native', ['config', 'set', 'AUTOROUTER_SECRET_STORE', 'keychain']);
  let found = lookup(accounts[1]);
  check('Native writes exact lexical Keychain account', found.status === 0 && found.stdout.trimEnd() === values[0]);
  check('Native removes plaintext after verified Keychain migration', !readFileSync(config, 'utf8').includes(values[0]));
  cli('node-reference', ['config', 'show', '--json']);
  cli('node-reference', ['config', 'set', 'TYPESAFE_API_KEY', '--stdin'], values[1] + '\n');
  found = lookup(accounts[1]);
  check('Node updates the same native-created credential', found.status === 0 && found.stdout.trimEnd() === values[1]);
  cli('native', ['config', 'show', '--json']);
  cli('native', ['config', 'set', 'AUTOROUTER_SECRET_STORE', 'file']);
  check('Native preserves the Node-updated credential on file migration', JSON.parse(readFileSync(config, 'utf8')).TYPESAFE_API_KEY === values[1]);
  check('Saved synthetic settings stay private', (statSync(config).mode & 0o777) === 0o600);
  check('Native file migration removes the scoped Keychain item', lookup(accounts[1]).status === 44);
  cli('node-reference', ['config', 'set', 'AUTOROUTER_SECRET_STORE', 'keychain']);
  found = lookup(accounts[1]);
  check('Node recreates the same exact account', found.status === 0 && found.stdout.trimEnd() === values[1]);
  cli('native', ['config', 'unset', 'TYPESAFE_API_KEY']);
  check('Native unset removes the shared synthetic account', lookup(accounts[1]).status === 44);
  report.passed = true;
} catch (error) {
  report.failure = error instanceof Error ? error.message : 'Synthetic canary failed';
} finally {
  for (const account of absent) {
    try {
      const before = lookup(account);
      if (before.status === 0) run('/usr/bin/security', ['delete-generic-password', '-s', 'claude-autorouter', '-a', account]);
      const cleared = lookup(account).status === 44;
      report.cleanup.push({ account_setting: account.split(':')[0], absent_after: cleared });
      if (!cleared) report.passed = false;
    } catch { report.cleanup.push({ account_setting: account.split(':')[0], absent_after: false }); report.passed = false; }
  }
  rmSync(scratch, { recursive: true, force: true });
  report.finished_at = new Date().toISOString();
  writeFileSync(join(output, 'report.json'), JSON.stringify(report, null, 2) + '\n', { mode: 0o600 });
}
console.log(JSON.stringify({ passed: report.passed, checks: report.checks.length, cleanup: report.cleanup.every(row => row.absent_after), failure: report.failure }));
process.exitCode = report.passed ? 0 : 1;
