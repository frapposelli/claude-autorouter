import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { createHash } from 'node:crypto';
import { chmodSync, existsSync, linkSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, realpathSync,
  rmSync, symlinkSync, truncateSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, join } from 'node:path';
import { capture, HOST, ownedWrite, printedFile, probe, resolveTool, ROOT_PACKAGES,
  selectEnvironment, SOURCE_CC_FLAGS, stdInventory, TARGET, TOOLCHAIN, validateNativeLaunch,
  refusalDiagnostics, validateRoots,
  validateRust, witness } from '../rust/distribution/ci/armv7-preflight.mjs';

function scratch(t) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'autorouter-armv7-preflight-')));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return root;
}
const environment = () => ({ PATH: '/synthetic/bin:/usr/bin', HOME: '/synthetic/home',
  CARGO_HOME: '/synthetic/cargo', RUSTUP_HOME: '/synthetic/rustup', RUSTUP_TOOLCHAIN: TOOLCHAIN });
const cfg = ['target_abi="eabihf"', 'target_arch="arm"', 'target_endian="little"',
  'target_env="gnu"', 'target_os="linux"', 'target_pointer_width="32"'].join('\n');
const rustcVersion = `rustc1.99.0\nrelease: 1.99.0\nhost: ${HOST}\nLLVM version: synthetic\n`;
const cargoVersion = 'cargo 1.99.0 (synthetic)';
const rootsText = ROOT_PACKAGES.map(name => name + '\t1.0-1').join('\n') + '\n';
const installedText = ROOT_PACKAGES.map(name => name + '\t1.0-1\tamd64').join('\n') + '\nunrelated\t1\tamd64\n';

test('ARMv7 selected child environment excludes credentials and refuses every nonabsolute PATH component', () => {
  const env = selectEnvironment({ ...environment(), FAKE_API_KEY: 'synthetic-secret', GITHUB_TOKEN: 'fake',
    AUTOROUTER_ARMV7_NATIVE_OWNER: '1' });
  assert.equal(env.FAKE_API_KEY, undefined); assert.equal(env.GITHUB_TOKEN, undefined);
  assert.equal(env.AUTOROUTER_ARMV7_NATIVE_OWNER, undefined);
  assert.equal(env.RUSTUP_AUTO_INSTALL, '0'); assert.equal(env.GIT_CONFIG_GLOBAL, '/dev/null');
  for (const PATH of ['/usr/bin:', '/usr/bin:relative', ':/usr/bin']) {
    assert.throws(() => selectEnvironment({ ...environment(), PATH }), /absolute PATH/);
  }
});

test('ARMv7 preflight rejects compiler, SDK, capability and execution overrides without exposing their values', () => {
  for (const name of ['RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_TARGET',
    'CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUNNER', 'CC', 'CFLAGS', 'LD_LIBRARY_PATH',
    'OPENSSL_armcap', 'PKG_CONFIG_PATH', 'RUSTUP_DIST_SERVER']) {
    assert.throws(() => selectEnvironment({ ...environment(), [name]: 'synthetic-secret-value' }), error => {
      assert.match(error.message, /override/); assert.ok(!error.message.includes('synthetic-secret')); return true;
    });
  }
  assert.throws(() => selectEnvironment({ ...environment(), RUSTUP_TOOLCHAIN: 'stable' }), /toolchain/);
});

test('ARMv7 source witness hashes real bytes and rejects symlinks, oversize and replacement during read', t => {
  const root = scratch(t), path = join(root, 'file'); writeFileSync(path, 'abcd');
  assert.equal(witness(path, 4).row.sha256, createHash('sha256').update('abcd').digest('hex'));
  assert.throws(() => witness(path, 3), /bound/);
  symlinkSync(path, join(root, 'link')); assert.throws(() => witness(join(root, 'link'), 4), /type/);
  let replaced = false;
  assert.throws(() => witness(path, 4, { tick: () => {
    if (!replaced) { replaced = true; unlinkSync(path); writeFileSync(path, 'abcd'); }
  } }), /changed/);
});

