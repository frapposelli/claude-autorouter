#!/usr/bin/env node
// Characterizes native JSON semantics against JSON.parse/JSON.stringify.
import { mkdir, writeFile } from 'node:fs/promises';

let exhaustive = false;
let destination;
const args = process.argv.slice(2);
for (let index = 0; index < args.length; index++) {
  if (args[index] === '--exhaustive') exhaustive = true;
  else if (args[index] === '--output' && args[index + 1] && !destination) destination = args[++index];
  else throw new Error('Usage: node rust/parity/generate-json.mjs [--exhaustive] [--output PATH]');
}

const cases = [];
const add = (id, raw) => cases.push({ id, op: 'js_json', input: { bytes: [...Buffer.from(raw)] }, source_tests: ['rust/crates/autorouter-core/src/js_json.rs'] });
const invalid = ['', '[1,]', '{"a":1,}', '01', '-01', '-', '+1', '1.', '.1', '1e+', 'true false', 'NaN', 'Infinity', '\ufeffnull', '"\\x20"', '"a\nb"', '[', '{', '{"a"}', '{1:2}', 'null\u00a0'];
for (const [index, value] of invalid.entries()) add(`json-invalid-${index}`, value);
const named = {
  ordering: '{"b":1,"2":2,"01":3,"0":4,"4294967295":5,"4294967294":6,"1":7,"b":8,"__proto__":9}',
  duplicate_escaped: '{"a":1,"\\u0061":2,"x":{"old":[1,2]},"x":3}',
  lone_key: '{"\\ud800":1,"\\udc00":2,"\\ud800":3}',
  numeric: '[-0,1e21,1e20,1e-6,1e-7,9007199254740993,1e400,-1e400,1e-400]',
  unicode: '["é","😀","\\ud800","\\udc00","\\ud83d\\ude00","\\u0000","\\b\\f\\n\\r\\t\\/\\\\\\\""]',
  whitespace: ' \t\r\n{"a":[true,false,null,0]} \t\r\n',
  deep_array: '['.repeat(1024) + '0' + ']'.repeat(1024),
  deep_object: '{"unknown":'.repeat(1024) + '0' + '}'.repeat(1024),
};
for (const [id, value] of Object.entries(named)) add(`json-${id}`, value);
for (const number of ['-0', '1e400', '-1e400', '1e-400', '9007199254740993', '1000000000000000128', '2.2250738585072014e-308', '5e-324', '1.7976931348623157e308']) add(`json-number-${cases.length}`, number);
for (const [index, bytes] of [[34, 255, 34], [34, 237, 160, 128, 34], [34, 240, 159, 34], [123, 34, 120, 34, 58, 34, 128, 34, 125]].entries()) add(`json-utf8-${index}`, Buffer.from(bytes));
const units = exhaustive ? Array.from({ length: 65536 }, (_, index) => index)
  : [...Array.from({ length: 128 }, (_, index) => index), 0x80, 0x800, 0x2028, 0x2029, 0x3000, 0xd7ff, 0xd800, 0xd801, 0xdbff, 0xdc00, 0xdc01, 0xdfff, 0xe000, 0xfeff, 0xfffd, 0xffff];
for (const unit of units) add(`json-utf16-${unit.toString(16).padStart(4, '0')}`, `"\\u${unit.toString(16).padStart(4, '0')}"`);

let state = 0x61c88647;
const random = () => { state ^= state << 13; state ^= state >>> 17; state ^= state << 5; return state >>> 0; };
const randomDoubles = exhaustive ? 10000 : 256;
for (let index = 0; index < randomDoubles; index++) {
  const bytes = Buffer.alloc(8);
  bytes.writeUInt32BE(random(), 0); bytes.writeUInt32BE(random(), 4);
  const value = bytes.readDoubleBE();
  if (Number.isFinite(value)) add(`json-random-double-${index}`, value.toString());
}
const output = destination ?? new URL(exhaustive ? '../../artifacts/rust-rewrite/json-exhaustive.jsonl' : 'cases/json.jsonl', import.meta.url);
if (!destination) await mkdir(new URL('.', output), { recursive: true });
await writeFile(output, cases.map(value => JSON.stringify(value)).join('\n') + '\n');
console.log(JSON.stringify({ cases: cases.length, seed: '0x61c88647', random_doubles: randomDoubles, exhaustive }));
