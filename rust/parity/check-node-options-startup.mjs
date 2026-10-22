// Temporary frozen-Node oracle. No provider calls or product Node dependency.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFileSync, chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { isDeepStrictEqual } from 'node:util';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const originalCandidate = resolve(process.argv[3] ?? join(root, 'rust/target/debug/claude-autorouter'));
await verifyBaseline(reference);
assert.equal(process.version, 'v22.14.0', 'Startup diagnostics use the frozen Node22.14 baseline');
const directory = mkdtempSync(join(tmpdir(), 'autorouter-startup-parity-'));
const candidate = join(directory, 'candidate');
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
try {
  copyFileSync(originalCandidate, candidate); chmodSync(candidate, 0o700);
  const cases = [];
  for (const options of [
    '--use-openssl-ca --use-bundled-ca', '--use-bundled-ca --use-openssl-ca',
    '--use-openssl-ca=true --use-bundled-ca=false', '--use-system-ca', '--use_system_ca',
    '--use-system-ca=false', '--use-system-ca=0', '--no-use-system-ca', '--no_use_system_ca',
    '"--use-system-ca"', '--use-system-ca --use-openssl-ca --use-bundled-ca',
  ]) {
    for (const args of [['--help'], ['--version'], ['statusline'], ['config', 'show'], ['setup'], ['doctor'], ['serve'], ['claude', '--help'], ['claude']]) {
      cases.push({ options, args, expected: 9 });
    }
  }
  for (const options of ['--use-openssl-ca --no-use-openssl-ca', '--use-openssl-ca --use-bundled-ca --no-use-bundled-ca', '--use-bundled-ca --use-openssl-ca --no-use-openssl-ca', '--no-use-bundled-ca', '--use-openssl-ca --no-use-bundled-ca']) {
    cases.push({ options, args: ['--help'], expected: 0 });
  }
  const results = cases.map((test, index) => {
    const home = join(directory, `case-${index}`); mkdirSync(home, { mode: 0o700 });
    writeFileSync(join(home, 'config.json'), 'PRIVATE malformed configuration', { mode: 0o600 });
    const env = { HOME: home, XDG_CONFIG_HOME: home, TMPDIR: home, PATH: home, AUTOROUTER_CONFIG: join(home, 'config.json'), NODE_OPTIONS: test.options };
    const invoke = (executable, prefix) => {
      const result = spawnSync(executable, [...prefix, ...test.args], { cwd: home, env, input: '', encoding: 'utf8', timeout: 3000, maxBuffer: 65536 });
      assert.ok(!result.error && !result.signal, `Unbounded startup case ${index}`);
      return { code: result.status, stdout: result.stdout, stderr: result.stderr.replaceAll(executable, '<runtime>') };
    };
    const node = invoke(process.execPath, [join(reference, 'bin/autorouter.mjs')]);
    const rust = invoke(candidate, []);
    return { index, ...test, node, rust, matched: node.code === test.expected && isDeepStrictEqual(node, rust) };
  });
  const report = {
    schema_version: 1, kind: 'node_options_startup_differential',
    provenance: { reference_integrity_verified: true, reference, node: process.version, node_sha256: digest(readFileSync(process.execPath)), candidate_sha256: digest(readFileSync(candidate)), candidate_execution: 'isolated immutable byte-identical copy' },
    corpus_sha256: digest(JSON.stringify(cases)), cases: results.length,
    passed: results.filter(result => result.matched).length, results,
  };
  const bytes = JSON.stringify(report, null, 2) + '\n';
  const target = join(root, 'artifacts/rust-rewrite/evidence'); mkdirSync(target, { recursive: true });
  const path = join(target, `${digest(bytes)}.json`); writeFileSync(path, bytes, { flag: 'wx', mode: 0o600 });
  console.log(JSON.stringify({ cases: report.cases, passed: report.passed, report: path }));
  if (report.passed !== report.cases) process.exitCode = 1;
} finally { rmSync(directory, { recursive: true, force: true }); }
