// Extract exact synthetic inputs from three frozen server definitions.
// This does not execute their callbacks or claim reference-test completion.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { runInNewContext } from 'node:vm';
import { gzipSync, gunzipSync } from 'node:zlib';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const destination = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/server-response-inputs-${Date.now()}`));
assert.ok(process.argv.length <= 4);
await verifyBaseline(reference);
const baseline = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json')));
const file = 'test/server.test.mjs';
const source = readFileSync(join(reference, file), 'utf8');
const hash = value => createHash('sha256').update(value).digest('hex');
assert.equal(hash(source), baseline.files.find(row => row.path === file)?.sha256);
const ts = createRequire(import.meta.url)('typescript');
const syntax = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
assert.equal(syntax.parseDiagnostics.length, 0);
const definitions = [];
const selected = new Set([2, 3, 20]);
let number = 0;
const statements = [];
const collectDefinitions = node => {
  if (ts.isExpressionStatement(node)) statements.push(node);
  ts.forEachChild(node, collectDefinitions);
};
collectDefinitions(syntax);
for (const statement of statements) {
  if (!ts.isExpressionStatement(statement) || !ts.isCallExpression(statement.expression)
      || statement.expression.expression.getText(syntax) !== 'test') continue;
  number++;
  if (!selected.has(number)) continue;
  const assertions = [];
  const visit = node => {
    if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)
        && node.expression.expression.getText(syntax) === 'assert') {
      assertions.push({ line: syntax.getLineAndCharacterOfPosition(node.getStart(syntax)).line + 1,
        expression: node.getText(syntax), sha256: hash(node.getText(syntax)) });
    }
    ts.forEachChild(node, visit);
  };
  visit(statement);
  definitions.push({ id: `${file}#${number}`, number,
    name: statement.expression.arguments[0].text,
    definition_sha256: hash(statement.getText(syntax)),
    assertions, statement });
}
assert.equal(definitions.length, 3);
const inventory = JSON.parse(readFileSync(join(root, 'rust/parity/coverage.json')));
for (const definition of definitions) {
  assert.equal(inventory.baseline_tests.find(row => row.id === definition.id)?.name, definition.name);
}
function initializer(node, name) {
  const found = [];
  const visit = value => {
    if (ts.isVariableDeclaration(value) && value.name.getText(syntax) === name) {
      assert.ok(value.initializer);
      found.push(value.initializer.getText(syntax));
    }
    ts.forEachChild(value, visit);
  };
  visit(node);
  assert.equal(found.length, 1, `Expected one frozen ${name} initializer`);
  return found[0];
}
function evaluate(expression, context = {}) {
  assert.ok(Buffer.byteLength(expression) <= 4096);
  return runInNewContext(`(${expression})`, context,
    { timeout: 100, contextCodeGeneration: { strings: false, wasm: false } });
}
// The verified source contains only literal synthetic data in these bindings.
const declarations = syntax.statements.filter(ts.isVariableStatement);
// Traverse actual AST statements, without evaluating unrelated declarations.
const topInitializer = name => {
  const declaration = declarations.flatMap(node => [...node.declarationList.declarations])
    .filter(node => node.name.getText(syntax) === name);
  assert.equal(declaration.length, 1);
  return declaration[0].initializer.getText(syntax);
};
const token = evaluate(topInitializer('token'));
const body = JSON.parse(JSON.stringify(evaluate(topInitializer('body'))));
const countError = evaluate(initializer(definitions[0].statement, 'error'));
const gzipInput = initializer(definitions[2].statement, 'compressed');
let gzipCalls = 0;
const compressed = evaluate(gzipInput, { gzipSync(value) {
  assert.equal(typeof value, 'string');
  assert.ok(Buffer.byteLength(value) <= 4096);
  gzipCalls++;
  return gzipSync(value);
} });
assert.equal(gzipCalls, 1);
assert.equal(typeof token, 'string');
assert.equal(typeof countError, 'string');
assert.ok(Buffer.isBuffer(compressed) && compressed.length <= 4096);
const rows = [
  { id: 'baseline-server-response-2', source_test_id: definitions[0].id,
    operation: 'token_count_error', token, request: body, response_status: 429,
    response_headers: { 'retry-after': '7', 'content-type': 'application/json' }, response_text: countError },
  { id: 'baseline-server-response-3', source_test_id: definitions[1].id,
    operation: 'local_rejections', token, request: body, maximum_body_bytes: 1000 },
  { id: 'baseline-server-response-20', source_test_id: definitions[2].id,
    operation: 'compressed_passthrough', token, request: body, response_status: 200,
    response_headers: { 'content-type': 'text/event-stream', 'content-encoding': 'gzip' },
    response_bytes: [...compressed], decoded_synthetic_text: gunzipSync(compressed).toString('utf8') },
];
const bytes = rows.map(row => JSON.stringify(row)).join('\n') + '\n';
assert.ok(Buffer.byteLength(bytes) <= 16384);
const report = { schema_version: 1, kind: 'frozen_server_response_input_extraction',
  baseline_commit: baseline.baseline_commit, baseline_file_sha256: hash(source),
  node_version: process.version, typescript_version: ts.version,
  platform: process.platform, architecture: process.arch, zlib_version: process.versions.zlib,
  generator_sha256: hash(readFileSync(new URL(import.meta.url))), cases_sha256: hash(bytes),
  definitions: definitions.map(({ statement, ...row }) => row), cases: rows.length,
  static_assertions: definitions.reduce((sum, row) => sum + row.assertions.length, 0),
  original_callbacks_executed: 0,
  scope: 'Only exact literal body/token/error initializers and the original gzip constructor expression are evaluated. Three complete original definitions and their assertions are hash-bound. Run unchanged frozen Node controls separately; this extraction is input provenance, not reference or native behavioral evidence.' };
mkdirSync(destination, { recursive: false, mode: 0o700 });
writeFileSync(join(destination, 'cases.jsonl'), bytes, { flag: 'wx', mode: 0o600 });
writeFileSync(join(destination, 'capture.json'), JSON.stringify(report, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify(report, null, 2));
