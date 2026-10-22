import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const workflow = new URL('../.github/workflows/rust-runtime32.yml', import.meta.url);
const first = '      - name: Build and audit ordinary production i686 CLI before runtime-tools xtask\n        run: |\n';
const next = '      - name: Build i686 package helper after the production executable is frozen\n';
const flags = '-C target-cpu=pentium4 -C linker=gcc';
const runner = 'python3 /synthetic/runtime32-proof.py run-test';
const fullTrace = ['git', 'home-cargo', 'home-rustup', 'begin', 'build', 'features',
  'finish', 'cmp', 'copy', 'setarch', 'cli', 'proof'];

function productionStep() {
  const source = readFileSync(workflow, 'utf8');
  assert.equal(source.split(first).length, 2, 'production step has one exact name/run boundary');
  assert.equal(source.split(next).length, 2, 'following helper step has one exact boundary');
  const start = source.indexOf(first) + first.length, end = source.indexOf(next, start);
  assert.ok(end > start);
  return source.slice(start, end).trimEnd().split('\n').map(line => {
    assert.ok(line.startsWith('          '), 'run block retains ten-space YAML indentation');
    return line.slice(10);
  }).join('\n') + '\n';
}

// The controller uses Bash; every external command it invokes is synthetic
// and runs this test's selected Node through an owned /bin/sh wrapper.
const stub = String.raw`
import assert from 'node:assert/strict';
import { appendFileSync, existsSync, mkdirSync, readFileSync, unlinkSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
const [role, ...args] = process.argv.slice(2), env = process.env;
const root = env.SYNTHETIC_ROOT, mode = env.SYNTHETIC_MODE;
const out = join(root, 'artifacts/rust-rewrite/runtime32');
const target = join(env.CARGO_TARGET_DIR, 'i686-unknown-linux-gnu/release/claude-autorouter');
const retained = join(out, 'production-native/claude-autorouter');
const bytes = '#!/bin/sh\nexit 88\n'; // Never executed: the copied CLI is replaced by an owned stub below.
const cpu = '-C target-cpu=pentium4 -C linker=gcc';
const temporary = ['AUTOROUTER_BUILD_TARGET', 'AUTOROUTER_BUILD_MACHINE', 'AUTOROUTER_BUILD_RUNNER',
  'OPENSSL_STATIC', 'OPENSSL_SRC_PERL', 'SOURCE_DATE_EPOCH'];
function record(stage) {
  appendFileSync(env.SYNTHETIC_TRACE, JSON.stringify({stage, args,
    flags: env.RUSTFLAGS, runner: env.CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_RUNNER,
    temporary: Object.fromEntries(temporary.map(k => [k, env[k] ?? null]))}) + '\n');
  if (mode === stage + '-failure') process.exit(23);
}
function productionEnvironment() {
  assert.equal(env.RUSTFLAGS, cpu);
  assert.equal(env.AUTOROUTER_BUILD_TARGET, 'i686-unknown-linux-gnu');
  assert.equal(env.AUTOROUTER_BUILD_MACHINE, 'x86_64');
  assert.equal(env.AUTOROUTER_BUILD_RUNNER, 'ubuntu-24.04');
  assert.equal(env.OPENSSL_STATIC, '1');
  assert.equal(env.OPENSSL_SRC_PERL, '/usr/bin/perl');
  assert.equal(env.SOURCE_DATE_EPOCH, '1234567890');
  assert.equal(env.CARGO_HOME, join(root, 'canonical-cargo'));
  assert.equal(env.RUSTUP_HOME, join(root, 'canonical-rustup'));
}
function restored() {
  assert.equal(env.RUSTFLAGS, '');
  for (const name of temporary) assert.equal(env[name], undefined, name + ' must not escape subshell');
  assert.equal(env.CARGO_HOME, join(root, 'original-cargo'));
  assert.equal(env.RUSTUP_HOME, join(root, 'original-rustup'));
  assert.equal(env.CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_RUNNER, 'python3 /synthetic/runtime32-proof.py run-test');
  assert.equal(readFileSync(env.GITHUB_ENV, 'utf8'), 'SYNTHETIC_EXISTING=1\n');
}
function value(name) { const i = args.indexOf(name); assert.ok(i >= 0); return args[i + 1]; }
if (role === 'git') {
  record('git'); assert.deepEqual(args, ['show', '-s', '--format=%ct', 'HEAD']);
  process.stdout.write('1234567890\n');
} else if (role === 'python3') {
  if (args[0] === '-c') {
    assert.equal(args.length, 2);
    const cargo = 'import os; print(os.path.realpath(os.environ.get("CARGO_HOME", os.path.expanduser("~/.cargo"))))';
    const rustup = 'import os; print(os.path.realpath(os.environ.get("RUSTUP_HOME", os.path.expanduser("~/.rustup"))))';
    assert.ok(args[1] === cargo || args[1] === rustup, 'only the two exact metadata snippets are admitted');
    assert.equal(env.RUSTFLAGS, cpu);
    const name = args[1] === cargo ? 'cargo' : 'rustup'; record('home-' + name);
    process.stdout.write(join(root, 'canonical-' + name) + '\n');
  } else {
    record('proof'); assert.deepEqual(args, ['distribution/ci/runtime32-proof.py', 'production']);
    restored(); assert.equal(readFileSync(retained, 'utf8'), bytes);
    process.stdout.write('{"synthetic_proof":true}\n');
  }
} else if (role === 'auditor') {
  const stage = args[0] === 'production-features' ? 'features' : args[1];
  record(stage); productionEnvironment();
  if (stage === 'begin') {
    assert.deepEqual(args.slice(0, 2), ['binary-provenance', 'begin']);
    assert.equal(value('--target'), 'i686-unknown-linux-gnu');
    assert.equal(value('--build-report'), join(out, 'production-build.jsonl'));
    assert.equal(value('--feature-report'), join(out, 'production-features.json'));
    assert.equal(value('--out'), join(out, 'production-inputs.json'));
    assert.equal(existsSync(value('--out')), false);
    writeFileSync(value('--out'), '{"synthetic_inputs":true}\n');
  } else if (stage === 'features') {
    assert.deepEqual(args, ['production-features', '--build-report', '../artifacts/rust-rewrite/runtime32/production-build.jsonl']);
    assert.equal(readFileSync(join(out, 'production-build.jsonl'), 'utf8'), '{"synthetic_build":true}\n');
    process.stdout.write('{"synthetic_features":true}\n');
  } else {
    assert.equal(stage, 'finish'); assert.equal(value('--inputs'), join(out, 'production-inputs.json'));
    assert.equal(value('--build-report'), join(out, 'production-build.jsonl'));
    assert.equal(value('--feature-report'), join(out, 'production-features.json'));
    assert.equal(value('--out'), join(out, 'production-native'));
    assert.equal(readFileSync(join(out, 'production-features.json'), 'utf8'), '{"synthetic_features":true}\n');
    mkdirSync(dirname(retained));
    writeFileSync(retained, mode === 'post-cmp-mismatch' ? 'different retained bytes\n' : bytes);
    writeFileSync(join(out, 'production-native/native-binary-provenance.json'), '{"synthetic_provenance":true}\n');
    if (mode === 'target-missing') unlinkSync(target);
  }
} else if (role === 'cargo') {
  record('build'); productionEnvironment();
  assert.deepEqual(args, ['build', '--release', '--target', 'i686-unknown-linux-gnu', '--package',
    'claude-autorouter', '--offline', '--locked', '--message-format=json']);
  mkdirSync(dirname(target), {recursive:true}); writeFileSync(target, bytes);
  process.stdout.write('{"synthetic_build":true}\n');
} else if (role === 'cmp') {
  record('cmp'); restored(); assert.deepEqual(args.map(path => resolve(path)), [target, retained]);
  assert.deepEqual(readFileSync(target), readFileSync(retained));
} else if (role === 'cp') {
  record('copy'); restored(); assert.deepEqual(args.map(path => resolve(path)), [target, join(out, 'claude-autorouter-i686')]);
  // The destination is an executable owned fake, not the synthetic build payload.
  writeFileSync(args[1], env.SYNTHETIC_CLI_WRAPPER, {mode:0o700});
} else if (role === 'setarch') {
  record('setarch'); restored(); assert.equal(args[0], 'i686');
  assert.equal(resolve(args[1]), join(out, 'claude-autorouter-i686'));
  const child = spawnSync(args[1], args.slice(2), {env, encoding:'utf8', timeout:10000, maxBuffer:65536});
  assert.equal(child.error, undefined); process.stdout.write(child.stdout); process.stderr.write(child.stderr);
  process.exit(child.status ?? 99);
} else {
  assert.equal(role, 'cli'); record('cli'); restored(); assert.deepEqual(args, ['--version']);
  process.stdout.write('0.0.0-synthetic\n');
}
`;

