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

## Remaining qualification boundaries

The supported-selector gate is `parity/check-tls-options.mjs`, producing
`artifacts/rust-rewrite/parity-tls-options.json`. It separately writes
`tls-policy-characterization.json`, retaining known differences for unsupported
TLS policy modes and curves. The latter is not passing TLS-parity evidence.
Neither report qualifies real providers or reads user certificate files: all
certificate sources and network peers are isolated synthetic fixtures.

`OPENSSL_CONF`, `--openssl-config`, `--openssl-shared-config`, cipher-list and TLS
version overrides, FIPS flags, and `NODE_TLS_REJECT_UNAUTHORIZED=0` remain explicit
unsupported modes in the native transport. P-521 end-entity handshake signatures
remain unqualified by the ring provider. Other curves, RSA-PSS-constrained keys,
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
restrictive config, including a negative control. It changes three upstream files and
contains no alternate certificate verification. `openssl-provenance.json`,
`openssl-node-trust.patch`, and `verify-openssl.mjs` reconstruct and hash-check all
112 original crate files. This patch requires local review; upstream has not
approved or committed to maintaining it. The upstream Apache-2.0 license remains
in `openssl/LICENSE`.

Primary contracts: [Node root-store construction](https://github.com/nodejs/node/blob/v22.14.0/src/crypto/crypto_context.cc),
[Node hostname matching](https://github.com/nodejs/node/blob/v22.14.0/lib/tls.js),
[Node option implications](https://github.com/nodejs/node/blob/v22.14.0/src/node_options.cc),
[Node OpenSSL build paths](https://github.com/nodejs/node/blob/v22.14.0/deps/openssl/openssl_common.gypi),
[OpenSSL 3.0 default store loaders](https://github.com/openssl/openssl/blob/openssl-3.0.15/crypto/x509/x509_d2.c),
[OpenSSL security levels](https://docs.openssl.org/3.0/man3/SSL_CTX_set_security_level/).
