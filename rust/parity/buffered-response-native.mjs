// Synthetic native fixture driven by the same declared wire stimuli as reference.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { readFile, writeFile } from 'node:fs/promises';
import net from 'node:net';
import tls from 'node:tls';
import { setImmediate as immediate, setTimeout as delay } from 'node:timers/promises';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';
import { cases, limits } from './buffered-response-cases.mjs';
assert.equal(process.version, 'v22.14.0');
assert.equal(process.argv.length, 6);
const [reference, caseId, output, nativeBinary] = process.argv.slice(2);
const spec = cases.find(row => row.id === caseId); assert.ok(spec);
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const builtinNames = ['_http_agent', '_http_client', '_http_common', '_http_incoming', '_http_server', 'internal/streams/readable', 'internal/streams/pipeline', 'net', 'https'];
const identity = { baseline: await verifyBaseline(reference), node: process.version, openssl: process.versions.openssl, node_sha256: hash(await readFile(process.execPath)), native_sha256: hash(await readFile(nativeBinary)), builtins: Object.fromEntries(builtinNames.map(name => [name, hash(process.binding('natives')[name])])) };
const trace = [], sockets = new Set(), servers = [], cleanupFailures = [], observations = [];
const stop = new AbortController();
let ordinal = 0, phase = 'scenario', failure, scheduleDone = false, interrupted, accepted = 0;
let client, providerSocket, providerEnded = false, consumerReleased = false, nativeChild, nativeReaped, nativeExit, nativeCleanup, control, state, pendingReply, terminalSeen = 0, logSeen = 0;
let diagnosticBytes = 0, diagnostic = '', workPromise;
const sanitize = error => ({ name: String(error?.name ?? 'Error').slice(0, 40), message: String(error?.message ?? error).slice(0, 200) });
let rejectObservation; const observedFailure = new Promise((_,reject) => { rejectObservation = reject; }); observedFailure.catch(() => {});
const fail = error => { if (phase === 'cleanup') { if (cleanupFailures.length < 16) cleanupFailures.push(sanitize(error)); } else if (!stop.signal.aborted) { stop.abort(); rejectObservation(error); } };
const guard = fn => (...args) => { try { return fn(...args); } catch (error) { fail(error); } };
const emit = (event, fields = {}) => { assert.ok(trace.length < limits.events); trace.push({ ordinal:ordinal++, phase, event, ...fields }); };
const seen = event => trace.some(row => row.phase === 'scenario' && row.event === event);
const code = value => typeof value === 'string' && /^[A-Z0-9_]{1,60}$/.test(value) ? value : 'other';
async function bounded(promise, ms, label) { let timer; try { return await Promise.race([promise,new Promise((_,reject) => { timer = setTimeout(() => reject(Error(label)),ms); })]); } finally { clearTimeout(timer); } }
async function wait(predicate,label,ms=limits.barrier_ms) { const deadline=Date.now()+ms; for (;;) { if(stop.signal.aborted) throw Error('Scenario cancelled'); if(await predicate())return; assert.ok(Date.now()<deadline,label); await delay(2,undefined,{signal:stop.signal}); } }
function track(socket,side) { assert.ok(sockets.size < limits.connections); sockets.add(socket); socket.on('end',guard(()=>emit(`${side}.end`))); socket.on('error',guard(error=>emit(`${side}.error`,{code:code(error.code)}))); socket.on('close',guard(hadError=>{sockets.delete(socket);emit(`${side}.close`,{had_error:hadError});}));return socket; }
const onSignal = name => { interrupted=name; fail(Error(`Native fixture interrupted by ${name}`)); };
const interrupt=()=>onSignal('SIGINT'),terminate=()=>onSignal('SIGTERM');process.once('SIGINT',interrupt);process.once('SIGTERM',terminate);
// Incremental fixture-only response accounting. It never retains a full response.
function wireObserver() {
  let input = Buffer.alloc(0), headers, mode, needed = 0, total = 0, done = false, bodyBytes = 0, trailerBytes = 0;
  const digest = createHash('sha256');
  const data = bytes => { bodyBytes += bytes.length; assert.ok(bodyBytes <= 2 * 1024 * 1024, 'Synthetic body count bound'); digest.update(bytes); };
  const push = bytes => {
    total += bytes.length; assert.ok(total <= 3 * 1024 * 1024, 'Synthetic wire count bound');
    input = Buffer.concat([input, bytes]); assert.ok(input.length <= 262144, 'Incremental wire scratch bound');
    for (;;) {
      if (!headers) {
        const end = input.indexOf('\r\n\r\n'); if (end < 0) { assert.ok(input.length <= 16384); return; }
        headers = input.toString('latin1', 0, end).split('\r\n'); input = input.subarray(end + 4);
        assert.match(headers[0], /^HTTP\/1\.[01] [0-9]{3}/);
        const chunked = headers.some(line => /^transfer-encoding:\s*chunked$/i.test(line));
        const length = headers.find(line => /^content-length:/i.test(line));
        mode = chunked ? 'chunk-size' : length ? 'length' : 'close';
        if (length) needed = Number(length.split(':')[1].trim());
        if (/^HTTP\/1\.[01] (204|304)\b/.test(headers[0])) { mode = 'length'; needed = 0; }
      }
      if (mode === 'close') { data(input); input = Buffer.alloc(0); return; }
      if (mode === 'length') { const take = Math.min(needed, input.length); data(input.subarray(0, take)); needed -= take; input = input.subarray(take); if (needed === 0) { done = true; mode = 'done'; } else return; }
      if (mode === 'chunk-size') {
        const end = input.indexOf('\r\n'); if (end < 0) return;
        const size = input.toString('ascii', 0, end).split(';')[0]; assert.match(size, /^[0-9a-f]+$/i); needed = Number.parseInt(size, 16); input = input.subarray(end + 2); mode = needed === 0 ? 'trailers' : 'chunk-data';
      }
      if (mode === 'chunk-data') { const take = Math.min(needed, input.length); data(input.subarray(0, take)); needed -= take; input = input.subarray(take); if (needed > 0) return; mode = 'chunk-crlf'; }
      if (mode === 'chunk-crlf') { if (input.length < 2) return; assert.equal(input.toString('ascii', 0, 2), '\r\n'); input = input.subarray(2); mode = 'chunk-size'; continue; }
      if (mode === 'trailers') { const end = input.indexOf('\r\n'); if (end < 0) return; trailerBytes += end; input = input.subarray(end + 2); if (end === 0) { done = true; mode = 'done'; } else continue; }
      if (mode === 'done') { assert.equal(input.length, 0, 'One downstream response only'); return; }
      if (!input.length) return;
    }
  };
  return { push, bytes: () => bodyBytes, snapshot: () => ({ wire_bytes: total, body_bytes: bodyBytes, body_sha256: digest.copy().digest('hex'), complete_framing: done, trailer_bytes: trailerBytes, status: headers ? Number(headers[0].split(' ')[1]) : null, pending_wire_bytes: input.length }) };
}
const wire = wireObserver();
async function payload(socket, bytes) {
  const unit = spec.writes === 'tiny' ? 17 : Math.max(bytes, 1);
  for (let offset = 0; offset < bytes; offset += unit) {
    if (stop.signal.aborted) throw Error('Provider write cancelled');
    const value = Buffer.alloc(Math.min(unit, bytes - offset), 120);
    if (!socket.write(value)) await bounded(once(socket, 'drain'), limits.barrier_ms, 'Provider drain');
    if (spec.writes === 'tiny' && offset % 1088 === 0) await immediate(undefined, { signal: stop.signal });
  }
}
async function stimulus() {
  const short = spec.framing === 'short-length', chunked = !['short-length', 'exact-length'].includes(spec.framing);
  providerSocket.write(`HTTP/1.1 ${spec.response_status ?? 200} ${spec.response_status ? 'Synthetic' : 'OK'}\r\n${chunked ? 'transfer-encoding: chunked' : `content-length: ${spec.declared_length}`}\r\n\r\n`);
  if (!chunked) await payload(providerSocket, spec.bytes);
  else if (spec.framing === 'invalid-chunk-length') providerSocket.write('ZZ\r\n');
  else if (spec.framing === 'truncated-chunk') { providerSocket.write('10\r\n'); await payload(providerSocket, spec.bytes); }
  else {
    if (spec.bytes) { providerSocket.write(`${spec.bytes.toString(16)}\r\n`); await payload(providerSocket, spec.bytes); providerSocket.write('\r\n'); }
    if (spec.framing === 'chunked-trailers') providerSocket.write(spec.trailer_lines ? `0\r\n${'x:\r\n'.repeat(spec.trailer_lines)}\r\n` : '0\r\nX-Synthetic-Trailer: exact\r\n\r\n');
    else if (spec.framing === 'truncated-trailers') providerSocket.write('0\r\nX-Synthetic-Trailer: unfinished');
    else if (spec.framing === 'invalid-trailers') providerSocket.write('0\r\nInvalid Trailer\r\n\r\n');
    else assert.equal(spec.framing, 'missing-final-chunk');
  }
  return short;
}
function update(value) {
  state = value;
  for (const row of (value.terminal ?? []).slice(terminalSeen)) { const { event, ...fields } = row; emit(`native.${event}`, fields); }
  terminalSeen = value.terminal?.length ?? terminalSeen;
  for (const row of (value.logs ?? []).slice(logSeen)) emit('gateway.log', { kind:row.event,status:row.status ?? null });
  logSeen = value.logs?.length ?? logSeen;
}
async function command(op) {
  assert.ok(control && !control.destroyed); assert.equal(pendingReply,undefined,'One owned control waiter');
  let waiter;
  const promise = new Promise((resolve,reject)=>{ waiter={resolve,reject};pendingReply=waiter; });
  try { control.write(`${JSON.stringify({op})}\n`); const value=await bounded(promise,limits.barrier_ms,`Native ${op} response`);update(value);return value; }
  finally { if(pendingReply===waiter)pendingReply=undefined; }
}
async function observe(predicate,label) { await wait(async()=>{await command('snapshot');return predicate(state);},label); }
function snapshot(label) { const row={label,buffers:state?.buffers,body_held:state?.body_held,terminal:state?.terminal,consumer_released:consumerReleased,...wire.snapshot()};assert.ok(observations.length<16);observations.push(row);emit('driver.snapshot',row);return row; }
async function work() {
  const onPeer=guard(socket=>{
    assert.equal(++accepted,1);providerSocket=track(socket,'provider');emit('provider.connected',{resumed:socket.isSessionReused?.()??false});
    let head=Buffer.alloc(0),received=false;
    socket.on('data',guard(chunk=>{assert.equal(received,false);head=Buffer.concat([head,chunk]);assert.ok(head.length<=limits.request_headers);if(!head.includes('\r\n\r\n'))return;assert.ok(head.toString('latin1').startsWith('GET /v1/models/session-0 HTTP/1.1\r\n'));received=true;head=Buffer.alloc(0);emit('provider.request');}));
  });
  const tlsOptions=spec.protocol==='HTTP'?undefined:{key:await readFile(process.env.AUTOROUTER_SYNTHETIC_TLS_KEY),cert:await readFile(process.env.AUTOROUTER_SYNTHETIC_TLS_CERT),minVersion:spec.protocol,maxVersion:spec.protocol};
  const provider=spec.protocol==='HTTP'?net.createServer({allowHalfOpen:true},onPeer):tls.createServer({...tlsOptions,allowHalfOpen:true},onPeer);
  servers.push(provider);provider.on('error',fail);provider.on('tlsClientError',guard(error=>emit('provider.tls-error',{code:code(error.code)})));provider.listen(0,'127.0.0.1');await bounded(once(provider,'listening'),limits.barrier_ms,'Provider listen');
  let resolveReady;const ready=new Promise(resolve=>{resolveReady=resolve;});
  const controlServer=net.createServer(socket=>{
    try {
      assert.equal(control,undefined);control=track(socket,'control');let bytes=Buffer.alloc(0);
      socket.on('data',guard(chunk=>{
        bytes=Buffer.concat([bytes,chunk]);assert.ok(bytes.length<=limits.control_bytes);
        for(;;){const end=bytes.indexOf(10);if(end<0)return;const value=JSON.parse(bytes.toString('utf8',0,end));bytes=bytes.subarray(end+1);
          if(value.port){resolveReady(value.port);continue;}
          if(value.cleanup){nativeCleanup=value;continue;}
          assert.ok(pendingReply,'Unsolicited control reply');const waiter=pendingReply;pendingReply=undefined;waiter.resolve(value);
        }
      }));
      socket.on('close',()=>{if(pendingReply){const waiter=pendingReply;pendingReply=undefined;waiter.reject(Error('Native control closed'));}});
    } catch(error){fail(error);}
  });
  servers.push(controlServer);controlServer.on('error',fail);controlServer.listen(0,'127.0.0.1');await bounded(once(controlServer,'listening'),limits.barrier_ms,'Control listen');
  assert.equal(stop.signal.aborted,false);
  const origin=`${spec.protocol==='HTTP'?'http':'https'}://127.0.0.1:${provider.address().port}`;
  nativeChild=spawn(nativeBinary,['--exact','tls_roots::openssl_spike::raw_pool::buffered::child::controlled_gateway_child','--nocapture','--test-threads=1'],{env:{...process.env,AUTOROUTER_SYNTHETIC_BUFFERED_CONTROL:`127.0.0.1:${controlServer.address().port}`,AUTOROUTER_EVALUATOR:'jev',TYPESAFE_API_KEY:'synthetic-evaluator',ANTHROPIC_API_KEY:'synthetic-provider',AUTOROUTER_TOKEN:'synthetic-intent-fixture-token',AUTOROUTER_UPSTREAM_URL:origin,AUTOROUTER_JEV_URL:`${origin}/v1/systemone`},stdio:['ignore','pipe','pipe']});
  nativeReaped=new Promise(resolve=>{nativeChild.once('error',error=>{fail(error);resolve();});nativeChild.once('close',(code,signal)=>{nativeExit={code,signal};resolve();});});
  for(const stream of [nativeChild.stdout,nativeChild.stderr])stream.on('data',guard(bytes=>{diagnosticBytes+=bytes.length;if(diagnosticBytes>limits.diagnostic_bytes){nativeChild.kill('SIGTERM');throw Error('Native diagnostic bound');}diagnostic+=bytes.toString('utf8');}));
  const port=await bounded(ready,limits.barrier_ms,'Native gateway ready');
  if(spec.schedule==='error-before-gateway-callback')await command('hold_response');
  if(spec.hold_socket_writes)await command('hold_writes');
  client=track(net.createConnection({host:'127.0.0.1',port,allowHalfOpen:true}),'client');
  let prefixSeen=false;client.on('data',guard(chunk=>{wire.push(chunk);if(!prefixSeen&&wire.bytes()){prefixSeen=true;emit('client.body-prefix',{bytes:wire.bytes()});}}));
  await bounded(once(client,'connect'),limits.barrier_ms,'Client connect');
  client.write('GET /v1/models/session-0 HTTP/1.1\r\nHost: synthetic\r\nX-Api-Key: synthetic-intent-fixture-token\r\n\r\n');
  await wait(()=>seen('provider.request'),'Provider request');await stimulus();
  if(spec.schedule==='error-before-gateway-callback'){
    await observe(value=>value.buffers.responses_ready===1,'Headers acquired before return');emit('driver.provider_end');providerEnded=true;providerSocket.end();
    await observe(value=>value.buffers.failed_sources===1,'Body failure before attachment');assert.equal(state.terminal.some(row=>row.event==='attached'),false);snapshot('body-error-before-callback');emit('driver.release_response_callback');await command('release_response');
  }else await observe(value=>value.body_held || value.buffers.failed_sources || value.buffers.clean_sources,'Body hold or independent decoder terminal');
  const release=async()=>{assert.equal(consumerReleased,false);consumerReleased=true;emit('driver.release_body');await command('release_body');};
  if(spec.schedule==='prefix-error'){await release();await wait(()=>wire.bytes()===spec.bytes,'Forwarded prefix');snapshot('prefix-forwarded');}
  if(spec.schedule==='cancel-before-provider-end'){emit('driver.reset_before_provider_end');client.resetAndDestroy();await observe(value=>value.terminal.some(row=>row.event==='failed'&&row.cause==='downstream'),'Downstream cancellation before end');}
  if(!providerEnded){emit('driver.provider_end');providerEnded=true;providerSocket.end();}
  if(spec.schedule.startsWith('saturated-')){
    await observe(value=>value.buffers.queued_bytes>=65536,'Actual native buffer saturation');assert.equal(state.buffers.failed_sources,0);assert.equal(state.buffers.consumer_polls,0);snapshot('saturated');
    if(spec.schedule==='saturated-cancel'){emit('driver.reset_while_held');client.resetAndDestroy();}else await release();
  }else if(spec.schedule==='hold-observe-release'){
    await observe(value=>value.buffers.clean_sources || value.buffers.failed_sources || value.buffers.queued_bytes>=65536,'Decoder terminal or saturation');snapshot('held-observation');
    if(!state.buffers.failed_sources)await release();
  }
  const clean=['exact-length','chunked-trailers'].includes(spec.framing);
  if(clean){
    await observe(value=>value.terminal.some(row=>row.event==='delivery_claimed'&&row.flushed),'Flushed native delivery');await wait(()=>wire.snapshot().complete_framing,'Wire framing complete');assert.equal(wire.bytes(),spec.bytes);assert.equal(wire.snapshot().body_sha256,hash(Buffer.alloc(spec.bytes,120)));assert.equal(wire.snapshot().trailer_bytes,0);
  }else{
    await wait(()=>seen('client.end')||seen('client.close'),'Independent downstream closure');await observe(value=>value.terminal.some(row=>row.event==='failed'),'Latched native failure');
    assert.equal(state.terminal.some(row=>row.event==='delivery_claimed'&&row.flushed),false);
    const firstCause=state.terminal.find(row=>row.event==='failed').cause;
    assert.equal(firstCause,['saturated-cancel','cancel-before-provider-end'].includes(spec.schedule)?'downstream':'upstream');
  }
  if(spec.schedule==='held-error'){assert.equal(consumerReleased,false);assert.equal(state.buffers.consumer_polls,0);}
  assert.ok(state.logs.some(row=>row.event==='upstream_response'&&row.status===(spec.response_status??200)));
  assert.ok(wire.snapshot().status===null||wire.snapshot().status===(spec.response_status??200));
  if(spec.observe_handoff){
    const failure=state.handoff_failures.find(row=>row.handoff.producer_epoch!==null);
    assert.ok(failure,'Request-specific producer failure snapshot');
    const failed=state.terminal.find(row=>row.event==='failed');
    assert.equal(failure.connection,failed.connection);assert.equal(failure.request,failed.request);
    if(spec.hold_socket_writes){
      const held=state.terminal.findIndex(row=>row.event==='write_held');
      assert.ok(held>=0&&held<state.terminal.findIndex(row=>row.event==='failed'),'Physical writer Pending before failure');
      assert.equal(state.writer.held,true);assert.ok(state.writer.blocked_calls>0);assert.equal(state.writer.written_bytes,0);
      assert.equal(failure.handoff.phase,'writer_pending');assert.equal(wire.bytes(),0);
    }else{
      assert.equal(state.writer.held,false);assert.equal(state.writer.blocked_calls,0);
      assert.equal(failure.handoff.submitted,spec.bytes,'All provider bytes admitted to encoder before error');
      assert.equal(failure.handoff.attempted,spec.bytes,'Writer attempt covers exact byte target before error');
      assert.equal(wire.bytes(),spec.bytes);
    }
  }
  snapshot('finished');scheduleDone=true;
}
try{workPromise=work();workPromise.catch(()=>{});await bounded(Promise.race([workPromise,observedFailure]),limits.scenario_ms,'Native scenario deadline');}catch(error){failure=sanitize(error);}
finally{
  phase='cleanup';stop.abort();
  if(control&&!control.destroyed){try{control.write('{"op":"stop"}\n');}catch(error){cleanupFailures.push(sanitize(error));}}
  if(nativeChild&&nativeReaped){
    try{await bounded(nativeReaped,limits.cleanup_ms,'Native graceful cleanup');}catch(error){cleanupFailures.push(sanitize(error));nativeChild.kill('SIGTERM');try{await bounded(nativeReaped,2000,'Native TERM reap');}catch(second){cleanupFailures.push(sanitize(second));nativeChild.kill('SIGKILL');try{await bounded(nativeReaped,2000,'Native KILL reap');}catch(third){cleanupFailures.push(sanitize(third));}}}
  }
  for(const socket of sockets)socket.destroy();
  const closes=servers.map(server=>new Promise(resolve=>{if(!server.listening)resolve();else server.close(resolve);}));
  try{await bounded(Promise.allSettled([workPromise,...closes]),limits.cleanup_ms,'Native fixture work/server cleanup');}catch(error){cleanupFailures.push(sanitize(error));}
  try{await bounded((async()=>{while(sockets.size)await delay(1);})(),limits.cleanup_ms,'Owned socket cleanup');}catch(error){cleanupFailures.push(sanitize(error));}
  try{assert.equal(hash(await readFile(nativeBinary)),identity.native_sha256);}catch(error){cleanupFailures.push(sanitize(error));}
  process.off('SIGINT',interrupt);process.off('SIGTERM',terminate);
}
const cleanup={sockets:sockets.size,servers:servers.filter(server=>server.listening).length,native_reaped:!!nativeExit,native_exit:nativeExit,native:nativeCleanup,failures:cleanupFailures};
try{assert.ok(nativeCleanup?.cleanup);for(const key of ['live','tasks','fetch_live','fetch_tasks','shutdown_handles','fetch_shutdown_handles','terminal_tasks','intents'])assert.equal(nativeCleanup[key],0,key);assert.equal(nativeCleanup.cache_released,true);assert.equal(nativeCleanup.terminal_observer_failed,false);assert.equal(nativeCleanup.writer.observer_failed,false);assert.equal(nativeCleanup.writer.live_io,0);assert.equal(nativeCleanup.writer.waiters,0);if(spec.hold_socket_writes){assert.equal(nativeCleanup.writer.held,true);assert.equal(nativeCleanup.writer.written_bytes,0);}for(const key of ['live_pools','allocated_blocks','outstanding_blocks','queued_bytes','producer_tasks'])assert.equal(nativeCleanup.buffers[key],0,key);}catch(error){cleanupFailures.push(sanitize(error));}
const report={schema_version:1,kind:'buffered_response_native_case',identity,case:spec,schedule_done:scheduleDone,passed:scheduleDone&&!failure&&!cleanupFailures.length&&sockets.size===0&&nativeExit?.code===0,failure,interrupted,observations,wire:wire.snapshot(),provider_ended:providerEnded,cleanup,trace,native_state:state,native_diagnostic:diagnostic};
const reportBytes=JSON.stringify(report,null,2)+'\n';assert.ok(Buffer.byteLength(reportBytes)<=limits.control_bytes,'Native report bound');await writeFile(output,reportBytes,{mode:0o600});if(!report.passed)process.exitCode=interrupted==='SIGINT'?130:interrupted==='SIGTERM'?143:1;
