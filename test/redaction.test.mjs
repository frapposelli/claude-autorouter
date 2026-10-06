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

// Synthetic values for the formats added after the AR-02 review. They are
// assembled at runtime so no literal token shape is committed.
const more = {
  googleOauth: ['ya', '29.', 'a'.repeat(30)].join(''),
  sendgrid: ['SG', 'b'.repeat(22), 'c'.repeat(22)].join('.'),
  shopify: ['shp', 'at_', 'd'.repeat(32)].join(''),
  huggingFace: ['hf', '_', 'e'.repeat(34)].join(''),
  digitalOcean: ['dop', '_v1_', 'f'.repeat(64)].join(''),
  pypi: ['pypi', '-', 'g'.repeat(60)].join(''),
  linear: ['lin', '_api_', 'h'.repeat(34)].join(''),
  notion: ['ntn', '_', 'i'.repeat(34)].join(''),
  databricks: ['dapi', 'a1'.repeat(16)].join(''),
  atlassian: ['ATATT', '3', 'j'.repeat(30)].join(''),
  mailgun: ['key', '-', '0a'.repeat(16)].join(''),
  twilio: ['SK', '1b'.repeat(16)].join(''),
  telegram: ['123456789', ':', 'k'.repeat(35)].join(''),
  stripeWebhook: ['whsec', '_', 'l'.repeat(24)].join(''),
};

test('redacts additional provider token formats', () => {
  for (const [name, value] of Object.entries(more)) {
    const output = redactSensitive(`token for the job: ${value} (rotate)`);
    assert.ok(!output.includes(value), `${name} must be redacted`);
    assert.match(output, /\[REDACTED:secret\]/, name);
  }
});

test('redacts URL credentials with an empty user or a password containing @', () => {
  const cases = [
    ['REDIS_URL=redis://:Sup3rS3cretPw@cache:6379', 'REDIS_URL=redis://[REDACTED:credentials]@cache:6379'],
    ['postgres://app:p@ssw0rdXYZ@db', 'postgres://[REDACTED:credentials]@db'],
    ['amqps://user:pass@word@mq.internal/vhost', 'amqps://[REDACTED:credentials]@mq.internal/vhost'],
  ];
  for (const [input, expected] of cases) assert.equal(redactSensitive(input), expected);
  for (const text of ['https://example.com:8080/path', 'http://localhost:3000', 'http://[::1]:8080/x', 'ssh://github.com/org/repo']) {
    assert.equal(redactSensitive(text), text);
  }
});

