import ts from 'typescript';
import { readFile } from 'node:fs/promises';
import { basename, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
let contracts;
function contractTypes() {
  if (contracts) return contracts;
  const path = resolve(root, 'test/static-contracts.mts');
  const program = ts.createProgram([path], { allowJs: true, noEmit: true, strict: true,
    noImplicitAny: false, module: ts.ModuleKind.NodeNext, moduleResolution: ts.ModuleResolutionKind.NodeNext,
    target: ts.ScriptTarget.ES2024, skipLibCheck: true, types: ['node'] });
  const checker = program.getTypeChecker();
  const source = program.getSourceFile(path);
  contracts = new Map();
  for (const statement of source.statements) {
    if (!ts.isImportDeclaration(statement) || !statement.importClause?.isTypeOnly) continue;
    for (const item of statement.importClause.namedBindings?.elements ?? []) {
      const symbol = checker.getSymbolAtLocation(item.name);
      const type = checker.getDeclaredTypeOfSymbol(checker.getAliasedSymbol(symbol));
      contracts.set(item.name.text, { type, checker, fields: new Set(type.getProperties().map(property => property.name)) });
    }
  }
  return contracts;
}

// TypeScript checks annotated implementation returns. This supplementary AST
// pass checks literal producers in transport code, whose provider data remains
// intentionally opaque. It never executes the source being inspected.
export function checkContractSources(sources) {
  const types = contractTypes();
  const errors = [];
  const report = (source, node, message) => {
    const position = source.getLineAndCharacterOfPosition(node.getStart(source));
    errors.push(`${source.fileName}:${position.line + 1}:${position.character + 1} ${message}`);
  };
  const nameOf = node => node && (ts.isIdentifier(node) || ts.isStringLiteral(node)) ? node.text : undefined;
  const literal = node => ts.isStringLiteral(node) || ts.isNumericLiteral(node)
    || node.kind === ts.SyntaxKind.TrueKeyword || node.kind === ts.SyntaxKind.FalseKeyword;
  const scalarMatches = (node, type) => {
    if (type.isUnion()) return type.types.some(member => scalarMatches(node, member));
    if (type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown)) return true;
    if (ts.isStringLiteral(node)) return Boolean(type.flags & ts.TypeFlags.String)
      || Boolean(type.flags & ts.TypeFlags.StringLiteral) && type.value === node.text;
    if (ts.isNumericLiteral(node)) return Boolean(type.flags & ts.TypeFlags.Number)
      || Boolean(type.flags & ts.TypeFlags.NumberLiteral) && type.value === Number(node.text);
    if ([ts.SyntaxKind.TrueKeyword, ts.SyntaxKind.FalseKeyword].includes(node.kind)) return Boolean(type.flags & ts.TypeFlags.Boolean)
      || Boolean(type.flags & ts.TypeFlags.BooleanLiteral) && type.intrinsicName === (node.kind === ts.SyntaxKind.TrueKeyword ? 'true' : 'false');
    return true;
  };
  const checkObject = (source, object, contract) => {
    if (!object || !ts.isObjectLiteralExpression(object)) return;
    const { type, checker, fields } = types.get(contract);
    for (const property of object.properties) {
      if (ts.isSpreadAssignment(property)) continue;
      const key = nameOf(property.name);
      if (!key || !fields.has(key)) { report(source, property, `Unknown ${contract} field ${key ?? '(computed)'}`); continue; }
      if (ts.isPropertyAssignment(property) && literal(property.initializer)) {
        const symbol = type.getProperty(key);
        const expected = checker.getTypeOfSymbolAtLocation(symbol, symbol.valueDeclaration ?? symbol.declarations[0]);
        if (!scalarMatches(property.initializer, expected)) report(source, property, `Invalid literal for ${contract}.${key}`);
      }
    }
  };
  const names = types.get('LifecycleEvent').type.getProperty('event');
  const eventType = types.get('LifecycleEvent').checker.getTypeOfSymbolAtLocation(names, names.valueDeclaration ?? names.declarations[0]);
  const eventNames = new Set(eventType.types.map(member => member.value));
  for (const [filename, text] of sources) {
    const source = ts.createSourceFile(filename, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
    const file = basename(filename);
    function walk(node, method) {
      if (ts.isMethodDeclaration(node)) method = nameOf(node.name);
      if (file === 'server.mjs' && ts.isCallExpression(node)) {
        const callee = nameOf(node.expression);
        if (callee === 'status' && ts.isStringLiteral(node.arguments[0])) {
          if (!eventNames.has(node.arguments[0].text)) report(source, node, 'Unknown lifecycle event');
          checkObject(source, node.arguments[1], 'TelemetryFields');
        }
        if (['onStatus', 'record', 'onRecord', 'onDecision'].includes(callee)) {
          checkObject(source, node.arguments[0], callee === 'onStatus' ? 'LifecycleEvent' : 'SessionRecord');
        }
      }
      if (file === 'router.mjs' && ts.isReturnStatement(node) && node.expression && ['route', 'classify'].includes(method)) {
        checkObject(source, node.expression, method === 'route' ? 'RoutingDecision' : 'ClassifierDecision');
      }
      if (file === 'router.mjs' && ts.isVariableDeclaration(node) && nameOf(node.name) === 'decision') {
        checkObject(source, node.initializer, 'ClassifierDecision');
      }
      // Selected scalar config accesses are checked even in unchecked modules.
      if (ts.isPropertyAccessExpression(node) && (nameOf(node.expression) === 'config'
        || (file === 'router.mjs' && nameOf(node.expression) === 'c'))) {
        if (!types.get('RouterConfig').fields.has(node.name.text)) report(source, node, `Unknown RouterConfig field ${node.name.text}`);
      }
      ts.forEachChild(node, child => walk(child, method));
    }
    walk(source);
  }
  return errors;
}

export async function checkContracts(paths) {
  return checkContractSources(await Promise.all(paths.map(async path => [path, await readFile(path, 'utf8')])));
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const paths = ['src/server.mjs', 'src/router.mjs', 'src/config-command.mjs', 'src/onboarding.mjs', 'bin/autorouter.mjs'];
  const errors = await checkContracts(paths.map(path => resolve(root, path)));
  for (const error of errors) console.error(error);
  if (errors.length) process.exitCode = 1;
  else console.log('Static configuration, routing-decision and event producer contracts checked.');
}
