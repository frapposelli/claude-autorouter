#!/usr/bin/env node
// Run from the repository root. Frozen pure functions only; no gateway/providers.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { join, relative, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const root = process.cwd();
assert.equal(JSON.parse(await readFile(join(root, 'package.json'), 'utf8')).name, 'claude-autorouter');
if (process.argv.length !== 4) {
  process.stderr.write('Usage: node capture-turn-fingerprint.mjs REFERENCE NEW_OUTPUT_DIRECTORY\n');
  process.exit(2);
}
const reference = resolve(process.argv[2]);
const output = resolve(process.argv[3]);
const { verifyBaseline } = await import(pathToFileURL(join(root, 'scripts/rust-reference.mjs')).href);
const verified = await verifyBaseline(reference);
assert.equal(verified.baseline_commit, 'ea930c247626ce2af5ccdad721b5121417bf4ad8');
const ts = createRequire(join(root, 'package.json'))('typescript');
const digest = data => createHash('sha256').update(data).digest('hex');
const source = await readFile(join(reference, 'src/router.mjs'), 'utf8');
const baseline = JSON.parse(await readFile(join(root, 'rust/parity/baseline.json'), 'utf8'));
assert.equal(digest(source), baseline.files.find(row => row.path === 'src/router.mjs').sha256);
const parsed = ts.createSourceFile('src/router.mjs', source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
assert.equal(parsed.parseDiagnostics.length, 0);
const expectedFunctions = { withoutCacheControl: '4b83f2b5f3098fc0560f55ebb27a7fcbcd114a35ba9efda3fc6553a8c53ce3f1', turnContent: '147698916bd23dda1ddfc3a7e45c826d60b840e58b8e39c19f4072485e68f6a9', turnInfo: 'e412a060388137f8338683a9d63ff8ed62e51d06d2265c210dd194ac38cc18b5' };
const names = Object.keys(expectedFunctions);
const functions = names.map(name => {
  const matches = parsed.statements.filter(row => ts.isFunctionDeclaration(row) && row.name?.text === name);
  assert.equal(matches.length, 1);
  const body = matches[0].getText(parsed);
  assert.equal(digest(body), expectedFunctions[name]);
  return { name, sha256: digest(body), text: body };
});
const { goalFeedbackIndexes } = await import(pathToFileURL(join(reference, 'src/prompt-state.mjs')).href);
let calls;
const hash = value => {
  const json = JSON.stringify(value);
  calls.push(json);
  return digest(json);
};
const turnInfo = Function('hash', 'goalFeedbackIndexes',
  `"use strict";${functions.map(row => row.text).join('\n')}\nreturn turnInfo;`)(hash, goalFeedbackIndexes);
const user = content => ({ role: 'user', content });
const assistant = content => ({ role: 'assistant', content });
const history = { system: [{ type: 'text', text: 'Synthetic system', cache_control: { type: 'ephemeral' } }],
  tools: [{ name: 'Synthetic', input_schema: { type: 'object', properties: { '9': {}, '2': {} } }, cache_control: null }],
  messages: [user('First synthetic task'), assistant([{ type: 'tool_use', id: 'tool-a', name: 'Synthetic',
    input: { cache_control: 'opaque input remains significant', '8': 1, '1': 2 } }]),
  user([{ type: 'tool_result', tool_use_id: 'tool-a', cache_control: { type: 'ephemeral' },
    content: [{ type: 'tool_result', content: [{ type: 'text', text: 'Synthetic result', cache_control: null }], cache_control: null }] }]),
  assistant('Synthetic response'), user('Second synthetic task'), { role: 'system', content: 'Turn-scoped note' }] };
const goal = { messages: [user('<command-name>/goal</command-name>\n<command-message>goal</command-message>\n<command-args>Synthetic goal</command-args>'),
  assistant('Synthetic progress'), user('Stop hook feedback:\n[Synthetic goal]: Continue the synthetic work.'),
  assistant('Synthetic progress two'), user('A new human task')] };
let nested = { type: 'text', text: 'Synthetic deeply nested result', cache_control: { type: 'ephemeral' } };
for (let i = 0; i < 32; i++) nested = { type: 'tool_result', content: [nested], cache_control: null };
const definitions = [
  { name: 'missing-system-tools-and-messages', raw: '{}' },
  { name: 'empty-arrays', body: { system: [], tools: [], messages: [] } },
  { name: 'scalar-system-escaping', body: { system: 'Synthetic é二😀\n\u0000"\\', messages: [user('A task')] } },
  { name: 'integer-duplicates-surrogates-numbers', raw: String.raw`{"system":[{"9":"last","2":"first","2":"replacement","text":"\ud800\udc00\ud800\udc00\ud800","cache_control":null}],"tools":[{"8":"eight","1":"one","name":"Synthetic","input_schema":{"9":-0,"2":1e400,"small":1e-400,"wide":9007199254740993,"cache_control":true},"cache_control":{"type":"ephemeral"}}],"messages":[{"9":true,"2":false,"role":"user","content":[{"9":1e400,"2":-0,"type":"text","text":"Task","cache_control":null}]}]}` },
  { name: 'array-and-primitive-system-blocks', body: { system: [[1, { cache_control: 'opaque' }], true, null, 'text', 0], messages: [user('Task')] } },
  { name: 'cache-removal-with-opaque-tool-values', body: { tools: [null, false, 'scalar', [1, { cache_control: true }], { cache_control: null, input_schema: { cache_control: 'retain' } }], messages: [user('Task')] } },
  { name: 'message-with-missing-content', body: { messages: [{ role: 'user', opaque: { '2': 2, '1': 1 } }] } },
  { name: 'tool-result-nonarray-content', body: { system: [{ type: 'tool_result', cache_control: null, content: { cache_control: 'retain', text: 'opaque' } }], messages: [user('Task')] } },
  { name: 'history-full', body: history },
  ...[0, 1, 2, 3, 4, 5, 6, 99].map(end => ({ name: `history-prefix-${end}`, body: history, end })),
  ...[1, 3, 5].map(end => ({ name: `goal-prefix-${end}`, body: goal, end })),
  { name: 'deep-nested-tool-results', body: { system: [nested], messages: [user('Task')] } },
  { name: 'signed-and-unknown-fields', body: { messages: [assistant([{ type: 'thinking', thinking: 'Synthetic', signature: 'synthetic-signature', cache_control: null }, { type: 'future', opaque: { cache_control: true } }]), user('Task')] } },
];
const cases = [];
for (const [number, definition] of definitions.entries()) {
  const raw = definition.raw ?? JSON.stringify(definition.body);
  const original = JSON.parse(raw);
  const body = definition.end === undefined ? original : { ...original, messages: original.messages.slice(0, definition.end) };
  const scope = number % 2 ? 'synthetic/scope:é二😀' : 'synthetic/scope';
  const promptId = number % 3 ? `synthetic-prompt-${number}` : '';
  calls = [];
  const expected = turnInfo(body, scope, promptId);
  assert.equal(calls.length, promptId ? 2 : 1);
  assert.equal(digest(calls[0]), expected.contentKey);
  assert.equal(digest(calls.at(-1)), expected.key);
  cases.push({ id: `turn-fingerprint-${number + 1}`, name: definition.name, raw_body: raw,
    scope, prompt_id: promptId, prefix_end: definition.end ?? null, expected,
    content_preimage: calls[0], prompt_preimage: promptId ? calls[1] : null });
}
// Tuple boundaries must not admit concatenation aliases.
for (const [scope, promptId] of [['ab', 'c'], ['a', 'bc']]) {
  calls = [];
  const expected = turnInfo(history, scope, promptId);
  cases.push({ id: `turn-fingerprint-${cases.length + 1}`, name: 'scope-prompt-tuple-boundary',
    raw_body: JSON.stringify(history), scope, prompt_id: promptId, prefix_end: null, expected,
    content_preimage: calls[0], prompt_preimage: calls[1] });
}
assert.equal(cases.length, 24);
assert.notEqual(cases.at(-1).expected.key, cases.at(-2).expected.key);
const cacheComposition = {
  system: [{ type: 'text', text: 'Synthetic cache composition', cache_control: { type: 'ephemeral' } }],
  tools: [{ name: 'SyntheticBulk', description: `Synthetic ${'x'.repeat(3600)}`,
    cache_control: { type: 'ephemeral' }, input_schema: { type: 'object',
      properties: { literal: { const: { cache_control: 'retain opaque schema value' } } } } }],
  messages: [user('Synthetic first task'), assistant([{ type: 'tool_use', id: 'synthetic-tool', name: 'SyntheticBulk',
    input: { cache_control: 'retain opaque tool input' } }]),
  user([{ type: 'tool_result', tool_use_id: 'synthetic-tool', cache_control: { type: 'ephemeral' },
    content: [{ type: 'tool_result', cache_control: null, content: [{ type: 'text', text: 'Synthetic result', cache_control: null }] }] }]),
  user('Synthetic next task')],
};
for (const { name, body, scope, promptId } of [
  { name: 'cache-composition-beyond-digest-batch', body: cacheComposition,
    scope: 'synthetic/bulk', promptId: 'synthetic-bulk-prompt' },
  { name: 'escaped-scope-and-prompt', body: { messages: [user('Synthetic task')] },
    scope: 'synthetic/"scope\\\n\u0000é二😀', promptId: 'synthetic/"prompt\\\t\u001fé二😀' },
]) {
  const raw = JSON.stringify(body);
  calls = [];
  const expected = turnInfo(JSON.parse(raw), scope, promptId);
  assert.equal(calls.length, 2);
  assert.equal(digest(calls[0]), expected.contentKey);
  assert.equal(digest(calls[1]), expected.key);
  if (name === 'cache-composition-beyond-digest-batch') {
    const bytes = Buffer.byteLength(calls[0]);
    assert.ok(bytes > 4096 && bytes <= 4500, 'Declared normalized preimage batch-boundary size');
    assert.ok(!calls[0].includes('ephemeral'), 'API cache markers are removed');
    assert.ok(calls[0].includes('retain opaque schema value') && calls[0].includes('retain opaque tool input'));
  }
  cases.push({ id: `turn-fingerprint-${cases.length + 1}`, name, raw_body: raw,
    scope, prompt_id: promptId, prefix_end: null, expected,
    content_preimage: calls[0], prompt_preimage: calls[1] });
}
assert.equal(cases.length, 26);
const corpus = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
assert.ok(Buffer.byteLength(corpus) <= 64 * 1024, 'Declared synthetic corpus byte ceiling');
assert.deepEqual(await verifyBaseline(reference), verified);
await mkdir(output, { mode: 0o700 });
await writeFile(join(output, 'cases.jsonl'), corpus, { flag: 'wx', mode: 0o600 });
const report = { schema_version: 1, kind: 'frozen_turn_fingerprint_preimages',
  baseline_commit: verified.baseline_commit, verified_files: verified.files_verified,
  sources: ['src/router.mjs', 'src/prompt-state.mjs'].map(path => ({ path,
    sha256: baseline.files.find(row => row.path === path).sha256 })),
  extracted_functions: functions.map(({ name, sha256 }) => ({ name, sha256 })),
  adapter: { path: relative(root, import.meta.filename).split('\\').join('/'), sha256: digest(await readFile(import.meta.filename)) },
  node: process.version, typescript: ts.version, cases: cases.length,
  corpus_bytes: Buffer.byteLength(corpus), corpus_sha256: digest(corpus),
  scope: 'Exact original pure functions and JSON.stringify/hash observations. Some synthetic pure-function values are outside request validation; no claim that the gateway accepts them. No timing or provider calls.' };
await writeFile(join(output, 'capture.json'), JSON.stringify(report, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
process.stdout.write(JSON.stringify(report) + '\n');
