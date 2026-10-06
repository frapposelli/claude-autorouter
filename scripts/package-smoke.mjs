#!/usr/bin/env node
import assert from 'node:assert/strict';
import http from 'node:http';
import { copyFile, lstat, mkdir, mkdtemp, readFile, readdir, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { delimiter, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { archiveFiles, npmEnvironment, packagePlan, packPackage, run } from './release-pack.mjs';
import { verifyPackagedLifecycle } from './package-lifecycle.mjs';

const JEV_KEY = 'package-smoke-jev-key';
const API_KEY = 'package-smoke-api-key';
const TEV1_MODEL = 'tev1:4b-q4_K_M';
const SENTINEL = 'AUTOROUTER_PRIVATE_PACKAGE_SENTINEL_5e73c512';
const FAKE_CLAUDE = `#!/usr/bin/env node
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
const args = process.argv.slice(2);
if (args.includes('--version')) {
  console.log('2.1.284 (Claude Code smoke fixture)');
} else if (args[0] === 'auth' && args[1] === 'status') {
  console.log(JSON.stringify({ loggedIn: true, authMethod: 'claude.ai', apiProvider: 'firstParty' }));
} else {
  assert.equal(process.env.TYPESAFE_API_KEY, undefined);
  assert.ok(process.env.ANTHROPIC_API_KEY);
  assert.notEqual(process.env.ANTHROPIC_API_KEY, ${JSON.stringify(API_KEY)});
  const headers = { 'content-type': 'application/json', 'x-api-key': process.env.ANTHROPIC_API_KEY,
    'x-claude-code-session-id': 'package-smoke-session', 'x-claude-code-request-class': 'main' };
  const health = await fetch(process.env.ANTHROPIC_BASE_URL + '/health', { headers });
  assert.equal(health.status, 200);
  const response = await fetch(process.env.ANTHROPIC_BASE_URL + '/v1/messages', { method: 'POST', headers,
    body: JSON.stringify({ model: process.env.ANTHROPIC_MODEL, max_tokens: 16, stream: false,
      messages: [{ role: 'user', content: 'Reply with zero.' }] }) });
  assert.equal(response.status, 200);
  assert.equal(process.env.ANTHROPIC_MODEL, 'claude-haiku-4-5-20251001');
  assert.equal((await response.json()).model, 'claude-sonnet-5');
  const settingsIndex = args.lastIndexOf('--settings');
  assert.ok(settingsIndex >= 0);
  const settingsFile = args[settingsIndex + 1];
  const settings = JSON.parse(await readFile(settingsFile, 'utf8'));
  assert.equal(settings.statusLine.type, 'command');
  assert.ok(settings.statusLine.command.includes('prefix with spaces'));
  assert.ok(settings.statusLine.command.includes('bin/statusline.mjs'));
  let line = '';
  for (let retry = 0; retry < 50; retry++) {
    line = execFileSync('/bin/sh', ['-c', settings.statusLine.command], {
      input: JSON.stringify({ session_id: 'package-smoke-session', model: { id: 'claude-haiku-4-5-20251001' } }),
      encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', COLUMNS: '200' }, timeout: 3000,
    }).trim();
    if (line.includes('Sonnet 5') && line.includes('saved')) break;
    await new Promise(resolve => setTimeout(resolve, 20));
  }
  assert.match(line, /AutoRouter/);
  assert.match(line, /Sonnet 5/);
  assert.match(line, /saved/);
  console.log(JSON.stringify({ smoke: 'ok', status_line: line, status_file: process.env.AUTOROUTER_STATUS_FILE, settings_file: settingsFile }));
}
`;

async function verifyInstalledConfiguration({ installedCommand, configPath, temporary, cwd, env }) {
  const directory = join(temporary, 'installed configuration commands');
  const scratch = join(directory, 'runtime files');
  const fakeBin = join(directory, 'fake Claude');
  await Promise.all([mkdir(scratch, { recursive: true }), mkdir(fakeBin, { recursive: true })]);
  const isolatedConfig = join(directory, 'config.json');
  await copyFile(configPath, isolatedConfig);
  const configEnv = { ...env, AUTOROUTER_CONFIG: isolatedConfig };
  const saved = JSON.parse(await readFile(isolatedConfig, 'utf8'));
  const readSaved = async () => JSON.parse(await readFile(isolatedConfig, 'utf8'));
  const invoke = (args, options = {}) => run(installedCommand, args, { cwd, env: configEnv, ...options });
  const secret = 'package-config-stdin-private-sentinel';
  const hidden = output => {
    for (const value of [JEV_KEY, API_KEY, secret]) assert.ok(!output.includes(value), 'Configuration output exposed a secret');
  };

  const initial = await invoke(['config', 'show', '--json', '--check-all']);
  const initialReport = JSON.parse(initial.stdout);
  assert.equal(initialReport.valid, true);
  assert.equal(initialReport.checked, 'all_evaluators');
  assert.equal(initialReport.settings.AUTOROUTER_AUTH_MODE.source, 'file');
  assert.equal(initialReport.settings.AUTOROUTER_JEV_TIMEOUT_MS.source, 'default');
  assert.deepEqual(initialReport.settings.TYPESAFE_API_KEY, { source: 'file', active: true, secret: true, present: true });
  hidden(initial.stdout + initial.stderr);
  const human = await invoke(['config', 'show']);
  assert.match(human.stdout, /TYPESAFE_API_KEY=\[set; hidden\]/);
  hidden(human.stdout + human.stderr);
  assert.deepEqual(await readSaved(), saved, 'Configuration inspection must remain read-only');

  const overrideEnv = { ...configEnv, AUTOROUTER_JEV_TIMEOUT_MS: '2400' };
  const changed = await invoke(['config', 'set', 'AUTOROUTER_JEV_TIMEOUT_MS', '2000'], { env: overrideEnv });
  assert.match(changed.stdout, /current environment still overrides AUTOROUTER_JEV_TIMEOUT_MS/);
  assert.deepEqual(await readSaved(), { ...saved, AUTOROUTER_JEV_TIMEOUT_MS: '2000' });
  const overridden = await invoke(['config', 'show', '--json'], { env: overrideEnv });
  assert.deepEqual(JSON.parse(overridden.stdout).settings.AUTOROUTER_JEV_TIMEOUT_MS,
    { source: 'environment', active: true, overrides_file: true, value: 2400 });
  hidden(changed.stdout + changed.stderr + overridden.stdout + overridden.stderr);
  await invoke(['config', 'set', 'AUTOROUTER_SESSION_LOG_MODE', 'metadata']);
  assert.deepEqual(await readSaved(), { ...saved, AUTOROUTER_JEV_TIMEOUT_MS: '2000', AUTOROUTER_SESSION_LOG_MODE: 'metadata' });
  await invoke(['config', 'unset', 'AUTOROUTER_JEV_TIMEOUT_MS']);
  await invoke(['config', 'unset', 'AUTOROUTER_SESSION_LOG_MODE']);
  assert.deepEqual(await readSaved(), saved, 'Focused edits must preserve all other keys and preferences');

  const entered = await invoke(['config', 'set', 'TYPESAFE_API_KEY', '--stdin'], { input: `${secret}\n` });
  hidden(entered.stdout + entered.stderr);
  assert.deepEqual(await readSaved(), { ...saved, TYPESAFE_API_KEY: secret });
  const redacted = await invoke(['config', 'show', '--json']);
  hidden(redacted.stdout + redacted.stderr);
  const beforeRejectedEdits = await readFile(isolatedConfig, 'utf8');
  await assert.rejects(invoke(['config', 'set', 'TYPESAFE_API_KEY', secret]), error => {
    assert.match(error.message, /Secret values are not accepted/);
    hidden(error.message);
    return true;
  });
  await assert.rejects(invoke(['config', 'set', 'AUTOROUTER_JEV_TIMEOUT_MS', '']), /AUTOROUTER_JEV_TIMEOUT_MS/);
  assert.equal(await readFile(isolatedConfig, 'utf8'), beforeRejectedEdits);
  assert.deepEqual(JSON.parse(await readFile(configPath, 'utf8')), saved, 'Acceptance fixtures must not modify routing fixtures');

  await writeFile(join(fakeBin, 'claude'), `#!/usr/bin/env node
import assert from 'node:assert/strict';
assert.equal(process.argv.length, 3);
assert.ok(['--help', '-h', '--version', '-v'].includes(process.argv[2]));
for (const key of ['ANTHROPIC_BASE_URL', 'AUTOROUTER_STATUS_FILE', 'TYPESAFE_API_KEY', 'AUTOROUTER_TOKEN']) {
  assert.equal(process.env[key], undefined);
}
console.log('CLAUDE_DIRECT:' + process.argv[2]);
`, { mode: 0o700 });
  const missingConfig = join(scratch, 'missing.json');
  const helpEnv = { ...env, PATH: [fakeBin, dirname(process.execPath)].join(delimiter), TMPDIR: scratch,
    AUTOROUTER_CONFIG: missingConfig, AUTOROUTER_EVALUATOR: 'ollama', AUTOROUTER_OLLAMA_URL: 'invalid endpoint',
    TYPESAFE_API_KEY: secret, AUTOROUTER_TOKEN: secret };
  for (const command of ['setup', 'doctor', 'config', 'serve', 'sessions']) {
    const result = await invoke([command, '--help'], { env: helpEnv });
    assert.match(result.stdout, new RegExp(`Usage: claude-autorouter ${command}`));
    assert.equal(result.stderr, '');
    hidden(result.stdout);
  }
  const configHelp = await invoke(['help', 'config'], { env: helpEnv });
  assert.match(configHelp.stdout, /config set KEY VALUE/);
  assert.equal(configHelp.stderr, '');
  for (const argument of ['--help', '-h', '--version', '-v']) {
    const result = await invoke(['claude', argument], { env: helpEnv });
    assert.equal(result.stdout.trim(), `CLAUDE_DIRECT:${argument}`);
    assert.equal(result.stderr, '');
  }
  assert.deepEqual(await readdir(scratch), [], 'Help/version must not prepare gateway or status files');
}

async function main() {
  const args = process.argv.slice(2);
  if (args.length && (args.length !== 2 || args[0] !== '--archive' || !args[1]?.trim())) {
    throw new Error('Usage: node scripts/package-smoke.mjs [--archive PATH]');
  }
  const project = resolve(dirname(fileURLToPath(import.meta.url)), '..');
  const temporary = await realpath(await mkdtemp(join(tmpdir(), 'autorouter-package-smoke-')));
  let mock;
  let mockError;
  let classifications = 0;
  let generations = 0;
  let localClassifications = 0;
  let localWarms = 0;
  let nimbleClassifications = 0;
  let nimbleWarms = 0;
  let pulls = 0;
  let localModelInstalled = false;
  let expectedLocalModel = TEV1_MODEL;
  try {
    const env = await npmEnvironment(join(temporary, 'npm isolated'));
    let packed;
    if (args.length) {
      const path = resolve(args[1]);
      let info;
      try { info = await lstat(path); } catch { throw new Error('--archive must name an existing regular .tgz file'); }
      if (!info.isFile() || !path.endsWith('.tgz') || info.size > 32 * 1024 * 1024) {
        throw new Error('--archive must name a regular .tgz file no larger than 32 MiB');
      }
      const archive = await realpath(path);
      const files = await archiveFiles(archive);
      const manifest = JSON.parse(files.get('package.json').toString('utf8'));
      const expected = JSON.parse(await readFile(join(project, 'package.json'), 'utf8'));
      if (!manifest || typeof manifest !== 'object' || Array.isArray(manifest)
        || manifest.name !== expected.name || manifest.version !== expected.version) {
        throw new Error('Archive package name and version must match this checkout');
      }
      if (manifest.private) throw new Error('Archive package is marked private');
      if (Object.keys(manifest.dependencies ?? {}).length || Object.keys(manifest.optionalDependencies ?? {}).length) {
        throw new Error('Archive package must remain dependency-free');
      }
      packed = { archive, files, manifest };
    } else {
      const plan = await packagePlan(project, env);
      const staging = join(temporary, 'package staging');
      for (const file of plan.files) {
        const target = join(staging, file.path);
        await mkdir(dirname(target), { recursive: true });
        await copyFile(join(project, file.path), target);
      }
      // Synthetic secrets verify the actual archive's contents without reading
      // or copying this checkout's .env, private artifacts, or transcripts.
      for (const path of ['.env', '.env.local', 'artifacts/private-transcript.json', 'test/private.test.mjs', 'src/private.env',
        'autorouter-session-private.jsonl', 'src/autorouter-session-private.jsonl']) {
        const target = join(staging, path);
        await mkdir(dirname(target), { recursive: true });
        await writeFile(target, SENTINEL);
      }
      packed = await packPackage(staging, join(temporary, 'archives'), env);
    }
    for (const [path, content] of packed.files) assert.ok(!content.includes(SENTINEL), `Private sentinel leaked into ${path}`);

    const prefix = join(temporary, 'prefix with spaces');
    const unrelated = join(temporary, 'unrelated working directory');
    const fakeBin = join(temporary, 'fake Claude bin');
    await Promise.all([mkdir(unrelated), mkdir(fakeBin)]);
    await run('npm', ['install', '--global', '--prefix', prefix, '--offline', '--ignore-scripts', '--no-audit', '--no-fund', packed.archive], { cwd: unrelated, env });
    const installedCommand = join(prefix, 'bin', 'claude-autorouter');
    const installed = await realpath(installedCommand);
    assert.ok(installed.startsWith(prefix));
    const version = await run(installedCommand, ['--version'], { cwd: unrelated, env });
    assert.ok(version.stdout.includes(packed.manifest.version));
    const help = await run(installedCommand, ['--help'], { cwd: unrelated, env });
    assert.match(help.stdout, /setup/);
    assert.match(help.stdout, /doctor/);
    await writeFile(join(fakeBin, 'claude'), FAKE_CLAUDE, { mode: 0o755 });

    mock = http.createServer(async (req, res) => {
      try {
        let raw = ''; for await (const chunk of req) raw += chunk;
        const body = raw ? JSON.parse(raw) : undefined;
        assert.equal(req.headers['x-autorouter-token'], undefined);
        if (req.url.startsWith('/api/')) {
          assert.equal(req.headers.authorization, undefined);
          assert.equal(req.headers['x-api-key'], undefined);
          res.writeHead(200, { 'content-type': 'application/json' });
          if (req.url === '/api/version') res.end(JSON.stringify({ version: '0.35.0' }));
          else if (req.url === '/api/tags') res.end(JSON.stringify({ models: [{ name: 'nimble:9b-q4_K_M' }, ...(localModelInstalled ? [{ name: TEV1_MODEL }] : [])] }));
          else if (req.url === '/api/show') {
            assert.equal(body.model, expectedLocalModel);
            res.end(JSON.stringify({ details: { parameter_size: body.model === TEV1_MODEL ? '4B' : '9B' } }));
          }
          else if (req.url === '/api/pull') {
            pulls++;
            assert.equal(body.model, TEV1_MODEL);
            localModelInstalled = true;
            res.end('{"status":"pulling manifest"}\n{"status":"success"}\n');
          } else assert.fail('Unexpected Ollama lifecycle endpoint');
        } else if (req.url === '/v1/systemone' && ['nimble:9b-q4_K_M', TEV1_MODEL].includes(body.model)) {
          assert.equal(body.model, expectedLocalModel);
          assert.equal(req.headers.authorization, undefined);
          assert.equal(req.headers['x-api-key'], undefined);
          assert.equal(body.questions.tier.type, 'choice');
          const nimble = body.model === 'nimble:9b-q4_K_M';
          if (body.state.current_task === 'Return the literal word ready.') { if (nimble) nimbleWarms++; else localWarms++; }
          else { if (nimble) nimbleClassifications++; else localClassifications++; assert.equal(body.state.current_task, 'Reply with zero.'); }
          res.writeHead(200, { 'content-type': 'application/json' });
          res.end(JSON.stringify({ model: body.model, answers: { tier: { type: 'choice', choice: 'sonnet',
            probabilities: { haiku: 0, sonnet: 1, opus: 0 }, confidence: 1 } }, usage: { input_tokens: 200, output_tokens: 1 } }));
        } else if (req.url === '/v1/systemone') {
          classifications++;
          assert.equal(req.headers.authorization, 'Bearer ' + JEV_KEY);
          assert.equal(body.state.current_task, 'Reply with zero.');
          res.writeHead(200, { 'content-type': 'application/json' });
          res.end(JSON.stringify({ answers: { tier: { choice: 'sonnet', confidence: 1 } } }));
        } else {
          generations++;
          assert.equal(req.url, '/v1/messages');
          assert.equal(req.headers['x-api-key'], API_KEY);
          assert.equal(req.headers.authorization, undefined);
          assert.equal(body.model, 'claude-sonnet-5');
          assert.deepEqual(body.messages, [{ role: 'user', content: 'Reply with zero.' }]);
          res.writeHead(200, { 'content-type': 'application/json' });
          res.end(JSON.stringify({ id: 'msg_package_smoke', type: 'message', role: 'assistant', model: body.model,
            content: [{ type: 'text', text: '0' }], stop_reason: 'end_turn', stop_sequence: null,
            usage: { input_tokens: 1000, output_tokens: 1, cache_creation_input_tokens: 0, cache_read_input_tokens: 0 } }));
        }
      } catch (error) {
        mockError = error;
        res.writeHead(500, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ type: 'error', error: { type: 'api_error', message: 'Synthetic smoke fixture failed' } }));
      }
    });
    await new Promise((resolveListen, reject) => { mock.once('error', reject); mock.listen(0, '127.0.0.1', resolveListen); });
    const endpoint = `http://127.0.0.1:${mock.address().port}`;
    const configPath = join(temporary, 'user config', 'config.json');
    const cliEnv = { ...env, PATH: [fakeBin, dirname(process.execPath), env.PATH].filter(Boolean).join(delimiter),
      AUTOROUTER_CONFIG: configPath, TYPESAFE_API_KEY: JEV_KEY, ANTHROPIC_API_KEY: API_KEY,
      AUTOROUTER_JEV_URL: `${endpoint}/v1/systemone`, AUTOROUTER_UPSTREAM_URL: endpoint };
    const sessionLogDir = join(temporary, 'session decision logs');
    await run(installedCommand, ['setup', '--auth-mode', 'api-key', '--evaluator', 'jev', '--secret-store', 'file', '--session-log-dir', sessionLogDir], { cwd: unrelated, env: cliEnv });
    const saved = JSON.parse(await readFile(configPath, 'utf8'));
    assert.equal(saved.AUTOROUTER_AUTH_MODE, 'api-key');
    assert.equal(saved.TYPESAFE_API_KEY, JEV_KEY);
    assert.equal(saved.ANTHROPIC_API_KEY, API_KEY);
    assert.equal(saved.AUTOROUTER_SESSION_LOG_DIR, sessionLogDir);
    delete cliEnv.TYPESAFE_API_KEY;
    delete cliEnv.ANTHROPIC_API_KEY;
    await verifyInstalledConfiguration({ installedCommand, configPath, temporary, cwd: unrelated, env });
    assert.equal(classifications + generations + localClassifications + localWarms + nimbleClassifications + nimbleWarms + pulls, 0,
      'Configuration and help commands must not contact providers');
    await run(installedCommand, ['doctor'], { cwd: unrelated, env: cliEnv });
    const launched = await run(installedCommand, ['claude'], { cwd: unrelated, env: { ...cliEnv, AUTOROUTER_SESSION_LOG_MODE: 'prompts' } });
    if (mockError) throw mockError;
    assert.equal(launched.stderr, '', 'Default launcher should remain quiet');
    const result = launched.stdout.split('\n').filter(Boolean).map(line => JSON.parse(line)).find(value => value.smoke === 'ok');
    assert.ok(result);
    assert.equal(classifications, 1);
    assert.equal(generations, 1);
    const sessionFiles = await readdir(sessionLogDir);
    assert.equal(sessionFiles.length, 1);
    const decisionLog = await readFile(join(sessionLogDir, sessionFiles[0]), 'utf8');
    const records = decisionLog.trimEnd().split('\n').map(line => JSON.parse(line));
    const decisions = records.filter(row => row.event === 'decision');
    const outcomes = records.filter(row => row.event === 'outcome');
    assert.equal(records.length, 2);
    assert.equal(decisions.length, 1);
    assert.equal(outcomes.length, 1);
    assert.ok(records.every(row => row.schema_version === 2));
    assert.equal(decisions[0].session_id, 'package-smoke-session');
    assert.equal(decisions[0].prompt_excerpt, 'Reply with zero.');
    assert.equal(decisions[0].selected_model, 'claude-sonnet-5');
    assert.equal(decisions[0].source, 'jev');
    assert.ok(Number.isFinite(decisions[0].decision_latency_ms) && decisions[0].decision_latency_ms >= 0);
    assert.equal(outcomes[0].request_id, decisions[0].request_id);
    assert.equal(outcomes[0].session_id, decisions[0].session_id);
    assert.equal(outcomes[0].status, 'completed');
    assert.equal(outcomes[0].completion_confirmed, true);
    assert.equal(outcomes[0].confirmed_model, decisions[0].selected_model);
    assert.equal(outcomes[0].baseline_model, 'claude-opus-5-5');
    assert.match(outcomes[0].pricing_version, /^\d{4}-\d\d-\d\d\.\d+$/);
    assert.ok(outcomes[0].usage.input_tokens > 0);
    for (const key of [JEV_KEY, API_KEY]) assert.ok(!decisionLog.includes(key));
    for (const path of [result.status_file, result.settings_file]) {
      await assert.rejects(readFile(path), error => error.code === 'ENOENT');
    }

    const subscriptionConfig = join(temporary, 'subscription-config.json');
    await writeFile(subscriptionConfig, JSON.stringify({ AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: JEV_KEY, AUTOROUTER_AUTH_MODE: 'subscription' }), { mode: 0o600 });
    const doctorEnv = { ...cliEnv, AUTOROUTER_CONFIG: subscriptionConfig };
    delete doctorEnv.AUTOROUTER_UPSTREAM_URL;
    await run(installedCommand, ['doctor'], { cwd: unrelated, env: doctorEnv });

    const ollamaEnv = { ...cliEnv, AUTOROUTER_CONFIG: join(temporary, 'ollama-config.json'),
      AUTOROUTER_OLLAMA_URL: endpoint, ANTHROPIC_API_KEY: API_KEY };
    await run(installedCommand, ['setup', '--auth-mode', 'api-key', '--secret-store', 'file', '--evaluator', 'ollama', '--ollama-model', TEV1_MODEL, '--pull'], { cwd: unrelated, env: ollamaEnv });
    const localSaved = JSON.parse(await readFile(ollamaEnv.AUTOROUTER_CONFIG, 'utf8'));
    assert.equal(localSaved.AUTOROUTER_EVALUATOR, 'ollama');
    assert.equal(localSaved.AUTOROUTER_OLLAMA_MODEL, TEV1_MODEL);
    assert.equal(localSaved.TYPESAFE_API_KEY, undefined);
    delete ollamaEnv.ANTHROPIC_API_KEY;
    await run(installedCommand, ['doctor'], { cwd: unrelated, env: ollamaEnv });
    const localLaunch = await run(installedCommand, ['claude'], { cwd: unrelated, env: ollamaEnv });
    if (mockError) throw mockError;
    assert.match(localLaunch.stderr, /Preparing local Ollama evaluator/);
    assert.ok(!localLaunch.stderr.includes('"event"'));
    const localResult = JSON.parse(localLaunch.stdout.trim());
    assert.match(localResult.status_line, /Ollama/);
    assert.equal(classifications, 1, 'Ollama must not contact Jev');
    assert.equal(localClassifications, 1);
    assert.equal(localWarms, 2, 'Setup and launch should prime the classifier prompt');
    assert.equal(pulls, 1);
    assert.equal(generations, 2);
    for (const path of [localResult.status_file, localResult.settings_file]) {
      await assert.rejects(readFile(path), error => error.code === 'ENOENT');
    }
    const nimbleEnv = { ...ollamaEnv, AUTOROUTER_CONFIG: join(temporary, 'nimble-config.json'), ANTHROPIC_API_KEY: API_KEY };
    expectedLocalModel = 'nimble:9b-q4_K_M';
    await run(installedCommand, ['setup', '--auth-mode', 'api-key', '--secret-store', 'file', '--evaluator', 'ollama'], { cwd: unrelated, env: nimbleEnv });
    delete nimbleEnv.ANTHROPIC_API_KEY;
    await run(installedCommand, ['doctor'], { cwd: unrelated, env: nimbleEnv });
    const nimbleLaunch = await run(installedCommand, ['claude'], { cwd: unrelated, env: nimbleEnv });
    if (mockError) throw mockError;
    const nimbleResult = JSON.parse(nimbleLaunch.stdout.trim());
    assert.match(nimbleResult.status_line, /Ollama/);
    assert.equal(nimbleClassifications, 1);
    assert.equal(nimbleWarms, 2);
    assert.equal(classifications, 1, 'Nimble must not contact Jev');
    assert.equal(pulls, 1, 'Installed Nimble must not be downloaded again');
    assert.equal(generations, 3);
    for (const path of [nimbleResult.status_file, nimbleResult.settings_file]) {
      await assert.rejects(readFile(path), error => error.code === 'ENOENT');
    }
    const lifecycle = await verifyPackagedLifecycle({ installedCommand, installedRoot: dirname(dirname(installed)), env,
      directory: join(temporary, 'installed lifecycle') });
    console.log(`Package lifecycle passed: ${lifecycle.join(', ')}.`);
    console.log(`Package smoke passed: ${packed.manifest.name}@${packed.manifest.version}, ${packed.files.size} safe archive files.`);
    if (args.length) console.log('Installed and tested the supplied archive without rebuilding it.');
    console.log('Verified offline installation, installed config edits/provenance/redaction/help, Jev/Nimble/Tev1 setup/doctor/routing, opt-in download, bundled status line, correlated private logs, metadata-only history, and session cleanup.');
    console.log(result.status_line);
    console.log(localResult.status_line);
  } finally {
    if (mock) { mock.closeAllConnections(); await new Promise(resolveClose => mock.close(resolveClose)); }
    await rm(temporary, { recursive: true, force: true });
  }
}

main().catch(error => { console.error(`Package smoke failed: ${error.message}`); process.exitCode = 1; });
