#!/usr/bin/env node
import { createHash, randomUUID } from 'node:crypto';
import { appendFile, lstat, mkdir, mkdtemp, opendir, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { basename, dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { archiveFiles, npmEnvironment, run } from './release-pack.mjs';
import { releaseMetadata } from './release-check.mjs';

const PACKAGE = 'claude-autorouter';
const REPOSITORY = 'frapposelli/claude-autorouter';
const REGISTRY = 'https://registry.npmjs.org';
const MAX_ARCHIVE = 32 * 1024 * 1024;
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
class ReleaseError extends Error {
  constructor(code, message, detail = {}) { super(message); this.code = code; this.detail = detail; }
}
const unavailable = (reason, detail = {}) => new ReleaseError('registry_unavailable', 'The registry cannot currently establish release availability.', { reason, ...detail });
const mismatch = reason => new ReleaseError('release_mismatch', 'Published release evidence does not match the tested archive.', { reason });

export function parseVersion(value) {
  if (typeof value !== 'string' || value.length > 199) throw new ReleaseError('invalid_version', 'A strict SemVer version without build metadata is required.');
  const match = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/.exec(value);
  if (!match || match[0] !== value || match.slice(1, 4).some(part => !Number.isSafeInteger(Number(part)))
    || match[4]?.split('.').some(part => /^\d+$/.test(part) && part.length > 1 && part.startsWith('0'))) {
    throw new ReleaseError('invalid_version', 'A strict SemVer version without build metadata is required.');
  }
  return { numbers: match.slice(1, 4).map(Number), prerelease: match[4]?.split('.') ?? [] };
}
export function compareVersions(a, b) {
  const left = parseVersion(a), right = parseVersion(b);
  for (let i = 0; i < 3; i++) if (left.numbers[i] !== right.numbers[i]) return left.numbers[i] < right.numbers[i] ? -1 : 1;
  if (!left.prerelease.length || !right.prerelease.length) return left.prerelease.length === right.prerelease.length ? 0 : left.prerelease.length ? -1 : 1;
  for (let i = 0; i < Math.max(left.prerelease.length, right.prerelease.length); i++) {
    const x = left.prerelease[i], y = right.prerelease[i];
    if (x === y) continue;
    if (x === undefined || y === undefined) return x === undefined ? -1 : 1;
    const nx = /^\d+$/.test(x), ny = /^\d+$/.test(y);
    if (nx && ny) return BigInt(x) < BigInt(y) ? -1 : 1;
    if (nx !== ny) return nx ? -1 : 1;
    return x < y ? -1 : 1;
  }
  return 0;
}

export async function inspectReleaseArtifact(archive, tag) {
  if (typeof tag !== 'string' || !tag.startsWith('v')) throw new ReleaseError('invalid_version', 'Release tag must be v<version>.');
  parseVersion(tag.slice(1));
  const path = resolve(archive), filename = `${PACKAGE}-${tag.slice(1)}.tgz`;
  if (basename(path) !== filename) throw new ReleaseError('invalid_archive', 'Archive filename must match the release tag.');
  const [stat, checksumStat] = await Promise.all([lstat(path), lstat(`${path}.sha256`)]);
  if (!stat.isFile() || stat.size > MAX_ARCHIVE || !checksumStat.isFile() || checksumStat.size > 1024) throw new ReleaseError('invalid_archive', 'Archive and checksum must be bounded regular files.');
  const bytes = await readFile(path);
  const sha256 = createHash('sha256').update(bytes).digest('hex');
  if ((await readFile(`${path}.sha256`, 'utf8')).trimEnd() !== `${sha256}  ${filename}`) throw new ReleaseError('invalid_archive', 'Canonical archive checksum does not match.');
  const files = await archiveFiles(path);
  const manifest = JSON.parse(files.get('package.json').toString('utf8'));
  const metadata = releaseMetadata(manifest, tag, {});
  return { version: metadata.version, tag, dist_tag: metadata.dist_tag, archive: path, filename,
    sha256, integrity: `sha512-${createHash('sha512').update(bytes).digest('base64')}`, bytes: bytes.length };
}

async function boundedResponse(response, maximum) {
  const length = response.headers.get('content-length');
  if (length && Number(length) > maximum) { await response.body?.cancel().catch(() => {}); throw unavailable('oversized_response'); }
  const chunks = [], reader = response.body?.getReader();
  if (!reader) return Buffer.alloc(0);
  let size = 0;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) return Buffer.concat(chunks, size);
      size += value.byteLength;
      if (size > maximum) { await reader.cancel().catch(() => {}); throw unavailable('oversized_response'); }
      chunks.push(Buffer.from(value));
    }
  } finally { reader.releaseLock(); }
}
async function get(url, { fetchImpl = fetch, signal, json = true, timeoutMs = 10000, headers = {} } = {}) {
  const deadline = AbortSignal.timeout(Math.max(1, Math.ceil(timeoutMs)));
  try {
    const response = await fetchImpl(url, { redirect: 'error', signal: signal ? AbortSignal.any([signal, deadline]) : deadline,
      headers: { accept: json ? 'application/json' : 'application/octet-stream', 'cache-control': 'no-cache, no-store', pragma: 'no-cache', ...headers } });
    if (response.status === 404) { await response.body?.cancel().catch(() => {}); return undefined; }
    if (!response.ok) { await response.body?.cancel().catch(() => {}); throw unavailable('http_error', { http_status: response.status }); }
    const body = await boundedResponse(response, json ? 16 * 1024 * 1024 : MAX_ARCHIVE);
    if (!json) return body;
    try { return JSON.parse(body.toString('utf8')); } catch { throw unavailable('invalid_json'); }
  } catch (error) {
    if (signal?.aborted) throw signal.reason;
    if (error instanceof ReleaseError) throw error;
    throw unavailable(deadline.aborted ? 'timeout' : 'network_error');
  }
}
async function registryEvidence(artifact, options) {
  const nonce = options.nonce?.() ?? randomUUID();
  const urls = [`${REGISTRY}/${PACKAGE}/${artifact.version}?autorouter_verify=${encodeURIComponent(nonce)}`,
    `${REGISTRY}/${PACKAGE}?autorouter_verify=${encodeURIComponent(nonce)}`];
  const results = await Promise.allSettled(urls.map(url => get(url, options)));
  for (const result of results) if (result.status === 'rejected') throw result.reason;
  const [exact, packument] = results.map(result => result.value);
  if (packument !== undefined && (!object(packument) || packument.name !== PACKAGE || !object(packument['dist-tags']) || !object(packument.versions))) throw unavailable('invalid_package_metadata');
  const packagedVersion = packument?.versions[artifact.version];
  const metadata = exact ?? packagedVersion;
  for (const metadata of [exact, packagedVersion]) if (metadata !== undefined) {
    if (!object(metadata) || metadata.name !== PACKAGE || metadata.version !== artifact.version || !object(metadata.dist)) throw mismatch('metadata_identity');
    if (metadata.dist.integrity !== artifact.integrity) throw mismatch('integrity');
    const expectedTarball = `${REGISTRY}/${PACKAGE}/-/${artifact.filename}`;
    if (metadata.dist.tarball !== expectedTarball) throw mismatch('tarball_location');
  }
  return { metadata, tags: packument?.['dist-tags'], metadata_source: exact ? 'version' : metadata ? 'packument' : 'unavailable' };
}
const reportBase = artifact => ({ schema_version: 1, package: PACKAGE, version: artifact.version, tag: artifact.tag,
  dist_tag: artifact.dist_tag, archive: artifact.filename, sha256: artifact.sha256, integrity: artifact.integrity });

