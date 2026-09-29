import test from 'node:test';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { gzipSync } from 'node:zlib';
import { checkRelease, releaseMetadata } from '../scripts/release-check.mjs';
import { npmEnvironment, packPackage } from '../scripts/release-pack.mjs';

const execute = promisify(execFile);
const SCRIPT = fileURLToPath(new URL('../scripts/release-check.mjs', import.meta.url));
const TAG = 'v1.2.3';
const ARCHIVE = 'claude-autorouter-1.2.3.tgz';
const manifest = () => ({
  name: 'claude-autorouter', version: '1.2.3', type: 'module', license: 'Apache-2.0',
  repository: { type: 'git', url: 'git+https://github.com/frapposelli/claude-autorouter.git' },
  publishConfig: { access: 'public', registry: 'https://registry.npmjs.org/' },
  bin: { 'claude-autorouter': 'bin/autorouter.mjs' },
  files: ['bin/*.mjs', 'src/*.mjs', 'docs/reference.md', 'docs/development.md', 'docs/releasing.md', 'docs/ollama-evaluation.md', '.env.example', 'LICENSE'],
});
const metadata = { version: '1.2.3', archive: `dist/${ARCHIVE}`, dist_tag: 'latest' };
const github = { GITHUB_ACTIONS: 'true', GITHUB_REPOSITORY: 'frapposelli/claude-autorouter', GITHUB_REF_TYPE: 'tag', GITHUB_REF: `refs/tags/${TAG}` };

async function fixture(t) {
  const base = await mkdtemp(join(tmpdir(), 'autorouter-release-test-'));
  t.after(() => rm(base, { recursive: true, force: true }));
  const cwd = join(base, 'checkout with spaces');
  await mkdir(cwd);
  for (const path of ['README.md', 'LICENSE', '.env.example', 'bin/autorouter.mjs', 'bin/statusline.mjs',
    'src/config.mjs', 'src/router.mjs', 'src/server.mjs', 'docs/reference.md', 'docs/development.md', 'docs/releasing.md', 'docs/ollama-evaluation.md']) {
    await mkdir(dirname(join(cwd, path)), { recursive: true });
    await writeFile(join(cwd, path), path.endsWith('.mjs') ? 'export const fixture = true;\n' : 'Synthetic release fixture.\n');
  }
  await writeFile(join(cwd, 'package.json'), `${JSON.stringify(manifest(), null, 2)}\n`);
  await writeFile(join(cwd, '.gitignore'), 'dist/\n');
  const git = async (...args) => (await execute('git', ['-c', 'commit.gpgsign=false', '-c', 'tag.gpgsign=false', '-c', 'core.hooksPath=/dev/null', ...args], {
    cwd, env: { ...process.env, GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_NOSYSTEM: '1' },
  })).stdout.trim();
  return { base, cwd, git };
}

async function gitFixture(t) {
  const result = await fixture(t);
  const { git } = result;
  await git('init', '--initial-branch=main');
  await git('config', 'user.name', 'Release Test');
  await git('config', 'user.email', 'release@example.invalid');
  await git('config', 'core.autocrlf', 'false');
  await git('add', '.');
  await git('commit', '-m', 'Initial synthetic release');
  await git('update-ref', 'refs/remotes/origin/main', 'HEAD');
  await git('tag', '-a', TAG, '-m', 'Synthetic annotated release');
  return result;
}

// Write a real tar/gzip so archive tests exercise checksum verification,
// parsing, file-list policy, and source comparison together.
async function writeArchive(cwd, files) {
  const blocks = [];
  for (const [path, content] of files) {
    const bytes = Buffer.from(content);
    const header = Buffer.alloc(512);
    header.write(`package/${path}`, 0);
    header.write('0000644\0', 100);
    header.write('0000000\0', 108);
    header.write('0000000\0', 116);
    header.write(`${bytes.length.toString(8).padStart(11, '0')}\0`, 124);
    header.write('00000000000\0', 136);
    header.fill(32, 148, 156);
    header.write('0', 156);
    header.write('ustar\0', 257);
    header.write('00', 263);
    const checksum = header.reduce((sum, byte) => sum + byte, 0);
    header.write(`${checksum.toString(8).padStart(6, '0')}\0 `, 148);
    blocks.push(header, bytes, Buffer.alloc((512 - bytes.length % 512) % 512));
  }
  blocks.push(Buffer.alloc(1024));
  const bytes = gzipSync(Buffer.concat(blocks));
  const directory = join(cwd, 'dist');
  await mkdir(directory, { recursive: true });
  await writeFile(join(directory, ARCHIVE), bytes);
  await writeFile(join(directory, `${ARCHIVE}.sha256`), `${createHash('sha256').update(bytes).digest('hex')}  ${ARCHIVE}\n`);
}

