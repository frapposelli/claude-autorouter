#!/usr/bin/env node
// Opt-in, bounded execution of two complete frozen redaction timing callbacks.
// Output contains lengths, digests and timing only; never prompt/response text.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import * as os from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const file = 'test/redaction.test.mjs';
const runtimeFile = 'src/redaction.mjs';
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const limits = Object.freeze({ child_ms: 90_000, heap_mib: 512, stdio_bytes: 65_536,
  input_bytes: 512 * 1024, inputs_total_bytes: 16 * 1024 * 1024,
  output_bytes: 2 * 1024 * 1024, outputs_total_bytes: 16 * 1024 * 1024 });
const expected = Object.freeze([
  { number: 5, name: 'redaction is idempotent and stays linear on adversarial input', count: 8, ticks: 2,
    definition_sha256: '0d9fec8f69d65dc344ee7c64b015bd1eafb8295aa5250189ab79be1ea97f945a',
    definition_statement_sha256: '6dd638f4c8ed240fe68c693a10032e8914fa6e1c0387a2028908e7b246a08edf',
    timing_assertion_sha256: '71d53105fb601cbd3c32f49b8b572eb3b0bcd3b268c98bc64b7f9d96aaad91d9' },
  { number: 16, name: 'the widened rules are idempotent and stay linear on adversarial input', count: 20, ticks: 3,
    definition_sha256: '8dc19f60fc16fb9053096d247311fe8444b4f895448d6ed266bae6886baf8f3a',
    definition_statement_sha256: '9daf176d2ba0dfaf8ef9bf7407ce3bf9d21986b4a2fa3fa8d6f5e0165dd2c81c',
    timing_assertion_sha256: '3ec2c9c2a7b5ebaba45d9bac4f6d9b1f19bbacb70a26491ccff47a0ffba3842f' },
]);
const known = Object.freeze({ baseline_commit: 'ea930c247626ce2af5ccdad721b5121417bf4ad8',
  baseline_file_sha256: '2c39564dea427488ffa04caeb78281a77c753d6a9deb61c067e8d1d505dab3a6',
  redaction_source_sha256: '880c883c68cae3bde03d93e7231b1d2a6d5f7b907c2f2555859894e5475fdff1' });
const ts = createRequire(join(root, 'package.json'))('typescript');

