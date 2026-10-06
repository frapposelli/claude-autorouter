import test from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, utimesSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { removeStaleStatusDirectories } from '../src/status-cleanup.mjs';

const NOW = Date.UTC(2026, 9, 6, 12);
const DEAD = 111111, LIVE = 222222;
const alive = pid => pid === LIVE;

function temporary(t) {
  const root = mkdtempSync(join(tmpdir(), 'autorouter-cleanup-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return root;
}

function statusDirectory(root, name, snapshot, { mode = 0o700, age } = {}) {
  const path = join(root, name);
  mkdirSync(path, { mode });
  chmodSync(path, mode);
  writeFileSync(join(path, 'claude-settings.json'), '{"env":{"PRIVATE":"value"}}', { mode: 0o600 });
  if (snapshot !== undefined) writeFileSync(join(path, 'state.json'), typeof snapshot === 'string' ? snapshot : JSON.stringify(snapshot));
  if (age !== undefined) utimesSync(path, new Date(NOW - age), new Date(NOW - age));
  return path;
}

const run = (root, options = {}) => removeStaleStatusDirectories({ directory: root, alive, now: NOW, ...options });

test('a directory left by a dead launcher is removed with its settings copy, a live one is kept', async t => {
  const root = temporary(t);
  const dead = statusDirectory(root, 'autorouter-status-dead', { pid: DEAD, heartbeat_at: NOW - 1000 });
  const live = statusDirectory(root, 'autorouter-status-live', { pid: LIVE, heartbeat_at: NOW - 1000 });
  assert.equal(await run(root), 1);
  assert.equal(existsSync(dead), false);
  assert.equal(existsSync(live), true);
});

test('a reused process ID with an ancient heartbeat does not protect a directory', async t => {
  const root = temporary(t);
  const reused = statusDirectory(root, 'autorouter-status-reused', { pid: LIVE, heartbeat_at: NOW - 25 * 60 * 60 * 1000 });
  const sleeping = statusDirectory(root, 'autorouter-status-sleeping', { pid: LIVE, heartbeat_at: NOW - 60 * 60 * 1000 });
  assert.equal(await run(root), 1);
  assert.equal(existsSync(reused), false);
  assert.equal(existsSync(sleeping), true, 'A suspended session is not abandoned after an hour');
});

test('a directory without a readable snapshot is removed only after a grace period', async t => {
  const root = temporary(t);
  const starting = statusDirectory(root, 'autorouter-status-starting', undefined, { age: 30 * 1000 });
  const abandoned = statusDirectory(root, 'autorouter-status-abandoned', 'not json', { age: 11 * 60 * 1000 });
  const oversized = statusDirectory(root, 'autorouter-status-oversized', ' '.repeat(1024 * 1024 + 1), { age: 11 * 60 * 1000 });
  const noPid = statusDirectory(root, 'autorouter-status-nopid', { pid: 'abc' }, { age: 11 * 60 * 1000 });
  assert.equal(await run(root), 3);
  assert.equal(existsSync(starting), true);
  for (const path of [abandoned, oversized, noPid]) assert.equal(existsSync(path), false);
});

test('only this user\'s private directories with the status prefix are touched', async t => {
  const root = temporary(t);
  const stale = { pid: DEAD, heartbeat_at: NOW };
  const loose = statusDirectory(root, 'autorouter-status-loose', stale, { mode: 0o755 });
  const unrelated = statusDirectory(root, 'other-status-dead', stale);
  const foreign = statusDirectory(root, 'autorouter-status-foreign', stale);
  const target = join(root, 'target');
  mkdirSync(target);
  writeFileSync(join(target, 'keep.txt'), 'keep');
  symlinkSync(target, join(root, 'autorouter-status-link'));
  writeFileSync(join(root, 'autorouter-status-file'), 'file');
  assert.equal(await run(root, { uid: process.getuid() + 1 }), 0, 'Directories owned by another user are skipped');
  assert.equal(await run(root), 1, 'Only the private directory is removed');
  assert.equal(existsSync(foreign), false);
  for (const path of [loose, unrelated, join(target, 'keep.txt'), join(root, 'autorouter-status-file')]) assert.equal(existsSync(path), true);
});

test('cleanup never throws, including for a missing directory or a failing filesystem', async t => {
  const root = temporary(t);
  assert.equal(await run(join(root, 'missing')), 0);
  statusDirectory(root, 'autorouter-status-dead', { pid: DEAD, heartbeat_at: NOW });
  const failing = { readdir: async () => ['autorouter-status-dead'], lstat: async () => { throw new Error('PRIVATE_PATH'); } };
  assert.equal(await run(root, { io: failing }), 0);
});
