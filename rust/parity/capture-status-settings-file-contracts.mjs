// Development-only execution of frozen status-settings actual-file callbacks #4/#6.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import * as fs from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

assert.equal(process.argv.length, 4, 'Pass frozen reference and a new output directory');
const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2]), output = resolve(process.argv[3]);
const hash = value => createHash('sha256').update(value).digest('hex');
const baseline = JSON.parse(fs.readFileSync(join(root, 'rust/parity/baseline.json')));
assert.equal((await verifyBaseline(reference)).files_verified, 111);
const ts = createRequire(join(root, 'package.json'))('typescript');
const sourcePath = 'test/status-settings.test.mjs', modulePath = 'src/status-settings.mjs';
const source = fs.readFileSync(join(reference, sourcePath), 'utf8');
const moduleSource = fs.readFileSync(join(reference, modulePath), 'utf8');
for (const [path, text] of [[sourcePath, source], [modulePath, moduleSource]]) {
  assert.equal(hash(text), baseline.files.find(row => row.path === path)?.sha256);
}
const { addStatusLineSettings } = await import(pathToFileURL(join(reference, modulePath)).href);
const syntax = ts.createSourceFile(sourcePath, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
assert.equal(syntax.parseDiagnostics.length, 0);
const find = (node, predicate) => {
  const found = [];
  const walk = value => { if (predicate(value)) found.push(value); ts.forEachChild(value, walk); };
  walk(node); return found;
};
const definitions = [], callbacks = [];
let index = 0, current;
const texts = [];
for (const statement of syntax.statements) {
  if (ts.isImportDeclaration(statement)) continue;
  const calls = find(statement, node => ts.isCallExpression(node) && node.expression.getText(syntax) === 'test');
  if (!calls.length) continue;
  index++;
  if (![4, 6].includes(index)) continue;
  assert.equal(calls.length, 1);
  const call = calls[0], sites = find(call.arguments.at(-1), node => ts.isCallExpression(node)
    && ts.isPropertyAccessExpression(node.expression) && node.expression.expression.getText(syntax) === 'assert');
  const inventoryEnd = syntax.statements[syntax.statements.indexOf(statement) + 1]?.getStart(syntax) ?? source.length;
  const definition = { number: index, id: `${sourcePath}#${index}`, name: call.arguments[0].text,
    line: syntax.getLineAndCharacterOfPosition(call.getStart(syntax)).line + 1,
    definition_sha256: hash(source.slice(statement.getStart(syntax), inventoryEnd)),
    ast_call_sha256: hash(source.slice(call.getStart(syntax), call.end)),
    statement_sha256: hash(statement.getText(syntax)), statement: statement.getText(syntax),
    assertions: sites.map((site, n) => ({ id: `status-settings#${index}:assert-${n + 1}`,
      line: syntax.getLineAndCharacterOfPosition(site.getStart(syntax)).line + 1,
      expression: site.getText(syntax), sha256: hash(site.getText(syntax)), executions: 0 })) };
  definitions.push(definition);
  let transformed = statement.getText(syntax);
  for (const [n, site] of sites.map((site, n) => [n, site]).reverse()) {
    const offset = statement.getStart(syntax);
    transformed = transformed.slice(0, site.getStart(syntax) - offset)
      + `__assertion(${JSON.stringify(definition.assertions[n].id)}, ${site.getText(syntax)})`
      + transformed.slice(site.end - offset);
  }
  texts.push(transformed);
}
assert.deepEqual(definitions.map(row => [row.number, row.assertions.length]), [[4, 2], [6, 2]]);
assert.deepEqual(definitions.map(row => row.definition_sha256), [
  'b49fa2ed767c505380fea6cb0c8a44e4a19bd849af4f5ab922f18eb024bcb1a3',
  '2bad70357b126f354b6340d24cf0f7bee87d8417ee23cc95cc7dc51f7af40041',
]);
assert.deepEqual(definitions.map(row => row.ast_call_sha256), [
  '311b97bab61a51aee5f877404d5c05c5b4503ddd2a31c62159dc055d80b688d0',
  '276ae14946d94503ff3668800ed68281c1de9f9a2d5c1e17fbc77aa87ef5c5fd',
]);
const originalCwd = process.cwd();
// Use the actual cwd spelling on macOS (/var may be a /private/var symlink),
// so original relative() calls retain the source-relative path unchanged.
const scratch = fs.realpathSync(fs.mkdtempSync(join(tmpdir(), 'autorouter-settings-capture-')));
fs.mkdirSync(join(scratch, 'cwd')); fs.mkdirSync(join(scratch, 'temporary'));
const marker = '/__synthetic_settings_root__';
const escape = value => value.replace(/[\\*?\[\]]/g, '\\$&');
function normalize(value) {
  if (Array.isArray(value)) return value.map(normalize);
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, value]) => [key, normalize(value)]));
  if (typeof value !== 'string') return value;
  const mappings = current.directories.flatMap(({ actual, stable }) => [
    [escape(actual), escape(stable)], [actual, stable],
    [escape(basename(actual)), escape(basename(stable))], [basename(actual), basename(stable)],
  ]).concat([[scratch, marker]]).sort((a, b) => b[0].length - a[0].length);
  for (const [from, to] of mappings) value = value.replaceAll(from, to);
  return value;
}
function observedSettings(args, directory) {
  assert.equal(current.calls.length, 0);
  const supplied = args[1], path = resolve(supplied), before = fs.readFileSync(path, 'utf8');
  const result = addStatusLineSettings(args, directory);
  const after = fs.readFileSync(path, 'utf8');
  assert.equal(after, before, 'Supplemental exact source bytes remain unchanged');
  const overlay = JSON.parse(fs.readFileSync(result[1], 'utf8'));
  current.calls.push({ args, source_path: path, source_directory: dirname(path), output_directory: directory,
    input_utf8: before, source_utf8_after: after, output_args: result, generated_permissions: overlay.permissions,
    generated_mode: fs.statSync(result[1]).mode & 0o777,
    generated_status_common: Object.fromEntries(['type', 'padding', 'refreshInterval'].map(key => [key, overlay.statusLine[key]])) });
  return result;
}
const counted = (id, value) => {
  const site = current.definition.assertions.find(row => row.id === id); assert.ok(site);
  site.executions++; current.assertions.push(id); return value;
};
const registered = (name, fn) => { assert.equal(name, definitions[callbacks.length].name); callbacks.push(fn); };
const temporary = prefix => {
  const actual = fs.mkdtempSync(prefix);
  const stable = `${marker}/temporary/${basename(prefix)}FIXTURE`;
  current.directories.push({ actual, stable }); return actual;
};
Function('test', 'assert', 'mkdirSync', 'mkdtempSync', 'writeFileSync', 'readFileSync', 'rmSync',
  'join', 'relative', 'tmpdir', 'addStatusLineSettings', '__assertion',
  `"use strict";\n${texts.join('\n')}`)(registered, assert, fs.mkdirSync, temporary, fs.writeFileSync,
  fs.readFileSync, fs.rmSync, join, (await import('node:path')).relative,
  () => join(scratch, 'temporary'), observedSettings, counted);