test('release tags enforce canonical SemVer and select stable/prerelease dist-tags', () => {
  assert.deepEqual(releaseMetadata(manifest(), TAG), metadata);
  for (const version of ['0.0.0', '1.2.3-alpha.0', '2.0.0-rc.1', '1.2.3-0A', '1.2.3-alpha-beta']) {
    const result = releaseMetadata({ ...manifest(), version }, `v${version}`);
    assert.equal(result.version, version);
    assert.equal(result.dist_tag, version.includes('-') ? 'next' : 'latest');
  }
  for (const tag of ['1.2.3', 'v01.2.3', 'v1.02.3', 'v1.2.03', 'v1.2', 'v1.2.3-', 'v1.2.3-01',
    'v1.2.3-alpha.01', 'v1.2.3-alpha..1', 'v1.2.3+build', 'v1.2.3\n', 'v1.2.3\narchive=evil',
    'v1.2.3/../../other', '--help', 'v9007199254740992.0.0']) {
    assert.throws(() => releaseMetadata({ ...manifest(), version: tag.slice(1) }, tag), /tag|SemVer/);
  }
  assert.throws(() => releaseMetadata(manifest(), 'v1.2.4'), /does not match/);
});

test('release metadata rejects wrong identity, publication policy, and runtime dependencies', () => {
  for (const change of [
    { name: '@someone/claude-autorouter' }, { private: true }, { private: 'false' },
    { repository: { url: 'git+https://github.com/someone/claude-autorouter.git' } },
    { repository: 'https://github.com/frapposelli/claude-autorouter' },
    { publishConfig: { access: 'restricted', registry: 'https://registry.npmjs.org/' } },
    { publishConfig: { access: 'public', registry: 'https://example.invalid/' } },
    { dependencies: { example: '*' } }, { optionalDependencies: { example: '*' } }, { dependencies: [] },
  ]) assert.throws(() => releaseMetadata({ ...manifest(), ...change }, TAG));
  assert.deepEqual(releaseMetadata({ ...manifest(), private: false, dependencies: {}, optionalDependencies: {} }, TAG), metadata);
});

test('GitHub identity must match the exact repository and tag when supplied', () => {
  assert.deepEqual(releaseMetadata(manifest(), TAG, github), metadata);
  for (const env of [
    { ...github, GITHUB_REPOSITORY: 'someone/claude-autorouter' },
    { ...github, GITHUB_REF_TYPE: 'branch', GITHUB_REF: 'refs/heads/main' },
    { ...github, GITHUB_REF: 'refs/tags/v1.2.4' },
    { GITHUB_ACTIONS: 'true' }, { GITHUB_REPOSITORY: github.GITHUB_REPOSITORY },
  ]) assert.throws(() => releaseMetadata(manifest(), TAG, env), /GitHub|GITHUB_REPOSITORY/);
});

test('source check accepts annotated and lightweight tags but rejects dirty source', async t => {
  const { cwd, git } = await gitFixture(t);
  assert.deepEqual(await checkRelease('source', TAG, { cwd, env: github }), metadata);
  await git('tag', '-d', TAG);
  await git('tag', TAG);
  assert.deepEqual(await checkRelease('source', TAG, { cwd, env: {} }), metadata);
  await mkdir(join(cwd, 'dist'));
  await writeFile(join(cwd, 'dist', 'ignored.tgz'), 'ignored build artifact');
  assert.deepEqual(await checkRelease('source', TAG, { cwd, env: {} }), metadata);
  await writeFile(join(cwd, 'README.md'), 'Uncommitted change');
  await assert.rejects(checkRelease('source', TAG, { cwd, env: {} }), /modified or untracked/);
  await git('checkout', '--', 'README.md');
  await writeFile(join(cwd, 'untracked.txt'), 'Untracked source');
  await assert.rejects(checkRelease('source', TAG, { cwd, env: {} }), /modified or untracked/);
});

test('source check requires tag at HEAD and ancestry from origin/main, not merely a matching version', async t => {
  const { cwd, git } = await gitFixture(t);
  const original = await git('rev-parse', 'HEAD');
  await writeFile(join(cwd, 'README.md'), 'Second commit');
  await git('add', 'README.md');
  await git('commit', '-m', 'Synthetic later commit');
  await assert.rejects(checkRelease('source', TAG, { cwd, env: {} }), /checked-out HEAD/);
  await git('tag', '-f', TAG);
  await assert.rejects(checkRelease('source', TAG, { cwd, env: {} }), /reachable from origin\/main/);
  await git('update-ref', 'refs/remotes/origin/main', 'HEAD');
  assert.deepEqual(await checkRelease('source', TAG, { cwd, env: {} }), metadata);
  await git('tag', '-f', TAG, original);
  await git('checkout', '--detach', original);
  assert.deepEqual(await checkRelease('source', TAG, { cwd, env: {} }), metadata);
  await git('update-ref', '-d', 'refs/remotes/origin/main');
  await assert.rejects(checkRelease('source', TAG, { cwd, env: {} }), /reachable from origin\/main/);
});