test('ARMv7 tool witnesses preserve multicall invocation aliases and accept real hardlinked SDK bytes', t => {
  const root = scratch(t), real = join(root, 'rustup-init'), alias = join(root, 'rustup');
  writeFileSync(real, 'fake multicall tool', { mode: 0o700 }); symlinkSync(real, alias);
  const tool = resolveTool('rustup', { PATH: root });
  assert.equal(tool.invocation, alias); assert.equal(tool.canonical, real);
  assert.equal(tool.file.bytes, 19);
  linkSync(real, join(root, 'sdk-alias')); assert.ok(lstatSync(real).nlink > 1);
  assert.equal(witness(real, 512).row.sha256, tool.file.sha256);
});

test('ARMv7 std inventory requires actual core/std files and rejects symlink, directory and sparse oversize entries', t => {
  const root = scratch(t), core = join(root, 'libcore-abc.rlib'), std = join(root, 'libstd-123.rlib');
  writeFileSync(core, 'core'); writeFileSync(std, 'std');
  assert.equal(stdInventory(root).total_bytes, 7);
  unlinkSync(std); assert.throws(() => stdInventory(root));
  symlinkSync(core, std); assert.throws(() => stdInventory(root), /type/); unlinkSync(std);
  mkdirSync(std); assert.throws(() => stdInventory(root), /type/); rmSync(std, { recursive: true });
  writeFileSync(std, ''); truncateSync(std, 64 * 1024 * 1024 + 1);
  assert.throws(() => stdInventory(root), /bound/);
});

test('ARMv7 std enumeration stops at the declared directory-entry bound', t => {
  const root = scratch(t);
  for (let i = 0; i < 257; i++) writeFileSync(join(root, `entry-${i}`), '');
  assert.throws(() => stdInventory(root), /directory entry bound/);
  assert.equal(readFileSync(join(root, 'entry-256')).length, 0);
});

test('ARMv7 Rust metadata admits empty feature rows only with the exact host/version/hard-float target', () => {
  validateRust(rustcVersion, cargoVersion, cfg);
  for (const [rust, cargo, target] of [
    [rustcVersion.replace(HOST, TARGET), cargoVersion, cfg],
    [rustcVersion.replace('release: 1.99.0', 'release: 1.98.0'), cargoVersion, cfg],
    [rustcVersion, cargoVersion.replace('1.99', '1.98'), cfg],
    [rustcVersion, cargoVersion, cfg.replace('eabihf', 'eabi')],
    [rustcVersion, cargoVersion, cfg + '\ntarget_arch="arm"'],
  ]) assert.throws(() => validateRust(rust, cargo, target));
});

test('ARMv7 APT observations enforce exact selected versions and reject duplicate/missing roots', () => {
  assert.equal(validateRoots(rootsText, installedText).length, 5);
  for (const [requested, installed] of [[rootsText.replace('1.0-1', '2.0-1'), installedText],
    [rootsText.split('\n').slice(1).join('\n'), installedText],
    [rootsText, installedText + installedText.split('\n')[0] + '\n']]) {
    assert.throws(() => validateRoots(requested, installed));
  }
  assert.equal(validateRoots(rootsText, installedText + 'unrelated\t1\tarmhf\n').length, 5);
});

test('ARMv7 printed unresolved optional names have no fabricated file identity; scripts are not traversed', t => {
  const root = scratch(t), path = join(root, 'libc.so');
  assert.throws(() => printedFile('libc.so'), /unresolved/);
  assert.deepEqual(printedFile('libatomic.so.1', { required: false }), {
    printed: 'libatomic.so.1', resolved: false, reason: 'unresolved basename; no file identity' });
  writeFileSync(path, '/* GNU ld script */\nGROUP ( /nonexistent/libc.so.6 AS_NEEDED ( /nonexistent/loader ) )\n');
  const row = printedFile(path);
  assert.equal(row.resolved, true); assert.equal(row.format, 'other');
  assert.deepEqual(row.absolute_script_references, ['/nonexistent/libc.so.6', '/nonexistent/loader']);
  assert.match(row.script_reference_scope, /not traversed/i);
});

test('ARMv7 printed path parsing allows one LF and rejects control spellings before filesystem resolution', t => {
  const root = scratch(t), path = join(root, 'supplier'); writeFileSync(path, 'supplier');
  assert.equal(printedFile(path + '\n').canonical, path);
  for (const reported of [path + '\r\n', path + '\n\n', '\t' + path + '\n',
    path + '\t\n', path + '\u007f\n', path + '\u0085\n']) {
    assert.throws(() => printedFile(reported), error => {
      assert.equal(error.message, 'ARMv7 preflight: printed file shape'); return true;
    });
  }
});