const rows = [];
try {
  process.chdir(join(scratch, 'cwd'));
  for (const [n, callback] of callbacks.entries()) {
    const after = [];
    current = { definition: definitions[n], directories: [], calls: [], assertions: [] };
    try {
      await callback({ after: fn => after.push(fn) });
      assert.equal(current.calls.length, 1);
      assert.equal(current.assertions.length, 2);
      rows.push({ source_test: current.definition.id, source_name: current.definition.name,
        cwd: `${marker}/cwd`, ...normalize(current.calls[0]), assertion_ids: current.assertions });
    } finally { for (const fn of after.reverse()) await fn(); }
  }
} finally {
  process.chdir(originalCwd); fs.rmSync(scratch, { recursive: true, force: true });
}
assert.ok(definitions.every(row => row.assertions.every(site => site.executions === 1)));
const corpus = `${rows.map(row => JSON.stringify(row)).join('\n')}\n`;
const metadata = { schema_version: 1, kind: 'frozen_status_settings_actual_file_schedules',
  baseline_commit: baseline.baseline_commit, callbacks_executed: 2, static_assertions: 4, expanded_assertions: 4,
  definition_hash_rule: 'Inventory SHA256 of source from test statement start to next statement start, including semicolon and two trailing newlines. Separate ast_call_sha256 and statement_sha256 retain exact TypeScript AST bounds.',
  sources: [{ path: sourcePath, sha256: hash(source), source }, { path: modulePath, sha256: hash(moduleSource), source: moduleSource }],
  definitions, corpus_sha256: hash(corpus), generator_sha256: hash(fs.readFileSync(import.meta.filename)),
  node_sha256: hash(fs.readFileSync(process.execPath)), node_version: process.version,
  normalization: 'Only private synthetic root and mkdtemp suffixes are replaced. Literal directory glob characters and escaped permission anchors remain present; relative source args stay relative. Renderer command differs between native/Node, so only common type/padding/refreshInterval is captured; native tests check its own full executable command separately.',
  supplemental_source_assertion: 'Original settings bytes are exactly unchanged across overlay generation.',
  provider_calls_performed: false, user_configuration_touched: false };
fs.mkdirSync(output, { mode: 0o700 });
fs.writeFileSync(join(output, 'cases.jsonl'), corpus, { flag: 'wx', mode: 0o600 });
fs.writeFileSync(join(output, 'capture.json'), `${JSON.stringify(metadata, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
assert.equal((await verifyBaseline(reference)).files_verified, 111);
console.log(JSON.stringify({ callbacks_executed: 2, static_assertions: 4, expanded_assertions: 4, cases: rows.length,
  corpus_sha256: hash(corpus), provider_calls_performed: false }));
