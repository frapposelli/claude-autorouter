import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { stat, mkdtemp, mkdir, readFile, writeFile, rm, symlink } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { npmEnvironment, packPackage, run as packageRun } from '../scripts/release-pack.mjs';
import { compareVersions, parseVersion, preflightRelease, verifyRelease, installPublishedRelease,
  validateArtifactSource, inspectReleaseArtifact } from '../scripts/release-verify.mjs';

const bytes = Buffer.from('synthetic canonical archive, already tested');
const artifact = { version: '0.4.0', tag: 'v0.4.0', dist_tag: 'latest', filename: 'claude-autorouter-0.4.0.tgz',
  bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex'),
  integrity: `sha512-${createHash('sha512').update(bytes).digest('base64')}` };
const metadata = (extra = {}) => ({ name: 'claude-autorouter', version: artifact.version,
  dist: { integrity: artifact.integrity, tarball: 'https://registry.npmjs.org/claude-autorouter/-/claude-autorouter-0.4.0.tgz' }, ...extra });
const packument = ({ exists = true, latest = artifact.version, next, entry = metadata() } = {}) => ({
  name: 'claude-autorouter', 'dist-tags': { ...(latest ? { latest } : {}), ...(next ? { next } : {}) },
  versions: exists ? { [artifact.version]: entry } : {},
});
const json = (body, status = 200) => new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
function registry({ exists = true, latest = artifact.version, exact404 = false, body = bytes, version = metadata(), status } = {}) {
  const calls = [];
  return { calls, fetchImpl: async (input, options) => {
    const url = new URL(input); calls.push({ url, options });
    assert.equal(url.origin, 'https://registry.npmjs.org');
    assert.equal(options.redirect, 'error');
    assert.equal(options.method, undefined, 'Registry verification must be read-only');
    if (status) return json({}, status);
    if (url.pathname.includes('/-/')) return body === undefined ? json({}, 404) : new Response(body);
    if (url.pathname === '/claude-autorouter/0.4.0') return exists && !exact404 ? json(version) : json({}, 404);
    if (url.pathname === '/claude-autorouter') return json(packument({ exists, latest, entry: version }));
    assert.fail('Unexpected registry path');
  } };
}
function clock() {
  let elapsed = 0;
  const waits = [];
  return { waits, now: () => elapsed, wait: async ms => { waits.push(ms); elapsed += ms; } };
}

test('stable version ordering uses numeric SemVer and rejects ambiguous inputs', () => {
  assert.equal(compareVersions('0.10.0', '0.9.9'), 1);
  assert.equal(compareVersions('1.0.0', '1.0.0-rc.2'), 1);
  assert.equal(compareVersions('1.0.0-rc.10', '1.0.0-rc.2'), 1);
  assert.equal(compareVersions('1.0.0-alpha', '1.0.0-beta'), -1);
  assert.equal(compareVersions('1.0.0-alpha.1', '1.0.0-alpha'), 1);
  for (const invalid of ['0.4.0+build', '01.4.0', '0.4.0-01', '0.4.0\n', '--tag', 'latest', '9007199254740992.0.0']) assert.throws(() => parseVersion(invalid));
});

test('preflight allows only a newer new stable version, and registry failures are not version absence', async () => {
  assert.equal((await preflightRelease(artifact, registry({ exists: false, latest: '0.3.7' }))).publish, true);
  for (const latest of ['0.4.0', '0.5.0']) await assert.rejects(preflightRelease(artifact, registry({ exists: false, latest })), { code: 'version_order' });
  for (const status of [401, 429, 503]) await assert.rejects(preflightRelease(artifact, registry({ status })), error =>
    error.code === 'registry_unavailable' && error.detail.http_status === status);
  await assert.rejects(preflightRelease(artifact, { fetchImpl: async () => { throw new Error('PRIVATE_NETWORK_DETAIL'); } }),
    error => error.code === 'registry_unavailable' && !error.message.includes('PRIVATE'));
});