test('redacts the common setting-name forms the first filter missed, without leaking any part of the value', () => {
  const cases = [
    ['DB_PASS=Xk29fjqLm3', 'Xk29fjqLm3'],
    ['STRIPE_KEY=abcdef123456', 'abcdef123456'],
    ['SIGNING_KEY="abcdef123456"', 'abcdef123456'],
    ['DB_AUTH=abcd1234', 'abcd1234'],
    ['DB_PASS=Xk29fjqLm3', 'Xk29fjqLm3'],
    ['AUTH=abcd1234', 'abcd1234'],
    ['config.pass=hunter22x', 'hunter22x'],
    ['{"pass": "hunter22x"}', 'hunter22x'],
    ['password=a;bcdefghijklmnop', 'bcdefghijklmnop'],
    ["DB_PASSWORD='hunter2 is my long pass'", 'long pass'],
    ['{"password": "two words here"}', 'two words'],
    ['Server=db;User Id=app;Password=pw1234xx;Database=app', 'pw1234xx'],
    ['AccountKey=abcdefghijkl1234==', 'abcdefghijkl'],
    ['Cookie: sid=abcdef123456789; theme=dark', 'abcdef123456789'],
  ];
  for (const [input, leaked] of cases) {
    const output = redactSensitive(input);
    assert.ok(!output.includes(leaked), `${input} leaked ${leaked}: ${output}`);
    assert.match(output, /\[REDACTED:/, input);
  }
  assert.match(redactSensitive('DB_PASS=Xk29fjqLm3'), /^DB_PASS=/, 'The setting name stays visible');
});

test('redacts credentials passed on command lines', () => {
  const cases = [
    ['mysql -u root -pS3cretValue1 appdb', 'S3cretValue1'],
    ['curl -u admin:S3cretValue1 https://example.test', 'S3cretValue1'],
    ['curl --user admin:S3cretValue1 https://example.test', 'S3cretValue1'],
    ['tool --api-key abcdef123456 run', 'abcdef123456'],
    ['tool --db-password=hunter22x run', 'hunter22x'],
    ["tool --token 'abcdef123456'", 'abcdef123456'],
    ['sshpass -p hunter22x ssh host', 'hunter22x'],
  ];
  for (const [input, leaked] of cases) {
    const output = redactSensitive(input);
    assert.ok(!output.includes(leaked), `${input} leaked: ${output}`);
  }
  for (const text of ['mysql -u root -p', 'find . -print0 -prune', 'tool --password-stdin file', 'tool --max-tokens 4096', 'ssh -p 2222 host']) {
    assert.equal(redactSensitive(text), text, text);
  }
});

test('redacts international phone numbers, Italian tax codes and US social security numbers', () => {
  assert.equal(redactSensitive('call +39 333 1234567 today'), 'call [REDACTED:phone] today');
  assert.equal(redactSensitive('call +1 (415) 555-0132.'), 'call [REDACTED:phone].');
  assert.equal(redactSensitive('CF RSSMRA85T10A562S ok'), 'CF [REDACTED:national_id] ok');
  assert.equal(redactSensitive('SSN 123-45-6789'), 'SSN [REDACTED:national_id]');
  for (const text of ['RSSMRA85T10A562X', 'version +1.2.3.4', 'at +123 ms', 'ratio 3-2-1', 'ssn 000-12-3456', 'ssn 666-12-3456', 'build 1.2.3+456789']) {
    assert.equal(redactSensitive(text), text, text);
  }
});

test('ordinary words that contain a secret keyword are left alone', () => {
  for (const text of ['The author: field is missing', 'passenger: 4242 seats', 'bypass_cache=true', 'pass: ok',
    'compass=north-east', 'oauth_state=abcdef', 'primary_key=1', 'monkey=banana', 'keyboard=qwerty',
    'AUTOROUTER_AUTH_MODE=subscription', "export const LOCAL_AUTH_HEADER = 'x-autorouter-token';", "env.MAX_THINKING_TOKENS = '0'",
    'checks.tests_pass = results.failed === 0', 'auth: true', 'pass: ok']) {
    assert.equal(redactSensitive(text), text, text);
  }
});

test('the widened rules are idempotent and stay linear on adversarial input', () => {
  const sample = ['REDIS_URL=redis://:pw1234@h', 'DB_PASS=abcd1234', "PW_SECRET='a b c d'", 'mysql -pabcd1234', 'curl -u a:bcd',
    'tool --api-key abcd1234', 'Cookie: sid=abcdef123456', '+39 333 1234567', 'SSN 123-45-6789'].join('\n');
  const once = redactSensitive(sample);
  assert.equal(redactSensitive(once), once);
  const adversarial = ['mysql '.repeat(60000), 'mysql -x '.repeat(40000), 'password="'.repeat(40000), "secret='".repeat(40000),
    '://:'.repeat(100000), 'a://:'.repeat(60000), 'http://' + ':'.repeat(300000), 'http://' + 'a:'.repeat(150000),
    '+1 '.repeat(100000), '+'.repeat(300000), '1-'.repeat(200000), ' -'.repeat(150000) + 'token', '--a'.repeat(100000),
    'DB_KEY'.repeat(60000), 'A_KEY='.repeat(60000), 'Cookie:'.repeat(60000), 'sshpass -p '.repeat(30000), `pass${'a'.repeat(300000)}`,
    `password=${'a;'.repeat(150000)}`, `password=${'a,'.repeat(150000)}`];
  for (const input of adversarial) {
    const started = performance.now();
    redactSensitive(input);
    assert.ok(performance.now() - started < 2000, `redaction took ${Math.round(performance.now() - started)} ms for ${JSON.stringify(input.slice(0, 16))}`);
  }
});

test('a .env file read as a tool result reaches the evaluator redacted', () => {
  const env = ['REDIS_URL=redis://:Sup3rS3cretPw@cache:6379', 'DB_PASS=Xk29fjqLm3', 'STRIPE_KEY=abcdef123456',
    'SESSION_SECRET=zz-synthetic-9876', 'ADMIN_EMAIL=ops@example.com', 'APP_NAME=autorouter'].join('\n');
  const body = { model: 'claude-sonnet-5', messages: [
    { role: 'user', content: 'Why does the cache connection fail?' },
    { role: 'assistant', content: [{ type: 'tool_use', id: 't1', name: 'Read', input: { file_path: '.env' } }] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 't1', content: env }] },
  ] };
  for (const state of [buildState(body, 12000), buildOllamaState(body, 3000)]) {
    const serialized = JSON.stringify(state);
    for (const value of ['Sup3rS3cretPw', 'Xk29fjqLm3', 'abcdef123456', 'zz-synthetic-9876', 'ops@example.com']) {
      assert.ok(!serialized.includes(value), `evaluator state leaked ${value}`);
    }
    assert.match(serialized, /APP_NAME=autorouter/, 'ordinary settings stay readable');
  }
});
