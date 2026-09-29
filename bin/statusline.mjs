#!/usr/bin/env node
import { createReadStream } from 'node:fs';
import { stat } from 'node:fs/promises';
import { renderStatusLine } from '../src/statusline.mjs';

const LIMIT = 1024 * 1024;
async function readJson(stream) {
  let size = 0;
  const chunks = [];
  try {
    for await (const chunk of stream) {
      size += chunk.length;
      if (size <= LIMIT) chunks.push(chunk);
      else chunks.length = 0;
    }
    return size <= LIMIT ? JSON.parse(Buffer.concat(chunks).toString('utf8')) : undefined;
  } catch { return undefined; }
}
async function readSnapshot(path) {
  if (!path) return undefined;
  try {
    const info = await stat(path);
    if (!info.isFile() || info.size > LIMIT) return undefined;
    // Bound the actual read as well: the file can grow after stat().
    return await readJson(createReadStream(path, { start: 0, end: LIMIT }));
  } catch { return undefined; }
}
const [input, snapshot] = await Promise.all([readJson(process.stdin), readSnapshot(process.env.AUTOROUTER_STATUS_FILE)]);
const alive = pid => { try { process.kill(pid, 0); return true; } catch (error) { return error.code === 'EPERM'; } };
process.stdout.write(renderStatusLine(input, snapshot, {
  color: !Object.hasOwn(process.env, 'NO_COLOR') && process.env.TERM !== 'dumb', alive,
}) + '\n');
