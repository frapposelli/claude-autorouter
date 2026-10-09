// Synthetic test executable only. This never selects a production transport.
import assert from 'node:assert/strict';
import { runTlsOptions } from './tls-options-matrix.mjs';
assert.equal(process.argv.length, 4, 'Usage: check-openssl-transport-spike.mjs <frozen-reference-directory> <runtime-lib-test-executable>');
await runTlsOptions({ spike: true });
