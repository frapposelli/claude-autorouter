// Synthetic local provider parser and request-pipeline characterization.
// This is acceptance/concurrency evidence, never a hardware performance result.
import http from 'node:http';
import net from 'node:net';
import { once } from 'node:events';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { transportHarness } from './transport-harness.mjs';
const { root, token, identity, gateway, watch, close, cleanup } = await transportHarness('autorouter-upstream-audit-');
const body='PAYLOAD';
const response=fields=>`HTTP/1.1 200 OK\r\nConnection: close\r\n${fields}\r\n${body}`;
const cases=[];
for(const count of [99,100,101,997,998,999,1000,1100,16300])cases.push({id:`response-header-lines-${count}`,raw:response(`content-length: ${body.length}\r\n`+'z:\r\n'.repeat(count)+'x-visible: final\r\n')});
for(const length of [16000,16300,16340,17000,32768])cases.push({id:`response-header-bytes-${length}`,raw:response(`content-length: ${body.length}\r\nx-padding: ${'x'.repeat(length)}\r\n`)});
for(const [id,fields] of [
 ['duplicate-length-same',`content-length: ${body.length}\r\ncontent-length: ${body.length}\r\n`],
 ['duplicate-length-different',`content-length: ${body.length}\r\ncontent-length: 1\r\n`],
 ['length-and-chunked',`content-length: ${body.length}\r\ntransfer-encoding: chunked\r\n`],
 ['chunked-and-length',`transfer-encoding: chunked\r\ncontent-length: ${body.length}\r\n`],
 ['duplicate-chunked',`transfer-encoding: chunked\r\ntransfer-encoding: chunked\r\n`],
 ['chunked-not-final',`transfer-encoding: chunked, gzip\r\n`],
 ['chunked-twice',`transfer-encoding: chunked, chunked\r\n`],
 ['gzip-then-chunked',`transfer-encoding: gzip, chunked\r\n`],
 ['folded-value',`content-length: ${body.length}\r\nx-test: first\r\n second\r\n`],
])cases.push({id,raw:response(fields).replace(body,fields.includes('chunked')?`7\r\n${body}\r\n0\r\n\r\n`:body)});
cases.push({id:'lf-only-response',raw:response(`content-length: ${body.length}\r\n`).replaceAll('\r\n','\n')});
cases.push({id:'informational-then-final',raw:'HTTP/1.1 100 Continue\r\nx-info: synthetic\r\n\r\n'+response(`content-length: ${body.length}\r\n`)});
for (const count of [99,100,101,1000,16300]) cases.push({id:`response-trailer-lines-${count}`,raw:`HTTP/1.1 200 OK\r\nConnection: close\r\ntransfer-encoding: chunked\r\n\r\n7\r\n${body}\r\n0\r\n${'z:\r\n'.repeat(count)}\r\n`});
for (const length of [16000,16370,16384,17000]) cases.push({id:`response-trailer-bytes-${length}`,raw:`HTTP/1.1 200 OK\r\nConnection: close\r\ntransfer-encoding: chunked\r\n\r\n7\r\n${body}\r\n0\r\nx:${'x'.repeat(length)}\r\n\r\n`});
for (const [id,extension] of [['valid-extension','synthetic=value'],['quoted-extension','synthetic="value with spaces"'],['invalid-extension-name','synthetic name=value'],['invalid-extension-value','synthetic=unquoted space']]) cases.push({id,raw:`HTTP/1.1 200 OK\r\nConnection: close\r\ntransfer-encoding: chunked\r\n\r\n7;${extension}\r\n${body}\r\n0\r\n\r\n`});
cases.push({id:'extensions-bound-per-chunk',raw:`HTTP/1.1 200 OK\r\nConnection: close\r\ntransfer-encoding: chunked\r\n\r\n4;x=${'a'.repeat(10000)}\r\nPAYL\r\n3;x=${'a'.repeat(10000)}\r\nOAD\r\n0\r\n\r\n`});
cases.push({id:'provider-trailer-is-consumed-not-forwarded',raw:`HTTP/1.1 200 OK\r\nConnection: close\r\ntransfer-encoding: chunked\r\ntrailer: x-provider-trailer\r\n\r\n7\r\n${body}\r\n0\r\nx-provider-trailer: synthetic\r\n\r\n`});
cases.push({id:'malformed-trailer-after-payload',raw:`HTTP/1.1 200 OK\r\nConnection: close\r\ntransfer-encoding: chunked\r\n\r\n7\r\n${body}\r\n0\r\ninvalid trailer without colon\r\n\r\n`});
for (const [index, extension] of ['', ';', 'x', 'x=', ' x = y ', 'x=""', 'x="a\\"b"', 'x="a\\\r"', 'x="\t"', 'x="\x85"', 'x=\x85', 'x=y;z=w', 'x ; z', 'x= y', 'x="value"junk', 'x="value" junk', 'x="a\\\x85"', 'x=y;', 'x=y; ', 'x="unterminated', 'x=y\t;z=w', 'x=;z=w', 'x=;', 'x= ', 'x=\t', 'x="y" ', 'x =y', 'x; z=w', 'x=y ;z=w'].entries()) cases.push({id:`extension-grammar-${index}`,raw:`HTTP/1.1 200 OK\r\nConnection: close\r\ntransfer-encoding: chunked\r\n\r\n7;${extension}\r\n${body}\r\n0\r\n\r\n`});
for (const length of [16382,16383,16384,16385]) for (const quoted of [false,true]) cases.push({id:`extension-bound-${length}-${quoted}`,raw:`HTTP/1.1 200 OK\r\nConnection: close\r\ntransfer-encoding: chunked\r\n\r\n7;x=${quoted?'"':''}${'a'.repeat(length)}${quoted?'"':''}\r\n${body}\r\n0\r\n\r\n`});
async function request(port,path) {
 return new Promise(resolve=>{
  const request=http.get({host:'127.0.0.1',port,path,maxHeaderSize:1048576,headers:{'x-api-key':token,te:'trailers'}},res=>{
   const chunks=[];res.on('data',chunk=>chunks.push(chunk));res.on('end',()=>resolve({status:res.statusCode,visible:res.headers['x-visible']??null,body:Buffer.concat(chunks).toString(),trailers:res.trailers}));res.on('error',()=>resolve({status:res.statusCode,error:'body_error'}));
  });request.setTimeout(2000,()=>request.destroy());request.on('error',()=>resolve({error:'request_error'}));
 });
}
async function parser(name) {
 const sockets=new Set();let index=0;
 const provider=watch(net.createServer(socket=>{sockets.add(socket);socket.on('close',()=>sockets.delete(socket));socket.on('error',()=>{});let received='';socket.on('data',data=>{received+=data.toString('latin1');if(received.includes('\r\n\r\n')){socket.removeAllListeners('data');socket.end(cases[index].raw);}})}));
 provider.listen(0,'127.0.0.1');await once(provider,'listening');const child=await gateway(name,`http://127.0.0.1:${provider.address().port}`);
 try {const results=[];for(index=0;index<cases.length;index++)results.push(await request(child.port,`/v1/models/synthetic-${index}`));return results;}
 finally {await child.stop();for(const socket of sockets)socket.destroy();await close(provider);}
}
async function pipeline(name) {
 const seen=[];let first;let markFirst;let markSecond;const firstReady=new Promise(ok=>markFirst=ok);const secondReady=new Promise(ok=>markSecond=ok);
 const provider=watch(http.createServer((req,res)=>{seen.push(req.url);if(req.url==='/v1/models/first'){first=res;markFirst();}else{res.end('second');markSecond();}}));
 provider.listen(0,'127.0.0.1');await once(provider,'listening');const child=await gateway(name,`http://127.0.0.1:${provider.address().port}`);const socket=net.connect(child.port,'127.0.0.1');socket.on('error',()=>{});socket.on('data',()=>{});
 try {await once(socket,'connect');socket.write(`GET /v1/models/first HTTP/1.1\r\nHost: localhost\r\nx-api-key: ${token}\r\n\r\nGET /v1/models/second HTTP/1.1\r\nHost: localhost\r\nx-api-key: ${token}\r\nConnection: close\r\n\r\n`);await Promise.race([firstReady,delay(2000).then(()=>{throw Error('First mock upstream request missing')})]);await Promise.race([secondReady,delay(150)]);const before=seen.slice();first.end('first');await Promise.race([secondReady,delay(2000)]);return {before_first_response_release:before,after_release:seen.slice()};}
 finally {socket.destroy();await child.stop();provider.closeAllConnections();await close(provider);}
}
try {
 const node=await parser('node');const rust=await parser('rust');const nodePipeline=await pipeline('node');const rustPipeline=await pipeline('rust');
 const report={schema_version:1,kind:'upstream_acceptance_and_pipeline_characterization',source_node:process.version,identity,corpus_sha256:(await import('node:crypto')).createHash('sha256').update(JSON.stringify(cases)).digest('hex'),cases:cases.map((row,index)=>({id:row.id,node:node[index],rust:rust[index],matched:JSON.stringify(node[index])===JSON.stringify(rust[index])})),pipeline:{node:nodePipeline,rust:rustPipeline,matched:JSON.stringify(nodePipeline)===JSON.stringify(rustPipeline)}};
 await writeFile(join(root,'artifacts/rust-rewrite/parity-upstream-transport.json'),JSON.stringify(report,null,2)+'\n');console.log(JSON.stringify({cases:cases.length,mismatches:report.cases.filter(row=>!row.matched),pipeline:report.pipeline},null,2));if(report.cases.some(row=>!row.matched)||!report.pipeline.matched)process.exitCode=1;
} finally {await cleanup();}
