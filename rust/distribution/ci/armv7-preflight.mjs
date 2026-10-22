// Native-owned build-tool observations. This never builds or executes an ARM executable.
import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { closeSync, constants, fstatSync, fsyncSync, lstatSync, openSync,
  readSync, opendirSync, realpathSync, unlinkSync, writeSync } from 'node:fs';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const NODE_VERSION = 'v22.14.0';
export function validateNativeLaunch(version, ambient) {
  check(version === NODE_VERSION, 'Node version');
  check(ambient.AUTOROUTER_ARMV7_NATIVE_OWNER === '1', 'native owner launch required');
}

export const TARGET = 'armv7-unknown-linux-gnueabihf';
export const HOST = 'x86_64-unknown-linux-gnu';
export const TOOLCHAIN = '1.99.0-' + HOST;
export const ROOT_PACKAGES = Object.freeze(['gcc-arm-linux-gnueabihf', 'binutils-arm-linux-gnueabihf',
  'libc6-dev-armhf-cross', 'make', 'perl']);
export const SOURCE_CC_FLAGS = Object.freeze(['-march=armv7-a', '-mfpu=vfpv3-d16', '-mfloat-abi=hard']);
const CC_CHECKSUM = 'f74872d07caf508b30a21f6836e7d7016a2eaf7d9ff4f48deaa58cd8a0407630';
const MIB = 1024 * 1024;
const PUBLIC = ['GITHUB_REPOSITORY', 'GITHUB_SHA', 'GITHUB_REF', 'GITHUB_WORKFLOW',
  'GITHUB_WORKFLOW_REF', 'GITHUB_WORKFLOW_SHA', 'GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT',
  'GITHUB_SERVER_URL'];
const SOURCES = ['.github/workflows/rust-armv7.yml', 'rust/distribution/ci/armv7-preflight.mjs',
  'rust/Cargo.lock', 'rust/rust-toolchain.toml', 'rust/.cargo/config.toml',
  'rust/distribution/platforms.json'];
const same = (a, b) => ['dev', 'ino', 'size', 'mtimeNs', 'ctimeNs', 'mode', 'nlink']
  .every(key => a[key] === b[key]);
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
function reject(code) { throw new Error('ARMv7 preflight: ' + code); }
function check(ok, code) { if (!ok) reject(code); }
function text(bytes) {
  try { return new TextDecoder('utf-8', { fatal: true }).decode(bytes); }
  catch { reject('invalid UTF8 metadata'); }
}

// No ambient credential, compiler flag, library path or target runner is forwarded.
export function selectEnvironment(input) {
  check(Object.entries(input).length <= 512, 'ambient environment count');
  for (const [key, value] of Object.entries(input)) {
    const compiler = /^(?:RUSTC|RUSTDOC|RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS|CC|CXX|AR|RANLIB|CFLAGS|CXXFLAGS|CPPFLAGS|LDFLAGS|LIBRARY_PATH|LD_LIBRARY_PATH|COMPILER_PATH|GCC_EXEC_PREFIX|CPATH|C_INCLUDE_PATH|CPLUS_INCLUDE_PATH)(?:$|_)/.test(key)
      || /^(?:CARGO_BUILD_|CARGO_TARGET_|CARGO_PROFILE_|OPENSSL_|PKG_CONFIG|RUSTUP_DIST_SERVER$|RUSTUP_UPDATE_ROOT$)/.test(key);
    check(!compiler || value === '', 'ambient compiler or target override');
  }
  const env = { PATH: input.PATH, HOME: input.HOME, CARGO_HOME: input.CARGO_HOME,
    RUSTUP_HOME: input.RUSTUP_HOME, RUSTUP_TOOLCHAIN: input.RUSTUP_TOOLCHAIN,
    RUSTUP_AUTO_INSTALL: '0', LC_ALL: 'C', LANG: 'C', GIT_CONFIG_NOSYSTEM: '1',
    GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_COUNT: '0', GIT_TERMINAL_PROMPT: '0' };
  for (const name of ['PATH', 'HOME', 'CARGO_HOME', 'RUSTUP_HOME', 'RUSTUP_TOOLCHAIN']) {
    check(typeof env[name] === 'string' && env[name].length > 0 && env[name].length <= 8192
      && !/[\0\r\n]/.test(env[name]), 'selected environment shape');
  }
  check(env.RUSTUP_TOOLCHAIN === TOOLCHAIN, 'toolchain selection');
  check(env.PATH.split(':').every(entry => entry && isAbsolute(entry)), 'absolute PATH');
  for (const key of ['HOME', 'CARGO_HOME', 'RUSTUP_HOME']) check(isAbsolute(env[key]), 'absolute home');
  return env;
}

