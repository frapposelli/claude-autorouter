// Exact finite synthetic inputs only; unchanged HTTP callbacks run separately.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { runInNewContext } from 'node:vm';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/server-subscription-${Date.now()}`));
assert.ok(process.argv.length <= 4);
await verifyBaseline(reference);
const file = 'test/server.test.mjs';
const source = readFileSync(join(reference, file), 'utf8');
const baseline = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json')));
const hash = value => createHash('sha256').update(value).digest('hex');
assert.equal(hash(source), baseline.files.find(row => row.path === file)?.sha256);
const ts = createRequire(import.meta.url)('typescript');
const syntax = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
assert.equal(syntax.parseDiagnostics.length, 0);
function find(node, predicate) {
  const rows = [];
  const visit = item => { if (predicate(item)) rows.push(item); ts.forEachChild(item, visit); };
  visit(node); return rows;
}
const all = find(syntax, node => ts.isExpressionStatement(node) && ts.isCallExpression(node.expression)
  && node.expression.expression.getText(syntax) === 'test');
const definitions = [11, 12, 13, 14].map(number => {
  const statement = all[number - 1], call = statement.expression;
  return { number, id: `${file}#${number}`, name: call.arguments[0].text,
    line: syntax.getLineAndCharacterOfPosition(statement.getStart(syntax)).line + 1,
    definition_statement: statement.getText(syntax), definition_sha256: hash(statement.getText(syntax)),
    definition_call_sha256: hash(call.getText(syntax)), statement,
    assertions: find(call, node => ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)
      && node.expression.expression.getText(syntax) === 'assert').map(node => ({
      line: syntax.getLineAndCharacterOfPosition(node.getStart(syntax)).line + 1,
      expression: node.getText(syntax), sha256: hash(node.getText(syntax)),
    })) };
});
const inventory = JSON.parse(readFileSync(join(root, 'rust/parity/coverage.json')));
for (const row of definitions) assert.equal(inventory.baseline_tests.find(item => item.id === row.id)?.name, row.name);
function initializer(node, name) {
  const found = find(node, item => ts.isVariableDeclaration(item) && item.name.getText(syntax) === name);
  assert.equal(found.length, 1, name); return found[0].initializer.getText(syntax);
}
const top = name => {
  const nodes = syntax.statements.filter(ts.isVariableStatement).flatMap(node => [...node.declarationList.declarations])
    .filter(node => node.name.getText(syntax) === name);
  assert.equal(nodes.length, 1); return nodes[0].initializer.getText(syntax);
};
const inputs = [];
function value(expression, context = {}) {
  assert.ok(Buffer.byteLength(expression) <= 4096);
  const result = runInNewContext(`(${expression})`, context, { timeout: 100, contextCodeGeneration: { strings: false, wasm: false } });
  const wire = JSON.stringify(result); assert.ok(Buffer.byteLength(wire) <= 8192);
  inputs.push({ expression, sha256: hash(expression) });
  return JSON.parse(wire);
}
const token = value(top('token')), body = value(top('body')), oauth = value(top('oauthHeaders'));
const context = { token, body, oauthHeaders: oauth };
const headersLoop = find(definitions[0].statement, ts.isForOfStatement);
assert.equal(headersLoop.length, 1);
const rejectionHeaders = value(headersLoop[0].expression.getText(syntax), context);
assert.equal(rejectionHeaders.length, 6);
const statuses = value(initializer(definitions[1].statement, 'statuses'));
assert.deepEqual(statuses, [401, 429]);
const writeHead = find(definitions[1].statement, node => ts.isCallExpression(node) && node.expression.getText(syntax) === 'res.writeHead');
const end = find(definitions[1].statement, node => ts.isCallExpression(node) && node.expression.getText(syntax) === 'res.end');
assert.equal(writeHead.length, 1); assert.equal(end.length, 1);
const responseHeaders = value(writeHead[0].arguments[1].getText(syntax));
const responses = statuses.map(status => ({ status, headers: responseHeaders,
  body_text: value(end[0].arguments[0].getText(syntax), { status }) }));
const payload = value(initializer(definitions[3].statement, 'payload'), context);
const beta = value(initializer(definitions[3].statement, 'beta'), context);
const rows = definitions.map(row => ({ id: `baseline-server-subscription-${row.number}`, source_test: row.id,
  token, request: row.number === 14 ? payload : body, oauth_headers: row.number === 14 ? { ...oauth, 'anthropic-beta': beta } : oauth,
  ...(row.number === 11 ? { rejection_headers: rejectionHeaders } : {}),
  ...(row.number === 12 ? { responses } : {}),
}));
const corpus = rows.map(row => JSON.stringify(row)).join('\n') + '\n';
assert.ok(Buffer.byteLength(corpus) <= 16384);
const report = { schema_version: 1, kind: 'frozen_server_subscription_input_extraction',
  baseline_commit: baseline.baseline_commit, baseline_file: file, baseline_file_sha256: hash(source),
  generator_sha256: hash(readFileSync(import.meta.filename)), node_version: process.version, typescript_version: ts.version,
  cases_sha256: hash(corpus), cases: rows.length, original_callbacks_executed: 0,
  static_assertions: definitions.reduce((sum, row) => sum + row.assertions.length, 0),
  definitions: definitions.map(({ statement, ...row }) => row), evaluated_inputs: inputs,
  scope: 'Only verified source literal/spread initializers, the original six headers, two status values and response constructor expressions are evaluated. Complete callbacks and assertions remain hash-bound; run unchanged frozen controls separately. No HTTP, reference-test or native behavior is established by input extraction alone.' };
mkdirSync(output, { mode: 0o700 });
writeFileSync(join(output, 'cases.jsonl'), corpus, { flag: 'wx', mode: 0o600 });
writeFileSync(join(output, 'capture.json'), JSON.stringify(report, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify({ cases: rows.length, assertions: report.static_assertions, cases_sha256: report.cases_sha256 }));