test('ARMv7 ELF prefix observations decode the declared byte order without inventing an unknown encoding', t => {
  const root = scratch(t), path = join(root, 'supplier'), bytes = Buffer.alloc(64);
  bytes.set([127, 69, 76, 70, 1, 1]); bytes.writeUInt16LE(40, 18); writeFileSync(path, bytes);
  assert.equal(printedFile(path).elf_machine, 40);
  bytes[5] = 2; bytes.writeUInt16BE(40, 18); writeFileSync(path, bytes);
  assert.equal(printedFile(path).elf_machine, 40);
  bytes[5] = 0; writeFileSync(path, bytes);
  assert.equal(printedFile(path).elf_machine, null);
});

test('ARMv7 create-only reports retain exact bytes/mode and never remove a colliding file', t => {
  const root = scratch(t), path = join(root, 'report.json');
  ownedWrite(path, { synthetic: true });
  const before = readFileSync(path); assert.equal(lstatSync(path).mode & 0o777, 0o600);
  assert.throws(() => ownedWrite(path, { changed: true }), /EEXIST/);
  assert.deepEqual(readFileSync(path), before);
  assert.throws(() => ownedWrite(join(root, 'too-large'), { value: 'x'.repeat(2 * 1024 * 1024) }), /bound/);
  assert.equal(existsSync(join(root, 'too-large')), false);
});

function fakeCapture(plan) {
  const child = new EventEmitter();
  // A PID read or any signal attempt is a regression: Node has no group handle.
  Object.defineProperty(child, 'pid', { get() { assert.fail('saved numeric PID accessed'); } });
  const destroyed = [], unreferenced = [];
  for (const name of ['stdout', 'stderr']) {
    child[name] = new EventEmitter(); child[name].destroy = () => destroyed.push(name);
  }
  child.unref = () => unreferenced.push(true);
  child.kill = () => assert.fail('Node must not signal a metadata child');
  const calls = [], timers = new Map();
  const options = {
    cwd: '/synthetic', env: { ONLY: 'synthetic' }, timeoutMs: 100, maxBytes: 16,
    spawnProcess: (...args) => { calls.push(args); queueMicrotask(() => plan(child, timers)); return child; },
    setTimer: callback => { const id = {}; timers.set(id, callback); return id; },
    clearTimer: id => timers.delete(id),
  };
  return { promise: capture('/synthetic/tool', ['--version'], options), calls, destroyed, unreferenced, timers };
}

test('ARMv7 command capture uses exact argv and closed env without creating a new process group', async () => {
  const f = fakeCapture(child => { child.stdout.emit('data', Buffer.from('ok')); child.emit('close', 0, null); });
  assert.deepEqual(await f.promise, { stdout: 'ok', stderr: '', bytes: 2 });
  assert.equal(f.calls.length, 1); assert.deepEqual(f.calls[0].slice(0, 2), ['/synthetic/tool', ['--version']]);
  assert.deepEqual(f.calls[0][2], { cwd: '/synthetic', env: { ONLY: 'synthetic' }, detached: false, stdio: ['ignore', 'pipe', 'pipe'] });
  assert.deepEqual(f.destroyed, []); assert.deepEqual(f.unreferenced, []); assert.equal(f.timers.size, 0);
});

for (const fault of ['overflow', 'combined-overflow', 'timeout', 'nonzero', 'invalid-utf8', 'spawn-error']) {
  test(`ARMv7 command capture contains synthetic ${fault} without leaking stderr`, async () => {
    const f = fakeCapture((child, timers) => {
      if (fault === 'overflow') child.stdout.emit('data', Buffer.alloc(17));
      else if (fault === 'combined-overflow') { child.stdout.emit('data', Buffer.alloc(9)); child.stderr.emit('data', Buffer.alloc(8)); }
      else if (fault === 'timeout') [...timers.values()][0]();
      else if (fault === 'spawn-error') child.emit('error', new Error('synthetic-private-body'));
      else {
        child.stderr.emit('data', fault === 'invalid-utf8' ? Buffer.from([255]) : Buffer.from('secret'));
        child.emit('close', fault === 'nonzero' ? 23 : 0, null);
      }
    });
    await assert.rejects(f.promise, error => {
      assert.match(error.message, /ARMv7 preflight:/); assert.ok(!error.message.includes('secret'));
      assert.ok(!error.message.includes('private-body')); return true;
    });
    const aborted = fault !== 'invalid-utf8';
    assert.deepEqual(f.destroyed, aborted ? ['stdout', 'stderr'] : []);
    assert.equal(f.unreferenced.length, aborted ? 1 : 0); assert.equal(f.timers.size, 0);
  });
}