export async function preflightRelease(artifact, options = {}) {
  const evidence = await registryEvidence(artifact, options);
  if (!evidence.tags) throw unavailable('package_metadata_unavailable');
  const current = evidence.tags[artifact.dist_tag];
  if (current !== undefined) {
    try { parseVersion(current); } catch { throw unavailable('invalid_dist_tag'); }
    if (artifact.dist_tag === 'latest' && parseVersion(current).prerelease.length) throw unavailable('latest_is_not_stable');
  }
  if (evidence.metadata) return { ...reportBase(artifact), state: 'submitted', phase: 'preflight', publish: false,
    reason: 'identical_version_exists', current_dist_tag: current ?? null, metadata_source: evidence.metadata_source };
  if (options.allowPublish === false) return { ...reportBase(artifact), state: 'validating_unavailable', phase: 'preflight', publish: false,
    reason: 'retry_submission_unknown', current_dist_tag: current ?? null };
  if (artifact.dist_tag === 'latest' && (current === undefined || compareVersions(artifact.version, current) <= 0)) {
    throw new ReleaseError('version_order', 'A new stable release must be newer than the current stable latest.', { current_latest: current ?? null });
  }
  return { ...reportBase(artifact), state: 'preflight_ready', phase: 'preflight', publish: true,
    current_dist_tag: current ?? null, reason: 'version_not_published' };
}