test('matching existing immutable versions skip publication even when the exact endpoint returns cached404', async () => {
  const mock = registry({ exact404: true, latest: '0.5.0' });
  const result = await preflightRelease(artifact, mock);
  assert.equal(result.publish, false);
  assert.equal(result.state, 'submitted');
  assert.equal(result.reason, 'identical_version_exists');
  assert.equal(result.metadata_source, 'packument');
  assert.equal(result.current_dist_tag, '0.5.0');
  assert.ok(mock.calls.every(call => call.options.headers['cache-control'].includes('no-cache') && call.url.searchParams.has('autorouter_verify')));
});

test('an unavailable retry never guesses that another submission is safe', async () => {
  const result = await preflightRelease(artifact, { ...registry({ exists: false, latest: '0.3.7' }), allowPublish: false });
  assert.equal(result.publish, false);
  assert.equal(result.state, 'validating_unavailable');
  assert.equal(result.reason, 'retry_submission_unknown');
});

test('existing metadata mismatches stop before publication, including conflicting cache views', async () => {
  for (const version of [metadata({ version: '9.0.0' }), metadata({ name: 'other' }),
    metadata({ dist: { ...metadata().dist, integrity: 'sha512-not-the-canonical-archive' } }),
    metadata({ dist: { ...metadata().dist, tarball: 'https://private.example/archive.tgz' } })]) {
    await assert.rejects(preflightRelease(artifact, registry({ version })), { code: 'release_mismatch' });
  }
  await assert.rejects(preflightRelease(artifact, { fetchImpl: async url => new URL(url).pathname === '/claude-autorouter/0.4.0'
    ? json(metadata()) : json(packument({ entry: metadata({ dist: { ...metadata().dist, integrity: 'sha512-conflicting' } }) })) }), { code: 'release_mismatch' });
});

test('bounded polling tolerates delayed metadata and tarballs then verifies the tested bytes and install', async () => {
  let reads = 0, tarballReads = 0, installs = 0;
  const fakeClock = clock(), states = [];
  const result = await verifyRelease(artifact, { ...fakeClock, timeoutMs: 15000, onState: state => states.push(state.state),
    fetchImpl: async url => {
      const path = new URL(url).pathname;
      if (path === '/claude-autorouter/0.4.0') return ++reads < 3 ? json({}, 404) : json(metadata());
      if (path === '/claude-autorouter') return json(packument({ exists: reads >= 3, latest: reads >= 3 ? artifact.version : '0.3.7' }));
      return ++tarballReads === 1 ? json({}, 404) : new Response(bytes);
    }, install: async target => { installs++; assert.equal(target, artifact); return { version: target.version, isolated: true, help: true }; },
  });
  assert.equal(result.state, 'verified');
  assert.equal(result.dist_tag_status, 'current');
  assert.equal(installs, 1);
  assert.deepEqual(fakeClock.waits, [1000, 2000, 4000]);
  assert.deepEqual(states, ['validating_unavailable', 'validating_unavailable', 'validating_unavailable', 'verified']);
});

test('verification timeout is pending and preserves distinct unavailable/error evidence', async () => {
  for (const [mock, reason] of [[registry({ exists: false, latest: '0.3.7' }), 'version_not_available'], [registry({ status: 503 }), 'http_error']]) {
    const result = await verifyRelease(artifact, { ...mock, ...clock(), timeoutMs: 5000, install: () => assert.fail('Unavailable release cannot be installed') });
    assert.equal(result.state, 'validating_unavailable');
    assert.equal(result.pending, true);
    assert.equal(result.reason, reason);
    assert.equal(result.sha256, artifact.sha256);
    assert.match(result.message, /do not submit a duplicate release/);
    if (reason === 'http_error') assert.equal(result.http_status, 503);
  }
});

