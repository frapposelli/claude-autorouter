import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, writeFileSync, readFileSync, rmSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';
import { tmpdir } from 'node:os';
import { execFileSync } from 'node:child_process';
import { addStatusLineSettings, shellQuote } from '../src/status-settings.mjs';

test('session overlay preserves other settings and literal args without editing saved files', t => {
  const directory = mkdtempSync(join(tmpdir(), 'autorouter-settings-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const path = join(directory, 'original.json');
  const original = { env: { EXAMPLE: 'private-value' }, permissions: { allow: ['Read'] }, statusLine: { type: 'command', command: 'old-command' } };
  writeFileSync(path, JSON.stringify(original));
  const args = ['--settings', path, '--model', 'sonnet', '--', '--settings=literal-prompt'];
  const output = addStatusLineSettings(args, directory);
  assert.deepEqual(output.slice(2), ['--model', 'sonnet', '--', '--settings=literal-prompt']);
  assert.ok(!output.join(' ').includes('private-value'));
  assert.deepEqual(JSON.parse(readFileSync(path, 'utf8')), original);
  const generated = JSON.parse(readFileSync(output[1], 'utf8'));
  assert.deepEqual(generated.env, original.env);
  assert.deepEqual(generated.permissions, original.permissions);
  assert.equal(generated.statusLine.refreshInterval, 1);
  assert.notEqual(generated.statusLine.command, 'old-command');
  assert.equal(statSync(output[1]).mode & 0o777, 0o600);
});

test('inline settings and last repeated settings take precedence; invalid input stays private', t => {
  const directory = mkdtempSync(join(tmpdir(), 'autorouter-settings-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const output = addStatusLineSettings(['--settings', '{"model":"old"}', '--settings={"model":"new"}', '-p', 'hi'], directory);
  assert.equal(JSON.parse(readFileSync(output[1], 'utf8')).model, 'new');
  assert.deepEqual(output.slice(2), ['-p', 'hi']);
  for (const args of [['--settings'], ['--settings', '{"secret":"do-not-print"'], ['--settings', 'missing.json']]) {
    assert.throws(() => addStatusLineSettings(args, directory), error => !error.message.includes('do-not-print') && /--settings/.test(error.message));
  }
});

test('status command shell quoting preserves metacharacters literally', () => {
  const text = "a b ' c $HOME $(echo wrong) `echo wrong`";
  const command = `printf %s ${shellQuote(text)}`;
  assert.equal(execFileSync('/bin/sh', ['-c', command], { encoding: 'utf8' }), text);
});

test('relocated file settings retain source-relative allow, ask and deny permission anchors', t => {
  const directory = mkdtempSync(join(tmpdir(), 'autorouter-settings-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const sourceDirectory = join(directory, 'original');
  const outputDirectory = join(directory, 'overlay');
  mkdirSync(sourceDirectory);
  mkdirSync(outputDirectory);
  const sourcePath = join(sourceDirectory, 'config.json');
  const original = { permissions: {
    allow: ['Read(/public/**)', 'Edit(/src/**/*.mjs)', 'Read', 'Bash(node --test)'],
    ask: ['Edit(/review/**)', 'Read(/logs/*.log)'],
    deny: ['Read(/secrets/**)', 'Edit(/protected/**)'],
    defaultMode: 'dontAsk',
  } };
  writeFileSync(sourcePath, JSON.stringify(original));
  const output = addStatusLineSettings(['--settings', relative(process.cwd(), sourcePath)], outputDirectory);
  const result = JSON.parse(readFileSync(output[1], 'utf8'));
  assert.deepEqual(result.permissions, {
    allow: [`Read(/${sourceDirectory}/public/**)`, `Edit(/${sourceDirectory}/src/**/*.mjs)`, 'Read', 'Bash(node --test)'],
    ask: [`Edit(/${sourceDirectory}/review/**)`, `Read(/${sourceDirectory}/logs/*.log)`],
    deny: [`Read(/${sourceDirectory}/secrets/**)`, `Edit(/${sourceDirectory}/protected/**)`],
    defaultMode: 'dontAsk',
  });
  assert.deepEqual(JSON.parse(readFileSync(sourcePath, 'utf8')), original);
});

test('inline settings retain cwd anchors and leave absolute, home and cwd-relative patterns unchanged', t => {
  const directory = mkdtempSync(join(tmpdir(), 'autorouter-settings-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const unchanged = ['Read(//private/secrets/**)', 'Edit(~/Documents/**)', 'Read(./.env)', 'Edit(src/**)', 'Read(!.env)', 'Write(/legacy/**)', 'Bash(cat /tmp/file)'];
  const permissions = {
    allow: ['Read(/public/**)', ...unchanged],
    ask: ['Edit(/review/**)', ...unchanged],
    deny: ['Read(/secrets/**)', ...unchanged],
  };
  const output = addStatusLineSettings([`--settings=${JSON.stringify({ permissions })}`], directory);
  const result = JSON.parse(readFileSync(output[1], 'utf8'));
  assert.deepEqual(result.permissions.allow, [`Read(/${process.cwd()}/public/**)`, ...unchanged]);
  assert.deepEqual(result.permissions.ask, [`Edit(/${process.cwd()}/review/**)`, ...unchanged]);
  assert.deepEqual(result.permissions.deny, [`Read(/${process.cwd()}/secrets/**)`, ...unchanged]);
});

test('literal glob characters in a settings directory cannot broaden rebased permissions', t => {
  const directory = mkdtempSync(join(tmpdir(), 'autorouter-settings-[literal]*?-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const path = join(directory, 'original.json');
  writeFileSync(path, JSON.stringify({ permissions: { allow: ['Edit(/src/**)'], deny: ['Read(/keys/**)'] } }));
  const output = addStatusLineSettings(['--settings', path], directory);
  const permissions = JSON.parse(readFileSync(output[1], 'utf8')).permissions;
  const anchor = directory.replace(/[\\*?\[\]]/g, '\\$&');
  assert.deepEqual(permissions, { allow: [`Edit(/${anchor}/src/**)`], deny: [`Read(/${anchor}/keys/**)`] });
  assert.ok(permissions.allow[0].includes('\\[literal\\]\\*\\?'));
});

test('absolute and home sandbox filesystem and credential paths are retained verbatim', t => {
  const directory = mkdtempSync(join(tmpdir(), 'autorouter-settings-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const sandbox = {
    enabled: true,
    filesystem: { allowRead: ['/public'], allowWrite: ['~/build'], denyRead: ['/private/secrets'], denyWrite: ['~/.ssh'] },
    credentials: { files: [{ path: '~/.aws/credentials', mode: 'deny' }, { path: '/private/credential', mode: 'mask' }] },
  };
  const output = addStatusLineSettings(['--settings', JSON.stringify({ sandbox })], directory);
  assert.deepEqual(JSON.parse(readFileSync(output[1], 'utf8')).sandbox, sandbox);
});

test('declines ambiguous relative sandbox paths without rewriting original security settings', t => {
  const directory = mkdtempSync(join(tmpdir(), 'autorouter-settings-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  for (const settings of [
    ...['allowRead', 'allowWrite', 'denyRead', 'denyWrite'].map(key => ({ sandbox: { filesystem: { [key]: ['./private-relative-path'] } } })),
    { sandbox: { credentials: { files: [{ path: '../credential', mode: 'deny' }] } } },
  ]) {
    assert.throws(() => addStatusLineSettings(['--settings', JSON.stringify(settings)], directory), error =>
      /source-relative sandbox paths/.test(error.message) && !error.message.includes('private-relative-path'));
    const path = join(directory, 'original.json');
    writeFileSync(path, JSON.stringify(settings));
    assert.throws(() => addStatusLineSettings(['--settings', path], directory), /source-relative sandbox paths/);
    assert.deepEqual(JSON.parse(readFileSync(path, 'utf8')), settings);
  }
});