async function compareInstalledFiles(root, files) {
  const directories = new Set(['']);
  for (const path of files.keys()) {
    let parent = dirname(path);
    while (parent !== '.') { directories.add(parent); parent = dirname(parent); }
  }
  const found = new Set();
  async function visit(relative = '') {
    const current = join(root, relative), stat = await lstat(current);
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw mismatch('installed_archive_structure');
    for await (const entry of await opendir(current)) {
      const path = relative ? `${relative}/${entry.name}` : entry.name;
      if (entry.isDirectory() && directories.has(path)) await visit(path);
      else if (entry.isFile() && files.has(path)) {
        const expected = files.get(path), info = await lstat(join(root, path));
        if (!info.isFile() || info.size !== expected.length || !(await readFile(join(root, path))).equals(expected)) throw mismatch('installed_archive_bytes');
        found.add(path);
      } else throw mismatch('installed_archive_structure');
    }
  }
  await visit();
  if (found.size !== files.size) throw mismatch('installed_archive_structure');
}

export async function installPublishedRelease(artifact, { runCommand = run, readArchive = archiveFiles,
  timeoutMs = 120000, now = () => performance.now() } = {}) {
  const temporary = await mkdtemp(join(tmpdir(), 'autorouter-registry-verify-'));
  const start = now();
  const remaining = maximum => {
    const left = timeoutMs - (now() - start);
    if (left <= 0) throw unavailable('isolated_install_timeout');
    return Math.max(1, Math.ceil(Math.min(maximum, left)));
  };
  try {
    const canonicalFiles = await readArchive(artifact.archive);
    const env = await npmEnvironment(join(temporary, 'npm'));
    const prefix = join(temporary, 'install prefix'), cwd = join(temporary, 'unrelated cwd');
    await mkdir(cwd);
    try { await runCommand('npm', ['install', '--global', '--prefix', prefix, '--ignore-scripts', '--no-audit', '--no-fund',
      '--prefer-online', '--registry', `${REGISTRY}/`, `${PACKAGE}@${artifact.version}`], { cwd, env, timeoutMs: remaining(120000) }); }
    catch { throw unavailable('isolated_install_unavailable'); }
    const cli = join(prefix, 'bin', PACKAGE);
    const installedRoot = join(prefix, 'lib', 'node_modules', PACKAGE);
    await compareInstalledFiles(installedRoot, canonicalFiles);
    if (await realpath(cli) !== await realpath(join(installedRoot, 'bin', 'autorouter.mjs'))) throw mismatch('installed_cli_target');
    let version, help;
    try {
      version = await runCommand(cli, ['--version'], { cwd, env, timeoutMs: remaining(10000) });
      help = await runCommand(cli, ['--help'], { cwd, env, timeoutMs: remaining(10000) });
    } catch (error) {
      if (error?.code === 'registry_unavailable') throw error;
      if (now() - start >= timeoutMs || /timed out/.test(error?.message ?? '')) throw unavailable('isolated_install_timeout');
      throw mismatch('installed_cli_failed');
    }
    if (version.stdout.trim() !== artifact.version || !help.stdout.includes('claude-autorouter') || !help.stdout.includes('setup')) throw mismatch('installed_cli_identity');
    return { version: artifact.version, help: true, isolated: true, registry: REGISTRY };
  } finally { await rm(temporary, { recursive: true, force: true }); }
}