test('tarball mismatch fails immediately and a newer latest is verified without any tag mutation', async () => {
  const failed = await verifyRelease(artifact, { ...registry({ body: Buffer.from('different bytes') }), ...clock(), timeoutMs: 5000,
    install: () => assert.fail('Different bytes cannot be installed') });
  assert.equal(failed.state, 'failed');
  assert.equal(failed.reason, 'tarball_bytes');
  assert.equal(failed.attempts, 1);
  const newer = registry({ latest: '0.5.0' });
  const result = await verifyRelease(artifact, { ...newer, ...clock(), timeoutMs: 5000,
    install: async () => ({ version: artifact.version, help: true, isolated: true }) });
  assert.equal(result.state, 'verified');
  assert.equal(result.current_dist_tag, '0.5.0');
  assert.equal(result.dist_tag_status, 'superseded_by_newer_version');
  assert.equal(newer.calls.length, 3);
});

test('an older dist-tag remains pending and installation is required before verified', async () => {
  let installs = 0;
  const pending = await verifyRelease(artifact, { ...registry({ latest: '0.3.7' }), ...clock(), timeoutMs: 2000,
    install: async () => { installs++; } });
  assert.equal(pending.state, 'validating_unavailable');
  assert.equal(pending.reason, 'dist_tag_not_updated');
  assert.equal(installs, 0);
  const failed = await verifyRelease(artifact, { ...registry(), ...clock(), timeoutMs: 2000,
    install: async () => { throw new Error('PRIVATE_INSTALL_ERROR'); } });
  assert.equal(failed.state, 'failed');
  assert.ok(!JSON.stringify(failed).includes('PRIVATE'));
});

test('registry install uses an isolated prefix/cache, exact version, no scripts and direct executable help/version checks', async () => {
  const calls = [];
  const files = new Map([['package.json', Buffer.from('{"name":"claude-autorouter"}')], ['bin/autorouter.mjs', Buffer.from('// synthetic CLI')]]);
  const installed = await installPublishedRelease(artifact, { readArchive: async () => files, runCommand: async (command, args, options) => {
    calls.push({ command, args, options });
    assert.equal(options.env.TYPESAFE_API_KEY, undefined);
    assert.equal(options.env.ANTHROPIC_API_KEY, undefined);
    assert.equal(options.env.AUTOROUTER_CONFIG, undefined);
    if (command === 'npm') {
      const prefix = args[args.indexOf('--prefix') + 1], root = join(prefix, 'lib', 'node_modules', 'claude-autorouter');
      for (const [path, contents] of files) { await mkdir(dirname(join(root, path)), { recursive: true }); await writeFile(join(root, path), contents); }
      await mkdir(join(prefix, 'bin'));
      await symlink(join(root, 'bin', 'autorouter.mjs'), join(prefix, 'bin', 'claude-autorouter'));
      return { stdout: '', stderr: '' };
    }
    return { stdout: args[0] === '--version' ? '0.4.0\n' : 'claude-autorouter setup --help\n', stderr: '' };
  } });
  assert.equal(installed.version, '0.4.0');
  assert.ok(calls[0].args.includes('claude-autorouter@0.4.0'));
  for (const flag of ['--ignore-scripts', '--global', '--no-audit', '--no-fund']) assert.ok(calls[0].args.includes(flag));
  assert.match(calls[1].command, /install prefix\/bin\/claude-autorouter$/);
  assert.deepEqual(calls.slice(1).map(call => call.args), [['--version'], ['--help']]);
  assert.match(calls[0].options.cwd, /unrelated cwd$/);
  await assert.rejects(stat(calls[0].options.cwd), { code: 'ENOENT' });
});

test('manual verification accepts only the original canonical artifact bound to the tag commit', () => {
  const commit = 'a'.repeat(40), identity = { runId: '123', artifactId: '456', commit, tag: 'v0.4.0' };
  const run = { id: 123, event: 'push', path: '.github/workflows/publish.yml', head_sha: commit, head_branch: 'v0.4.0',
    head_repository: { full_name: 'frapposelli/claude-autorouter' } };
  const archive = { id: 456, name: 'npm-package-123-1', expired: false, workflow_run: { id: 123, head_sha: commit } };
  assert.deepEqual(validateArtifactSource(run, archive, identity), { run_id: '123', artifact_id: '456', notes_artifact_name: 'release-notes-123-1' });
  for (const changed of [{ event: 'pull_request' }, { head_branch: 'main' }, { head_sha: 'b'.repeat(40) },
    { path: '.github/workflows/ci.yml' }, { head_repository: { full_name: 'untrusted/fork' } }]) {
    assert.throws(() => validateArtifactSource({ ...run, ...changed }, archive, identity));
  }
  for (const changed of [{ expired: true }, { id: 999 }, { name: 'other-artifact' }, { workflow_run: { id: 999, head_sha: commit } }]) {
    assert.throws(() => validateArtifactSource(run, { ...archive, ...changed }, identity));
  }
});