test('CLI writes only validated release metadata to GITHUB_OUTPUT', async t => {
  const { base, cwd } = await gitFixture(t);
  const output = join(base, 'github output');
  const env = { ...Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GITHUB_'))), ...github, GITHUB_OUTPUT: output };
  const result = await execute(process.execPath, [SCRIPT, 'source', TAG], { cwd, env });
  assert.match(result.stdout, /Verified source release/);
  assert.equal(await readFile(output, 'utf8'), `version=1.2.3\narchive=dist/${ARCHIVE}\ndist_tag=latest\n`);
  const before = await readFile(output, 'utf8');
  await assert.rejects(execute(process.execPath, [SCRIPT, 'source', `${TAG}\narchive=malicious`], { cwd, env }));
  assert.equal(await readFile(output, 'utf8'), before);
});

test('archive check validates actual packed files, checksums, manifest, and source bytes', async t => {
  const { base, cwd } = await fixture(t);
  const packed = await packPackage(cwd, join(cwd, 'dist'), await npmEnvironment(join(base, 'npm')));
  const original = new Map(packed.files);
  await writeArchive(cwd, original);
  assert.deepEqual(await checkRelease('archive', TAG, { cwd, env: {} }), metadata);

  await t.test('checksum tampering is rejected', async () => {
    await writeFile(join(cwd, 'dist', `${ARCHIVE}.sha256`), `${'0'.repeat(64)}  ${ARCHIVE}\n`);
    await assert.rejects(checkRelease('archive', TAG, { cwd, env: {} }), /SHA256 does not match/);
  });
  await t.test('checksum filename cannot reference another path', async () => {
    await writeFile(join(cwd, 'dist', `${ARCHIVE}.sha256`), `${'0'.repeat(64)}  ../${ARCHIVE}\n`);
    await assert.rejects(checkRelease('archive', TAG, { cwd, env: {} }), /exact release archive/);
  });
  await t.test('stale or additional artifacts are rejected', async () => {
    await writeArchive(cwd, original);
    const extra = join(cwd, 'dist', 'old.tgz');
    await writeFile(extra, 'old');
    await assert.rejects(checkRelease('archive', TAG, { cwd, env: {} }), /only the expected/);
    await rm(extra);
  });
  await t.test('changed manifest metadata is rejected even with a matching checksum', async () => {
    const changed = new Map(original);
    changed.set('package.json', Buffer.from(JSON.stringify({ ...manifest(), description: 'Altered metadata' })));
    await writeArchive(cwd, changed);
    await assert.rejects(checkRelease('archive', TAG, { cwd, env: {} }), /package.json differs/);
  });
  await t.test('changed runtime code is rejected even with a matching checksum', async () => {
    const changed = new Map(original);
    changed.set('src/router.mjs', Buffer.from('export const replaced = true;\n'));
    await writeArchive(cwd, changed);
    await assert.rejects(checkRelease('archive', TAG, { cwd, env: {} }), /content differs.*src\/router.mjs/);
  });
  await t.test('additional otherwise-allowed source paths are rejected', async () => {
    const changed = new Map(original);
    changed.set('src/injected.mjs', Buffer.from('export {};\n'));
    await writeArchive(cwd, changed);
    await assert.rejects(checkRelease('archive', TAG, { cwd, env: {} }), /file list differs/);
  });
  await t.test('missing optional package files are rejected by the exact file list', async () => {
    const changed = new Map(original);
    changed.delete('LICENSE');
    await writeArchive(cwd, changed);
    await assert.rejects(checkRelease('archive', TAG, { cwd, env: {} }), /file list differs/);
  });
  await t.test('private files fail the shared package allowlist', async () => {
    const changed = new Map(original);
    changed.set('.env', Buffer.from('SYNTHETIC_SENTINEL=not-a-real-secret\n'));
    await writeArchive(cwd, changed);
    await assert.rejects(checkRelease('archive', TAG, { cwd, env: {} }), /Unexpected file/);
  });
  await t.test('symlinked artifacts are rejected before reading their targets', async () => {
    await writeArchive(cwd, original);
    const path = join(cwd, 'dist', ARCHIVE);
    const target = join(base, 'other archive.tgz');
    await writeFile(target, await readFile(path));
    await rm(path);
    await symlink(target, path);
    await assert.rejects(checkRelease('archive', TAG, { cwd, env: {} }), /regular file/);
  });
});