const sleep = (ms, signal) => new Promise((resolveWait, reject) => {
  const abort = () => { clearTimeout(timer); reject(signal.reason); };
  const timer = setTimeout(() => { signal?.removeEventListener('abort', abort); resolveWait(); }, ms);
  if (signal?.aborted) abort();
  else signal?.addEventListener('abort', abort, { once: true });
});
export async function verifyRelease(artifact, { timeoutMs = 15 * 60 * 1000, fetchImpl = fetch, signal,
  now = () => performance.now(), wait = sleep, nonce, install = installPublishedRelease, onState = () => {} } = {}) {
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 3600000) throw new ReleaseError('invalid_timeout', 'Verification timeout must be 1–3600000 ms.');
  const start = now(), events = [], base = reportBase(artifact);
  let attempts = 0, delay = 1000, last;
  const state = async row => {
    last = { ...base, ...row, attempts, elapsed_ms: Math.max(0, Math.round(now() - start)) };
    events.push({ state: row.state, reason: row.reason, elapsed_ms: last.elapsed_ms });
    if (events.length > 32) events.shift();
    await onState(last);
    return last;
  };
  while (now() - start < timeoutMs) {
    if (signal?.aborted) throw signal.reason;
    attempts++;
    try {
      const requestOptions = () => {
        const remaining = timeoutMs - (now() - start);
        if (remaining <= 0) throw unavailable('verification_deadline');
        return { fetchImpl, signal, nonce, timeoutMs: Math.min(10000, remaining) };
      };
      const evidence = await registryEvidence(artifact, requestOptions());
      if (!evidence.metadata) throw unavailable('version_not_available');
      const tarball = await get(`${evidence.metadata.dist.tarball}?autorouter_verify=${encodeURIComponent(nonce?.() ?? randomUUID())}`,
        { ...requestOptions(), json: false });
      if (!tarball) throw unavailable('tarball_not_available');
      if (tarball.length !== artifact.bytes || createHash('sha256').update(tarball).digest('hex') !== artifact.sha256
        || `sha512-${createHash('sha512').update(tarball).digest('base64')}` !== artifact.integrity) throw mismatch('tarball_bytes');
      const current = evidence.tags?.[artifact.dist_tag];
      if (typeof current !== 'string') throw unavailable('dist_tag_unavailable');
      let order;
      try {
        if (artifact.dist_tag === 'latest' && parseVersion(current).prerelease.length) throw new Error('Unstable latest');
        order = compareVersions(current, artifact.version);
      } catch { throw unavailable('invalid_dist_tag'); }
      if (order < 0) throw unavailable('dist_tag_not_updated');
      requestOptions(); // Do not start an install after the polling deadline.
      const installed = await install(artifact, { timeoutMs: Math.max(1, Math.min(120000, timeoutMs - (now() - start))) });
      await state({ state: 'verified', reason: 'archive_and_public_install_verified', current_dist_tag: current,
        dist_tag_status: order > 0 ? 'superseded_by_newer_version' : 'current', install: installed });
      return { ...last, events };
    } catch (error) {
      if (signal?.aborted) throw signal.reason;
      if (error?.code !== 'registry_unavailable') {
        await state({ state: 'failed', reason: error?.detail?.reason ?? error?.code ?? 'verification_error' });
        return { ...last, events };
      }
      await state({ state: 'validating_unavailable', reason: error.detail.reason,
        ...(error.detail.http_status ? { http_status: error.detail.http_status } : {}) });
    }
    const remaining = timeoutMs - (now() - start);
    if (remaining <= 0) break;
    await wait(Math.min(delay, remaining), signal);
    delay = Math.min(15000, delay * 2);
  }
  return { ...last, state: 'validating_unavailable', pending: true, elapsed_ms: Math.max(0, Math.round(now() - start)), events,
    message: 'Verification remains pending. Keep the original archive and rerun verification; do not submit a duplicate release.' };
}