test('polling caps backoff and retained timeline while preserving the full elapsed budget', async () => {
  const fakeClock = clock();
  const result = await verifyRelease(artifact, { ...registry({ exists: false }), ...fakeClock, timeoutMs: 600000 });
  assert.equal(result.state, 'validating_unavailable');
  assert.equal(result.elapsed_ms, 600000);
  assert.equal(Math.max(...fakeClock.waits), 15000);
  assert.equal(result.events.length, 32);
});

test('prerelease preflight targets next without treating a newer latest as a publication target', async () => {
  const prerelease = { ...artifact, version: '0.4.0-beta.1', tag: 'v0.4.0-beta.1', dist_tag: 'next' };
  const result = await preflightRelease(prerelease, { fetchImpl: async url => new URL(url).pathname === '/claude-autorouter'
    ? json(packument({ exists: false, latest: '0.5.0', next: '0.4.0-beta.0' })) : json({}, 404) });
  assert.equal(result.publish, true);
  assert.equal(result.dist_tag, 'next');
});

test('real canonical archive inspection and submitted CLI reports preserve exact identity without registry calls', async t => {
  const temporary = await mkdtemp(join(tmpdir(), 'autorouter-verify-cli-'));
  t.after(() => rm(temporary, { recursive: true, force: true }));
  const cwd = join(temporary, 'source'), env = await npmEnvironment(join(temporary, 'npm'));
  const manifest = { name: 'claude-autorouter', version: '0.4.0', license: 'Apache-2.0', type: 'module',
    repository: { type: 'git', url: 'git+https://github.com/frapposelli/claude-autorouter.git' },
    publishConfig: { access: 'public', registry: 'https://registry.npmjs.org/' }, bin: { 'claude-autorouter': 'bin/autorouter.mjs' },
    files: ['bin/*.mjs', 'src/*.mjs', 'docs/*.md', 'LICENSE'] };
  await mkdir(cwd);
  await writeFile(join(cwd, 'package.json'), JSON.stringify(manifest));
  for (const path of ['README.md', 'LICENSE', 'bin/autorouter.mjs', 'bin/statusline.mjs', 'src/config.mjs', 'src/router.mjs',
    'src/server.mjs', 'docs/reference.md', 'docs/development.md', 'docs/releasing.md', 'docs/ollama-evaluation.md']) {
    await mkdir(dirname(join(cwd, path)), { recursive: true });
    await writeFile(join(cwd, path), path === 'bin/autorouter.mjs'
      ? '#!/usr/bin/env node\nconsole.log(process.argv.includes("--version") ? "0.4.0" : "claude-autorouter setup help");\n'
      : path.endsWith('.mjs') ? 'export const synthetic = true;\n' : 'Synthetic release documentation.\n');
  }
  const packed = await packPackage(cwd, join(temporary, 'dist'), env);
  const content = await readFile(packed.archive), sha256 = createHash('sha256').update(content).digest('hex');
  await writeFile(`${packed.archive}.sha256`, `${sha256}  claude-autorouter-0.4.0.tgz\n`);
  const inspected = await inspectReleaseArtifact(packed.archive, 'v0.4.0');
  assert.equal(inspected.sha256, sha256);
  assert.equal(inspected.integrity, `sha512-${createHash('sha512').update(content).digest('base64')}`);
  const installed = await installPublishedRelease(inspected, { runCommand: (command, args, options) => command === 'npm'
    ? packageRun(command, [...args.slice(0, -1), '--offline', packed.archive], options)
    : packageRun(command, args, options) });
  assert.equal(installed.version, '0.4.0', 'Real offline npm extraction matches canonical bytes and its executable runs');
  const reportPath = join(temporary, 'report.json'), outputPath = join(temporary, 'github-output');
  const script = fileURLToPath(new URL('../scripts/release-verify.mjs', import.meta.url));
  const execute = promisify(execFile);
  await execute(process.execPath, [script, 'submitted', 'v0.4.0', '--archive', packed.archive, '--report', reportPath],
    { env: { ...env, GITHUB_OUTPUT: outputPath } });
  const report = JSON.parse(await readFile(reportPath, 'utf8'));
  assert.equal(report.state, 'submitted');
  assert.equal(report.sha256, sha256);
  assert.equal(await readFile(outputPath, 'utf8'), 'state=submitted\n');
  await writeFile(`${packed.archive}.sha256`, `${'0'.repeat(64)}  claude-autorouter-0.4.0.tgz\n`);
  await assert.rejects(inspectReleaseArtifact(packed.archive, 'v0.4.0'), { code: 'invalid_archive' });
  await assert.rejects(execute(process.execPath, [script, 'submitted', 'v0.4.0', '--archive', packed.archive, '--report', reportPath], { env }), { code: 1 });
  assert.equal(JSON.parse(await readFile(reportPath, 'utf8')).state, 'failed');
  assert.equal(JSON.parse(await readFile(reportPath, 'utf8')).reason, 'invalid_archive');
});