const quote = value => "'" + value.replaceAll("'", "'\\''") + "'";

/** @param {import('node:test').TestContext} t @param {string} mode */
function run(t, mode) {
  const root = mkdtempSync(join(tmpdir(), 'autorouter-runtime32-workflow-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  assert.ok(!resolve(root).startsWith(resolve(dirname(fileURLToPath(workflow)), '../..') + '/'));
  const bin = join(root, 'bin'), out = join(root, 'artifacts/rust-rewrite/runtime32');
  mkdirSync(bin); mkdirSync(out, { recursive: true }); mkdirSync(join(root, 'rust'));
  const helper = join(root, 'synthetic.mjs'), trace = join(root, 'trace.jsonl'), githubEnv = join(root, 'github-env');
  writeFileSync(helper, stub); writeFileSync(githubEnv, 'SYNTHETIC_EXISTING=1\n');
  const wrapper = role => '#!/bin/sh\nexec ' + quote(process.execPath) + ' ' + quote(helper) + ' ' + quote(role) + ' "$@"\n';
  for (const role of ['git', 'cargo', 'python3', 'setarch', 'cmp', 'cp']) {
    writeFileSync(join(bin, role), wrapper(role), { mode: 0o700 });
  }
  writeFileSync(join(out, 'autorouter-audit-bootstrap'), wrapper('auditor'), { mode: 0o700 });
  const child = spawnSync('/bin/bash', ['-c', productionStep()], { cwd: join(root, 'rust'),
    env: { PATH: bin, HOME: join(root, 'home'), CARGO_HOME: join(root, 'original-cargo'),
      RUSTUP_HOME: join(root, 'original-rustup'), CARGO_TARGET_DIR: join(root, 'target'),
      RUSTFLAGS: '', CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_RUNNER: runner,
      GITHUB_WORKSPACE: root, GITHUB_ENV: githubEnv, SYNTHETIC_ROOT: root,
      SYNTHETIC_MODE: mode, SYNTHETIC_TRACE: trace, SYNTHETIC_CLI_WRAPPER: wrapper('cli') },
    encoding: 'utf8', timeout: 30000, maxBuffer: 65536 });
  assert.equal(child.error, undefined, 'owned synthetic Bash completed within bounds');
  assert.equal(child.signal, null);
  assert.equal(readFileSync(githubEnv, 'utf8'), 'SYNTHETIC_EXISTING=1\n');
  const rows = readFileSync(trace, 'utf8').trim().split('\n').map(line => JSON.parse(line));
  return { child, rows };
}

test('i686 production workflow preserves the provenance boundary and restores the original proof environment', t => {
  const { child, rows } = run(t, 'success');
  assert.equal(child.status, 0, child.stderr);
  assert.deepEqual(rows.map(row => row.stage), fullTrace);
  for (const row of rows.filter(row => ['begin', 'build', 'features', 'finish'].includes(row.stage))) {
    assert.equal(row.flags, flags); assert.equal(row.runner, runner);
  }
  for (const row of rows.filter(row => ['cmp', 'copy', 'setarch', 'cli', 'proof'].includes(row.stage))) {
    assert.equal(row.flags, ''); assert.equal(row.runner, runner);
    assert.ok(Object.values(row.temporary).every(value => value === null));
  }
});

for (const [mode, last] of [['begin-failure', 'begin'], ['build-failure', 'build'],
  ['features-failure', 'features'], ['finish-failure', 'finish'],
  ['post-cmp-mismatch', 'cmp'], ['target-missing', 'cmp'], ['proof-failure', 'proof']]) {
  test(`i686 production workflow stops after synthetic ${mode}`, t => {
    const { child, rows } = run(t, mode);
    assert.notEqual(child.status, 0);
    assert.deepEqual(rows.map(row => row.stage), fullTrace.slice(0, fullTrace.indexOf(last) + 1));
  });
}
