import test from 'node:test';
import assert from 'node:assert/strict';
import { redactSensitive } from '../src/redaction.mjs';
import { buildState } from '../src/prompt-state.mjs';
import { buildOllamaState } from '../src/ollama-evaluator.mjs';
import { readConfig } from '../src/config.mjs';
import { Router } from '../src/router.mjs';

// Synthetic, format-valid values assembled at runtime so no literal token
// shape is committed. Public documentation examples are used where they exist.
const fake = {
  anthropic: ['sk', 'ant', 'api03', 'Z'.repeat(40)].join('-'),
  stripe: ['sk', 'live', 'Q'.repeat(24)].join('_'),
  aws: ['AKIA', 'IOSFODNN7EXAMPLE'].join(''),
  github: ['gh', 'p_', 'B'.repeat(36)].join(''),
  githubPat: ['github', 'pat', 'C'.repeat(30)].join('_'),
  gitlab: ['glpat', 'D'.repeat(24)].join('-'),
  slack: ['xoxb', '1234567890', 'E'.repeat(12)].join('-'),
  slackHook: ['https://hooks.slack.com', 'services', 'T000', 'B000', 'F'.repeat(12)].join('/'),
  google: ['AI', 'za', 'G'.repeat(35)].join(''),
  npm: ['npm', 'H'.repeat(36)].join('_'),
  jwt: ['eyJ' + 'h'.repeat(12), 'eyJ' + 'p'.repeat(12), 's'.repeat(16)].join('.'),
  pem: ['-----BEGIN RSA PRIVATE KEY-----', 'M'.repeat(64), 'N'.repeat(64), '-----END RSA PRIVATE KEY-----'].join('\n'),
};

test('redacts recognized credential formats while keeping surrounding task text', () => {
  for (const [name, value] of Object.entries(fake)) {
    const output = redactSensitive(`Rotate this value: ${value} before deploy.`);
    assert.ok(!output.includes(value), `${name} must be redacted`);
    assert.match(output, /^Rotate this value: .*\[REDACTED:(?:secret|private_key)\].* before deploy\.$/s, name);
  }
});

test('redacts secret assignments, authorization headers and URL credentials but keeps their names', () => {
  const cases = [
    ['DB_PASSWORD=synthetic-P4ss', 'DB_PASSWORD=[REDACTED:secret]'],
    ['"client_secret": "abcd1234"', '"client_secret": "[REDACTED:secret]"'],
    ['api_key: zz-synthetic-value', 'api_key: [REDACTED:secret]'],
    ['export GITHUB_TOKEN=abcdefgh', 'export GITHUB_TOKEN=[REDACTED:secret]'],
    ['Authorization: Basic dXNlcjpwYXNz', 'Authorization: Basic [REDACTED:secret]'],
    ['curl -H "Bearer abcdefghijklmnopqrstuvwxyz"', 'curl -H "Bearer [REDACTED:secret]"'],
    ['postgres://admin:hunter2@db.internal:5432/app', 'postgres://[REDACTED:credentials]@db.internal:5432/app'],
  ];
  for (const [input, expected] of cases) assert.equal(redactSensitive(input), expected);
});

test('redacts emails and checksum-valid IBANs and card numbers only', () => {
  assert.equal(redactSensitive('Contact jane.doe@example.com today'), 'Contact [REDACTED:email] today');
  assert.equal(redactSensitive('IBAN GB82 WEST 1234 5698 7654 32 on file'), 'IBAN [REDACTED:iban] on file');
  assert.equal(redactSensitive('IBAN GB82WEST12345698765432'), 'IBAN [REDACTED:iban]');
  assert.equal(redactSensitive('card 4111 1111 1111 1111 expired'), 'card [REDACTED:card] expired');
  assert.equal(redactSensitive('card 4111-1111-1111-1111'), 'card [REDACTED:card]');
  // Checksum failures, timestamps and ordinary identifiers are not personal data.
  for (const text of ['GB00WEST12345698765432', 'card 4111 1111 1111 1112', 'at 1791278893123 ms',
    'order 1234567890123456', 'commit dea6caeef764891a270a035b0d960bb279cb8d33', 'see RFC 7519']) {
    assert.equal(redactSensitive(text), text);
  }
});

test('keeps ordinary engineering text intact', () => {
  for (const text of [
    'Fix the token counter so cached input is included.',
    'Add password reset tests for the login form.',
    'The author: field is missing from package.json.',
    'Explain why the secret rotation job fails intermittently.',
    'Bearer tokens are validated upstream.',
  ]) assert.equal(redactSensitive(text), text);
});

