#!/usr/bin/env node
import { writeFile } from 'node:fs/promises';
const locales = 'af am ar as az be bg bn bo br bs ca ceb cs cy da de dz ee el en eo es et eu fa fi fil fo fr fy ga gl gu ha he hi hr hu hy id ig is it ja ka kk kl km kn ko kok ku ky lb lo lt lv mk ml mn mr ms mt my nb ne nl nn no om or pa pl ps pt ro ru se si sk sl sq sr su sv sw ta te th tk to tr uk ur uz vi wae wo xh yi yo zh zu zz und en-u-kn-true en-u-kf-upper en-u-kf-lower es-u-co-trad de-u-co-phonebk zh-u-co-stroke'.split(' ');
const keys = 'a A aa aA Aa AA ab Ab aB b B c C ch Ch cH CH ci d D dd dz dzs e E f ff g gg gy h i I j J l L ll ly n N ng ny r R rh s S sh sz t T th ty u U v V w W x X y Y z Z zs _a -a .a :a /a a_b a-b a.b a:b a/b 2 10 01 4294967294 4294967295 model2 model10 _2 2_'.split(' ');
const rows = [];
for (const locale of locales) for (const reverse of [false, true]) {
  const ordered = reverse ? keys.toReversed() : keys;
  const content = ordered.map((model,index) => JSON.stringify({ schema_version: 2, event: 'decision', request_id: `r${index}`, timestamp: '2026-10-09T12:00:00.000Z', selected_model: model, requested_model: 'claude-opus-5-5' })).join('\n') + '\n';
  rows.push({ id: `history-locale-${rows.length}`, op: 'history_session', input: { bytes: [...Buffer.from(content)], locale, order: true, text: true }, source_tests: ['test/session-history.test.mjs'] });
}
const path = process.argv[2] ?? '/private/tmp/autorouter-history-locales.jsonl';
await writeFile(path, rows.map(row => JSON.stringify(row)).join('\n') + '\n');
console.log(JSON.stringify({ cases: rows.length, node: process.version, icu: process.versions.icu, cldr: process.versions.cldr, path }));
