#!/usr/bin/env node
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { gunzipSync } from 'node:zlib';

const ROOT_FILES = new Set(['package.json', 'README.md', 'CONTRIBUTING.md', 'LICENSE', '.env.example',
  'SECURITY.md', 'SUPPORT.md', 'CODE_OF_CONDUCT.md']);
const CORE_DOC_FILES = ['docs/reference.md', 'docs/development.md', 'docs/releasing.md', 'docs/ollama-evaluation.md'];
const DOC_FILES = new Set([...CORE_DOC_FILES, 'docs/subscription-integration.md', 'docs/router-performance.md', 'docs/router-performance.json',
  'docs/status-performance.md', 'docs/status-performance.json', 'docs/hardware-benchmark.md',
  'docs/hardware-results-16gb.md', 'docs/hardware-results-16gb.json',
  'docs/hardware-comparison.md', 'docs/hardware-results-64gb.json']);
const REQUIRED_FILES = ['package.json', 'README.md', 'bin/autorouter.mjs', 'bin/statusline.mjs',
  'src/config.mjs', 'src/router.mjs', 'src/server.mjs', ...CORE_DOC_FILES];

export function assertPackageFiles(files) {
  const paths = files.map(file => typeof file === 'string' ? file : file.path);
  if (new Set(paths).size !== paths.length) throw new Error('Package contains duplicate paths');
  for (const path of paths) {
    const runtime = /^(?:bin|src)\/(?:[A-Za-z0-9_-]+\/)*[A-Za-z0-9_-]+\.mjs$/.test(path);
    const privatePath = /(?:^|[\/._-])(?:artifacts?|tests?|specs?|fixtures?|probes?|secrets?|node_modules)(?:[\/._-]|$)/i.test(path);
    if ((!ROOT_FILES.has(path) && !DOC_FILES.has(path) && !runtime) || privatePath) {
      throw new Error(`Unexpected file in public package: ${path}`);
    }
  }
  for (const required of REQUIRED_FILES) {
    if (!paths.includes(required)) throw new Error(`Public package is missing ${required}`);
  }
  return paths.sort();
}

export function run(command, args, { cwd, env, timeoutMs = 60000, input } = {}) {
  return new Promise((resolveRun, reject) => {
    const child = spawn(command, args, { cwd, env, stdio: ['pipe', 'pipe', 'pipe'] });
    let stdout = '', stderr = '', bytes = 0, failure;
    const timer = setTimeout(() => {
      failure = new Error(`${basename(command)} timed out`);
      child.kill('SIGKILL');
    }, timeoutMs);
    const collect = destination => chunk => {
      bytes += chunk.length;
      if (bytes > 4 * 1024 * 1024) {
        failure = new Error(`${basename(command)} exceeded the output limit`);
        child.kill('SIGKILL');
      } else if (destination === 'stdout') stdout += chunk;
      else stderr += chunk;
    };
    child.stdout.on('data', collect('stdout'));
    child.stderr.on('data', collect('stderr'));
    child.once('error', error => { clearTimeout(timer); reject(error); });
    child.once('close', (code, signal) => {
      clearTimeout(timer);
      if (failure) reject(failure);
      else if (code !== 0) reject(new Error(`${basename(command)} exited ${code ?? signal}: ${stderr.trim() || stdout.trim()}`));
      else resolveRun({ stdout, stderr });
    });
    child.stdin.on('error', () => {});
    child.stdin.end(input);
  });
}

// Keep the user's HOME value unchanged, while using separate npm config and
// cache files and an environment without inherited API keys or auth tokens.
export async function npmEnvironment(directory) {
  await mkdir(directory, { recursive: true });
  const userConfig = join(directory, 'user.npmrc');
  const globalConfig = join(directory, 'global.npmrc');
  await Promise.all([writeFile(userConfig, ''), writeFile(globalConfig, '')]);
  const inherited = new Set(['PATH', 'HOME', 'USERPROFILE', 'SystemRoot', 'WINDIR', 'TMPDIR', 'TMP', 'TEMP', 'LANG', 'LC_ALL']);
  return {
    ...Object.fromEntries(Object.entries(process.env).filter(([key]) => inherited.has(key))),
    npm_config_cache: join(directory, 'cache'), npm_config_userconfig: userConfig,
    npm_config_globalconfig: globalConfig, npm_config_update_notifier: 'false',
    npm_config_audit: 'false', npm_config_fund: 'false', npm_config_ignore_scripts: 'true',
  };
}

