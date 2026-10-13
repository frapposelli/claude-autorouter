// Development-only source capture. This never runs providers or the captured tests.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

assert.equal(process.argv.length, 4, 'Pass a frozen reference and a new capture path');
const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2]);
const output = resolve(process.argv[3]);
const hash = value => createHash('sha256').update(value).digest('hex');
const baseline = JSON.parse(await readFile(join(root, 'rust/parity/baseline.json'), 'utf8'));
const verification = await verifyBaseline(reference);
assert.equal(verification.files_verified, 111);
const ts = createRequire(join(root, 'package.json'))('typescript');
const inputs = [
  'test/benchmark-bundle.test.mjs', 'test/live-rubric.test.mjs', 'test/protocol-fixtures.test.mjs',
  'scripts/benchmark-bundle.mjs', 'scripts/live-validation.mjs', 'src/evaluation-report.mjs',
  'test/fixtures/claude-protocol-v1.json',
];
const sources = [];
const tests = [];
const helpers = [];
for (const path of inputs) {
  const source = await readFile(join(reference, path), 'utf8');
  const sha256 = hash(source);
  const expected = baseline.files.find(row => row.path === path);
  assert.ok(expected, `${path}: baseline input must be declared`);
  assert.equal(sha256, expected.sha256, `${path}: exact frozen source`);
  sources.push({ path, sha256, source });
  if (!path.endsWith('.test.mjs')) continue;
  const syntax = ts.createSourceFile(path, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
  assert.equal(syntax.parseDiagnostics.length, 0, `${path}: JavaScript syntax`);
  const line = node => syntax.getLineAndCharacterOfPosition(node.getStart(syntax)).line + 1;
  const assertions = node => {
    const rows = [];
    const visit = child => {
      if (ts.isCallExpression(child) && ts.isPropertyAccessExpression(child.expression)
        && ts.isIdentifier(child.expression.expression) && child.expression.expression.text === 'assert') {
        rows.push({ line: line(child), expression: child.getText(syntax) });
      }
      ts.forEachChild(child, visit);
    };
    visit(node);
    return rows;
  };
  const visit = node => {
    if (ts.isCallExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === 'test') {
      const callback = [...node.arguments].reverse().find(argument => ts.isArrowFunction(argument) || ts.isFunctionExpression(argument));
      assert.ok(callback, `${path}: original test callback`);
      let ancestor = node.parent;
      const parameter_loops = [];
      while (ancestor && ancestor !== syntax) {
        if (ts.isForOfStatement(ancestor)) parameter_loops.push(ancestor.expression.getText(syntax));
        ancestor = ancestor.parent;
      }
      const start = node.getStart(syntax), end = node.end;
      tests.push({
        baseline_test: `${path}#${tests.filter(row => row.path === path).length + 1}`,
        path, name: ts.isStringLiteral(node.arguments[0]) ? node.arguments[0].text : node.arguments[0].getText(syntax),
        line: line(node), start, end, definition_sha256: hash(source.slice(start, end)),
        parameter_loops, assertion_calls: assertions(callback),
      });
      return;
    }
    if (ts.isFunctionDeclaration(node) && node.name?.text === 'fixtureGateway') {
      helpers.push({ path, name: node.name.text, line: line(node), definition_sha256: hash(node.getText(syntax)), assertion_calls: assertions(node) });
    }
    ts.forEachChild(node, visit);
  };
  visit(syntax);
}
assert.equal(tests.length, 5);
assert.deepEqual(tests.map(row => row.baseline_test), [
  'test/benchmark-bundle.test.mjs#1', 'test/live-rubric.test.mjs#1', 'test/live-rubric.test.mjs#2',
  'test/protocol-fixtures.test.mjs#1', 'test/protocol-fixtures.test.mjs#2',
]);
const fixture = JSON.parse(sources.find(row => row.path === 'test/fixtures/claude-protocol-v1.json').source);
// Importing this frozen module only declares cases; its CLI guard is inactive.
// Project only the prompt/label fields consumed by the original rubric test.
const { LIVE_CASES } = await import(pathToFileURL(join(reference, 'scripts/live-validation.mjs')).href);
const live_cases = Object.fromEntries(Object.entries(LIVE_CASES).map(([name, scenario]) => [name,
  Object.fromEntries(['prompts', 'expectedClassifiedTiers', 'expectedClassifiedTier', 'expectedTier']
    .filter(field => Object.hasOwn(scenario, field)).map(field => [field, scenario[field]])),
]));
await writeFile(output, `${JSON.stringify({
  schema_version: 1, kind: 'frozen_tooling_rubric_protocol_source_contracts',
  baseline_commit: baseline.baseline_commit,
  definition_hash_rule: 'SHA256 of exact TypeScript-AST test call source[start:end], UTF8, excluding the statement semicolon.',
  captured_tests_executed: false, provider_calls_performed: false,
  sources, tests, helpers,
  live_cases,
  protocol_case_ids: fixture.cases.map(row => row.id),
  protocol_steps: fixture.cases.reduce((count, row) => count + row.steps.length, 0),
  generator_sha256: hash(await readFile(import.meta.filename)),
}, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify({ tests: tests.length, sources: sources.length, protocol_cases: fixture.cases.length, provider_calls_performed: false }));
