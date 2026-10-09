# Explicit native TLS root dependency

`node-v22.14.0.pem` contains the 149 public Mozilla CA certificates bundled with
the frozen Node 22.14.0 baseline. It was exported with `tls.rootCertificates` and
every ordered DER fingerprint was compared with the official
[`src/node_root_certs.h`](https://github.com/nodejs/node/blob/v22.14.0/src/node_root_certs.h).
The source SHA-256, bundle hash, certificate subjects/fingerprints and original
Node license hash are retained in `provenance.json`. Run
`node rust/vendor/verify-node-ca.mjs` to verify the retained files offline.

This root set is an explicit native-build dependency, not a claim that all Node
22/24 releases or operating-system stores contain the same certificates. Audit
and refresh it before cutover and when updating the frozen reference. Updating
requires recording the new official source, comparing every DER certificate,
reviewing trust additions/removals, rerunning isolated TLS fixtures and updating
the native package's third-party notices.

The native client adds `NODE_EXTRA_CA_CERTS` once at process initialization.
Unreadable or malformed extra files emit a sanitized warning; certificates parsed
before the first malformed record remain available. `SSL_CERT_FILE` and
`SSL_CERT_DIR` do not implicitly alter bundled-mode trust. No operating-system
root union is performed.

Alternate trust modes and TLS policy flags currently produce an explicit
unsupported-options diagnostic rather than silently changing trust. These
include OpenSSL/system trust selectors, `NODE_USE_SYSTEM_CA=1`, certificate
verification disablement, TLS version/cipher overrides and FIPS selectors.
Their platform-specific semantics remain a separate compatibility gate. This
bundle alone does not establish full OpenSSL/Rustls verification equivalence.
