// Local-only raw HTTP characterization of the frozen and native executables.
// Edge cases are retained even when a difference is a safer native rejection;
// no normalization turns a protocol change into a parity pass.
import { spawn } from 'node:child_process';
import net from 'node:net';
import { mkdtemp, mkdir, rm, writeFile, copyFile, chmod, readFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
const root=resolve(import.meta.dirname,'../..');
const args = process.argv.slice(2).filter(value => value !== '--single-write');
assert.ok(args.length <= 2, 'Usage: check-http-parser.mjs [REFERENCE] [CANDIDATE] [--single-write]');
const reference = resolve(args[0] ?? join(root, 'artifacts/rust-rewrite/reference'));
const originalCandidate = resolve(args[1] ?? join(root, 'rust/target/debug/claude-autorouter'));
await verifyBaseline(reference);
const scratch=await mkdtemp(join(tmpdir(),'autorouter-http-parser-'));
const candidate = join(scratch, 'candidate');
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const token='synthetic-local-parser-token';
const auth=`x-api-key: ${token}\r\n`;
const head=(fields,target='/health',version='1.1')=>`GET ${target} HTTP/${version}\r\nHost: localhost\r\nConnection: close\r\n${fields}\r\n`;
const cases=[];
const segmented = !process.argv.includes('--single-write');
for(const length of [8000,16000,16200,16300,16340,16384,16385,17000,32768])cases.push({id:`header-bytes-${length}`,raw:head(auth+`x-padding: ${'x'.repeat(length)}\r\n`)});
for(const count of [99,100,101,990,997,998,999,1000,1001,1900,1997,1998,1999,2000,2001,2100,3900,4096,8192,16300,16384])for(const late of [false,true])cases.push({id:`header-lines-${count}-auth-${late?'last':'first'}`,raw:head((late?'':auth)+'z:\r\n'.repeat(count)+(late?auth:''))});
for(const[ id, fields,target,version]of[
 ['folded-value',auth+'x-fold: first\r\n next\r\n'],['bad-local-first',`x-api-key: wrong\r\n${auth}`],['good-local-first',auth+'x-api-key: wrong\r\n'],
 ['bad-authorization-first',`authorization: wrong\r\nauthorization: Bearer ${token}\r\n`],['good-authorization-first',`authorization: Bearer ${token}\r\nauthorization: wrong\r\n`],
 ['duplicate-length-same',auth+'content-length: 0\r\ncontent-length: 0\r\n'],['duplicate-length-different',auth+'content-length: 0\r\ncontent-length: 1\r\n'],
 ['length-and-chunked',auth+'content-length: 0\r\ntransfer-encoding: chunked\r\n'],['space-before-colon',`x-api-key : ${token}\r\n`],['nul-in-value',auth+'x-test: \0\r\n'],
 ['absolute-target',auth,'http://untrusted.example.invalid/health'],['scheme-relative-target',auth,'//untrusted.example.invalid/health'],['backslash-target',auth,'/test/../health'],
 ['upgrade',auth+'connection: upgrade\r\nupgrade: websocket\r\n'],['upgrade-unknown',auth+'connection: upgrade\r\nupgrade: synthetic\r\n'],['http10',auth,'/health','1.0'],['empty-origin',auth+'origin:\r\n'],['duplicate-origin',auth+'origin:\r\norigin: https://example.invalid\r\n'],
])cases.push({id,raw:head(fields,target,version)});
for(const amount of [16000,16300,16340,17000,500000,1000000])for(const position of ['leading','trailing'])cases.push({id:`ows-${position}-${amount}`,raw:head(auth+`x-padding: ${position==='leading'?' '.repeat(amount):''}x${position==='trailing'?' '.repeat(amount):''}\r\n`)});
for(const method of ['FOO','OPTIONS','PATCH','CONNECT','SEARCH','M-SEARCH','GET'.toLowerCase()])cases.push({id:`method-${method}`,raw:head(auth).replace(/^GET /,`${method} `)});
for(const amount of [16000,16300,16340,17000])cases.push({id:`target-length-${amount}`,raw:head(auth,'/'+ 'a'.repeat(amount))});
for(const [id,line]of [['request-multiple-spaces','GET  /health  HTTP/1.1'],['request-tabs','GET\t/health\tHTTP/1.1'],['leading-crlf','\r\nGET /health HTTP/1.1'],['extra-version-space','GET /health HTTP/1.1 ']])cases.push({id,raw:head(auth).replace('GET /health HTTP/1.1',line)});
cases.push({id:'lf-only',raw:head(auth).replaceAll('\r\n','\n')});
cases.push({id:'crlf-in-header-value',raw:head(auth+'x-fold: a\r\n\tsecond\r\n')});
async function rawRequest(address,raw){return new Promise(resolve=>{
 const socket=net.connect(address.port,'127.0.0.1');let bytes=Buffer.alloc(0);let done=false;const finish=(reason)=>{if(done)return;done=true;socket.destroy();const text=bytes.toString('latin1');resolve({status:Number(text.match(/^HTTP\/1\.[01] (\d+)/)?.[1])||null,...(!bytes.length?{closed:reason}:{} )});};socket.setTimeout(1500,()=>finish('timeout'));socket.on('connect',async()=>{if(!segmented||raw.length<=65536){socket.write(raw);return;}for(let offset=0;offset<raw.length&&!done;offset+=4096){socket.write(raw.slice(offset,offset+4096));await delay(1);}});socket.on('data',chunk=>{bytes=Buffer.concat([bytes,chunk]);if(bytes.includes('\r\n\r\n'))finish('response')});socket.on('error',()=>finish('error'));socket.on('end',()=>finish('end'));
});}
async function run(name){const directory=join(scratch,name);await mkdir(directory);const child=spawn(name==='node'?process.execPath:candidate,name==='node'?[join(reference,'bin/autorouter.mjs'),'serve']:['serve'],{cwd:directory,env:{HOME:directory,XDG_CONFIG_HOME:directory,PATH:directory,AUTOROUTER_PORT:'0',AUTOROUTER_EVALUATOR:'jev',TYPESAFE_API_KEY:'synthetic-evaluator',ANTHROPIC_API_KEY:'synthetic-provider',AUTOROUTER_TOKEN:token},stdio:['ignore','ignore','pipe']});let stderr='';child.stderr.on('data',chunk=>{stderr+=chunk});const ended=new Promise((ok,fail)=>{child.on('error',fail);child.on('close',ok)});const watchdog=setTimeout(()=>child.kill('SIGKILL'),15000);try{let port;for(let i=0;i<1000;i++){port=Number(stderr.match(/AutoRouter listening on http:\/\/127\.0\.0\.1:(\d+)/)?.[1]);if(port)break;if(child.exitCode!==null)break;await delay(5)}if(!port)throw Error('Synthetic parser gateway failed startup');const results=[];for(const row of cases)results.push(await rawRequest({port},row.raw));return results;}finally{child.kill('SIGTERM');await ended;clearTimeout(watchdog)}}
try{await copyFile(originalCandidate,candidate);await chmod(candidate,0o700);const identity={reference_integrity_verified:true,node:process.version,candidate_sha256:digest(await readFile(candidate)),node_sha256:digest(await readFile(process.execPath)),candidate_execution:'isolated immutable byte-identical copy'};const node=await run('node');const rust=await run('rust');const report={schema_version:1,kind:'raw_http_parser_differential',identity,transport:{large_requests:segmented?'4096-byte chunks with 1ms pacing':'single write',normalization:'none'},cases:cases.length,results:cases.map((row,i)=>({id:row.id,node:node[i],rust:rust[i],matched:JSON.stringify(node[i])===JSON.stringify(rust[i])}))};report.passed=report.results.every(row=>row.matched);await writeFile(join(root,segmented?'artifacts/rust-rewrite/parity-http-parser.json':'artifacts/rust-rewrite/parity-http-parser-single-write.json'),JSON.stringify(report,null,2)+'\n');console.log(JSON.stringify({cases:cases.length,passed:report.passed,mismatches:report.results.filter(row=>!row.matched)},null,2));if(!report.passed)process.exitCode=1;}finally{await rm(scratch,{recursive:true,force:true})}