test('ARMv7 failed capture releases handles and settles without waiting for an unowned close event', async () => {
  let held;
  const f = fakeCapture(child => { held = child; child.emit('error', new Error('synthetic')); });
  await assert.rejects(f.promise, /spawn failed/);
  assert.deepEqual(f.destroyed, ['stdout', 'stderr']); assert.equal(f.unreferenced.length, 1);
  // Late events cannot resolve the rejected capture or repeat its cleanup.
  held.stdout.emit('data', Buffer.from('late'));
  held.emit('close', 0, null); held.emit('error', new Error('late private body'));
  assert.deepEqual(f.destroyed, ['stdout', 'stderr']); assert.equal(f.unreferenced.length, 1);
  assert.equal(f.timers.size, 0);
});

test('ARMv7 capture never signals a reaped leader while its streams remain open', async () => {
  const f = fakeCapture((child, timers) => { child.emit('exit', 0, null); [...timers.values()][0](); });
  await assert.rejects(f.promise, /metadata timeout/);
  assert.deepEqual(f.destroyed, ['stdout', 'stderr']); assert.equal(f.unreferenced.length, 1);
});

test('ARMv7 stream errors release handles and never expose error payloads', async () => {
  const f = fakeCapture(child => child.stderr.emit('error', new Error('synthetic-private-body')));
  await assert.rejects(f.promise, error => {
    assert.equal(error.message, 'ARMv7 preflight: metadata stream failed'); return true;
  });
  assert.deepEqual(f.destroyed, ['stdout', 'stderr']); assert.equal(f.unreferenced.length, 1);
});

test('ARMv7 synchronous spawn refusal settles without a child or numeric PID', async () => {
  let timers = 0;
  await assert.rejects(capture('/synthetic/tool', [], {
    spawnProcess: () => { throw new Error('synthetic-private-body'); },
    setTimer: () => { timers++; }, clearTimer: () => {},
  }), error => { assert.equal(error.message, 'ARMv7 preflight: metadata spawn failed'); return true; });
  assert.equal(timers, 0);
});

test('ARMv7 CLI requires the pinned Node version and explicit native launch contract', () => {
  validateNativeLaunch('v22.14.0', { AUTOROUTER_ARMV7_NATIVE_OWNER: '1' });
  for (const version of ['v22.14.1', 'v24.0.0', '22.14.0']) {
    assert.throws(() => validateNativeLaunch(version, { AUTOROUTER_ARMV7_NATIVE_OWNER: '1' }), /Node version/);
  }
  for (const value of [undefined, '', '0', 'true', '1\n']) {
    assert.throws(() => validateNativeLaunch('v22.14.0', { AUTOROUTER_ARMV7_NATIVE_OWNER: value }), /native owner/);
  }
});

test('ARMv7 failure diagnostics retain safe stage and reason while excluding arbitrary metadata bodies', () => {
  const error = new Error('ARMv7 preflight: metadata timeout'); error.stage = 'gcc-version';
  assert.deepEqual(refusalDiagnostics(error), {
    kind: 'ARMv7_preflight_refusal', stage: 'gcc-version', reason: 'ARMv7 preflight: metadata timeout' });
  const unsafe = new Error('private stderr\nsecret value'); unsafe.stage = 'gcc-version\nsecret';
  assert.deepEqual(refusalDiagnostics(unsafe), {
    kind: 'ARMv7_preflight_refusal', stage: null, reason: 'ARMv7 preflight: refused' });
});

