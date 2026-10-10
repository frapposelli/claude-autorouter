// Exact consumed JSON numeric semantics; no network or provider calls.
import { writeFileSync } from 'node:fs';
const patches = [
  '"thinking":1e400', '"thinking":false', '"thinking":{"type":"adaptive"}',
  '"output_config":{"effort":1e400}', '"output_config":{"effort":0}',
  '"output_config":{"effort":"\\ud800"}', '"output_config":{"effort":{"type":"high"}}',
  '"max_tokens":1e400', '"max_tokens":64000', '"max_tokens":64001',
  '"safeguards":1e400', '"safeguards":null', '"context_management":1e400',
  '"tools":[{"name":"Synthetic","input_schema":{"type":"object","opaque":"\\ud800"}}]',
  '"messages":[{"role":"user","content":[{"type":"text","text":"Task"},{"type":"tool_result","tool_use_id":"t","content":1e400}]}]',
];
const cases = [];
for (const profile of ['compatible','auto']) for (const tier of ['haiku','sonnet','opus']) for (const patch of patches) {
  const json = `{"model":"claude-sonnet-5-5","messages":[{"role":"user","content":"Synthetic task"}],${patch}}`;
  cases.push({id:`guards-router-json-${cases.length}`,op:'router',input:{env:{AUTOROUTER_CLIENT_PROFILE:profile},steps:[{op:'route',bytes:[...Buffer.from(json)],decision:{tier,classified_tier:tier,source:'jev',evaluator:'jev',reason:'classified',confidence:0.98},count:1200,options:{scope:'synthetic',promptId:'task',requestId:'request'}}]}});
}
writeFileSync(new URL('./cases/guards-router-json.jsonl',import.meta.url),cases.map(value=>JSON.stringify(value)).join('\n')+'\n');