test('installed bytes are checked before any package executable is run', async () => {
  const files = new Map([['bin/autorouter.mjs', Buffer.from('// reviewed content')]]);
  await assert.rejects(installPublishedRelease(artifact, { readArchive: async () => files,
    runCommand: async (command, args) => {
      assert.equal(command, 'npm', 'Mismatched installed code must never execute');
      const prefix = args[args.indexOf('--prefix') + 1];
      const path = join(prefix, 'lib', 'node_modules', 'claude-autorouter', 'bin', 'autorouter.mjs');
      await mkdir(dirname(path), { recursive: true });
      await writeFile(path, '// unreviewed content');
      return { stdout: '', stderr: '' };
    } }), error => error.code === 'release_mismatch' && error.detail.reason === 'installed_archive_bytes');
});

test('elapsed metadata checks cannot start a tarball download after the verification deadline', async () => {
  let elapsed = 0, requests = 0;
  const result = await verifyRelease(artifact, { timeoutMs: 1000, now: () => elapsed,
    fetchImpl: async url => {
      requests++;
      elapsed = 1500;
      return new URL(url).pathname === '/claude-autorouter' ? json(packument()) : json(metadata());
    }, install: () => assert.fail('Deadline prevents installation') });
  assert.equal(requests, 2, 'Only the concurrent metadata requests should have started');
  assert.equal(result.state, 'validating_unavailable');
  assert.equal(result.reason, 'verification_deadline');
});

test('an install that consumes the remaining verification budget stays unavailable instead of becoming a mismatch', async () => {
  let elapsed = 0, executions = 0;
  const files = new Map([['bin/autorouter.mjs', Buffer.from('// canonical CLI')]]);
  await assert.rejects(installPublishedRelease(artifact, { timeoutMs: 100, now: () => elapsed, readArchive: async () => files,
    runCommand: async (command, args) => {
      executions++;
      assert.equal(command, 'npm', 'No executable may start after the overall budget expires');
      const prefix = args[args.indexOf('--prefix') + 1], path = join(prefix, 'lib', 'node_modules', 'claude-autorouter', 'bin', 'autorouter.mjs');
      await mkdir(dirname(path), { recursive: true });
      await writeFile(path, files.get('bin/autorouter.mjs'));
      await mkdir(join(prefix, 'bin'));
      await symlink(path, join(prefix, 'bin', 'claude-autorouter'));
      elapsed = 101;
      return { stdout: '', stderr: '' };
    } }), error => error.code === 'registry_unavailable' && error.detail.reason === 'isolated_install_timeout');
  assert.equal(executions, 1);
});