export function witness(path, limit, { contents = false, prefix = 0, tick = () => {} } = {}) {
  check(isAbsolute(path) && Number.isSafeInteger(limit) && limit >= 0, 'file request');
  const pre = lstatSync(path, { bigint: true });
  check(pre.isFile() && !pre.isSymbolicLink() && pre.size <= BigInt(limit), 'file type or byte bound');
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  const hash = createHash('sha256'), chunks = [];
  let count = 0, head = Buffer.alloc(0);
  try {
    check(same(pre, fstatSync(fd, { bigint: true })), 'file replaced before read');
    const buffer = Buffer.alloc(65536);
    while (true) {
      tick();
      const n = readSync(fd, buffer, 0, Math.min(buffer.length, limit - count + 1), null);
      if (!n) break;
      count += n; check(count <= limit, 'file grew beyond bound');
      const part = buffer.subarray(0, n); hash.update(part);
      if (contents) chunks.push(Buffer.from(part));
      if (head.length < prefix) head = Buffer.concat([head, part.subarray(0, prefix - head.length)]);
    }
    check(same(pre, fstatSync(fd, { bigint: true })) && same(pre, lstatSync(path, { bigint: true }))
      && BigInt(count) === pre.size, 'file changed while read');
  } finally { closeSync(fd); }
  return { row: { path, bytes: count, sha256: hash.digest('hex'), mode: Number(pre.mode & 0o7777n) },
    ...(contents ? { bytes: Buffer.concat(chunks) } : {}), ...(prefix ? { prefix: head } : {}) };
}

export function resolveTool(name, env, tick = () => {}) {
  check(/^[a-z0-9+-]+$/.test(name), 'tool name');
  for (const directory of env.PATH.split(':')) {
    const invocation = join(directory, name);
    let info;
    try { info = lstatSync(invocation); } catch (error) { if (error.code === 'ENOENT') continue; throw error; }
    if (!info.isFile() && !info.isSymbolicLink()) continue;
    const canonical = realpathSync(invocation), selected = lstatSync(canonical);
    if (!selected.isFile() || !(selected.mode & 0o111)) continue;
    return { invocation, canonical, file: witness(canonical, 512 * MIB, { tick }).row };
  }
  reject('required tool absent');
}

// Only the fixed native auditor launcher owns the collector's process group.
// Metadata children inherit that group. Node owns its pipe handles, not a saved
// numeric PID/PGID; on failure it releases those handles and lets the collector
// exit so the native owner can clean the reserved group before reaping it.
export function capture(executable, args, { cwd, env, timeoutMs = 15000, maxBytes = 65536,
  spawnProcess = spawn, setTimer = setTimeout, clearTimer = clearTimeout } = {}) {
  check(isAbsolute(executable) && Number.isInteger(timeoutMs) && timeoutMs > 0 && timeoutMs <= 15000
    && Number.isInteger(maxBytes) && maxBytes > 0 && maxBytes <= MIB, 'command bounds');
  return new Promise((resolvePromise, rejectPromise) => {
    let child, timer, total = 0, done = false;
    const stdout = [], stderr = [];
    const stop = code => {
      if (done) return; done = true; clearTimer(timer);
      // No process.kill/child.kill or process-group liveness inference occurs
      // here. This is cooperative handle release, not descendant cleanup.
      for (const stream of [child?.stdout, child?.stderr]) {
        try { stream?.destroy(); } catch { /* Native owner still owns cleanup. */ }
      }
      try { child?.unref(); } catch { /* Native owner still owns cleanup. */ }
      stdout.length = 0; stderr.length = 0;
      rejectPromise(new Error('ARMv7 preflight: ' + code));
    };
    const finish = (code, signal) => {
      if (done) return;
      if (code !== 0 || signal) { stop('metadata command failed'); return; }
      done = true; clearTimer(timer);
      try { resolvePromise({ stdout: text(Buffer.concat(stdout)), stderr: text(Buffer.concat(stderr)), bytes: total }); }
      catch (error) { rejectPromise(error); }
    };
    try {
      child = spawnProcess(executable, args, { cwd, env, detached: false, stdio: ['ignore', 'pipe', 'pipe'] });
      timer = setTimer(() => stop('metadata timeout'), timeoutMs);
      for (const [stream, chunks] of [[child.stdout, stdout], [child.stderr, stderr]]) {
        stream.on('error', () => stop('metadata stream failed'));
        stream.on('data', chunk => {
          if (done) return;
          total += chunk.length;
          if (total > maxBytes) { stop('metadata output bound'); return; }
          chunks.push(Buffer.from(chunk));
        });
      }
      child.on('error', () => stop('metadata spawn failed'));
      child.on('close', finish);
    } catch { stop('metadata spawn failed'); }
  });
}

