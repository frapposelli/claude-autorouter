// Exact frozen onboarding #31/#32 callbacks; synthetic file homes, no inference.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire, syncBuiltinESMExports } from 'node:module';
import childProcess from 'node:child_process';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { verifyBaseline } from '../../scripts/rust-reference.mjs';
const root = resolve(import.meta.dirname, '../..');
const reference = resolve(process.argv[2]);
const output = resolve(process.argv[3]);
assert.equal(process.argv.length,4,'Pass reference and fresh output');
const hash = raw => createHash('sha256').update(raw).digest('hex');
const before = await verifyBaseline(reference); assert.equal(before.files_verified,111);
const sourceFile = 'test/onboarding.test.mjs', source = readFileSync(join(reference,sourceFile),'utf8');
const baseline = JSON.parse(readFileSync(join(root,'rust/parity/baseline.json')));
assert.equal(hash(source),baseline.files.find(row=>row.path===sourceFile).sha256);
const controlsPath=join(root,'rust/parity/cases/doctor-dispatch-contracts.controls.json');
const controlBytes=readFileSync(controlsPath), controls=JSON.parse(controlBytes);
assert.equal(controls.source_sha256,hash(source));
const ts=createRequire(join(root,'package.json'))('typescript');
const syntax=ts.createSourceFile(sourceFile,source,ts.ScriptTarget.Latest,true,ts.ScriptKind.JS);
assert.equal(syntax.parseDiagnostics.length,0);
const definitions=[],callbacks=[],texts=[],helpers=[],assertionCalls=[],operations=[];
const identities=new WeakMap(); let current, fault;
let fetchCalls=0;const builtinCalls=[];
const bounds={operations:3,static_assertions:12,expanded_assertions:9,events:32,field_bytes:262144,aggregate_bytes:2097152};
function observe(fn){try{return fn();}catch(error){fault??=error;throw error;}}
function snapshot(value,depth=0){
  assert.ok(depth<20);
  if(value===undefined)return {$js_type:'undefined'};
  if(value===null||typeof value==='boolean')return value;
  if(typeof value==='number'){assert.ok(Number.isFinite(value));return value;}
  if(typeof value==='string'){assert.ok(Buffer.byteLength(value)<=bounds.field_bytes);return value;}
  if(Array.isArray(value))return value.map(item=>snapshot(item,depth+1));
  assert.equal(typeof value,'object');assert.ok(Object.getPrototypeOf(value)===Object.prototype);
  return Object.fromEntries(Object.entries(Object.getOwnPropertyDescriptors(value)).filter(([,d])=>d.enumerable).map(([key,d])=>{assert.ok(Object.hasOwn(d,'value'));return [key,snapshot(d.value,depth+1)];}));
}
function errorInfo(error){
  assert.ok(error instanceof Error);
  if(!identities.has(error))identities.set(error,`error-${operations.length}`);
  return {id:identities.get(error),name:error.name,message:error.message};
}
let number=0;
for(const statement of syntax.statements){
  if(ts.isFunctionDeclaration(statement)&&statement.name?.text==='fixture')helpers.push({name:'fixture',text:statement.getText(syntax),sha256:hash(statement.getText(syntax))});
  if(ts.isVariableStatement(statement)&&statement.declarationList.declarations.some(row=>row.name.getText(syntax)==='doctor'))helpers.push({name:'doctor',text:statement.getText(syntax),sha256:hash(statement.getText(syntax))});
  if(!ts.isExpressionStatement(statement)||!ts.isCallExpression(statement.expression)||statement.expression.expression.getText(syntax)!=='test')continue;
  number++;if(![31,32].includes(number))continue;
  const call=statement.expression,sites=[];
  const visit=node=>{if(ts.isCallExpression(node)&&ts.isPropertyAccessExpression(node.expression)&&node.expression.expression.getText(syntax)==='assert')sites.push(node);ts.forEachChild(node,visit);};
  visit(call.arguments.at(-1));
  const row={id:`${sourceFile}#${number}`,number,name:call.arguments[0].text,source_line:syntax.getLineAndCharacterOfPosition(statement.getStart(syntax)).line+1,statement:statement.getText(syntax),statement_sha256:hash(statement.getText(syntax)),call_sha256:hash(call.getText(syntax)),assertions:sites.map((site,index)=>({id:`${sourceFile}#${number}:assert-${index+1}`,expression:site.getText(syntax),sha256:hash(site.getText(syntax)),line:syntax.getLineAndCharacterOfPosition(site.getStart(syntax)).line+1,executions:0}))};
  definitions.push(row);let transformed=row.statement;
  const offset=statement.getStart(syntax),edits=[];
  for(const [index,site]of sites.entries()){edits.push({at:site.getStart(syntax)-offset,text:`(__assertion(${JSON.stringify(row.assertions[index].id)}), `});edits.push({at:site.end-offset,text:')'});}
  for(const edit of edits.sort((a,b)=>b.at-a.at))transformed=transformed.slice(0,edit.at)+edit.text+transformed.slice(edit.at);
  texts.push(transformed);
}
assert.equal(number,34);assert.deepEqual(definitions.map(row=>[row.number,row.assertions.length]),[[31,7],[32,5]]);
assert.equal(helpers.length,2);
const transformed=helpers.map(row=>row.text).join('\n')+'\n'+texts.join('\n');
assert.equal(ts.createSourceFile('callbacks.mjs',transformed,ts.ScriptTarget.Latest,true,ts.ScriptKind.JS).parseDiagnostics.length,0);
function registered(name,callback){assert.equal(name,definitions[callbacks.length].name);callbacks.push(callback);}
function counted(id){observe(()=>{assert.ok(assertionCalls.length<bounds.expanded_assertions);const site=current.assertions.find(row=>row.id===id);assert.ok(site);site.executions++;assertionCalls.push(id);});}
const realFetch=globalThis.fetch;
const originalExecFile=childProcess.execFile, originalSpawnSync=childProcess.spawnSync;
for(const name of ['execFile','spawnSync'])childProcess[name]=(...args)=>{builtinCalls.push({function:name,command:String(args[0])});throw Error('Capture forbids actual subprocesses and Keychain');};
syncBuiltinESMExports();
const generator=readFileSync(import.meta.filename),nodeHash=hash(readFileSync(process.execPath));
mkdirSync(output);writeFileSync(join(output,'generator.mjs'),generator,{flag:'wx',mode:0o600});writeFileSync(join(output,'transformed.mjs'),transformed,{flag:'wx',mode:0o600});
const {doctor:actualDoctor}=await import(pathToFileURL(join(reference,'src/onboarding.mjs')).href);
const {saveUserConfig}=await import(pathToFileURL(join(reference,'src/user-config.mjs')).href);
async function recordedDoctor(options){
  assert.ok(operations.length<bounds.operations);
  const row={id:`doctor-${operations.length+1}`,source_test:current.id,evaluate_local:options.evaluateLocal,json:options.json??false,run_calls:[],diagnostic_calls:[],writes:[],progress:[],signal_supplied:options.signal!==undefined,config_before_utf8:readFileSync(options.env.AUTOROUTER_CONFIG,'utf8')};operations.push(row);
  const wrapped={...options,write(...args){observe(()=>row.writes.push(snapshot(args)));return options.write(...args);}};
  if(options.run)wrapped.run=(...args)=>{observe(()=>row.run_calls.push(snapshot(args)));return options.run(...args);};
  if(options.diagnosticRunner)wrapped.diagnosticRunner=async(config,inner)=>{
    observe(()=>row.diagnostic_calls.push({config:snapshot(config),signal_same_as_caller:inner.signal===options.signal}));
    const wrappedInner={...inner,onProgress(value){observe(()=>{assert.ok(row.progress.length<bounds.events);row.progress.push(snapshot(value));});return inner.onProgress(value);}};
    try{const result=await options.diagnosticRunner(config,wrappedInner);observe(()=>{row.runner_result=snapshot(result);});return result;}
    catch(error){observe(()=>{row.runner_error=errorInfo(error);row.signal_aborted_after_runner=options.signal?.aborted??false;row.signal_reason_is_runner_error=options.signal?.reason===error;});throw error;}
  };
  try{const result=await actualDoctor(wrapped);observe(()=>{row.result=snapshot(result);});return result;}
  catch(error){observe(()=>{row.error=errorInfo(error);row.doctor_error_is_original_runner_error=row.runner_error?.id===row.error.id;row.signal_reason_is_doctor_error=options.signal?.reason===error;});throw error;}
  finally{row.config_after_utf8=readFileSync(options.env.AUTOROUTER_CONFIG,'utf8');}
}
const deadline=setTimeout(()=>{process.stderr.write('Doctor original capture deadline exceeded\n');process.exit(1);},30000);
try{
  globalThis.fetch=()=>{fetchCalls++;throw Error('Capture forbids actual network');};
  Function('test','assert','__assertion','runDoctor','mkdtempSync','tmpdir','join','rmSync','saveUserConfig',transformed)(registered,assert,counted,recordedDoctor,mkdtempSync,tmpdir,join,rmSync,saveUserConfig);
  assert.equal(callbacks.length,2);
  for(const [index,callback]of callbacks.entries()){
    current=definitions[index];const cleanups=[];
    try{await callback({after(fn){cleanups.push(fn);}});}finally{for(const fn of cleanups.reverse())await fn();}
  }
  if(fault)throw fault;
  assert.equal(assertionCalls.length,9);assert.equal(operations.length,3);
  assert.ok(operations.every(row=>row.run_calls.length===0));assert.equal(fetchCalls,0);assert.equal(builtinCalls.length,0);
  assert.equal(operations[0].diagnostic_calls.length,1);assert.deepEqual(operations[0].progress,[{event:'startup'},{event:'case_complete'}]);
  assert.equal(operations[1].diagnostic_calls.length,0);assert.equal(operations[2].diagnostic_calls.length,1);
  assert.equal(operations[2].doctor_error_is_original_runner_error,true);assert.equal(operations[2].signal_reason_is_doctor_error,true);
  const after=await verifyBaseline(reference);assert.deepEqual(after,before);
  assert.equal(hash(generator),hash(readFileSync(import.meta.filename)));assert.equal(nodeHash,hash(readFileSync(process.execPath)));
  const report={schema_version:1,status:'passed',kind:'exact_frozen_doctor_original31_32_capture',baseline_commit:before.baseline_commit,baseline_files_verified:111,source_file:sourceFile,source_sha256:hash(source),generator_sha256:hash(generator),node_sha256:nodeHash,node_version:process.version,typescript_version:ts.version,controls_sha256:hash(controlBytes),definitions,helpers,assertion_calls:assertionCalls,counts:{callbacks:2,doctor_calls:3,static_assertions:12,expanded_assertions:9,dormant_fail_guards:3,subprocess_calls:builtinCalls.length,fetch_calls:fetchCalls},builtin_calls:builtinCalls,operations,bounds,boundaries:['Original callbacks/helper statements/predicate expressions remain byte-bound; only imports and value-preserving observation wrappers are adapted.','Actual caller Error identity, AbortSignal and runner report object are forwarded unchanged; undefined is tagged, never serialized as null.','The source injected diagnostic runner makes zero HTTP requests; native tests execute actual bounded mock-backed CLI diagnostics, with an explicit reviewed embedding boundary rather than invented passthrough equality.','Temporary filesystem paths and all original saved configuration bytes are retained privately without normalization. No credentials, real provider, model, Keychain or user configuration is used.']};
  const raw=JSON.stringify(report,null,2)+'\n';assert.ok(Buffer.byteLength(raw)<bounds.aggregate_bytes);writeFileSync(join(output,'capture.json'),raw,{flag:'wx',mode:0o600});
  console.log(JSON.stringify({...report.counts,sha256:hash(raw)}));
}finally{globalThis.fetch=realFetch;childProcess.execFile=originalExecFile;childProcess.spawnSync=originalSpawnSync;syncBuiltinESMExports();clearTimeout(deadline);}
