// Temporary whole-engine oracle for the Rust live harness. The caller verifies
// every imported frozen source file first. This file never runs Claude, chooses
// fixture labels, writes configuration, or persists request/response contents.
import { pathToFileURL } from 'node:url';
import { join } from 'node:path';
import { createInterface } from 'node:readline';

const root = process.argv[2];
if (Number(process.versions.node.split('.')[0]) < 22) process.exit(1);
const load = path => import(pathToFileURL(join(root, path)));
const [{ readConfig }, { Router, buildState, contextSizeBytes }, { createRouterServer, listen }] =
  await Promise.all(['src/config.mjs', 'src/router.mjs', 'src/server.mjs'].map(load));
const send = (type, value) => process.stdout.write(JSON.stringify({ type, value }) + '\n');
let server;
let stopping = false;
async function stop(code = 0) {
  if (stopping) return;
  stopping = true;
  if (server) {
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  }
  process.stdout.write(JSON.stringify({ type: 'closed', value: code === 0 }) + '\n', () => process.exit(code));
}
for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => void stop());
process.on('uncaughtException', () => void stop(1));
process.on('unhandledRejection', () => void stop(1));

function metadata(body, context) {
  const blocks = body.messages.flatMap(message => Array.isArray(message.content) ? message.content : []);
  return {
    requested_model: body.model,
    request_class: ['main', 'compaction', 'auxiliary'].includes(context.requestClass) ? context.requestClass : context.requestClass ? 'other' : 'unspecified',
    message_count: body.messages.length,
    message_roles: body.messages.map(message => message.role),
    request_bytes: Buffer.byteLength(JSON.stringify(body)),
    system_bytes: Buffer.byteLength(JSON.stringify(body.system ?? null)),
    tools_bytes: Buffer.byteLength(JSON.stringify(body.tools ?? [])),
    messages_bytes: Buffer.byteLength(JSON.stringify(body.messages)),
    has_system_messages: body.messages.some(message => message.role === 'system'),
    thinking_type: ['enabled', 'adaptive', 'disabled'].includes(body.thinking?.type) ? body.thinking.type : 'unspecified',
    thinking_history_count: blocks.filter(block => ['thinking', 'redacted_thinking'].includes(block.type)).length,
    tool_result_count: blocks.filter(block => block.type === 'tool_result').length,
    tool_count: body.tools?.length ?? 0,
    typed_tools: [...new Set((body.tools ?? []).map(tool => tool.type).filter(Boolean))],
    max_tokens: body.max_tokens,
    has_output_effort: Boolean(body.output_config?.effort),
    has_context_management: Boolean(body.context_management),
  };
}

const input = createInterface({ input: process.stdin, terminal: false, crlfDelay: Infinity });
let initialized = false;
input.once('close', () => void stop());
input.on('line', line => {
  if (initialized || Buffer.byteLength(line) > 1024 * 1024) return void stop(1);
  initialized = true;
  void (async () => {
    const { env, scenario, outage, reminder } = JSON.parse(line);
    const config = readConfig(env);
    const actual = new Router(config, outage ? { fetchImpl: async () => { throw new TypeError('simulated_classifier_outage'); } } : {});
    const router = {
      complete: (id, evidence) => actual.complete(id, evidence),
      async route(body, context) {
        if (scenario.syntheticReminder && !['compaction', 'auxiliary'].includes(context.requestClass)) {
          const first = body.messages.find(message => message.role === 'user');
          if (first) {
            const content = typeof first.content === 'string' ? [{ type: 'text', text: first.content }] : first.content;
            first.content = [{ type: 'text', text: reminder }, ...content];
          }
        }
        const evidence = metadata(body, context);
        const state = buildState(body);
        evidence.expected_classified_tier = scenario.expectedClassifiedTiers
          ? scenario.expectedClassifiedTiers[scenario.prompts.indexOf(state.current_task)]
          : scenario.expectedClassifiedTier ?? scenario.expectedTier;
        if (scenario.syntheticReminder) {
          evidence.context_guard_bytes = contextSizeBytes(body);
          evidence.classifier_contains_example = JSON.stringify(state).includes(JSON.stringify(scenario.prompts[0]).slice(1, -1));
        }
        const decision = await actual.route(body, context);
        send('route', { ...evidence, ...decision });
        return decision;
      },
    };
    server = createRouterServer(config, { router, onStatus: value => send('status', value), log: value => send('log', value) });
    const address = await listen(server, 0);
    send('ready', { address: `127.0.0.1:${address.port}`, node_version: process.version, adapter_version: 1 });
  })().catch(() => void stop(1));
});
