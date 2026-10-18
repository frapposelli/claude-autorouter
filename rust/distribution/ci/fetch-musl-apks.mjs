// Build-only SDK inputs. Exact reviewed bytes replace mutable index resolution.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {closeSync, constants, lstatSync, openSync, readFileSync, readdirSync,
  unlinkSync, writeFileSync, writeSync} from 'node:fs';
import https from 'node:https';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';

export async function fetchLockedApks(arch, pinsPath, directory, provenance, {get = https.get} = {}) {
  assert.ok(['x86_64', 'aarch64'].includes(arch));
  assert.ok(lstatSync(pinsPath).isFile());
  assert.ok(lstatSync(pinsPath).size <= 65536);
  const pinsBytes = readFileSync(pinsPath);
  assert.ok(pinsBytes.length <= 65536);
  const pins = JSON.parse(pinsBytes);
  const set = pins.apk_package_set;
  assert.equal(set.schema_version, 1);
  assert.equal(set.kind, 'closed_alpine_apk_sha256_set');
  assert.equal(set.repository, 'https://dl-cdn.alpinelinux.org/alpine/v3.21/main');
  assert.equal(set.filenames.length, 61);
  assert.deepEqual(set.filenames, [...new Set(set.filenames)].sort());
  assert.deepEqual(Object.keys(set.sha256_base64).sort(), ['aarch64', 'x86_64']);
  for (const name of set.filenames) {
    assert.match(name, /^[a-z0-9][a-z0-9+_.-]*-r[0-9]+\.apk$/);
  }
  const decoded = Object.fromEntries(Object.entries(set.sha256_base64).map(([target, hashes]) => {
    assert.equal(hashes.length, set.filenames.length);
    return [target, hashes.map(value => {
      assert.equal(typeof value, 'string');
      const bytes = Buffer.from(value, 'base64');
      assert.equal(bytes.length, 32);
      assert.equal(bytes.toString('base64'), value);
      return bytes.toString('hex');
    })];
  }));
  const expected = decoded[arch];
  const info = lstatSync(directory);
  assert.ok(info.isDirectory() && !info.isSymbolicLink());
  assert.deepEqual(readdirSync(directory), []);
  const expectedPath = join(dirname(provenance), 'apk-package-expected.sha256');
  for (const path of [provenance, expectedPath]) {
    try {
      lstatSync(path);
      throw new Error('Locked APK provenance already exists');
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
    }
  }
  const deadline = performance.now() + 600000;
  const files = [];
  let totalBytes = 0;
  for (const [index, name] of set.filenames.entries()) {
    const duration = Math.min(60000, deadline - performance.now());
    assert.ok(duration > 0, 'Locked APK set download deadline');
    const path = join(directory, name);
    const fd = openSync(path, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL, 0o600);
    const hash = createHash('sha256');
    let bytes = 0;
    let complete = false;
    try {
      await new Promise((resolve, reject) => {
        let request;
        let response;
        let settled = false;
        const finish = error => {
          if (settled) return;
          settled = true;
          clearTimeout(timer);
          response?.destroy();
          request?.destroy();
          if (error) reject(error);
          else resolve();
        };
        const timer = setTimeout(() => finish(new Error('Locked APK download deadline')), duration);
        try {
          request = get(set.repository + '/' + arch + '/' + encodeURIComponent(name), {agent: false}, res => {
            response = res;
            if (res.statusCode !== 200) {
              finish(new Error('Locked APK unavailable: ' + name));
              return;
            }
            res.on('data', chunk => {
              if (settled) return;
              try {
                bytes += chunk.length;
                totalBytes += chunk.length;
                assert.ok(bytes <= 64 * 1024 * 1024, 'Locked APK file bound');
                assert.ok(totalBytes <= 256 * 1024 * 1024, 'Locked APK set bound');
                hash.update(chunk);
                let at = 0;
                while (at < chunk.length) {
                  const count = writeSync(fd, chunk, at, chunk.length - at);
                  assert.ok(count > 0);
                  at += count;
                }
              } catch (error) { finish(error); }
            });
            res.on('aborted', () => finish(new Error('Locked APK response aborted')));
            res.on('error', () => finish(new Error('Locked APK response failed')));
            res.on('end', () => {
              if (settled) return;
              try {
                assert.equal(hash.digest('hex'), expected[index], 'Locked APK digest: ' + name);
                finish();
              } catch (error) { finish(error); }
            });
          });
          request.on('error', () => finish(new Error('Locked APK request failed')));
        } catch (error) { finish(error); }
      });
      complete = true;
    } finally {
      closeSync(fd);
      if (!complete) unlinkSync(path);
    }
    files.push({filename: name, sha256: expected[index], bytes});
  }
  writeFileSync(expectedPath,
    files.map(file => file.sha256 + '  ' + join(directory, file.filename) + '\n').join(''),
    {flag: 'wx', mode: 0o644});
  writeFileSync(provenance, JSON.stringify({schema_version: 1,
    kind: 'verified_closed_alpine_apk_set', arch,
    pins_sha256: createHash('sha256').update(pinsBytes).digest('hex'),
    package_count: files.length, total_bytes: totalBytes, files}, null, 2) + '\n',
  {flag: 'wx', mode: 0o644});
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  assert.equal(process.argv.length, 6);
  await fetchLockedApks(...process.argv.slice(2));
}
