// Capture only the five frozen pure release definitions; never run Git/npm/registry operations.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';

const ts = createRequire(import.meta.url)('typescript');
const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2] ?? join(root, 'artifacts/rust-rewrite/reference'));
const output = resolve(process.argv[3] ?? join(root, `artifacts/rust-rewrite/release-contracts-${Date.now()}`));
assert.ok(process.argv.length <= 4);
await verifyBaseline(reference);
const baseline = JSON.parse(readFileSync(join(root, 'rust/parity/baseline.json')));
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const checked = path => {
  const source = readFileSync(join(reference, path), 'utf8');
  assert.equal(hash(source), baseline.files.find(row => row.path === path)?.sha256);
  return source;
};
const check = await import(pathToFileURL(join(reference, 'scripts/release-check.mjs')).href);
const verify = await import(pathToFileURL(join(reference, 'scripts/release-verify.mjs')).href);
const cases = [], definitions = [];
let current;
const clone = value => JSON.parse(JSON.stringify(value));
function tracked(name, fn) {
  return (...args) => {
    const input = clone(args);
    const id = `baseline-release-${current.file.includes('verification') ? 'verification' : 'check'}-${current.number}-${current.calls.length + 1}`;
    let outcome;
    try {
      const result = fn(...args);
      outcome = { ok: true, result: clone(result) };
      return result;
    } catch (error) {
      outcome = { ok: false, error: { name: error.name, message: error.message,
        ...(error.code === undefined ? {} : { code: error.code }),
        ...(error.detail === undefined ? {} : { detail: clone(error.detail) }) } };
      throw error;
    } finally {
      assert.deepEqual(clone(args), input, 'Pure release API mutated its arguments');
      current.calls.push(id);
      cases.push({ id, source_test_id: current.id, op: name, input, node_expected: outcome });
    }
  };
}
const pure = {
  releaseMetadata: tracked('release_metadata', check.releaseMetadata),
  parseVersion: tracked('parse_version', verify.parseVersion),
  compareVersions: tracked('compare_versions', verify.compareVersions),
  validateArtifactSource: tracked('validate_artifact_source', verify.validateArtifactSource),
};
const selected = new Map([
  ['test/release-check.test.mjs', new Set([1, 2, 3])],
  ['test/release-verification.test.mjs', new Set([1, 11])],
]);
const temporary = mkdtempSync(join(tmpdir(), 'autorouter-pure-release-capture-'));
const token = '__autorouterPureReleaseCapture';
assert.equal(globalThis[token], undefined);
try {
  for (const [file, numbers] of selected) {
    const source = checked(file);
    const syntax = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
    assert.equal(syntax.parseDiagnostics.length, 0);
    const edits = [], callbacks = [];
    let number = 0;
    for (const statement of syntax.statements) {
      if (ts.isImportDeclaration(statement)) {
        const specifier = statement.moduleSpecifier.text;
        if (specifier === 'node:test' || specifier === 'node:assert/strict' || specifier.startsWith('../scripts/')) {
          edits.push({ start: statement.getStart(syntax), end: statement.end, value: '' });
        }
        continue;
      }
      if (!ts.isExpressionStatement(statement) || !ts.isCallExpression(statement.expression)
        || statement.expression.expression.getText(syntax) !== 'test') continue;
      const call = statement.expression;
      number++;
      if (!numbers.has(number)) {
        edits.push({ start: statement.getStart(syntax), end: statement.end, value: '' });
        continue;
      }
      const row = { id: `${file}#${number}`, file, number, name: call.arguments[0].text,
        baseline_file_sha256: hash(source), definition_sha256: hash(statement.getText(syntax)),
        assertions: [], calls: [], executed_assertions: 0 };
      const walk = node => {
        if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)
          && node.expression.expression.getText(syntax) === 'assert') {
          row.assertions.push({ line: syntax.getLineAndCharacterOfPosition(node.getStart(syntax)).line + 1,
            method: node.expression.name.text, expression: node.getText(syntax) });
        }
        ts.forEachChild(node, walk);
      };
      walk(call);
      definitions.push(row);
    }
    const declarations = definitions.filter(row => row.file === file);
    let transformed = source;
    for (const edit of edits.sort((a, b) => b.start - a.start)) {
      transformed = transformed.slice(0, edit.start) + edit.value + transformed.slice(edit.end);
    }
    globalThis[token] = { ...pure,
      test(name, callback) {
        const row = declarations[callbacks.length];
        assert.equal(name, row.name); assert.equal(typeof callback, 'function');
        callbacks.push({ row, callback });
      },
      assert: new Proxy(assert, { get(target, method) {
        const value = Reflect.get(target, method);
        if (typeof value !== 'function') return value;
        return (...args) => { const result = Reflect.apply(value, target, args); current.executed_assertions++; return result; };
      } }),
    };
    const prologue = `const {test,assert,releaseMetadata,parseVersion,compareVersions,validateArtifactSource}=globalThis[${JSON.stringify(token)}];\n`;
    const path = join(temporary, `${declarations[0].number}-${file.includes('verification') ? 'verify' : 'check'}.mjs`);
    writeFileSync(path, prologue + transformed, { flag: 'wx', mode: 0o600 });
    await import(pathToFileURL(path).href);
    assert.equal(callbacks.length, numbers.size);
    for (const { row, callback } of callbacks) {
      current = row;
      assert.equal(callback(), undefined, 'Only synchronous pure definitions may run');
      assert.ok(row.calls.length); assert.ok(row.executed_assertions >= row.assertions.length);
    }
  }
} finally {
  delete globalThis[token];
  rmSync(temporary, { recursive: true, force: true });
}
const bytes = cases.map(row => JSON.stringify(row)).join('\n') + '\n';
const report = { schema_version: 1, kind: 'frozen_pure_release_contract_capture', passed: true,
  baseline_commit: baseline.baseline_commit, node_version: process.version, typescript_version: ts.version,
  generator_sha256: hash(readFileSync(new URL(import.meta.url))), cases_sha256: hash(bytes),
  definitions, cases: cases.length, static_assertions: definitions.reduce((n, row) => n + row.assertions.length, 0),
  executed_assertions: definitions.reduce((n, row) => n + row.executed_assertions, 0),
  scope: 'Original five pure callbacks, assertions and loop arguments execute unchanged in a same-realm temporary module. Only imports are rebound and unselected test registrations removed. Unused filesystem/npm/Git helper declarations remain unexecuted. Pure inputs and complete returned values/errors are retained. No registry, npm, Git, tag or publication operation runs. Error messages/codes unasserted by the source remain observations, not automatically claimed native API parity.' };
mkdirSync(output, { recursive: false, mode: 0o700 });
writeFileSync(join(output, 'cases.jsonl'), bytes, { flag: 'wx', mode: 0o600 });
writeFileSync(join(output, 'capture.json'), JSON.stringify(report, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify(report, null, 2));