export async function packagePlan(project, env) {
  const { stdout } = await run('npm', ['pack', '--dry-run', '--json', '--ignore-scripts', '--offline'], { cwd: project, env });
  const plans = JSON.parse(stdout);
  if (!Array.isArray(plans) || plans.length !== 1 || !Array.isArray(plans[0].files)) throw new Error('Unexpected npm pack report');
  assertPackageFiles(plans[0].files);
  return plans[0];
}

// Inspect the actual gzip/tar payload as well as npm's file report. This
// package has short, ordinary paths and needs no links or extended headers.
export async function archiveFiles(path) {
  const tar = gunzipSync(await readFile(path), { maxOutputLength: 32 * 1024 * 1024 });
  const files = new Map();
  let offset = 0;
  while (offset + 512 <= tar.length) {
    const header = tar.subarray(offset, offset + 512);
    if (header.every(byte => byte === 0)) break;
    const field = (start, end) => header.subarray(start, end).toString('utf8').split('\0')[0];
    const prefix = field(345, 500);
    const name = `${prefix ? `${prefix}/` : ''}${field(0, 100)}`;
    const rawSize = field(124, 136).trim();
    if (!/^[0-7]+$/.test(rawSize)) throw new Error('Unexpected tar size field');
    const size = Number.parseInt(rawSize, 8);
    const type = field(156, 157);
    if (!Number.isSafeInteger(size) || size < 0 || offset + 512 + size > tar.length) throw new Error('Invalid tar entry size');
    if (!name.startsWith('package/')) throw new Error('Tar entry is outside package/');
    if (type === '' || type === '0') {
      const relative = name.slice('package/'.length);
      if (files.has(relative)) throw new Error('Duplicate tar entry');
      files.set(relative, tar.subarray(offset + 512, offset + 512 + size));
    } else if (type !== '5') throw new Error(`Unexpected tar entry type for ${name}`);
    offset += 512 + Math.ceil(size / 512) * 512;
  }
  assertPackageFiles([...files.keys()]);
  return files;
}

export async function packPackage(project, destination, env) {
  const plan = await packagePlan(project, env);
  await mkdir(destination, { recursive: true });
  const { stdout } = await run('npm', ['pack', '--json', '--ignore-scripts', '--offline', '--pack-destination', destination], { cwd: project, env });
  const reports = JSON.parse(stdout);
  if (!Array.isArray(reports) || reports.length !== 1) throw new Error('Unexpected npm pack result');
  const report = reports[0];
  if (typeof report.filename !== 'string' || basename(report.filename) !== report.filename || !report.filename.endsWith('.tgz')) {
    throw new Error('Unsafe npm archive filename');
  }
  const archive = join(destination, report.filename);
  try {
    const expected = assertPackageFiles(plan.files);
    if (JSON.stringify(assertPackageFiles(report.files)) !== JSON.stringify(expected)) throw new Error('Package files changed after the dry run');
    const actual = await archiveFiles(archive);
    if (JSON.stringify([...actual.keys()].sort()) !== JSON.stringify(expected)) throw new Error('Archive does not match the npm file report');
    const manifest = JSON.parse(actual.get('package.json').toString('utf8'));
    if (manifest.private) throw new Error('Package is still marked private');
    if (Object.keys(manifest.dependencies ?? {}).length || Object.keys(manifest.optionalDependencies ?? {}).length) {
      throw new Error('This package must remain dependency-free');
    }
    return { archive, report, files: actual, manifest };
  } catch (error) { await rm(archive, { force: true }); throw error; }
}

async function main() {
  const project = resolve(dirname(fileURLToPath(import.meta.url)), '..');
  const temporary = await mkdtemp(join(tmpdir(), 'autorouter-release-'));
  try {
    const env = await npmEnvironment(temporary);
    const { archive, report } = await packPackage(project, join(project, 'dist'), env);
    const checksum = createHash('sha256').update(await readFile(archive)).digest('hex');
    await writeFile(`${archive}.sha256`, `${checksum}  ${basename(archive)}\n`);
    console.log(`Created dist/${basename(archive)} (${report.files.length} verified files)`);
    console.log(`SHA256 ${checksum}`);
  } finally { await rm(temporary, { recursive: true, force: true }); }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => { console.error(`Release pack failed: ${error.message}`); process.exitCode = 1; });
}