test('redaction is idempotent and stays linear on adversarial input', () => {
  const once = redactSensitive(`password=${'x'.repeat(20)} and jane@example.com`);
  assert.equal(redactSensitive(once), once);
  const adversarial = ['a-'.repeat(200000), 'a@'.repeat(200000), '4 '.repeat(200000), 'AB12 '.repeat(100000),
    `-----BEGIN PRIVATE KEY-----${'-----BEGIN PRIVATE KEY-----'.repeat(5000)}`, 'Q'.repeat(400000),
    `${'Q'.repeat(400000)}\n-----END PRIVATE KEY-----`, `${'A'.repeat(20)}\n`.repeat(20000)];
  for (const input of adversarial) {
    const started = performance.now();
    redactSensitive(input);
    assert.ok(performance.now() - started < 2000, `redaction took too long for ${input.slice(0, 12)}`);
  }
});

test('evaluator state never contains a secret from tool output, system text or long excerpts', () => {
  const secret = 'synthetic-P4ss-value';
  const filler = 'ordinary log line\n'.repeat(5000);
  const body = { model: 'claude-sonnet-5', system: `Deploy key: ${fake.aws}`, messages: [
    { role: 'user', content: `Fix the deploy. My email is jane.doe@example.com` },
    { role: 'assistant', content: [{ type: 'tool_use', id: 't1', name: 'Bash', input: { command: 'cat .env' } }] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 't1',
      content: `DB_PASSWORD=${secret}\n${fake.pem}\n${filler}\nIBAN GB82WEST12345698765432\nTOKEN=${secret}` }] },
  ] };
  for (const state of [buildState(body, 12000), buildOllamaState(body, 3000)]) {
    const serialized = JSON.stringify(state);
    for (const value of [secret, fake.aws, 'jane.doe@example.com', 'GB82WEST', 'M'.repeat(64)]) {
      assert.ok(!serialized.includes(value), `evaluator state leaked ${value.slice(0, 8)}`);
    }
    assert.match(serialized, /\[REDACTED:/);
    assert.match(state.current_task, /^Fix the deploy\. My email is \[REDACTED:email\]$/);
  }
});

test('a private key cut by excerpt windows is redacted at either edge', () => {
  const keyBody = Array.from({ length: 400 }, () => 'K'.repeat(64)).join('\n');
  for (const content of [
    `-----BEGIN PRIVATE KEY-----\n${keyBody}\n${'y'.repeat(60000)}`,
    `${'y'.repeat(60000)}\n${keyBody}\n-----END PRIVATE KEY-----`,
  ]) {
    const state = buildState({ model: 'claude-sonnet-5', messages: [{ role: 'user', content }] }, 12000);
    assert.ok(!JSON.stringify(state).includes('K'.repeat(64)));
  }
});

test('the Jev request body carries only redacted excerpts; Anthropic receives the request unchanged', async () => {
  let payload;
  const router = new Router(readConfig({ AUTOROUTER_EVALUATOR: 'jev', TYPESAFE_API_KEY: 'test-jev', ANTHROPIC_API_KEY: 'test-anthropic' }), {
    fetchImpl: async (_url, options) => {
      payload = options.body;
      return Response.json({ answers: { tier: { choice: 'sonnet', confidence: 0.9 } } });
    },
  });
  const body = { model: 'claude-sonnet-5', max_tokens: 1024, messages: [{ role: 'user', content: `Use ${fake.github} to push` }] };
  const before = structuredClone(body);
  await router.route(body);
  assert.ok(payload && !payload.includes(fake.github));
  assert.match(payload, /Use \[REDACTED:secret\] to push/);
  assert.deepEqual(body, before);
});

test('session-log prompt excerpts are redacted, including a secret at the retention boundary', async () => {
  const { promptExcerpt } = await import('../src/prompt-state.mjs');
  const { normalizeSessionRecord } = await import('../src/telemetry-event.mjs');
  const request = text => ({ messages: [{ role: 'user', content: text }] });
  assert.equal(promptExcerpt(request(`email jane.doe@example.com token=${fake.github}`)), 'email [REDACTED:email] token=[REDACTED:secret]');
  // The secret begins inside the 500-character window and ends after it.
  const straddling = `${'x'.repeat(490)} ${fake.github} tail`;
  const excerpt = promptExcerpt(request(straddling), 501);
  assert.ok(!excerpt.includes(fake.github.slice(0, 12)), 'no fragment of the secret may remain');
  assert.match(excerpt, /\[REDACTED:/, "the cut may fall inside the marker, never inside the secret");
  assert.ok([...excerpt].length <= 501);
  // Records written before redaction existed are redacted again on normalization and reading.
  const row = normalizeSessionRecord({ schema_version: 2, event: 'decision', request_id: 'r1', requested_model: 'claude-sonnet-5',
    selected_model: 'claude-sonnet-5', prompt_excerpt: `use ${fake.aws} now` });
  assert.equal(row.prompt_excerpt, 'use [REDACTED:secret] now');
});