function wholeFixture(t, mode = 'success') {
  const root = scratch(t), bin = join(root, 'bin'), home = join(root, 'home');
  const cargo = join(home, '.cargo'), rustup = join(home, '.rustup'), toolchain = join(rustup, 'toolchains', TOOLCHAIN);
  const out = join(root, 'artifacts/rust-rewrite/armv7-preflight'), output = join(out, 'observations.json');
  for (const path of [bin, cargo, join(toolchain, 'bin'), join(toolchain, 'lib/rustlib', TARGET, 'lib'),
    join(root, 'rust/.cargo'), join(root, '.github/workflows'), join(root, 'rust/distribution/ci'), out, join(root, 'sdk')]) mkdirSync(path, { recursive: true });
  const put = (path, value) => { writeFileSync(path, value); return path; };
  for (const name of ['git', 'rustup', 'rustc', 'cargo', 'dpkg-query', 'arm-linux-gnueabihf-gcc',
    'arm-linux-gnueabihf-ar', 'arm-linux-gnueabihf-ranlib', 'arm-linux-gnueabihf-ld', 'arm-linux-gnueabihf-readelf']) {
    put(join(bin, name), 'synthetic tool ' + name); chmodSync(join(bin, name), 0o700);
  }
  for (const name of ['rustc', 'cargo']) { put(join(toolchain, 'bin', name), 'actual synthetic ' + name); chmodSync(join(toolchain, 'bin', name), 0o700); }
  put(join(toolchain, 'lib/librustc_driver-abc.so'), 'synthetic compiler library');
  put(join(rustup, 'settings.toml'), 'synthetic settings');
  const core = put(join(toolchain, 'lib/rustlib', TARGET, 'lib/libcore-abc.rlib'), 'core');
  put(join(toolchain, 'lib/rustlib', TARGET, 'lib/libstd-abc.rlib'), 'std');
  for (const name of ['.github/workflows/rust-armv7.yml', 'rust/distribution/ci/armv7-preflight.mjs',
    'rust/Cargo.lock', 'rust/distribution/platforms.json']) put(join(root, name), 'synthetic source ' + name);
  put(join(root, 'rust/Cargo.lock'), 'version = 4\n[[package]]\nname = "cc"\nversion = "1.6.0"\nchecksum = "f74872d07caf508b30a21f6836e7d7016a2eaf7d9ff4f48deaa58cd8a0407630"\n');
  put(join(root, 'rust/rust-toolchain.toml'), '[toolchain]\nchannel = "1.99.0"\n');
  put(join(root, 'rust/.cargo/config.toml'), '[alias]\nxtask = "run --package xtask --"\n');
  const commit = 'a'.repeat(40);
  put(join(out, 'source-commit.txt'), commit + '\n'); put(join(out, 'source-status.txt'), '');
  put(join(out, 'requested-root-versions.tsv'), rootsText); put(join(out, 'installed-packages.tsv'), installedText);
  for (const name of ['apt-policy.txt', 'apt-index-targets.txt', 'os-release.txt', 'kernel.txt']) put(join(out, name), 'synthetic setup');
  const ambient = { PATH: bin, HOME: home, CARGO_HOME: cargo, RUSTUP_HOME: rustup, RUSTUP_TOOLCHAIN: TOOLCHAIN,
    RUNNER_OS: 'Linux', RUNNER_ARCH: 'X64', ImageOS: 'synthetic-ubuntu24', ImageVersion: 'synthetic',
    GITHUB_REPOSITORY: 'synthetic/project', GITHUB_SHA: commit, GITHUB_WORKFLOW_SHA: commit,
    GITHUB_REF: 'refs/pull/1/merge', GITHUB_WORKFLOW: 'Rust ARMv7 cross-tool observations',
    GITHUB_WORKFLOW_REF: 'synthetic/project/.github/workflows/rust-armv7.yml@refs/pull/1/merge',
    GITHUB_RUN_ID: '123', GITHUB_RUN_ATTEMPT: '1', GITHUB_SERVER_URL: 'https://github.com',
    FAKE_API_KEY: 'synthetic-secret' };
  const calls = [];
  const captureCommand = async (executable, args, options) => {
    calls.push({ executable, args }); assert.equal(options.env.FAKE_API_KEY, undefined);
    assert.ok(options.timeoutMs > 0 && options.timeoutMs <= 15000); assert.equal(options.cwd, join(root, 'rust'));
    const role = basename(executable); let stdout;
    if (role === 'git') stdout = args[0] === 'rev-parse' ? commit : '';
    else if (role === 'rustup') stdout = join(toolchain, 'bin', args.at(-1));
    else if (role === 'rustc') {
      if (args[0] === '-vV') stdout = rustcVersion;
      else if (args[1] === 'cfg') stdout = cfg;
      else if (args[1] === 'target-cpus') stdout = 'generic - default';
      else { assert.deepEqual(args, ['--print', 'sysroot']); stdout = toolchain; }
    } else if (role === 'cargo') { assert.deepEqual(args, ['-vV']); stdout = cargoVersion; }
    else if (role === 'dpkg-query') stdout = mode === 'changed-APT' && calls.filter(row => basename(row.executable) === role).length === 2
      ? installedText.replace('1.0-1', '2.0-1') : installedText;
    else if (role === 'arm-linux-gnueabihf-gcc') {
      if (args[0] === '-dumpmachine') stdout = mode === 'host-gcc' ? HOST : 'arm-linux-gnueabihf';
      else if (args[0].startsWith('-print-file-name=')) {
        const name = args[0].slice('-print-file-name='.length);
        stdout = name === 'libatomic.so.1' ? name : put(join(root, 'sdk', name), 'synthetic selected file');
        if (mode === 'unresolved-CRT' && name === 'crt1.o') stdout = name;
      } else if (args[0].startsWith('-print-prog-name=')) stdout = join(bin, 'arm-linux-gnueabihf-gcc');
      else {
        stdout = args[0] === '-print-sysroot' ? '/' : 'synthetic metadata';
        if (mode === 'changed-source' && args[0] === '--version') put(join(root, 'rust/Cargo.lock'), 'changed source');
        if (mode === 'changed-std' && args[0] === '--version') put(core, 'changed std');
        if (mode === 'changed-tool' && args[0] === '--version') put(join(bin, 'arm-linux-gnueabihf-gcc'), 'changed compiler bytes');
        if (mode === 'metadata-failure' && args[0] === '--version') throw new Error('ARMv7 preflight: metadata timeout');
      }
    } else { assert.match(role, /^arm-linux-gnueabihf-(?:ar|ranlib|ld|readelf)$/); assert.deepEqual(args, ['--version']); stdout = 'synthetic version'; }
    stdout += '\n'; return { stdout, stderr: '', bytes: Buffer.byteLength(stdout) };
  };
  return { root, output, ambient, calls, captureCommand };
}

