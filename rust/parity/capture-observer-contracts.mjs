// Execute the unchanged frozen assertions, retaining every observe() schedule.
// Native contract tests feed these chunks through the actual ObservedBody.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/observer-contracts-${Date.now()}`));
assert.ok(process.argv.length <= 4, 'Usage: capture-observer-contracts.mjs [frozen-reference] [new-output-directory]');
await verifyBaseline(reference);
const sourcePath = 'test/response-observer.test.mjs';
const source = readFileSync(join(reference, sourcePath), 'utf8');
const manifest = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json'), 'utf8'));
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
assert.equal(hash(source), manifest.files.find(row => row.path === sourcePath)?.sha256);
const { createResponseObserver } = await import(pathToFileURL(join(reference, 'src/response-observer.mjs')).href);
const imports = [
  "import test from 'node:test';",
  "import assert from 'node:assert/strict';",
  "import { once } from 'node:events';",
  "import { createResponseObserver } from '../src/response-observer.mjs';",
];
let body = source;
for (const line of imports) {
  assert.equal(body.split(line).length, 2, 'Frozen import topology changed');
  body = body.replace(line, '');
}
assert.doesNotMatch(body, /^\s*import\b/m, 'Unexpected executable import');
const returned = 'return { models, errors, usages, executions, completions, received };';
assert.equal(body.split(returned).length, 2, 'Frozen helper topology changed');
// Keep every baseline assertion, write/end call, callback and parameter loop.
// The only helper insertion observes its completed result before returning it.
body = body.replace(returned, `capture(chunks, options, { models, errors, usages, executions, completions, received });\n  ${returned}`);
const tests = [], cases = [], migrationBoundaries = [];
let current;
function test(name, callback) {
  assert.equal(typeof callback, 'function');
  tests.push({ id: `${sourcePath}#${tests.length + 1}`, name, callback, case_ids: [], skipped_calls: [] });
}
function capture(chunks, options, result) {
  const input = { chunks: chunks.map(chunk => [...chunk]) };
  if (options.contentType !== undefined) input.content_type = options.contentType;
  if (options.maxBufferBytes !== undefined) input.max_buffer_bytes = options.maxBufferBytes;
  const panicCallbacks = [];
  const asynchronous = [];
  for (const [option, kind] of [['onModel', 'models'], ['onError', 'errors'], ['onUsage', 'usages'], ['onExecution', 'executions'], ['onComplete', 'completions']]) {
    if (options[option] === undefined) continue;
    assert.equal(typeof options[option], 'function');
    if (options[option].constructor.name === 'AsyncFunction') asynchronous.push(option);
    else panicCallbacks.push(kind);
  }
  const call = current.case_ids.length + current.skipped_calls.length + 1;
  if (asynchronous.length) {
    const boundary = { baseline_test: current.id, call, callbacks: asynchronous, reason: 'Rust observation callbacks are synchronous; a rejected JavaScript Promise is an explicit API migration boundary.' };
    current.skipped_calls.push(boundary);
    migrationBoundaries.push(boundary);
    return;
  }
  if (panicCallbacks.length) input.panic_callbacks = panicCallbacks;
  const id = `baseline-observer-${tests.indexOf(current) + 1}-${call}`;
  const { received, ...events } = result;
  const expected = structuredClone({ ...events, forwarded: [...Buffer.concat(received)] });
  cases.push({ id, input, node_expected: expected, source_tests: [current.id] });
  current.case_ids.push(id);
}
Function('test', 'assert', 'once', 'createResponseObserver', 'capture', `"use strict";\n${body}`)(test, assert, once, createResponseObserver, capture);
assert.equal(tests.length, 32, 'Frozen definition inventory changed');
for (const definition of tests) {
  current = definition;
  await definition.callback();
}
const bytes = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
const report = {
  schema_version: 1, kind: 'frozen_response_observer_contract_capture', passed: true,
  baseline_commit: manifest.baseline_commit, source_path: sourcePath, source_sha256: hash(source),
  generator_sha256: hash(readFileSync(new URL(import.meta.url))), node_version: process.version,
  cases: cases.length, cases_sha256: hash(bytes),
  tests: tests.map(({ callback, ...definition }) => definition), migration_boundaries: migrationBoundaries,
  limits: 'All32 baseline definitions and their assertions execute. Helper schedules capture real Node stream output. Direct stream timing/destruction definitions1/13/20 have separate native tests. Async callback API cases are skipped explicitly. Capture alone is not native parity evidence.',
};
mkdirSync(output, { mode: 0o700 });
writeFileSync(join(output, 'cases.jsonl'), bytes, { flag: 'wx', mode: 0o600 });
writeFileSync(join(output, 'capture.json'), JSON.stringify(report, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify({ passed: true, definitions: tests.length, cases: cases.length, migration_boundaries: migrationBoundaries.length, output }));
