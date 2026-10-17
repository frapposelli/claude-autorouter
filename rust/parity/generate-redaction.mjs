// Development-only synthetic privacy cases; no credentials or captured sessions.
// Pipe stdout to a JSONL file and pass it to cargo xtask parity --cases.
let sequence = 0;
const emit = input => process.stdout.write(`${JSON.stringify({ id: `redact-${sequence++}`, op: 'redact', input })}\n`);
const values = ['abc', 'abcd', 'abcde', 'a;bcd', 'a,bcd', 'a&bcd', 'a; bcd', 'a, bcd', '[REDACTED:secret]',
  'two words here', '😀', '😀😀', '😀abc', 'true', 'false', 'undefined', 'trueish', 'null', 'none',
  'a'.repeat(512), 'a'.repeat(513), '😀'.repeat(256), '😀'.repeat(257), '-abcdef', 'abc\rdef', 'abc\ndef', 'abc\u2028def'];
for (const name of ['password', 'DB_PASSWORD', 'client_secret', 'token', 'api_key', 'AccountKey', 'STRIPE_KEY',
  'DB_PASS', 'AUTH', 'pass', 'auth', 'bypass_cache', 'oauth_state', 'AUTOROUTER_AUTH_MODE']) {
  for (const separator of ['=', ': ', '": "', "' = '"]) for (const value of values) {
    for (const quote of ['', '"', "'"]) emit(`${name}${separator}${quote}${value}${quote} tail`);
  }
}
for (const value of values) for (const header of ['Authorization: ', 'authorization="', 'Proxy-Authorization: ', 'Cookie: ', 'Set-Cookie: ']) {
  for (const scheme of ['', 'Bearer ', 'Basic\t', 'Digest ', 'Unknown ']) emit(`${header}${scheme}${value}`);
}
for (const value of values) for (const prefix of ['tool --password ', 'tool --token=', "tool --api-key '", 'sshpass -p ',
  'mysql -p', 'mysql -u root -p', 'curl -u user:', 'curl --user=', 'auth=', 'config.pass=']) emit(`${prefix}${value} tail`);
for (const scheme of ['redis', 'postgres', 'HTTPS', 'amqps', 'x', 'abcdefghijklmnopqrstu', 'abcdefghijklmnopqrstuv']) {
  for (const user of ['', 'user', 'a'.repeat(256), 'a'.repeat(257), '😀'.repeat(128), '😀'.repeat(129)]) {
    for (const password of ['a', 'ab@cd', 'a'.repeat(256), 'a'.repeat(257), '😀'.repeat(128), '😀'.repeat(129), 'a@😀@z']) {
      emit(`${scheme}://${user}:${password}@host/path`);
    }
  }
}
for (const spaces of [' ', '\t', '\n', '\r', '\v', '\f', '\u0085', '\u00a0', '\u1680', '\u180e', '\u2000',
  '\u2028', '\u2029', '\u202f', '\u205f', '\u3000', '\ufeff']) {
  emit(`tool${spaces}--token${spaces}abcd`);
  emit(`password=${spaces}abcd`);
  emit(`Bearer${spaces}abcdefghijklmnop`);
}
for (const token of [`sk-ant-api03-${'Z'.repeat(40)}`, `ghp_${'B'.repeat(36)}`, `hf_${'C'.repeat(34)}`,
  'jane.doe@example.com', 'GB82WEST12345698765432', '4111 1111 1111 1111', 'RSSMRA85T10A562S', '123-45-6789', '+39 333 1234567']) {
  for (const prefix of ['', 'a', '-', '_', '+', 'é', 'ſ', 'K', '😀', '.']) emit(`${prefix}${token} tail`);
}
for (const length of [0, 15, 16, 63, 64, 65, 127, 128, 129, 199, 200, 201]) {
  for (const character of ['a', '😀']) {
    emit(`curl -u ${character.repeat(length)}:abcd tail`);
    emit(`mysql ${character.repeat(length)} -pabcdef tail`);
    emit(`${character.repeat(length)}\n-----END PRIVATE KEY----- tail`);
  }
}
emit('password=abcd auth=true DB_PASS=abcd');
emit('a-auth=abc.auth=abcdef');
emit('paſſword=abcdef');
emit('API_KEY=abcdef');

// The original privacy tests include these long adversarial forms. Keep them
// opt-in for corpus generation; equality and termination are the assertions.
if (process.argv.includes('--adversarial')) {
  for (const input of ['a-'.repeat(200000), 'a@'.repeat(200000), '4 '.repeat(200000), 'AB12 '.repeat(100000),
    `-----BEGIN PRIVATE KEY-----${'-----BEGIN PRIVATE KEY-----'.repeat(5000)}`, 'Q'.repeat(400000),
    `${'Q'.repeat(400000)}\n-----END PRIVATE KEY-----`, `${'A'.repeat(20)}\n`.repeat(20000),
    'mysql '.repeat(60000), 'mysql -x '.repeat(40000), 'password="'.repeat(40000), "secret='".repeat(40000),
    '://:'.repeat(100000), 'a://:'.repeat(60000), 'http://' + ':'.repeat(300000), 'http://' + 'a:'.repeat(150000),
    '+1 '.repeat(100000), '+'.repeat(300000), '1-'.repeat(200000), ' -'.repeat(150000) + 'token', '--a'.repeat(100000),
    'DB_KEY'.repeat(60000), 'A_KEY='.repeat(60000), 'Cookie:'.repeat(60000), 'sshpass -p '.repeat(30000),
    `pass${'a'.repeat(300000)}`, `password=${'a;'.repeat(150000)}`, `password=${'a,'.repeat(150000)}`]) emit(input);
}
