// Actual system localhost resolution by the installed native client and frozen Node.
// No external DNS/provider/model traffic. This does not qualify arbitrary DNS deployments.
import assert from 'node:assert/strict';
import http from 'node:http';
import { once } from 'node:events';
import { lookup } from 'node:dns/promises';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { transportHarness } from './transport-harness.mjs';
const { root, token, identity, gateway, watch, close, cleanup } = await transportHarness('autorouter-localhost-audit-');
const body = 'synthetic-localhost-client-body';
const deadline = setTimeout(() => { void cleanup().finally(() => process.exit(1)); }, 30000);
function request(port) {
  return new Promise((resolve, reject) => {
    const req = http.get({ host: '127.0.0.1', port, path: '/v1/models/synthetic-localhost', headers: { 'x-api-key': token } }, res => {
      const chunks = []; let bytes = 0;
      res.on('data', chunk => { bytes += chunk.length; if (bytes > 4096) req.destroy(Error('Synthetic response bound')); else chunks.push(chunk); });
      res.on('end', () => resolve({ status: res.statusCode, body: Buffer.concat(chunks).toString('utf8') }));
      res.on('error', reject);
    });
    req.setTimeout(4000, () => req.destroy(Error('Synthetic request deadline')));
    req.on('error', reject);
  });
}
try {
  const resolved = await lookup('localhost', { all: true });
  assert.ok(resolved.length && resolved.every(row => row.address === '::1' || row.address.startsWith('127.')));
  const rows = [];
  for (const family of ['ipv4-only', 'ipv6-only', 'dual-loopback']) {
    const calls = []; const servers = [];
    const handler = (req, res) => { assert.ok(calls.length < 8); calls.push(req.url); res.end(body); };
    const first = watch(http.createServer(handler)); servers.push(first);
    const host = family === 'ipv6-only' ? '::1' : '127.0.0.1';
    first.listen({ host, port: 0, ipv6Only: true }); await once(first, 'listening');
    const port = first.address().port;
    if (family === 'dual-loopback') {
      const second = watch(http.createServer(handler)); servers.push(second);
      second.listen({ host: '::1', port, ipv6Only: true }); await once(second, 'listening');
    }
    const row = { family };
    try {
      for (const engine of ['node', 'rust']) {
        const before = calls.length;
        const child = await gateway(engine, `http://localhost:${port}`);
        try {
          row[engine] = await request(child.port);
          assert.deepEqual(row[engine], { status: 200, body });
          assert.deepEqual(calls.slice(before), ['/v1/models/synthetic-localhost']);
        } finally { await child.stop(); }
      }
    } finally {
      for (const server of servers) { server.closeAllConnections(); await close(server); }
    }
    assert.deepEqual(row.node, row.rust); rows.push(row);
  }
  const report = { schema_version: 1, kind: 'synthetic_system_localhost_transport', passed: true, identity, resolved, rows,
    scope: 'Actual native/Node localhost resolver and IPv4/IPv6 connection fallback on this OS/libc only; no remote DNS server/search/CNAME/NXDOMAIN/corporate qualification.' };
  await writeFile(join(root, 'artifacts/rust-rewrite/parity-localhost-transport.json'), JSON.stringify(report, null, 2) + '\n', { flag: 'wx' });
  console.log(JSON.stringify({ passed: true, cases: rows.length, candidate_sha256: identity.candidate_sha256 }));
} finally { clearTimeout(deadline); await cleanup(); }