export function validateRoots(requested, installed) {
  const parse = (value, width) => value.trimEnd().split('\n').map(line => {
    const fields = line.split('\t');
    check(fields.length === width && fields.every(field => /^[0-9A-Za-z.+:~_-]+$/.test(field)), 'APT inventory row');
    return fields;
  });
  const roots = parse(requested, 2), all = parse(installed, 3), entries = new Map();
  check(roots.length === ROOT_PACKAGES.length && roots.every(([name], i) => name === ROOT_PACKAGES[i]), 'APT selected roots');
  check(all.length > roots.length && all.length <= 8192, 'APT installed count');
  for (const [name, version, arch] of all) {
    const key = name + '\t' + arch;
    check(!entries.has(key), 'APT duplicate package'); entries.set(key, version);
  }
  return roots.map(([name, version]) => {
    const matches = all.filter(row => row[0] === name);
    check(matches.length === 1 && matches[0][1] === version, 'APT selected version mismatch');
    return { name, version, architecture: matches[0][2] };
  });
}

export function validateRust(rustc, cargo, cfg) {
  check(/^release: 1\.99\.0$/m.test(rustc) && /^host: x86_64-unknown-linux-gnu$/m.test(rustc)
    && /^cargo 1\.99\.0 \(/.test(cargo), 'actual Rust host or version');
  const lines = cfg.trimEnd().split('\n'); check(new Set(lines).size === lines.length, 'duplicate target cfg');
  for (const row of ['target_abi="eabihf"', 'target_arch="arm"', 'target_endian="little"',
    'target_env="gnu"', 'target_os="linux"', 'target_pointer_width="32"']) check(lines.includes(row), 'ARM target cfg');
}

export function stdInventory(path, tick = () => {}) {
  const stat = lstatSync(path); check(stat.isDirectory() && !stat.isSymbolicLink()
    && realpathSync(path) === path, 'target std directory');
  const names = directoryNames(path, 256, tick); check(names.length > 1, 'target std count');
  let total = 0;
  const files = names.map(name => {
    check(/^[A-Za-z0-9_.+-]+$/.test(name), 'target std name');
    const row = witness(join(path, name), 64 * MIB, { tick }).row;
    total += row.bytes; check(total <= 256 * MIB, 'target std aggregate'); return row;
  });
  check(names.some(name => /^libcore-[a-z0-9]+\.rlib$/.test(name))
    && names.some(name => /^libstd-[a-z0-9]+\.rlib$/.test(name)), 'target core or std absent');
  return { files, total_bytes: total };
}

function directoryNames(path, maximum, tick) {
  const directory = opendirSync(path), names = [];
  try {
    for (let entry; (entry = directory.readSync()) !== null;) {
      tick(); check(names.length < maximum, 'directory entry bound'); names.push(entry.name);
    }
  } finally { directory.closeSync(); }
  return names.sort();
}

function printedPath(printed) {
  // Linux fixed commands normally add one LF; CRLF and other control bytes are
  // not silently normalized into a different pathname.
  const path = printed.endsWith('\n') ? printed.slice(0, -1) : printed;
  check(path.length > 0 && path.length <= 8192 && !/[\u0000-\u001f\u007f-\u009f]/.test(path), 'printed file shape');
  return path;
}

export function printedFile(printed, { required = true, tick = () => {} } = {}) {
  const path = printedPath(printed);
  if (!isAbsolute(path)) {
    check(!required && /^[A-Za-z0-9_.+-]+$/.test(path), 'unresolved required printed file');
    return { printed: path, resolved: false, reason: 'unresolved basename; no file identity' };
  }
  const canonical = realpathSync(path), result = witness(canonical, 512 * MIB, { prefix: 65536, tick });
  const head = result.prefix, elf = head.subarray(0, 4).equals(Buffer.from([127, 69, 76, 70]));
  return { printed: path, resolved: true, canonical, file: result.row,
    format: elf ? 'ELF' : head.subarray(0, 8).toString() === '!<arch>\n' ? 'archive' : 'other',
    ...(elf && head.length >= 20 ? { elf_class: head[4], elf_endian: head[5],
      elf_machine: head[5] === 1 ? head.readUInt16LE(18) : head[5] === 2 ? head.readUInt16BE(18) : null } : {}),
    // References are data only; this first slice does not traverse a linker script.
    ...(!elf && head.subarray(0, 64).toString('ascii').includes('GNU ld script') ? {
      absolute_script_references: [...new Set(text(head).replace(/\/\*[\s\S]*?\*\//g, '').match(/\/[^\s()]+/g) ?? [])].slice(0, 16),
      script_reference_scope: 'First64KiB references only, not traversed or qualified; selected file SHA binds remaining bytes' } : {}) };
}

export function ownedWrite(path, value) {
  const raw = Buffer.from(JSON.stringify(value, null, 2) + '\n'); check(raw.length <= 2 * MIB, 'report byte bound');
  const parent = lstatSync(dirname(path), { bigint: true }); check(parent.isDirectory() && !parent.isSymbolicLink(), 'output directory');
  let fd, owned;
  try {
    fd = openSync(path, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
    owned = fstatSync(fd, { bigint: true });
    let offset = 0; while (offset < raw.length) { const n = writeSync(fd, raw, offset); check(n > 0, 'report write'); offset += n; }
    fsyncSync(fd); closeSync(fd); fd = undefined;
    const after = lstatSync(dirname(path), { bigint: true });
    check(parent.dev === after.dev && parent.ino === after.ino && parent.mode === after.mode, 'output directory replaced');
    const info = lstatSync(path, { bigint: true });
    check(info.dev === owned.dev && info.ino === owned.ino && info.mode === owned.mode, 'output file replaced');
    check(witness(path, 2 * MIB).row.sha256 === digest(raw), 'retained report bytes');
  } catch (error) {
    if (fd !== undefined) closeSync(fd);
    if (owned) {
      try { const now = lstatSync(path, { bigint: true });
        if (now.dev === owned.dev && now.ino === owned.ino) unlinkSync(path);
      } catch (cleanup) { if (cleanup.code !== 'ENOENT') throw cleanup; }
    }
    throw error;
  }
}

export async function probe(root, output, ambient = process.env, { captureCommand = capture } = {}) {
  root = realpathSync(root);
  check(isAbsolute(output) && dirname(output) === join(root, 'artifacts/rust-rewrite/armv7-preflight'), 'fixed output location');
  try { lstatSync(output); reject('output already exists'); } catch (error) { if (error.code !== 'ENOENT') throw error; }
  const started = performance.now(), deadline = started + 180000;
  const tick = () => check(performance.now() < deadline, 'whole probe deadline');
  const env = selectEnvironment(ambient), cwd = join(root, 'rust');
  check(ambient.RUNNER_OS === 'Linux' && ambient.RUNNER_ARCH === 'X64', 'actual hosted runner');
  const publicWorkflow = {};
  for (const key of PUBLIC) {
    check(typeof ambient[key] === 'string' && ambient[key].length > 0 && ambient[key].length <= 2048
      && !/[\0\r\n]/.test(ambient[key]), 'public workflow metadata');
    publicWorkflow[key] = ambient[key];
  }
  check(publicWorkflow.GITHUB_SERVER_URL === 'https://github.com'
    && /^[1-9][0-9]*$/.test(publicWorkflow.GITHUB_RUN_ID) && /^[1-9][0-9]*$/.test(publicWorkflow.GITHUB_RUN_ATTEMPT)
    && /^[a-f0-9]{40}$/.test(publicWorkflow.GITHUB_SHA) && /^[a-f0-9]{40}$/.test(publicWorkflow.GITHUB_WORKFLOW_SHA)
    && publicWorkflow.GITHUB_WORKFLOW_REF.startsWith(publicWorkflow.GITHUB_REPOSITORY + '/.github/workflows/rust-armv7.yml@'), 'workflow association');
  for (const key of ['ImageOS', 'ImageVersion']) check(typeof ambient[key] === 'string'
    && ambient[key].length <= 256 && !/[\0\r\n]/.test(ambient[key]), 'runner image metadata');
  for (const name of ['HOME', 'CARGO_HOME', 'RUSTUP_HOME']) {
    check(realpathSync(env[name]) === env[name] && lstatSync(env[name]).isDirectory(), 'canonical selected home');
  }
  for (let parent = cwd; ; parent = dirname(parent)) {
    for (const leaf of ['config', 'config.toml']) {
      const path = join(parent, '.cargo', leaf);
      if (path !== join(cwd, '.cargo/config.toml')) {
        try { lstatSync(path); reject('unreviewed Cargo config'); } catch (error) { if (error.code !== 'ENOENT') throw error; }
      }
    }
    if (dirname(parent) === parent) break;
  }
  for (const leaf of ['config', 'config.toml']) {
    try { lstatSync(join(env.CARGO_HOME, leaf)); reject('Cargo home config'); }
    catch (error) { if (error.code !== 'ENOENT') throw error; }
  }
  const tools = Object.fromEntries(['git', 'rustup', 'rustc', 'cargo', 'dpkg-query',
    'arm-linux-gnueabihf-gcc', 'arm-linux-gnueabihf-ar', 'arm-linux-gnueabihf-ranlib',
    'arm-linux-gnueabihf-ld', 'arm-linux-gnueabihf-readelf'].map(name => [name, resolveTool(name, env, tick)]));
  const commands = [];
  let aggregate = 0;
  const run = async (label, executable, args, maxBytes = 65536, preserveWhitespace = false) => {
    tick(); check(commands.length < 48, 'command inventory bound');
    let result;
    try { result = await captureCommand(executable, args, { cwd, env, maxBytes,
      timeoutMs: Math.max(1, Math.min(15000, Math.floor(deadline - performance.now()))) }); }
    catch (error) { error.stage = label; throw error; }
    aggregate += result.bytes; check(aggregate <= 4 * MIB, 'metadata aggregate bound');
    commands.push({ label, executable, args, ...result });
    return preserveWhitespace ? result.stdout : result.stdout.trim();
  };
  const git = async args => run('git', tools.git.invocation, args);
  try {
    const commit = await git(['rev-parse', 'HEAD']); check(/^[a-f0-9]{40}$/.test(commit), 'Git commit');
    check(await git(['status', '--porcelain']) === '', 'source worktree not clean');
    const sources = () => SOURCES.map(name => ({ ...witness(join(root, name), 8 * MIB, { tick }).row, path: name }));
    const before = sources();
    const lock = text(witness(join(cwd, 'Cargo.lock'), 8 * MIB, { contents: true, tick }).bytes);
    const ccBlocks = lock.split('[[package]]').filter(block => /^name = "cc"$/m.test(block));
    check(ccBlocks.length === 1 && /^version = "1\.6\.0"$/m.test(ccBlocks[0])
      && ccBlocks[0].includes('checksum = "' + CC_CHECKSUM + '"'), 'source-backed cc lock binding');
    const initialCommit = witness(join(dirname(output), 'source-commit.txt'), 128, { contents: true, tick });
    check(text(initialCommit.bytes).trim() === commit
      && witness(join(dirname(output), 'source-status.txt'), 65536, { tick }).row.bytes === 0, 'initial source differs');
    const channel = text(witness(join(cwd, 'rust-toolchain.toml'), 65536, { contents: true }).bytes);
    check(channel.match(/^channel = "1\.99\.0"$/gm)?.length === 1, 'workspace pinned Rust version');
    const actual = {};
    for (const name of ['rustc', 'cargo']) {
      const selected = printedPath(await run('rustup-which-' + name, tools.rustup.invocation,
        ['which', '--toolchain', TOOLCHAIN, name], 65536, true));
      const expected = join(env.RUSTUP_HOME, 'toolchains', TOOLCHAIN, 'bin', name);
      check(selected === expected && realpathSync(selected) === selected, 'actual toolchain path');
      actual[name] = { invocation: selected, file: witness(selected, 512 * MIB, { tick }).row };
    }
    const rustc = (label, args, preserveWhitespace = false) => run(label, actual.rustc.invocation, args, 65536, preserveWhitespace);
    const rustVersion = await rustc('rustc-version', ['-vV']);
    const cargoVersion = await run('cargo-version', actual.cargo.invocation, ['-vV']);
    const cfg = await rustc('target-cfg', ['--print', 'cfg', '--target', TARGET]);
    validateRust(rustVersion, cargoVersion, cfg);
    await rustc('target-cpus', ['--print', 'target-cpus', '--target', TARGET]);
    const sysroot = printedPath(await rustc('rust-sysroot', ['--print', 'sysroot'], true));
    check(sysroot === join(env.RUSTUP_HOME, 'toolchains', TOOLCHAIN), 'actual Rust sysroot');
    const std = stdInventory(join(sysroot, 'lib/rustlib', TARGET, 'lib'), tick);
    const compilerLibraries = directoryNames(join(sysroot, 'lib'), 256, tick).filter(name => /^librustc_driver-.*\.so$/.test(name));
    check(compilerLibraries.length === 1, 'host compiler library inventory');
    const compilerLibrary = witness(join(sysroot, 'lib', compilerLibraries[0]), 512 * MIB, { tick }).row;
    const rustupSettings = witness(join(env.RUSTUP_HOME, 'settings.toml'), 65536, { tick }).row;
    const out = dirname(output), requested = witness(join(out, 'requested-root-versions.tsv'), 65536, { contents: true, tick });
    const installed = await run('dpkg-installed', tools['dpkg-query'].invocation,
      ['-W', '-f=${Package}\t${Version}\t${Architecture}\n'], MIB);
    const selectedRoots = validateRoots(text(requested.bytes), installed);
    const setupFiles = {};
    for (const [name, limit] of [['apt-policy.txt', 65536], ['apt-index-targets.txt', 4 * MIB],
      ['installed-packages.tsv', MIB], ['os-release.txt', 65536], ['kernel.txt', 4096]]) {
      setupFiles[name] = witness(join(out, name), limit, { tick }).row;
    }
    check(text(witness(join(out, 'installed-packages.tsv'), MIB, { contents: true, tick }).bytes).trim() === installed, 'initial package inventory differs');
    const observations = {};
    const cc = (label, args, preserveWhitespace = false) => run(label, tools['arm-linux-gnueabihf-gcc'].invocation, args, 65536, preserveWhitespace);
    check(await cc('gcc-target', ['-dumpmachine']) === 'arm-linux-gnueabihf', 'cross GCC target');
    for (const [label, args] of [['gcc-version', ['--version']], ['gcc-sysroot', ['-print-sysroot']],
      ['gcc-search-dirs', ['-print-search-dirs']], ['gcc-default-options', ['-Q', '--help=target']],
      ['gcc-default-macros', ['-dM', '-E', '-x', 'c', '-']],
      ['gcc-source-cc-macros', [...SOURCE_CC_FLAGS, '-dM', '-E', '-x', 'c', '-']]]) await cc(label, args);
    for (const name of ['ar', 'ranlib', 'ld', 'readelf']) await run('cross-' + name + '-version', tools['arm-linux-gnueabihf-' + name].invocation, ['--version']);
    for (const name of ['crt1.o', 'crti.o', 'crtn.o', 'libgcc.a', 'libc.so', 'libgcc_s.so.1', 'libatomic.so.1', 'ld-linux-armhf.so.3']) {
      const printed = await cc('gcc-file-' + name, ['-print-file-name=' + name], true);
      observations[name] = printedFile(printed, { required: ['crt1.o', 'crti.o', 'crtn.o', 'libgcc.a', 'libc.so'].includes(name), tick });
    }
    for (const name of ['cc1', 'as', 'collect2', 'ld']) observations['program-' + name] = printedFile(await cc('gcc-program-' + name, ['-print-prog-name=' + name], true), { tick });
    check(await run('dpkg-installed-after', tools['dpkg-query'].invocation,
      ['-W', '-f=${Package}\t${Version}\t${Architecture}\n'], MIB) === installed, 'installed packages changed');
    check(await git(['rev-parse', 'HEAD']) === commit && await git(['status', '--porcelain']) === '', 'source changed');
    check(JSON.stringify(before) === JSON.stringify(sources()), 'source bytes changed');
    // Rehash tools/std after commands. No compiler dynamic-library closure claim.
    for (const [name, value] of Object.entries(tools)) check(JSON.stringify(resolveTool(name, env, tick)) === JSON.stringify(value), 'tool invocation changed');
    for (const value of [...Object.values(actual), { file: compilerLibrary }, { file: rustupSettings }]) check(witness(value.file.path, 512 * MIB, { tick }).row.sha256 === value.file.sha256, 'tool changed');
    check(witness(requested.row.path, 65536, { tick }).row.sha256 === requested.row.sha256, 'selected roots changed');
    check(JSON.stringify(stdInventory(join(sysroot, 'lib/rustlib', TARGET, 'lib'), tick)) === JSON.stringify(std), 'target std changed');
    tick();
    ownedWrite(output, { schema_version: 1, kind: 'ARMv7_hosted_cross_tool_preflight_observation',
      passed: true, target: TARGET, host: HOST, portable_cpu: false, production_release_approved: false,
      source_commit: commit, public_workflow: publicWorkflow,
      runner: { os: ambient.RUNNER_OS, arch: ambient.RUNNER_ARCH, ImageOS: ambient.ImageOS, ImageVersion: ambient.ImageVersion },
      source_files: before, tools, actual_rust_tools: actual, compiler_library: compilerLibrary, rustup_settings: rustupSettings,
      preprocess_flag_source: { package: 'cc', version: '1.6.0', package_sha256: CC_CHECKSUM,
        source: 'src/lib.rs:2840-2849', scope: 'Probe argv only; no inherited CFLAGS adoption or native build-closure proof' },
      target_std: std, selected_APT_roots: selectedRoots, requested_roots_file: requested.row, setup_files: setupFiles,
      commands, selected_linker_files: observations, elapsed_ms: performance.now() - started,
      limits: { command_count: 48, command_ms: 15000, command_bytes: MIB, aggregate_command_bytes: 4 * MIB,
        cooperative_whole_ms: 180000, native_owner_ms: 190000, report_bytes: 2 * MIB,
        supplier_file_bytes: 512 * MIB, directory_entries: 256 },
      scope: 'Metadata and empty-stdin C preprocessing only. No ARM Cargo dependency build, object/link/runtime/emulation, immutable SDK, complete compiler/library closure, GLIBC floor, CPU/platform/lifecycle/provider or release qualification. Linker-script references are not traversed. A separate host-only auditor bootstrap supplies process-group ownership; no standalone Node descendant-cleanup guarantee.' });
  } catch (error) {
    // Completed fixed-command metadata remains useful when an unknown SDK path
    // refuses. A failed command's stderr/body is never attached here.
    error.partial = { schema_version: 1, kind: 'ARMv7_incomplete_cross_tool_observations',
      passed: false, portable_cpu: false, production_release_approved: false,
      completed_commands: commands, scope: 'Incomplete observations only; source/SDK stability and whole probe success not established' };
    throw error;
  }
}

export function refusalDiagnostics(error) {
  // No arbitrary child stderr, environment, filesystem error or failing argv.
  const reason = /^ARMv7 preflight: [A-Za-z0-9 /-]+$/.test(error.message) ? error.message : 'ARMv7 preflight: refused';
  const stage = typeof error.stage === 'string' && /^[a-z0-9_.-]+$/.test(error.stage) ? error.stage : null;
  return { kind: 'ARMv7_preflight_refusal', stage, reason };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  try {
    validateNativeLaunch(process.version, process.env);
    check(args.length === 4 && args[0] === '--root' && args[2] === '--out', 'arguments');
    await probe(args[1], args[3]);
  } catch (error) {
    const diagnostic = refusalDiagnostics(error);
    if (error.partial && args[2] === '--out' && isAbsolute(args[3])) {
      try { ownedWrite(join(dirname(args[3]), 'observations-incomplete.json'), { ...error.partial,
        failed_stage: diagnostic.stage, reason: diagnostic.reason }); }
      catch { /* Refusal diagnostics never overwrite another file or success. */ }
    }
    console.error(JSON.stringify(diagnostic)); process.exitCode = 1;
  }
}
