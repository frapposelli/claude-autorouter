// Held-out B1 cases declared before the first candidate differential run.
// No config initialization, session resumption, or backend-wide qualification.
export function tlsB1Cases(fixtures) {
  const cases = [];
  const trusted = { NODE_EXTRA_CA_CERTS: fixtures.ca };
  for (const version of ['TLSv1', 'TLSv1.1']) {
    for (const [label, option, status] of [
      ['default-rejects', '', 502],
      ['legacy-level1', `--tls-min-v${version === 'TLSv1' ? '1.0' : '1.1'}`, 502],
      ['legacy-level0', `--tls-min-v${version === 'TLSv1' ? '1.0' : '1.1'} --tls-cipher-list=DEFAULT:@SECLEVEL=0`, 200],
      ['newer-min-rejects', '--tls-min-v1.2 --tls-cipher-list=DEFAULT:@SECLEVEL=0', 502],
      ['oldest-min-wins', '--tls-min-v1.2 --tls-min-v1.0 --tls-cipher-list=DEFAULT:@SECLEVEL=0', 200],
      ['negated-oldest', '--tls-min-v1.0 --no-tls-min-v1.0 --tls-cipher-list=DEFAULT:@SECLEVEL=0', 502],
    ]) cases.push({ id: `b1-${version}-${label}`, leaf: `only-${version}`, env: { ...trusted, NODE_OPTIONS: option }, inspect_handshake: true, expected_status: status });
  }
  for (const [index, option] of [
    '--tls-min-v1.0', '--tls-min-v1.1', '--tls_min_v1.1=false',
    '--tls-min-v1.0 --tls-max-v1.2', '--tls-min-v1.1 --tls-max-v1.2',
    '--tls-min-v1.1 --no-tls-min-v1.1',
  ].entries()) for (const version of ['TLSv1.2', 'TLSv1.3']) cases.push({ id: `b1-modern-peer-${index}-${version}`, leaf: `only-${version}`, env: { ...trusted, NODE_OPTIONS: option }, inspect_handshake: true });
  for (const [index, expression] of [
    'DEFAULT', 'DEFAULT:@SECLEVEL=0', 'DEFAULT:@SECLEVEL=1', 'DEFAULT:@SECLEVEL=2',
    'ECDHE-ECDSA-AES128-GCM-SHA256', 'ECDHE-RSA-AES128-GCM-SHA256',
    'TLS_AES_128_GCM_SHA256', 'TLS_AES_256_GCM_SHA384:ECDHE-ECDSA-AES128-GCM-SHA256',
    'TLS_AES_128_GCM_SHA256:DEFAULT:@SECLEVEL=2',
    'DEFAULT:BOGUS', '::DEFAULT::', '!TLS_AES_128_GCM_SHA256:DEFAULT',
  ].entries()) for (const version of ['TLSv1.2', 'TLSv1.3']) cases.push({ id: `b1-cipher-${index}-${version}`, leaf: `only-${version}`, env: { ...trusted, NODE_OPTIONS: `--tls-cipher-list=${expression}` }, inspect_handshake: true, allow_zero_attempts: index === 11 });
  for (const [index, option] of [
    '--tls-cipher-list=BOGUS', '--tls-cipher-list=::',
    '--tls-cipher-list=TLS_BOGUS', '--tls-cipher-list=TLS_AES_128_GCM_SHA256:BOGUS',
    '--tls-cipher-list="DEFAULT:@SECLEVEL=0"', '--tls_cipher_list DEFAULT',
    '--tls-cipher-list=BOGUS --tls-cipher-list=DEFAULT',
    '--tls-cipher-list=DEFAULT --tls-cipher-list=BOGUS',
    '--tls-cipher-list', '--tls-cipher-list=', '--no-tls-cipher-list',
    '--tls-cipher-list --tls-min-v1.3',
    '--no-tls-cipher-list=DEFAULT', '--no_tls_cipher_list=DEFAULT',
  ].entries()) cases.push({ id: `b1-cipher-operand-${index}`, env: { ...trusted, NODE_OPTIONS: option }, inspect_attempts: true });
  for (const leaf of ['rsa1024-leaf', 'rsa1024-leaf-tls12', 'sha1-leaf']) {
    for (const level of [0, 1, 2]) cases.push({ id: `b1-${leaf}-level${level}`, leaf, env: { ...trusted, NODE_OPTIONS: `--tls-cipher-list=DEFAULT:@SECLEVEL=${level}` }, inspect_attempts: true });
  }
  for (const [leaf, env] of [
    ['direct', {}], ['wrong-host', trusted], ['wrong-purpose', trusted],
    ['expired', { NODE_EXTRA_CA_CERTS: fixtures.expired }], ['unknown-critical', trusted],
  ]) for (const reject of ['0', '1', 'false', '00']) cases.push({ id: `b1-verification-${leaf}-${reject}`, leaf, env: { ...env, NODE_TLS_REJECT_UNAUTHORIZED: reject }, inspect_attempts: true, inspect_warning: true, expected_status: reject === '0' ? 200 : 502 });
  for (const version of ['TLSv1.2', 'TLSv1.3']) {
    cases.push({ id: `b1-warning-once-profiles-${version}`, leaf: `only-${version}`, env: { NODE_TLS_REJECT_UNAUTHORIZED: '0' }, inspect_profiles: true, inspect_warning: true, expected_status: 200 });
    for (const reject of ['0', '1']) cases.push({ id: `b1-tampered-handshake-${version}-${reject}`, leaf: `tampered-${version}`, env: { ...trusted, NODE_TLS_REJECT_UNAUTHORIZED: reject }, inspect_handshake: true, inspect_warning: true, inspect_tamper: true, expected_status: 502 });
  }
  cases.push({ id: 'b1-no-warning-without-tls', env: { NODE_TLS_REJECT_UNAUTHORIZED: '0' }, idle_only: true, inspect_warning: true, inspect_attempts: true, expected_status: 200 });
  return cases;
}
