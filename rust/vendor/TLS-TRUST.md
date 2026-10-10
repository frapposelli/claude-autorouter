# Native TLS trust contract

The native client preserves the 149 roots exported by the official Node
**22.14.0** release. This is a versioned build dependency, not a claim that every
Node 22/24 release or distributor uses the same roots. `verify-node-ca.mjs`
checks the PEM SHA-256 and every certificate's DER fingerprint. The Node license
and source provenance are in `node-ca/`.

Certificate-chain verification uses the pinned, statically built OpenSSL
**3.6.3** (`openssl-src 300.6.1+3.6.3`, `openssl-sys 0.9.117`, Rust bindings
`openssl 0.10.81`). The frozen reference uses OpenSSL **3.0.15+quic**. These are
different implementations; the differential fixtures qualify specific behavior,
not universal equivalence. A runtime unit test asserts the linked version number
`0x30600030`, and release builds must not disable Cargo's vendored backend.

## Verified policy

- Bundled roots are the default. Operating-system certificates, `SSL_CERT_FILE`,
  and `SSL_CERT_DIR` are not added in this mode.
- `NODE_EXTRA_CA_CERTS` loads ordinary PEM certificates once. Valid records before
  a malformed record remain loaded; AUX `TRUSTED CERTIFICATE` records are not
  treated as ordinary certificates. Diagnostics omit file paths and contents.
- `--use-openssl-ca` replaces the bundled set with OpenSSL file, hash-directory,
  and URI-store lookups; extra certificates still extend that store. The third
  lookup matters: a valid `hash.1` can be found despite an absent/broken `hash.0`.
  File contents are loaded once, while directory lookup remains lazy.
- Default paths match the official frozen Node build: `/etc/ssl/cert.pem` and
  `/etc/ssl/certs` on Linux, `/System/Library/OpenSSL/cert.pem` and
  `/System/Library/OpenSSL/certs` on macOS. Explicit environment paths override
  these. Empty, missing, or malformed sources do not fall back to bundled roots.
- Verification checks the complete chain, root validity, server-auth purpose,
  name constraints, and critical extensions. It uses OpenSSL level **1** and
  depth **100**, matching the frozen release, with `TRUSTED_FIRST` and without
  partial-chain or time/critical-extension bypass flags. No verification failure
  is accepted through a fallback. Node does not enable CRL checking or request
  OCSP by default; this implementation likewise does not enable either.
- Node's hostname/IP matching is preserved separately from chain verification,
  including first-label partial wildcards, common-name fallback only when DNS
  names are absent, IP SAN matching, and malformed/empty DNS-field presence.
- Rustls continues to drive TLS and ring verifies its existing supported keys.
  For ordinary RSA keys of **1024–2047 bits**, selected before verification,
  OpenSSL verifies the handshake signature because ring's RSA-PSS minimum is
  2048. RSA-PSS uses the advertised hash for both digest and MGF1, and a salt
  exactly the digest length. PKCS#1 signatures are allowed only in TLS 1.2.
  Tampering, a wrong message/key/digest/scheme/salt, and keys below 1024 bits fail.

## Version and P-521 qualification

The client additionally parses `--tls-min-v1.2`, `--tls-min-v1.3`,
`--tls-max-v1.2`, and `--tls-max-v1.3`. It preserves the frozen release's
independent Boolean flags, underscore aliases, negation, ignored Boolean values,
fixed minimum/maximum priority, and startup conflict diagnostic. The protocol
selection is passed to Rustls before building the connector. Default trust and
default TLS versions remain unchanged. Simultaneous trust-selector and version
conflicts produce both diagnostics in Node's order, each with the executable
prefix and exit status 9 before command dispatch.

The verifier also selects OpenSSL for P-521 handshake signatures: SHA-256,
SHA-384, or SHA-512 in TLS 1.2; only SHA-512 in TLS 1.3. Advertising the P-521
scheme also advertises ECDSA/SHA-512 for P-256/P-384 in TLS 1.2, so those two
new hash/curve combinations use the same selected verifier. Existing ring
combinations retain ring. The selection happens before signature verification;
errors never trigger a second verifier. Unit fixtures reject tampered and
malformed signatures, wrong messages/keys/digests, non-ECDSA schemes, and
TLS 1.3 curve/hash mismatches. The focused 13-test TLS suite and the preserved
104-case startup differential pass.

The local handshake matrix restricts peer protocol versions and signature
schemes, records connection attempts, and requires fresh, non-resumed
handshakes. All 115 qualified cases pass, including TLS 1.2/1.3 bounds and the
forced hash/curve combinations. Four P-521 TLS 1.2 cases remain explicit known
differences: Node succeeds, while the native client rejects the handshake once,
without a retry. OpenSSL checks the certificate curve against the client's
supported groups; ring has no P-521 key exchange. Signature verification alone
does not close that negotiation gap, and the client does not advertise an
unsupported key-exchange group to bypass it. The failed discovery report and
previous characterization reports are retained under content-derived names in
`artifacts/rust-rewrite/evidence/`.

