//! Explicit fresh-handshake policy for the pinned reference. Configuration
//! initializers and session caches deliberately remain outside stage A.
use std::path::{Path, PathBuf};

use openssl::ssl::{
    SslContext, SslContextBuilder, SslMethod, SslMode, SslOptions, SslSessionCacheMode,
    SslVerifyMode, SslVersion,
};
use openssl::x509::{X509PurposeId, verify::X509VerifyFlags};

use super::super::{Warning, default_paths, load};
use super::options::Options;
use crate::http_client::HttpError;
use crate::tls_policy::Mode;

// src/node_constants.h at v22.14.0, no custom build cipher override.
const CIPHERS13: &str =
    "TLS_AES_256_GCM_SHA384:TLS_CHACHA20_POLY1305_SHA256:TLS_AES_128_GCM_SHA256";
const CIPHERS12: &str = concat!(
    "ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES128-GCM-SHA256:",
    "ECDHE-RSA-AES256-GCM-SHA384:ECDHE-ECDSA-AES256-GCM-SHA384:",
    "DHE-RSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-SHA256:DHE-RSA-AES128-SHA256:",
    "ECDHE-RSA-AES256-SHA384:DHE-RSA-AES256-SHA384:ECDHE-RSA-AES256-SHA256:",
    "DHE-RSA-AES256-SHA256:HIGH:!aNULL:!eNULL:!EXPORT:!DES:!RC4:!MD5:!PSK:!SRP:!CAMELLIA"
);
// OpenSSL 3.0.15 ssl/t1_lib.c, default provider's available groups. Explicit
// selection prevents OpenSSL 3.6 silently adding ML-KEM or changing preferences.
const GROUPS: &str =
    "X25519:P-256:X448:P-521:P-384:ffdhe2048:ffdhe3072:ffdhe4096:ffdhe6144:ffdhe8192";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Profile {
    Raw,
    Fetch,
}

#[derive(Clone)]
pub(super) struct Context {
    pub tls: SslContext,
    // Node creates a secure context on connection, so an invalid cipher
    // expression fails that request before opening a socket, not gateway boot.
    pub valid_ciphers: bool,
}

pub(super) fn context(profile: Profile) -> Result<Context, HttpError> {
    let policy = Options::process()?;
    let extra = std::env::var_os("NODE_EXTRA_CA_CERTS").filter(|value| !value.is_empty());
    let (default_file, default_directory) = default_paths();
    let file = std::env::var_os("SSL_CERT_FILE").map_or(default_file, PathBuf::from);
    let directory = match std::env::var("SSL_CERT_DIR") {
        Ok(directory) => directory,
        Err(std::env::VarError::NotPresent) => default_directory.into(),
        Err(_) if policy.mode == Mode::Bundled => default_directory.into(),
        Err(_) => return Err(HttpError::UnsupportedTrustOptions),
    };
    if policy.mode == Mode::OpenSsl && file.to_str().is_none() {
        return Err(HttpError::UnsupportedTrustOptions);
    }
    let (store, warning) = load(
        policy.mode,
        extra.as_deref().map(Path::new),
        &file,
        &directory,
    )?;
    match warning {
        Some(Warning::UnreadableExtra) => eprintln!(
            "Warning: Ignoring extra certificates from NODE_EXTRA_CA_CERTS: file could not be read."
        ),
        Some(Warning::MalformedExtra) => eprintln!(
            "Warning: Ignoring remaining certificates from NODE_EXTRA_CA_CERTS: malformed certificate data."
        ),
        None => {}
    }
    let failure = |_| HttpError::CertificateRoots;
    let mut context = SslContextBuilder::new(SslMethod::tls_client()).map_err(failure)?;
    context.set_security_level(1);
    context.set_options(SslOptions::NO_SSLV2 | SslOptions::NO_SSLV3 | SslOptions::NO_COMPRESSION);
    context.set_mode(
        SslMode::ACCEPT_MOVING_WRITE_BUFFER
            | SslMode::ENABLE_PARTIAL_WRITE
            | SslMode::AUTO_RETRY
            | SslMode::RELEASE_BUFFERS,
    );
    context
        .set_min_proto_version(Some(policy.minimum))
        .map_err(failure)?;
    context
        .set_max_proto_version(Some(policy.maximum))
        .map_err(failure)?;
    // The OpenSSL default TLS1.3 suites remain when the expression has no
    // TLS_ terms. Pin that default to the reference before applying overrides.
    context.set_ciphersuites(CIPHERS13).map_err(failure)?;
    let valid_ciphers = configure_ciphers(&mut context, &policy).is_ok();
    context.set_groups_list(GROUPS).map_err(failure)?;
    context.set_cert_store(store);
    context.set_verify(SslVerifyMode::PEER);
    context.set_verify_depth(100);
    let parameters = context.verify_param_mut();
    // Let libssl derive authentication strength from the effective cipher
    // security level; a fixed auth_level would override explicit @SECLEVEL.
    parameters
        .set_purpose(X509PurposeId::SSL_SERVER)
        .map_err(failure)?;
    parameters
        .set_flags(X509VerifyFlags::TRUSTED_FIRST)
        .map_err(failure)?;
    // Fresh handshakes only: no internal storage or application session cache.
    context.set_session_cache_mode(SslSessionCacheMode::OFF);
    if profile == Profile::Fetch {
        context.set_alpn_protos(b"\x08http/1.1").map_err(failure)?;
    }
    Ok(Context {
        tls: context.build(),
        valid_ciphers,
    })
}

