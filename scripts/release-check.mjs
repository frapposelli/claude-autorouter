#!/usr/bin/env node
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { appendFile, lstat, mkdtemp, readFile, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { archiveFiles, npmEnvironment, packagePlan } from './release-pack.mjs';

const execute = promisify(execFile);
const PACKAGE = 'claude-autorouter';
const REPOSITORY = 'frapposelli/claude-autorouter';
const REPOSITORY_URL = `git+https://github.com/${REPOSITORY}.git`;
const REGISTRY = 'https://registry.npmjs.org/';

const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);

export function releaseMetadata(manifest, tag, env = {}) {
  if (typeof tag !== 'string' || tag.length > 200 || !tag.startsWith('v')) throw new Error('Release tag must be v<SemVer>');
  const version = tag.slice(1);
  const match = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/.exec(version);
  if (!match || match[0] !== version || match.slice(1, 4).some(part => !Number.isSafeInteger(Number(part)))
    || match[4]?.split('.').some(part => /^\d+$/.test(part) && part.length > 1 && part.startsWith('0'))) {
    throw new Error('Release tag must contain strict SemVer without build metadata');
  }
  if (!object(manifest) || manifest.name !== PACKAGE) throw new Error(`Package name must be ${PACKAGE}`);
  if (manifest.version !== version) throw new Error('Release tag does not match package.json version');
  if (manifest.repository?.url !== REPOSITORY_URL) throw new Error('Package repository.url does not match the release repository');
  if (manifest.publishConfig?.access !== 'public' || manifest.publishConfig?.registry !== REGISTRY) {
    throw new Error('Package publishConfig must select the public npm registry and public access');
  }
  if (manifest.private !== undefined && manifest.private !== false) throw new Error('Release package must not be private');
  for (const field of ['dependencies', 'optionalDependencies']) {
    if (manifest[field] !== undefined && (!object(manifest[field]) || Object.keys(manifest[field]).length)) {
      throw new Error(`Release package must have no ${field}`);
    }
  }
  if (env.GITHUB_REPOSITORY !== undefined && env.GITHUB_REPOSITORY !== REPOSITORY) {
    throw new Error('GITHUB_REPOSITORY does not match the release repository');
  }
  const githubIdentity = env.GITHUB_ACTIONS === 'true'
    || ['GITHUB_REPOSITORY', 'GITHUB_REF', 'GITHUB_REF_TYPE', 'GITHUB_SHA'].some(key => env[key] !== undefined);
  if (githubIdentity && (env.GITHUB_REF_TYPE !== 'tag' || env.GITHUB_REF !== `refs/tags/${tag}`)) {
    throw new Error('GitHub release must run on the exact version tag');
  }
  return { version, archive: `dist/${PACKAGE}-${version}.tgz`, dist_tag: match[4] ? 'next' : 'latest' };
}

async function git(cwd, args) {
  const { stdout } = await execute('git', ['--no-pager', ...args], {
    cwd, encoding: 'utf8', timeout: 10000, maxBuffer: 1024 * 1024,
    env: { ...process.env, GIT_OPTIONAL_LOCKS: '0' },
  });
  return stdout.trim();
}

async function checkSource(cwd, tag) {
  let head, tagged;
  try {
    head = await git(cwd, ['rev-parse', '--verify', 'HEAD^{commit}']);
    tagged = await git(cwd, ['rev-parse', '--verify', `refs/tags/${tag}^{commit}`]);
  } catch { throw new Error('Release tag and HEAD must resolve to commits; fetch the full repository and tags'); }
  if (head !== tagged) throw new Error('Release tag does not resolve to the checked-out HEAD commit');
  try { await git(cwd, ['merge-base', '--is-ancestor', head, 'refs/remotes/origin/main']); }
  catch { throw new Error('Release commit must be reachable from origin/main; fetch the full main branch'); }
  if (await git(cwd, ['status', '--porcelain=v1', '--untracked-files=all'])) throw new Error('Release checkout must have no modified or untracked source files');
}

async function regularFile(path, maxBytes) {
  const info = await lstat(path);
  if (!info.isFile() || info.size > maxBytes) throw new Error('Release artifact must be a bounded regular file');
}

async function checkArchive(cwd, manifest, metadata) {
  const directory = join(cwd, 'dist');
  if (!(await lstat(directory)).isDirectory()) throw new Error('dist must be a real directory');
  const filename = basename(metadata.archive);
  const expectedEntries = [filename, `${filename}.sha256`].sort();
  if (JSON.stringify((await readdir(directory)).sort()) !== JSON.stringify(expectedEntries)) {
    throw new Error('dist must contain only the expected release archive and its SHA256 file');
  }
  const archive = join(cwd, metadata.archive);
  const checksum = `${archive}.sha256`;
  await regularFile(archive, 32 * 1024 * 1024);
  await regularFile(checksum, 1024);
  const checksumText = await readFile(checksum, 'utf8');
  const checksumMatch = /^([a-f0-9]{64})  ([^\r\n]+)\n?$/.exec(checksumText);
  if (!checksumMatch || checksumMatch[0] !== checksumText || checksumMatch[2] !== filename) {
    throw new Error('SHA256 file must identify the exact release archive');
  }
  const actualChecksum = createHash('sha256').update(await readFile(archive)).digest('hex');
  if (checksumMatch[1] !== actualChecksum) throw new Error('Release archive SHA256 does not match');
  const files = await archiveFiles(archive);
  const embeddedManifest = JSON.parse(files.get('package.json').toString('utf8'));
  releaseMetadata(embeddedManifest, `v${metadata.version}`);
  // Require the entire manifest, not just its version, to match this checkout.
  if (JSON.stringify(embeddedManifest) !== JSON.stringify(manifest)) throw new Error('Archive package.json differs from the checkout');

  const temporary = await mkdtemp(join(tmpdir(), 'autorouter-release-check-'));
  try {
    const plan = await packagePlan(cwd, await npmEnvironment(temporary));
    const expectedFiles = plan.files.map(file => file.path).sort();
    if (JSON.stringify([...files.keys()].sort()) !== JSON.stringify(expectedFiles)) {
      throw new Error('Release archive file list differs from the checkout package plan');
    }
    for (const [path, bytes] of files) {
      const source = join(cwd, path);
      await regularFile(source, 32 * 1024 * 1024);
      if (!bytes.equals(await readFile(source))) throw new Error(`Archive content differs from checkout: ${path}`);
    }
  } finally { await rm(temporary, { recursive: true, force: true }); }
}

export async function checkRelease(mode, tag, { cwd = process.cwd(), env = process.env } = {}) {
  if (!['source', 'archive'].includes(mode)) throw new Error('Usage: node scripts/release-check.mjs source|archive v<version>');
  const manifest = JSON.parse(await readFile(join(cwd, 'package.json'), 'utf8'));
  const metadata = releaseMetadata(manifest, tag, env);
  if (mode === 'source') await checkSource(cwd, tag);
  else await checkArchive(cwd, manifest, metadata);
  return metadata;
}

async function main() {
  if (process.argv.length !== 4) throw new Error('Usage: node scripts/release-check.mjs source|archive v<version>');
  const metadata = await checkRelease(process.argv[2], process.argv[3]);
  if (process.env.GITHUB_OUTPUT) {
    await appendFile(process.env.GITHUB_OUTPUT, Object.entries(metadata).map(([key, value]) => `${key}=${value}\n`).join(''));
  }
  console.log(`Verified ${process.argv[2]} release: ${JSON.stringify(metadata)}`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => { console.error(`Release check failed: ${error.message}`); process.exitCode = 1; });
}