## Remaining qualification boundaries

The supported-selector gate is `parity/check-tls-options.mjs`, producing
`artifacts/rust-rewrite/parity-tls-options.json`. It separately writes
`tls-policy-characterization.json`, retaining known differences for unsupported
TLS policy modes and curves. The latter is not passing TLS-parity evidence.
Neither report qualifies real providers or reads user certificate files: all
certificate sources and network peers are isolated synthetic fixtures.

`OPENSSL_CONF`, `--openssl-config`, `--openssl-shared-config`, cipher-list overrides,
TLS 1.0/1.1 minimum overrides, FIPS flags, and `NODE_TLS_REJECT_UNAUTHORIZED=0` remain
explicit unsupported modes in the native transport. Together with the four
P-521 TLS 1.2 cases, these account for 11 unqualified characterization rows;
they are not counted in the 115 passing cases. Other curves, RSA-PSS-constrained keys,
revocation overrides, custom Node builds, and later Node system-root behavior
also need separate evidence. The frozen version ignores `NODE_USE_SYSTEM_CA` and
rejects `--use-system-ca` before application dispatch; those frozen behaviors do
not imply support for later system-root modes. Non-UTF-8 OpenSSL file/directory
paths are explicitly unsupported rather than silently selecting default roots.

The small local Rust OpenSSL binding patch exposes initialization without implicit
configuration loading, raw DNS bytes, partial PEM success, and the public
URI-store lookup. The initializer runs before store creation and never mutates
the process environment. Embedding callers must invoke
`openssl::init_without_config()` before any independent OpenSSL use; already
loaded global configuration cannot be undone. An isolated subprocess regression
compares explicit initialization with ordinary initialization under a private
restrictive config, including a negative control. The complete local patch changes five upstream files and contains no alternate
certificate verification. `openssl-provenance.json`,
`openssl-node-trust.patch`, and `verify-openssl.mjs` reconstruct and hash-check all
112 original crate files. This patch requires local review; upstream has not
approved or committed to maintaining it. The upstream Apache-2.0 license remains
in `openssl/LICENSE`.

## Context-bound session and store APIs

The B2 binding extension adds an immutable `ContextBoundSession` created only
by the actual new-session callback, a guarded safe setter, and
`X509StoreRef::try_clone`. The session callback and setter both inspect the
binding's retained original `SESSION_CTX_INDEX` context and refuse a currently
swapped context. A snapshot retains that original context and decodes a new
native session per installation. Callers cannot construct snapshots from
arbitrary bytes, extract their secret DER, or Debug-format them. Typed errors
carry no secret data. The store helper shares one existing store through
OpenSSL's public checked reference-count increment; it adds no trust sources.

These APIs do not enable a production OpenSSL transport or session cache.
A snapshot proves context ownership, not peer acceptance: runtime integration
must delay publication until verification/hostname checks pass and isolate
origin, profile and policy generations. Callbacks that retain snapshots must
capture caches weakly to avoid a context/callback/cache cycle. Private DER uses
ordinary heap storage, without an additional guaranteed-wiping mechanism;
`encoded_len` supports explicit future cache byte budgets. Configuration
initialization, runtime cache behavior, pooling and lifecycle tests remain
separate gates. Targeted vendor ownership/doc-tests are maintained with the
binding patch and require a dedicated pinned-backend test invocation, since
vendor crates are excluded from the application workspace.

The targeted suite passes seven ownership tests on OpenSSL3.6.3, including
clean and abrupt close followed by real TLS1.2/1.3 resumption, original/current
context rejection, independent concurrent installations, callback lifetime,
and shared-store ownership. Three compile-fail doc tests reject forged byte
construction, private-field access and Debug formatting. Tests use an isolated
copy with the application dependency versions: the original upstream test lock
uses OpenSSL3.6.2 and is retained unchanged for source provenance. The only new
active test dependency is its already-declared `hex0.4.3` dev dependency.

`openssl-node-trust.patch` is a byte-preserved unified-diff artifact. Its blank
context lines require a single-space prefix; the directory's `.gitattributes`
disables only the end-of-line-space check for that exact patch file. Source
whitespace checks and the patch's context/hash verification remain intact.

Primary contracts: [Node root-store construction](https://github.com/nodejs/node/blob/v22.14.0/src/crypto/crypto_context.cc),
[Node hostname matching](https://github.com/nodejs/node/blob/v22.14.0/lib/tls.js),
[Node option implications](https://github.com/nodejs/node/blob/v22.14.0/src/node_options.cc),
[Node OpenSSL build paths](https://github.com/nodejs/node/blob/v22.14.0/deps/openssl/openssl_common.gypi),
[OpenSSL 3.0 default store loaders](https://github.com/openssl/openssl/blob/openssl-3.0.15/crypto/x509/x509_d2.c),
[OpenSSL security levels](https://docs.openssl.org/3.0/man3/SSL_CTX_set_security_level/).