export function validateArtifactSource(runInfo, artifactInfo, { runId, artifactId, commit, tag }) {
  if (!/^[1-9]\d*$/.test(String(runId)) || !/^[1-9]\d*$/.test(String(artifactId))
    || !Number.isSafeInteger(Number(runId)) || !Number.isSafeInteger(Number(artifactId)) || !/^[a-f0-9]{40}$/.test(commit)) throw new Error('Invalid release artifact identity.');
  if (typeof tag !== 'string' || !tag.startsWith('v')) throw new Error('Release tag must be v<version>.');
  parseVersion(tag.slice(1));
  if (runInfo.id !== Number(runId) || runInfo.event !== 'push' || runInfo.path !== '.github/workflows/publish.yml'
    || runInfo.head_sha !== commit || runInfo.head_branch !== tag || runInfo.head_repository?.full_name !== REPOSITORY
    || artifactInfo.id !== Number(artifactId) || artifactInfo.expired !== false
    || !new RegExp(`^npm-package-${runId}-[1-9][0-9]*$`).test(artifactInfo.name)
    || artifactInfo.workflow_run?.id !== Number(runId) || artifactInfo.workflow_run?.head_sha !== commit) {
    throw new Error('Artifact must be the canonical package from the original tag-triggered release run.');
  }
  return { run_id: String(runId), artifact_id: String(artifactId), notes_artifact_name: artifactInfo.name.replace(/^npm-package-/, 'release-notes-') };
}
async function artifactSource(tag, args) {
  if (!tag?.startsWith('v')) throw new Error('Release tag must be v<version>.');
  parseVersion(tag.slice(1));
  const runId = args['--run-id'], artifactId = args['--artifact-id'];
  if (!/^[1-9][0-9]{0,15}$/.test(runId ?? '') || !/^[1-9][0-9]{0,15}$/.test(artifactId ?? '')) throw new Error('Run and artifact IDs must be positive integers.');
  const { stdout } = await run('git', ['rev-parse', '--verify', `refs/tags/${tag}^{commit}`]);
  await run('git', ['merge-base', '--is-ancestor', stdout.trim(), 'refs/remotes/origin/main']);
  const token = process.env.GH_TOKEN;
  if (!token) throw new Error('GH_TOKEN is required to read the original private workflow artifact.');
  const base = `https://api.github.com/repos/${REPOSITORY}/actions`;
  const [runInfo, artifactInfo] = await Promise.all([get(`${base}/runs/${runId}`, { headers: { authorization: `Bearer ${token}` } }),
    get(`${base}/artifacts/${artifactId}`, { headers: { authorization: `Bearer ${token}` } })]);
  return validateArtifactSource(runInfo ?? {}, artifactInfo ?? {}, { runId, artifactId, commit: stdout.trim(), tag });
}
async function notes(artifact, path) {
  const head = (await run('git', ['rev-parse', '--verify', 'HEAD^{commit}'])).stdout.trim();
  let previous;
  try { previous = (await run('git', ['describe', '--tags', '--abbrev=0', '--match', 'v*', 'HEAD^'])).stdout.trim(); } catch {}
  const commits = (await run('git', ['log', '--format=- %s', ...(previous ? [`${previous}..HEAD`] : ['-n', '20']), '--'])).stdout.trim();
  await mkdir(dirname(resolve(path)), { recursive: true });
  await writeFile(path, `# ${PACKAGE} ${artifact.version}\n\nSource commit: ${head}\n\nCanonical archive: ${artifact.filename}\nSHA256: ${artifact.sha256}\n\n## Changes${previous ? ` since ${previous}` : ' (recent commits)'}\n\n${commits}\n\n## Registry verification\n\nSubmission and public availability are separate states. See the retained verification report.\n\nAfter verification: \`npm install -g ${PACKAGE}@${artifact.version}\`\n`);
}
async function persist(path, report) {
  if (!path) return;
  await mkdir(dirname(resolve(path)), { recursive: true });
  await writeFile(path, `${JSON.stringify(report, null, 2)}\n`);
}
async function output(values) {
  if (process.env.GITHUB_OUTPUT) await appendFile(process.env.GITHUB_OUTPUT, Object.entries(values).map(([key, value]) => `${key}=${value}\n`).join(''));
}
async function main() {
  const [mode, tag, ...flags] = process.argv.slice(2), args = {};
  if (!['preflight', 'submitted', 'verify', 'artifact-source', 'notes'].includes(mode) || flags.length % 2) throw new Error('Usage: release-verify.mjs preflight|submitted|verify|notes TAG --archive PATH --report PATH; artifact-source TAG --run-id N --artifact-id N');
  for (let i = 0; i < flags.length; i += 2) {
    if (!['--archive', '--report', '--timeout-ms', '--run-id', '--artifact-id'].includes(flags[i]) || Object.hasOwn(args, flags[i])) throw new Error('Unknown or duplicate release verification option.');
    args[flags[i]] = flags[i + 1];
  }
  if (mode === 'artifact-source') { const result = await artifactSource(tag, args); await output(result); console.log('Canonical release artifact provenance verified.'); return; }
  let artifact, report;
  try {
    if (!args['--archive']) throw new Error('Provide the original canonical archive with --archive.');
    artifact = await inspectReleaseArtifact(args['--archive'], tag);
    if (mode === 'notes') { if (!args['--report']) throw new Error('Provide the release notes path with --report.'); await notes(artifact, args['--report']); return; }
    if (mode === 'preflight') report = await preflightRelease(artifact, {
      allowPublish: process.env.GITHUB_RUN_ATTEMPT === undefined || process.env.GITHUB_RUN_ATTEMPT === '1',
    });
    else if (mode === 'submitted') report = { ...reportBase(artifact), state: 'submitted', timestamp: new Date().toISOString() };
    else {
      if (args['--timeout-ms'] !== undefined && !/^[0-9]+$/.test(args['--timeout-ms'])) throw new Error('Verification timeout must be an integer.');
      report = await verifyRelease(artifact, { ...(args['--timeout-ms'] ? { timeoutMs: Number(args['--timeout-ms']) } : {}),
        onState: async state => { await persist(args['--report'], state); console.log(`${state.state}: ${state.reason} (attempt ${state.attempts})`); } });
    }
  } catch (error) {
    report = { ...(artifact ? reportBase(artifact) : { schema_version: 1 }), state: 'failed', phase: mode,
      reason: error?.code ?? 'invalid_release_input', error_code: error?.code ?? 'invalid_release_input', ...(error instanceof ReleaseError ? error.detail : {}) };
  }
  await persist(args['--report'], report);
  await output({ state: report.state, ...(mode === 'preflight' && report.state !== 'failed' ? { publish: String(report.publish), can_verify: 'true' } : {}) });
  console.log(JSON.stringify(report, null, 2));
  if (report.state === 'failed') process.exitCode = 1;
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(() => { console.error('Release verification could not complete safely.'); process.exitCode = 1; });
}
