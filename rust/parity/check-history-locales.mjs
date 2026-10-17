#!/usr/bin/env node
// Actual CLI locale selection with isolated files; no provider or startup calls.
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createHash } from 'node:crypto';
const root = resolve(import.meta.dirname, '../..');
const candidate = resolve(process.argv[2] ?? join(root, 'rust/target/debug/claude-autorouter'));
const output = resolve(process.argv[3] ?? join(root, 'artifacts/rust-rewrite/parity-history-locale-cli.json'));
const directory = await mkdtemp(join(tmpdir(), 'autorouter-history-locales-'));
const id = 'autorouter-session-fixture';
const keys = ['a','A','_a','-a','a_b','ab','a-b','i','I','h','ch','g','gy','z','Z','2','10','model2','model10'];
const content = keys.map((model,index) => JSON.stringify({ schema_version: 2, event: 'decision', timestamp: '2026-10-09T12:00:00.000Z', request_id: `r${index}`, requested_model: 'claude-opus-5-5', selected_model: model })).join('\n') + '\n';
const environments = [{}, ...['C','POSIX','C.UTF-8','en_US.UTF-8','cs_CZ.UTF-8','da_DK.UTF-8','tr_TR.UTF-8','th_TH.UTF-8','hu_HU.UTF-8','lt_LT.UTF-8','sv_SE.UTF-8','no_NO@nynorsk','en_US.UTF-8@euro','en-US-u-kn-true','invalid@locale'].map(LANG => ({LANG})),
  {LANG:'cs_CZ.UTF-8',LC_ALL:'da_DK.UTF-8'}, {LANG:'cs_CZ.UTF-8',LC_MESSAGES:'tr_TR.UTF-8'}, {LANG:'cs_CZ.UTF-8',LC_MESSAGES:'tr_TR.UTF-8',LC_ALL:'th_TH.UTF-8'}, {LANG:'cs_CZ.UTF-8',LC_ALL:''}, {LANG:'cs_CZ.UTF-8',LC_MESSAGES:''}, {LANG:'en_US.UTF-8',LC_COLLATE:'cs_CZ.UTF-8'}];
const failures = [];
try {
  await mkdir(join(directory, 'history'), {mode:0o700});
  await writeFile(join(directory, 'config.json'), '{}', {mode:0o600});
  await writeFile(join(directory, 'history', `${id}.jsonl`), content, {mode:0o600});
  for (const [index,locale] of environments.entries()) {
    const env = {HOME:directory,PATH:directory,AUTOROUTER_CONFIG:join(directory,'config.json'),AUTOROUTER_SESSION_LOG_DIR:join(directory,'history'),...locale};
    const args = ['sessions','show',id];
    const expected = spawnSync(process.execPath,[join(root,'artifacts/rust-rewrite/reference/bin/autorouter.mjs'),...args], {cwd:directory,env,encoding:'utf8',timeout:10000,maxBuffer:1024*1024});
    const actual = spawnSync(candidate,args,{cwd:directory,env,encoding:'utf8',timeout:10000,maxBuffer:1024*1024});
    if (expected.status !== 0 || actual.status !== 0 || expected.stdout !== actual.stdout || expected.stderr !== actual.stderr) failures.push({index,locale,expected_status:expected.status,actual_status:actual.status,stdout_equal:expected.stdout===actual.stdout,stderr_equal:expected.stderr===actual.stderr,expected_selected:expected.stdout.split('\n').find(line=>line.startsWith('Selected models:')),actual_selected:actual.stdout.split('\n').find(line=>line.startsWith('Selected models:'))});
  }
  const report={schema_version:1,kind:'native_history_locale_cli_differential',passed:failures.length===0,cases:environments.length,matched:environments.length-failures.length,fixture_sha256:createHash('sha256').update(content).digest('hex'),candidate_sha256:createHash('sha256').update(await readFile(candidate)).digest('hex'),node:process.version,icu:process.versions.icu,cldr:process.versions.cldr,native_icu4x:'2.3.1',native_cldr:'48.2.1',failures,scope:'Exact terminal output and exit for synthetic history with explicit locale environment; no provider/model evidence.'};
  await writeFile(output,JSON.stringify(report,null,2)+'\n',{flag:'wx'}); console.log(JSON.stringify(report,null,2)); if(failures.length)process.exitCode=1;
} finally {await rm(directory,{recursive:true,force:true});}