test('ARMv7 complete synthetic preflight binds real fixture files and makes only metadata/preprocess calls', async t => {
  const f = wholeFixture(t); await probe(f.root, f.output, f.ambient, f);
  const report = JSON.parse(readFileSync(f.output));
  assert.equal(report.passed, true); assert.equal(report.portable_cpu, false);
  assert.equal(report.production_release_approved, false); assert.equal(report.target, TARGET);
  assert.equal(report.selected_linker_files['libatomic.so.1'].resolved, false);
  assert.equal(report.commands.find(row => row.label === 'gcc-sysroot').stdout.trim(), '/');
  assert.equal(report.selected_APT_roots.length, 5); assert.equal(report.target_std.files.length, 2);
  assert.equal(report.commands.length, 36); assert.equal(f.calls.length, 36);
  assert.deepEqual(f.calls.filter(row => basename(row.executable) === 'cargo').map(row => row.args), [['-vV']]);
  assert.deepEqual(f.calls.find(row => row.args.includes('-mfpu=vfpv3-d16')).args,
    [...SOURCE_CC_FLAGS, '-dM', '-E', '-x', 'c', '-']);
  assert.equal(lstatSync(f.output).mode & 0o777, 0o600);
  assert.ok(!readFileSync(f.output, 'utf8').includes('synthetic-secret'));
});

for (const mode of ['host-gcc', 'unresolved-CRT', 'changed-source', 'changed-std', 'changed-APT', 'metadata-failure', 'changed-tool']) {
  test(`ARMv7 whole synthetic preflight refuses ${mode} without publishing success`, async t => {
    const f = wholeFixture(t, mode);
    await assert.rejects(probe(f.root, f.output, f.ambient, f), error => {
      assert.equal(error.partial.passed, false); assert.equal(error.partial.production_release_approved, false);
      assert.ok(!JSON.stringify(error.partial).includes('synthetic-secret'));
      if (mode === 'metadata-failure') {
        assert.equal(error.stage, 'gcc-version');
        assert.equal(error.partial.completed_commands.some(row => row.label === 'gcc-version'), false);
      }
      return true;
    });
    assert.equal(existsSync(f.output), false);
    assert.ok(f.calls.every(row => !row.args.includes('build') && !row.args.includes('--emit')));
  });
}
