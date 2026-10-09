// Exact observed differential with retained failures; only the selected declared scope.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, X509Certificate } from 'node:crypto';
import { chmod, copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { declaration } from './buffered-response-cases.mjs';

assert.equal(process.version, 'v22.14.0');
const args = process.argv.slice(2), selectedGroup = args.find(value => value.startsWith('--group='))?.slice(8) ?? 'original-http';
const selectedId = args.find(value => value.startsWith('--case='))?.slice(7);
const nativePath = args.find(value => value.startsWith('--native='))?.slice(9);
assert.ok(nativePath || args.includes('--list'), 'Explicit native executable required');
assert.ok(args.filter(value => value.startsWith('--native=')).length <= 1);
assert.ok(args.every(value => value.startsWith('--group=') || value.startsWith('--case=') || value === '--list' || value.startsWith('--native=')), 'Only declared group/case selection is supported');
assert.equal(args.filter(value => value.startsWith('--group=')).length <= 1, true); assert.equal(args.filter(value => value.startsWith('--case=')).length <= 1, true);
const selected = declaration.cases.filter(spec => (selectedGroup === 'all' || (selectedGroup === 'native-declared' && !spec.reference_only) || spec.group === selectedGroup) && (!selectedId || spec.id === selectedId));
assert.ok(selected.length, 'Selection must name declared cases');
assert.ok(selected.every(spec => !spec.reference_only), 'Selected handoff characterization has no native implementation yet');
if (args.includes('--list')) { console.log(JSON.stringify({ ...declaration, selected: selected.map(spec => spec.id) }, null, 2)); process.exit(0); }
const root = resolve(import.meta.dirname, '../..'), artifacts = join(root, 'artifacts/rust-rewrite'), evidence = join(artifacts, 'evidence');
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const sourcePaths = ['rust/parity/check-openssl-buffered-differential.mjs', 'rust/parity/buffered-response-native.mjs', 'rust/parity/buffered-response-reference.mjs', 'rust/parity/buffered-response-cases.mjs', 'scripts/rust-reference.mjs', 'rust/parity/baseline.json'];
const builtinNames = ['_http_agent', '_http_client', '_http_common', '_http_incoming', '_http_server', 'internal/streams/readable', 'internal/streams/pipeline', 'net', 'https'];
const children = new Set(), results = [], failures = [], sources = [];
let interrupted, scratch, snapshot, baseline, nodeIdentity, certificateIdentity, nativeIdentity;
const onSignal = signal => { interrupted = signal; for (const child of children) child.kill('SIGTERM'); };
const interrupt = () => onSignal('SIGINT'), terminate = () => onSignal('SIGTERM');
process.once('SIGINT', interrupt); process.once('SIGTERM', terminate);
const errorView = error => ({ name: String(error?.name ?? 'Error').slice(0, 40), message: String(error?.message ?? error).slice(0, 200) });
async function retained(bytes, extension = 'json') {
  const sha256 = hash(bytes), path = join(evidence, `${sha256}.${extension}`);
  await writeFile(path, bytes, { flag: 'wx', mode: 0o600 }).catch(error => { if (error.code !== 'EEXIST') throw error; });
  assert.equal(hash(await readFile(path)), sha256, 'Existing evidence bytes match');
  return { path: path.slice(root.length + 1), sha256, bytes: bytes.length };
}
async function bounded(promise, ms, label) {
  let timer;
  try { return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(Error(label)), ms); })]); }
  finally { clearTimeout(timer); }
}
async function ownedCommand(command, argv, env) {
  assert.equal(interrupted, undefined, 'Setup interrupted');
  const child = spawn(command, argv, { env, stdio: ['ignore', 'pipe', 'pipe'] });
  children.add(child);
  let count = 0, overflow = false, spawnError;
  for (const stream of [child.stdout, child.stderr]) stream.on('data', bytes => { count += bytes.length; if (count > 1048576) { overflow = true; child.kill('SIGTERM'); } });
  child.reaped = new Promise(resolve => {
    child.once('error', error => { spawnError = error; resolve(); });
    child.once('close', (code, signal) => resolve({ code, signal }));
  });
  try {
    const result = await bounded(child.reaped, 10000, 'Synthetic certificate command deadline');
    if (spawnError) throw spawnError;
    assert.equal(result.code, 0, 'Synthetic certificate command failed');
    assert.equal(overflow, false, 'Synthetic certificate command output bound');
    assert.equal(interrupted, undefined, 'Setup interrupted');
  } finally {
    if (child.exitCode === null && child.signalCode === null && !spawnError) {
      child.kill('SIGTERM');
      try { await bounded(child.reaped, 2000, 'Setup command stop/reap'); }
      catch { child.kill('SIGKILL'); await bounded(child.reaped, 2000, 'Setup command kill/reap'); }
    }
    children.delete(child);
  }
}
async function tlsFixtures() {
  const empty = join(scratch, 'empty.cnf'); await writeFile(empty, '');
  const openssl = async (...argv) => ownedCommand('openssl', argv, { PATH: process.env.PATH, OPENSSL_CONF: empty });
  const ca = join(scratch, 'ca'), leaf = join(scratch, 'leaf');
  await writeFile(`${ca}.cnf`, '[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=ext\n[dn]\nCN=Synthetic Buffered Response Root\n[ext]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n');
  await openssl('ecparam', '-genkey', '-name', 'prime256v1', '-noout', '-out', `${ca}.key`);
  await openssl('req', '-new', '-x509', '-sha256', '-key', `${ca}.key`, '-out', `${ca}.crt`, '-days', '1', '-config', `${ca}.cnf`);
  await writeFile(`${leaf}.cnf`, '[req]\nprompt=no\ndistinguished_name=dn\n[dn]\nCN=Synthetic Buffered Response Leaf\n');
  await openssl('ecparam', '-genkey', '-name', 'prime256v1', '-noout', '-out', `${leaf}.key`);
  await openssl('req', '-new', '-sha256', '-key', `${leaf}.key`, '-out', `${leaf}.csr`, '-config', `${leaf}.cnf`);
  await writeFile(`${leaf}.ext`, 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1\n');
  await openssl('x509', '-req', '-sha256', '-in', `${leaf}.csr`, '-CA', `${ca}.crt`, '-CAkey', `${ca}.key`, '-CAcreateserial', '-out', `${leaf}.crt`, '-days', '1', '-extfile', `${leaf}.ext`);
  const rootCert = new X509Certificate(await readFile(`${ca}.crt`)), leafCert = new X509Certificate(await readFile(`${leaf}.crt`));
  assert.notEqual(rootCert.subject, leafCert.subject); assert.equal(leafCert.issuer, rootCert.subject); assert.ok(leafCert.verify(rootCert.publicKey));
  certificateIdentity = { root_sha256: hash(rootCert.raw), leaf_sha256: hash(leafCert.raw), synthetic: true };
  return { NODE_EXTRA_CA_CERTS: `${ca}.crt`, AUTOROUTER_SYNTHETIC_TLS_KEY: `${leaf}.key`, AUTOROUTER_SYNTHETIC_TLS_CERT: `${leaf}.crt`, SSL_CERT_FILE: empty, SSL_CERT_DIR: scratch };
}
async function childCase(spec, tlsEnvironment, mode) {
  const directory = join(scratch, `${spec.id}-${mode}`); await mkdir(directory, { mode: 0o700 });
  const output = join(directory, 'result.json'), diagnostic = [], row = { id: spec.id, protocol: spec.protocol, mode };
  let child, ended, bytes = 0, overflow = false;
  try {
    assert.equal(interrupted, undefined, 'Run interrupted');
    child = spawn(join(snapshot, 'node-reference'), [join(snapshot, `inputs/rust/parity/buffered-response-${mode}.mjs`), join(snapshot, 'reference'), spec.id, output, ...(mode === 'native' ? [join(snapshot, 'runtime-tests')] : [])], { cwd: directory, env: { HOME: directory, XDG_CONFIG_HOME: directory, TMPDIR: directory, PATH: directory, ...tlsEnvironment, ...(spec.protocol === 'HTTP' ? {} : { NODE_OPTIONS: spec.protocol === 'TLSv1.2' ? '--tls-min-v1.2 --tls-max-v1.2' : '--tls-min-v1.3 --tls-max-v1.3' }) }, stdio: ['ignore', 'ignore', 'pipe'] });
    children.add(child);
    ended = new Promise(resolve => { child.once('error', error => { row.spawn_error = errorView(error); resolve(); }); child.once('close', (code, signal) => { row.exit_code = code; row.signal = signal; row.child_reaped = true; resolve(); }); });
    child.reaped = ended;
    child.stderr.on('data', chunk => { bytes += chunk.length; if (bytes > declaration.limits.diagnostic_bytes) { overflow = true; child.kill('SIGTERM'); } else diagnostic.push(chunk); });
    await bounded(ended, declaration.limits.scenario_ms + 6000, 'Reference child outer deadline');
  } catch (error) { row.owner_failure = errorView(error); }
  finally {
    if (child && !row.child_reaped && !row.spawn_error) {
      child.kill('SIGTERM');
      try { await bounded(ended, 2500, 'Reference child graceful stop'); }
      catch (error) { row.stop_failure = errorView(error); child.kill('SIGKILL'); try { await bounded(ended, 2000, 'Reference child kill/reap'); } catch (killError) { row.kill_failure = errorView(killError); } }
    }
    if (child) children.delete(child);
    row.diagnostic_overflow = overflow;
    if (diagnostic.length) row.diagnostic = await retained(Buffer.concat(diagnostic), 'log');
    try {
      const raw = await readFile(output); assert.ok(raw.length <= declaration.limits.control_bytes); row.report = await retained(raw); row.observation = JSON.parse(raw);
      assert.deepEqual(row.observation.case, spec, 'Exact declared case identity'); assert.equal(row.observation.identity.node_sha256, nodeIdentity.sha256); assert.deepEqual(row.observation.identity.builtins, nodeIdentity.builtins);
      if (mode === 'native') assert.equal(row.observation.identity.native_sha256, nativeIdentity.sha256);
    } catch (error) { row.report_failure = errorView(error); }
  }
  row.passed = row.exit_code === 0 && row.child_reaped === true && row.observation?.passed === true && !row.owner_failure && !row.report_failure && !row.stop_failure && !row.kill_failure && !overflow;
  return row;
}
const comparisonContract = {
  declared_before_execution: true,
  exact_fields: ['upstream_status', 'first_failure_cause', 'delivery_completed', 'wire_status', 'body_bytes', 'body_sha256', 'complete_framing', 'trailer_bytes', 'consumer_released'],
  prerequisite: 'Both executions pass the identical declared case relations and owned cleanup.',
  exclusions: ['wire header formatting and chunk boundaries', 'raw timestamps and packet/frame segmentation', 'Node object flags and native implementation IDs', 'TLS session policy beyond the separate retained pool matrix', 'total process heap/RSS and allocator overhead'],
  pending_families: declaration.pending_families,
};
function projection(spec, row) {
  const result = row.observation, trace = result.trace.filter(event => event.phase === 'scenario');
  const logs = trace.filter(event => event.event === 'gateway.log');
  const clean = ['exact-length', 'chunked-trailers'].includes(spec.framing);
  let cause = null, delivered;
  if (row.mode === 'native') {
    cause = result.native_state.terminal.find(event => event.event === 'failed')?.cause ?? null;
    delivered = result.native_state.terminal.some(event => event.event === 'delivery_claimed' && event.flushed);
  } else {
    const aborted = trace.find(event => event.event === 'upstream.aborted')?.ordinal;
    const signal = trace.find(event => event.event === 'signal.abort')?.ordinal;
    if (!clean) cause = signal !== undefined && (aborted === undefined || signal < aborted) ? 'downstream' : 'upstream';
    delivered = trace.some(event => event.event === 'downstream.response.finish');
  }
  return { upstream_status: logs.find(event => event.kind === 'upstream_response')?.status ?? null, first_failure_cause: cause, delivery_completed: delivered, wire_status: result.wire.status, body_bytes: result.wire.body_bytes, body_sha256: result.wire.body_sha256, complete_framing: result.wire.complete_framing, trailer_bytes: result.wire.trailer_bytes, consumer_released: result.observations.at(-1)?.consumer_released ?? false };
}
function compare(spec, reference, native) {
  try {
    assert.ok(reference.passed && native?.passed, 'Both exact schedules and cleanup must pass');
    const left = projection(spec, reference), right = projection(spec, native);
    const differences = comparisonContract.exact_fields.filter(field => JSON.stringify(left[field]) !== JSON.stringify(right[field]));
    return { matches: !differences.length, differences, reference: left, native: right };
  } catch (error) { return { matches: false, failure: errorView(error) }; }
}
try {
  await mkdir(evidence, { recursive: true }); scratch = await mkdtemp(join(tmpdir(), 'autorouter-buffered-response-')); snapshot = await mkdtemp(join(artifacts, 'buffered-response-differential-snapshot-'));
  const nativeBytes = await readFile(resolve(nativePath)); await writeFile(join(snapshot, 'runtime-tests'), nativeBytes, { mode: 0o500 });
  nativeIdentity = { path: resolve(nativePath), snapshot: join(snapshot, 'runtime-tests').slice(root.length + 1), sha256: hash(nativeBytes), bytes: nativeBytes.length };
  for (const path of sourcePaths) {
    const bytes = await readFile(join(root, path)), target = join(snapshot, 'inputs', path); await mkdir(join(target, '..'), { recursive: true }); await writeFile(target, bytes, { mode: 0o444 });
    sources.push({ source: path, snapshot: target.slice(root.length + 1), ...await retained(bytes, path.endsWith('.json') ? 'json' : 'mjs') });
  }
  const manifestBytes = Buffer.from(JSON.stringify({ ...declaration, selected: selected.map(spec => spec.id) }, null, 2) + '\n');
  const manifest = await retained(manifestBytes); await writeFile(join(snapshot, 'manifest.json'), manifestBytes, { mode: 0o444 });
  baseline = JSON.parse(await readFile(join(snapshot, 'inputs/rust/parity/baseline.json'), 'utf8'));
  for (const row of baseline.files) {
    const bytes = await readFile(join(artifacts, 'reference', row.path)); assert.equal(bytes.length, row.bytes); assert.equal(hash(bytes), row.sha256);
    const target = join(snapshot, 'reference', row.path); await mkdir(join(target, '..'), { recursive: true }); await writeFile(target, bytes, { mode: 0o444 });
  }
  await copyFile(process.execPath, join(snapshot, 'node-reference')); await chmod(join(snapshot, 'node-reference'), 0o500);
  nodeIdentity = { path: process.execPath, snapshot: join(snapshot, 'node-reference').slice(root.length + 1), sha256: hash(await readFile(process.execPath)), builtins: Object.fromEntries(builtinNames.map(name => [name, hash(process.binding('natives')[name])])) };
  assert.equal(hash(await readFile(join(snapshot, 'node-reference'))), nodeIdentity.sha256);
  for (const name of builtinNames) await retained(Buffer.from(process.binding('natives')[name]), 'js');
  const descriptor = { kind: 'pre_execution_differential_snapshot', native: nativeIdentity, sources, manifest, node: nodeIdentity, baseline: { commit: baseline.baseline_commit, files: baseline.files }, captured_before_execution: true };
  await writeFile(join(snapshot, 'snapshot.json'), JSON.stringify(descriptor, null, 2) + '\n', { mode: 0o444 });
  const tlsEnvironment = selected.some(spec => spec.protocol !== 'HTTP') ? await tlsFixtures() : {};
  for (const spec of selected) {
    if (interrupted) break;
    const reference = await childCase(spec, tlsEnvironment, 'reference');
    const native = interrupted ? undefined : await childCase(spec, tlsEnvironment, 'native');
    const row = { id: spec.id, reference, native, comparison: compare(spec, reference, native) };
    row.passed = row.comparison.matches; results.push(row);
    console.log(JSON.stringify({ id: spec.id, passed: row.passed, reference: reference.report?.path, native: native?.report?.path, comparison: row.comparison }));
  }
} catch (error) { failures.push({ phase: 'setup_or_execution', ...errorView(error) }); }
finally {
  for (const child of children) child.kill('SIGKILL');
  if (children.size) {
    try { await bounded(Promise.all([...children].map(child => child.reaped)), 2000, 'Final owned child reap'); }
    catch (error) { failures.push({ phase: 'child_cleanup', ...errorView(error) }); }
    children.clear();
  }
  if (snapshot && nativeIdentity) {
    try { assert.equal(hash(await readFile(join(root, nativeIdentity.snapshot))), nativeIdentity.sha256); }
    catch (error) { failures.push({ phase: 'native_snapshot_verification', ...errorView(error) }); }
  }
  if (snapshot && nodeIdentity) {
    try { assert.equal(hash(await readFile(join(root, nodeIdentity.snapshot))), nodeIdentity.sha256); }
    catch (error) { failures.push({ phase: 'node_snapshot_verification', ...errorView(error) }); }
  }
  for (const source of sources) {
    try { assert.equal(hash(await readFile(join(root, source.source))), source.sha256); assert.equal(hash(await readFile(join(root, source.snapshot))), source.sha256); }
    catch (error) { failures.push({ phase: 'source_verification', source: source.source, ...errorView(error) }); }
  }
  if (snapshot && baseline) for (const row of baseline.files) {
    try { assert.equal(hash(await readFile(join(snapshot, 'reference', row.path))), row.sha256); }
    catch (error) { failures.push({ phase: 'baseline_verification', source: row.path, ...errorView(error) }); }
  }
  if (scratch) try { await rm(scratch, { recursive: true, force: true }); } catch (error) { failures.push({ phase: 'scratch_cleanup', ...errorView(error) }); }
  process.off('SIGINT', interrupt); process.off('SIGTERM', terminate);
}
const report = { schema_version: 1, kind: 'buffered_response_differential', candidate_executed: true, native: nativeIdentity, comparison_contract: comparisonContract, native_equivalence: 'selected finite observations only; seven pending families and full allocation qualification remain open', declaration, selected: selected.map(spec => spec.id), snapshot: snapshot?.slice(root.length + 1), sources, node: nodeIdentity, certificates: certificateIdentity, interrupted, failures, completed: results.length === selected.length, passed: !interrupted && !failures.length && results.length === selected.length && results.every(row => row.passed), results };
const bytes = Buffer.from(JSON.stringify(report, null, 2) + '\n'), reportPath = join(evidence, `buffered-response-differential-${hash(bytes)}.json`);
await mkdir(evidence, { recursive: true }); await writeFile(reportPath, bytes, { flag: 'wx', mode: 0o600 }).catch(error => { if (error.code !== 'EEXIST') throw error; }); assert.equal(hash(await readFile(reportPath)), hash(bytes));
console.log(JSON.stringify({ report: reportPath.slice(root.length + 1), passed: report.passed, completed: report.completed, passed_cases: results.filter(row => row.passed).length, selected_cases: selected.length, declared_cases: declaration.cases.length, pending_families: declaration.pending_families.length }));
if (!report.passed) process.exitCode = interrupted === 'SIGINT' ? 130 : interrupted === 'SIGTERM' ? 143 : 1;