function callsNamed(node, name) {
  const found = [];
  const visit = child => {
    if (ts.isCallExpression(child) && child.expression.getText(node.getSourceFile()) === name) found.push(child);
    ts.forEachChild(child, visit);
  };
  visit(node);
  return found;
}
async function prepare(reference) {
  const baseline = await verifyBaseline(reference);
  assert.equal(baseline.baseline_commit, known.baseline_commit);
  const manifest = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json'), 'utf8'));
  const source = readFileSync(join(reference, file), 'utf8');
  const runtime = readFileSync(join(reference, runtimeFile));
  assert.equal(hash(source), known.baseline_file_sha256);
  assert.equal(hash(runtime), known.redaction_source_sha256);
  for (const [path, digest] of [[file, hash(source)], [runtimeFile, hash(runtime)]]) {
    assert.equal(manifest.files.find(row => row.path === path)?.sha256, digest);
  }
  const syntax = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
  assert.equal(syntax.parseDiagnostics.length, 0);
  const tests = syntax.statements.filter(statement => ts.isExpressionStatement(statement)
    && ts.isCallExpression(statement.expression) && statement.expression.expression.getText(syntax) === 'test');
  assert.equal(tests.length, 17);
  const definitions = expected.map(spec => {
    const statement = tests[spec.number - 1], call = statement.expression;
    assert.equal(call.arguments.length, 2);
    assert.equal(call.arguments[0].text, spec.name);
    assert.equal(hash(statement.getText(syntax)), spec.definition_statement_sha256);
    assert.equal(hash(call.getText(syntax)), spec.definition_sha256);
    const callback = call.arguments[1];
    assert.ok(ts.isArrowFunction(callback) && ts.isBlock(callback.body));
    const [equal] = callsNamed(callback, 'assert.equal');
    const [ok] = callsNamed(callback, 'assert.ok');
    assert.equal(callsNamed(callback, 'assert.equal').length, 1);
    assert.equal(callsNamed(callback, 'assert.ok').length, 1);
    assert.equal(callsNamed(callback, 'redactSensitive').length, 3);
    assert.equal(callsNamed(callback, 'performance.now').length, spec.ticks);
    assert.ok(equal && ok);
    const predicate = ok.arguments[0];
    assert.ok(ts.isBinaryExpression(predicate)
      && predicate.operatorToken.kind === ts.SyntaxKind.LessThanToken
      && ts.isNumericLiteral(predicate.right) && predicate.right.text === '2000');
    assert.ok(ts.isBinaryExpression(predicate.left)
      && predicate.left.operatorToken.kind === ts.SyntaxKind.MinusToken
      && predicate.left.left.getText(syntax) === 'performance.now()'
      && predicate.left.right.getText(syntax) === 'started');
    const loop = callback.body.statements.at(-1);
    assert.ok(ts.isForOfStatement(loop) && loop.expression.getText(syntax) === 'adversarial');
    assert.equal(loop.statement.getText(syntax).includes('redactSensitive(input)'), true);
    assert.equal(hash(ok.getText(syntax)), spec.timing_assertion_sha256);
    return { ...spec, id: `${file}#${spec.number}`, callback: callback.getText(syntax) };
  });
  return { baseline, definitions, typescript_version: ts.version };
}
function publicDefinition(row) {
  const { id, name, definition_sha256, definition_statement_sha256, timing_assertion_sha256, count, ticks } = row;
  return { id, name, definition_sha256, definition_statement_sha256, timing_assertion_sha256,
    adversarial_cases: count, now_calls_per_case: ticks };
}
function declaration(prepared) {
  return { schema_version: 1, kind: 'frozen_redaction_adversarial_declaration',
    baseline_commit: known.baseline_commit, baseline_file_sha256: known.baseline_file_sha256,
    sources: [{ path: file, sha256: known.baseline_file_sha256 },
      { path: runtimeFile, sha256: known.redaction_source_sha256 }],
    definitions: prepared.definitions.map(publicDefinition), adversarial_cases: 28,
    original_predicate: 'performance.now() - started < 2000', original_limit_ms: 2000,
    limits, node_version: process.version, typescript_version: prepared.typescript_version,
    generator_sha256: hash(readFileSync(import.meta.filename)),
    execution: 'One child, original complete callbacks #5 then #16, no extra warmup, real assertion verdicts.' };
}
function conditions() {
  return { wall_time: new Date().toISOString(), node: process.version,
    platform: process.platform, arch: process.arch, os_release: os.release(),
    logical_cpus: os.availableParallelism(), total_memory_bytes: os.totalmem(),
    free_memory_bytes: os.freemem(), load_average: os.loadavg(),
    process_memory: process.memoryUsage(), process_resources: process.resourceUsage() };
}
async function child(reference, output) {
  const prepared = await prepare(reference);
  assert.deepEqual(JSON.parse(readFileSync(join(output, 'declaration.json'), 'utf8')), declaration(prepared));
  const { redactSensitive: original } = await import(pathToFileURL(join(reference, runtimeFile)).href);
  assert.equal(typeof original, 'function');
  const realNow = globalThis.performance.now.bind(globalThis.performance);
  const before = conditions(), entries = [];
  let current;
  const wrapped = input => {
    assert.ok(current);
    const start = realNow();
    const result = original(input);
    const end = realNow();
    current.calls.push({ input, result, start, end, order: ++current.order });
    return result;
  };
  const performance = { now() {
    const value = realNow();
    current.now.push({ value, order: ++current.order });
    return value;
  } };
  const observedAssert = {
    equal(...args) { current.equals++; return Reflect.apply(assert.equal, assert, args); },
    ok(...args) {
      const row = { order: ++current.order, passed: false };
      current.assertions.push(row);
      try {
        Reflect.apply(assert.ok, assert, args);
        row.passed = true;
      } catch (error) {
        const index = current.assertions.length - 1;
        const ticks = current.now.slice(index * current.definition.ticks);
        process.stdout.write(JSON.stringify({ status: 'failed', kind: 'original_assertion',
          id: `redaction-adversarial-${current.definition.number}-${index + 1}`,
          original_assertion_passed: false,
          original_now_ticks_ms: ticks.map(tick => tick.value),
          original_elapsed_ms: ticks.length >= 2 ? ticks[1].value - ticks[0].value : null }));
        throw error;
      }
    },
  };
  for (const definition of prepared.definitions) {
    current = { definition, calls: [], now: [], assertions: [], equals: 0, order: 0 };
    const callback = Function('redactSensitive', 'assert', 'performance',
      `"use strict";return (${definition.callback});`)(wrapped, observedAssert, performance);
    assert.equal(callback(), undefined);
    assert.equal(current.calls.length, definition.count + 2);
    assert.equal(current.equals, 1);
    assert.equal(current.assertions.length, definition.count);
    assert.ok(current.assertions.every(row => row.passed));
    assert.equal(current.now.length, definition.count * definition.ticks);
    assert.ok(current.calls[1].order < current.now[0].order, 'Two prefix calls precede timing');
    entries.push(current);
  }
  // No hashing, UTF-8 conversion or serialization ran inside a source timer.
  const cases = [], timings = [];
  let inputTotal = 0, outputTotal = 0;
  for (const entry of entries) {
    const { definition } = entry;
    for (let index = 0; index < definition.count; index++) {
      const call = entry.calls[index + 2];
      assert.equal(typeof call.input, 'string');
      assert.equal(typeof call.result, 'string');
      const inputBytes = Buffer.byteLength(call.input), outputBytes = Buffer.byteLength(call.result);
      inputTotal += inputBytes; outputTotal += outputBytes;
      assert.ok(inputBytes <= limits.input_bytes && inputTotal <= limits.inputs_total_bytes);
      assert.ok(outputBytes <= limits.output_bytes && outputTotal <= limits.outputs_total_bytes);
      const id = `redaction-adversarial-${definition.number}-${index + 1}`;
      cases.push({ id, source_test: definition.id, input_bytes: inputBytes,
        input_sha256: hash(Buffer.from(call.input)), output_bytes: outputBytes,
        output_sha256: hash(Buffer.from(call.result)) });
      const ticks = entry.now.slice(index * definition.ticks, (index + 1) * definition.ticks);
      assert.ok(ticks[0].order < call.order && call.order < ticks[1].order);
      assert.ok(call.start >= ticks[0].value && call.end <= ticks[1].value);
      assert.ok(entry.assertions[index].order > ticks.at(-1).order);
      const originalElapsed = ticks[1].value - ticks[0].value;
      assert.ok(Number.isFinite(originalElapsed) && originalElapsed >= 0 && originalElapsed < 2000);
      timings.push({ id, source_test: definition.id, original_now_ticks_ms: ticks.map(tick => tick.value),
        original_elapsed_ms: originalElapsed, method_started_ms: call.start, method_ended_ms: call.end,
        method_elapsed_ms: call.end - call.start, original_assertion_passed: entry.assertions[index].passed });
    }
  }
  assert.equal(cases.length, 28);
  assert.equal(timings.length, 28);
  assert.deepEqual(await verifyBaseline(reference), prepared.baseline);
  const identities = { schema_version: 1, kind: 'frozen_redaction_adversarial_identities',
    baseline_commit: known.baseline_commit, baseline_file_sha256: known.baseline_file_sha256,
    sources: [{ path: file, sha256: known.baseline_file_sha256 },
      { path: runtimeFile, sha256: known.redaction_source_sha256 }],
    definitions: prepared.definitions.map(publicDefinition), cases };
  writeFileSync(join(output, 'identities.json'), JSON.stringify(identities, null, 2) + '\n',
    { flag: 'wx', mode: 0o600 });
  const timing = { status: 'passed', kind: 'frozen_redaction_adversarial_timing',
    before, after: conditions(), node_heap_limit_mib: limits.heap_mib,
    original_assertions: 28, prefix_equal_assertions: 2, prefix_redaction_calls: 4,
    input_total_bytes: inputTotal, output_total_bytes: outputTotal,
    timing_scope: 'Original <2000ms predicates use real performance.now and include wrapper overhead; method elapsed brackets the actual redactor call. Normal-use synthetic evidence, not a whole-gateway or hardware qualification.',
    cases: timings };
  const bytes = Buffer.from(JSON.stringify(timing));
  assert.ok(bytes.length <= limits.stdio_bytes);
  process.stdout.write(bytes);
}
async function parent(reference, output) {
  mkdirSync(output, { recursive: false, mode: 0o700 });
  let phase = 'preflight';
  let childStatus = null, childReport = null;
  try {
    const prepared = await prepare(reference);
    const declared = declaration(prepared);
    writeFileSync(join(output, 'declaration.json'), JSON.stringify(declared, null, 2) + '\n',
      { flag: 'wx', mode: 0o600 });
    phase = 'child';
    const result = spawnSync(process.execPath,
      [`--max-old-space-size=${limits.heap_mib}`, import.meta.filename, '--child', reference, output],
      { cwd: root, env: { LANG: 'C', LC_ALL: 'C' }, encoding: 'utf8',
        timeout: limits.child_ms, killSignal: 'SIGKILL', maxBuffer: limits.stdio_bytes });
    childStatus = { exit_code: result.status, signal: result.signal,
      error_code: result.error?.code ?? null, stdout_bytes: Buffer.byteLength(result.stdout ?? ''),
      stderr_bytes: Buffer.byteLength(result.stderr ?? ''),
      stdout_sha256: hash(result.stdout ?? ''), stderr_sha256: hash(result.stderr ?? '') };
    try { childReport = JSON.parse(result.stdout); } catch { /* A killed child may have no report. */ }
    assert.equal(result.status, 0, 'Child did not complete successfully');
    assert.equal(result.signal, null);
    assert.equal(result.error, undefined);
    const timing = childReport;
    assert.equal(timing.status, 'passed');
    assert.equal(timing.cases?.length, 28);
    const identitiesBytes = readFileSync(join(output, 'identities.json'));
    assert.ok(identitiesBytes.length <= limits.stdio_bytes);
    const identities = JSON.parse(identitiesBytes);
    assert.equal(identities.kind, 'frozen_redaction_adversarial_identities');
    assert.equal(identities.cases?.length, 28);
    assert.deepEqual(identities.cases.map(row => row.id), timing.cases.map(row => row.id));
    assert.deepEqual(await verifyBaseline(reference), prepared.baseline);
    assert.deepEqual(JSON.parse(readFileSync(join(output, 'declaration.json'), 'utf8')), declared);
    assert.equal(hash(readFileSync(import.meta.filename)), declared.generator_sha256);
    const capture = { schema_version: 1, kind: 'frozen_redaction_adversarial_capture',
      status: 'passed', baseline_commit: known.baseline_commit,
      declaration_sha256: hash(readFileSync(join(output, 'declaration.json'))),
      identities_sha256: hash(identitiesBytes), child: childStatus, timing };
    writeFileSync(join(output, 'capture.json'), JSON.stringify(capture, null, 2) + '\n',
      { flag: 'wx', mode: 0o600 });
    process.stdout.write(JSON.stringify({ output, status: 'passed', identities_sha256: capture.identities_sha256 }) + '\n');
  } catch {
    writeFileSync(join(output, 'capture.json'), JSON.stringify({ schema_version: 1,
      kind: 'frozen_redaction_adversarial_capture', status: 'failed', phase,
      child: childStatus, child_report: childReport?.status === 'failed' ? childReport : null,
      incomplete_is_not_evidence: true }, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
    process.stderr.write('Redaction adversarial capture failed; see capture.json.\n');
    process.exitCode = 1;
  }
}

if (process.argv[2] === '--child') {
  try { await child(resolve(process.argv[3]), resolve(process.argv[4])); }
  catch { process.stderr.write('Redaction adversarial child failed.\n'); process.exitCode = 1; }
} else {
  assert.ok(process.argv.length <= 4,
    'Usage: capture-redaction-adversarial-bounds.mjs [reference] [NEW-output-directory]');
  const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
  const output = resolve(process.argv[3] ?? join(root,
    `artifacts/rust-rewrite/redaction-adversarial-${Date.now()}-${process.pid}`));
  await parent(reference, output);
}
