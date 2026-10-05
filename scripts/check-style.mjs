import ts from 'typescript';
import { readFile } from 'node:fs/promises';

// A deliberately small style policy: preserve readable existing layouts while
// checking whitespace, var declarations and accidental coercing comparisons.
export function checkStyle(filename, text) {
  const errors = [];
  if (!text.endsWith('\n') || text.endsWith('\n\n')) errors.push(`${filename}: expected one final newline`);
  if (text.includes('\r')) errors.push(`${filename}: use LF line endings`);
  for (const [index, line] of text.split('\n').entries()) {
    if (/[ \t]+$/.test(line)) errors.push(`${filename}:${index + 1}: trailing whitespace`);
    if (/^\t/.test(line)) errors.push(`${filename}:${index + 1}: indent with spaces`);
  }
  const source = ts.createSourceFile(filename, text, ts.ScriptTarget.Latest, true,
    filename.endsWith('.mts') ? ts.ScriptKind.TS : ts.ScriptKind.JS);
  const report = (node, message) => errors.push(`${filename}:${source.getLineAndCharacterOfPosition(node.getStart(source)).line + 1}: ${message}`);
  function walk(node) {
    if (ts.isVariableDeclarationList(node) && !(node.flags & (ts.NodeFlags.Let | ts.NodeFlags.Const))) report(node, 'use const or let');
    if (ts.isBinaryExpression(node) && [ts.SyntaxKind.EqualsEqualsToken, ts.SyntaxKind.ExclamationEqualsToken].includes(node.operatorToken.kind)
      && node.left.kind !== ts.SyntaxKind.NullKeyword && node.right.kind !== ts.SyntaxKind.NullKeyword) report(node, 'use strict equality except explicit null/undefined checks');
    ts.forEachChild(node, walk);
  }
  walk(source);
  return errors;
}

export async function checkStyles(paths) {
  return (await Promise.all(paths.map(async path => checkStyle(path, await readFile(path, 'utf8'))))).flat();
}