fn configure_ciphers(context: &mut SslContextBuilder, policy: &Options) -> Result<(), ()> {
    let defaults = format!("{CIPHERS13}:{CIPHERS12}");
    let expression = policy.ciphers.as_deref().unwrap_or(&defaults);
    // Safe setters use CString::new(...).unwrap(); reject interior NUL before
    // crossing that boundary. Environment values cannot contain it, but tests can.
    if expression.contains('\0') {
        return Err(());
    }
    let (suites, legacy): (Vec<_>, Vec<_>) = expression
        .split(':')
        .filter(|term| !term.is_empty())
        .partition(|term| term.starts_with("TLS_") || term.starts_with("!TLS_"));
    if suites.is_empty() && legacy.is_empty() {
        return Err(());
    }
    if !suites.is_empty() {
        context
            .set_ciphersuites(&suites.join(":"))
            .map_err(|_| ())?;
    }
    let legacy = legacy.join(":");
    if let Err(error) = context.set_cipher_list(&legacy) {
        // openssl-src3.6.3 include/openssl/{err.h.in,sslerr.h}: ERR_LIB_SSL=20,
        // SSL_R_NO_CIPHER_MATCH=185. Only deliberate empty legacy-list clearing
        // may suppress this exact error; no fallback for invalid expressions.
        if !legacy.is_empty()
            || error.errors().is_empty()
            || !error
                .errors()
                .iter()
                .all(|error| error.library_code() == 20 && error.reason_code() == 185)
        {
            return Err(());
        }
    }
    if legacy.is_empty()
        && policy.minimum != SslVersion::TLS1_3
        && policy.maximum == SslVersion::TLS1_3
    {
        context
            .set_min_proto_version(Some(SslVersion::TLS1_3))
            .map_err(|_| ())?;
    }
    Ok(())
}

#[test]
fn cipher_errors_do_not_fallback_or_override_selected_security_level() {
    openssl::init_without_config().unwrap();
    for (expression, level) in [
        ("DEFAULT:@SECLEVEL=0", 0),
        ("DEFAULT:@SECLEVEL=2", 2),
        ("TLS_AES_128_GCM_SHA256", 1),
    ] {
        let policy = Options::parse(&format!("--tls-cipher-list={expression}")).unwrap();
        let mut context = SslContextBuilder::new(SslMethod::tls_client()).unwrap();
        context.set_security_level(1);
        configure_ciphers(&mut context, &policy).unwrap();
        assert_eq!(context.build().security_level(), level);
    }
    for expression in [
        "BOGUS",
        "::",
        "DEFAULT\0",
        "TLS_BOGUS",
        "TLS_AES_128_GCM_SHA256:BOGUS",
    ] {
        let policy = Options::parse(&format!("--tls-cipher-list={expression}")).unwrap();
        let mut context = SslContextBuilder::new(SslMethod::tls_client()).unwrap();
        assert!(
            configure_ciphers(&mut context, &policy).is_err(),
            "{expression:?}"
        );
    }
}
