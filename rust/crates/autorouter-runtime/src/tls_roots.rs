//! Frozen Node 22.14.0 trust policy with full OpenSSL certificate validation.
//!
//! Rustls retains the handshake and CertificateVerify signature checks. A CA
//! certificate is deliberately not reduced to a WebPKI trust anchor: doing so
//! loses root expiry, complete-chain requirements, and OpenSSL trust metadata.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use openssl::nid::Nid;
use openssl::ssl::SslFiletype;
use openssl::stack::Stack;
use openssl::x509::store::{X509Lookup, X509Store, X509StoreBuilder};
use openssl::x509::verify::{X509VerifyFlags, X509VerifyParam};
use openssl::x509::{X509, X509PurposeId, X509StoreContext};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error, SignatureScheme};

const BUNDLED: &[u8] = include_bytes!("../../../vendor/node-ca/node-v22.14.0.pem");

use crate::tls_policy::{Mode, process_policy};
pub use crate::tls_policy::{TrustError, startup_diagnostic, startup_error};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Warning {
    UnreadableExtra,
    MalformedExtra,
}

// Official frozen Node binaries use these OPENSSLDIR settings. Do not inherit
// the vendored OpenSSL build machine's paths or implicitly union platform roots.
fn default_paths() -> (PathBuf, &'static str) {
    if cfg!(target_os = "macos") {
        (
            "/System/Library/OpenSSL/cert.pem".into(),
            "/System/Library/OpenSSL/certs",
        )
    } else {
        ("/etc/ssl/cert.pem".into(), "/etc/ssl/certs")
    }
}

fn load(
    mode: Mode,
    extra: Option<&Path>,
    file: &Path,
    directory: &str,
) -> Result<(X509Store, Option<Warning>), TrustError> {
    let failure = |_| TrustError::InvalidBundle;
    openssl::init_without_config().map_err(failure)?;
    let mut store = X509StoreBuilder::new().map_err(failure)?;
    let mut parameters = X509VerifyParam::new().map_err(failure)?;
    parameters
        .set_purpose(X509PurposeId::SSL_SERVER)
        .map_err(failure)?;
    parameters
        .set_flags(X509VerifyFlags::TRUSTED_FIRST)
        .map_err(failure)?;
    // OpenSSL 3.0's TLS default is level 1 (changed to 2 in 3.2). Its ordinary
    // X509_STORE_CTX default is 0, so omitting this would weaken Node policy.
    parameters.set_auth_level(1);
    parameters.set_depth(100);
    store.set_param(&parameters).map_err(failure)?;
    if mode == Mode::Bundled {
        for certificate in X509::stack_from_pem(BUNDLED).map_err(failure)? {
            store.add_cert(certificate).map_err(failure)?;
        }
    } else {
        // OpenSSL owns AUX trust interpretation and hashed-directory lookup.
        // Missing/empty/bad sources are nonfatal, exactly like default_paths.
        // File material is loaded now; directory material is loaded on demand.
        if let Some(path) = file.to_str().filter(|path| !path.contains('\0')) {
            let _ = store
                .add_lookup(X509Lookup::file())
                .map_err(failure)?
                .load_cert_file(path, SslFiletype::PEM);
        }
        if !directory.contains('\0') {
            let _ = store
                .add_lookup(X509Lookup::hash_dir())
                .map_err(failure)?
                .add_dir(directory, SslFiletype::PEM);
            // Default paths also use the URI loader: unlike hash_dir alone it
            // can find hash.1 when hash.0 is missing or malformed. Preserve the
            // original directory-list string for OpenSSL's URI interpretation.
            if let Ok(uri) = std::ffi::CString::new(directory) {
                let _ = store
                    .add_lookup(X509Lookup::store())
                    .map_err(failure)?
                    .add_store(&uri);
            }
        }
    }
    let Some(extra) = extra else {
        return Ok((store.build(), None));
    };
    let pem = match std::fs::read(extra) {
        Ok(pem) => pem,
        Err(_) => return Ok((store.build(), Some(Warning::UnreadableExtra))),
    };
    // Extra certificates use ordinary PEM X509 certificates, not the AUX trust
    // loader. Keep valid preceding records if a later record is malformed.
    let (certificates, parse_error) = X509::stack_from_pem_partial(&pem).map_err(failure)?;
    for certificate in certificates {
        if store.add_cert(certificate).is_err() {
            return Ok((store.build(), Some(Warning::MalformedExtra)));
        }
    }
    Ok((store.build(), parse_error.map(|_| Warning::MalformedExtra)))
}

pub struct NodeCertVerifier {
    store: X509Store,
    algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
}

impl fmt::Debug for NodeCertVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NodeCertVerifier")
            .finish_non_exhaustive()
    }
}

// This implements Node's checkServerIdentity name matching over parsed ASN.1
// names. OpenSSL still verifies signatures, constraints, EKU, dates and trust.
fn dns_matches(host: &str, pattern: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    let pattern = pattern
        .strip_suffix('.')
        .unwrap_or(pattern)
        .to_ascii_lowercase();
    let host: Vec<_> = host.split('.').collect();
    let pattern: Vec<_> = pattern.split('.').collect();
    if host.len() != pattern.len()
        || pattern
            .iter()
            .any(|part| part.is_empty() || part.bytes().any(|byte| !(0x21..=0x7f).contains(&byte)))
        || host[1..] != pattern[1..]
    {
        return false;
    }
    let Some((prefix, suffix)) = pattern[0].split_once('*') else {
        return host[0] == pattern[0];
    };
    if pattern[0].contains("xn--") {
        return host[0] == pattern[0];
    }
    pattern.len() > 2
        && !suffix.contains('*')
        && prefix.len() + suffix.len() <= host[0].len()
        && host[0].starts_with(prefix)
        && host[0].ends_with(suffix)
}

fn valid_identity(certificate: &X509, server_name: &ServerName<'_>) -> bool {
    let names = certificate.subject_alt_names();
    match server_name {
        ServerName::IpAddress(address) => {
            let address: std::net::IpAddr = (*address).into();
            names.as_ref().is_some_and(|names| {
                names.iter().any(|name| {
                    name.ipaddress().is_some_and(|bytes| match address {
                        std::net::IpAddr::V4(address) => bytes == address.octets(),
                        std::net::IpAddr::V6(address) => bytes == address.octets(),
                    })
                })
            })
        }
        ServerName::DnsName(host) => {
            let dns: Vec<_> = names
                .as_ref()
                .map(|names| {
                    names
                        .iter()
                        .filter_map(|name| name.dnsname_bytes())
                        .collect()
                })
                .unwrap_or_default();
            if !dns.is_empty() {
                return dns.into_iter().any(|name| {
                    std::str::from_utf8(name).is_ok_and(|name| dns_matches(host.as_ref(), name))
                });
            }
            certificate
                .subject_name()
                .entries_by_nid(Nid::COMMONNAME)
                .any(|entry| {
                    entry
                        .data()
                        .to_string()
                        .ok()
                        .is_some_and(|name| dns_matches(host.as_ref(), &name))
                })
        }
        _ => false,
    }
}

impl ServerCertVerifier for NodeCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let bad_encoding = |_| Error::InvalidCertificate(CertificateError::BadEncoding);
        let certificate = X509::from_der(end_entity).map_err(bad_encoding)?;
        let mut chain = Stack::new().map_err(bad_encoding)?;
        for intermediate in intermediates {
            chain
                .push(X509::from_der(intermediate).map_err(bad_encoding)?)
                .map_err(bad_encoding)?;
        }
        let mut context = X509StoreContext::new().map_err(bad_encoding)?;
        // No PARTIAL_CHAIN, NO_CHECK_TIME, IGNORE_CRITICAL, or callback override.
        // Node doesn't request OCSP or enable CRL checking by default either.
        let valid = context
            .init(&self.store, &certificate, &chain, |context| {
                context.verify_cert()
            })
            .map_err(bad_encoding)?;
        if !valid {
            return Err(Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ));
        }
        if !valid_identity(&certificate, server_name) {
            return Err(Error::InvalidCertificate(CertificateError::NotValidForName));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        if let Some(valid) =
            verify_openssl_signature(message, cert, dss.scheme, dss.signature(), false)?
        {
            return Ok(valid);
        }
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        if let Some(valid) =
            verify_openssl_signature(message, cert, dss.scheme, dss.signature(), true)?
        {
            return Ok(valid);
        }
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        let mut schemes = self.algorithms.supported_schemes();
        // Preserve the existing preference order. In TLS 1.2 this new scheme
        // also advertises ECDSA/SHA512 with the already supported EC curves.
        schemes.push(SignatureScheme::ECDSA_NISTP521_SHA512);
        schemes
    }
}

// Select the vetted OpenSSL verifier by key type/size/curve and scheme before
// verifying; a failed signature is never retried elsewhere. Node's OpenSSL 3.0
// level-1 policy permits 1024..2047-bit RSA keys (ring PSS starts at 2048), and
// the ECDSA extension below handles only the combinations missing from ring.
fn verify_openssl_signature(
    message: &[u8],
    certificate: &CertificateDer<'_>,
    scheme: SignatureScheme,
    signature: &[u8],
    tls13: bool,
) -> Result<Option<HandshakeSignatureValid>, Error> {
    use openssl::hash::MessageDigest;
    use openssl::rsa::Padding;
    use openssl::sign::{RsaPssSaltlen, Verifier};
    let bad_signature = || Error::InvalidCertificate(CertificateError::BadSignature);
    let certificate = X509::from_der(certificate)
        .map_err(|_| Error::InvalidCertificate(CertificateError::BadEncoding))?;
    let key = certificate.public_key().map_err(|_| bad_signature())?;
    if key.id() == openssl::pkey::Id::EC {
        return verify_extended_ecdsa(message, &key, scheme, signature, tls13);
    }
    if key.id() != openssl::pkey::Id::RSA || key.bits() >= 2048 {
        return Ok(None);
    }
    if key.bits() < 1024 {
        return Err(bad_signature());
    }
    let (digest, pss) = match scheme {
        SignatureScheme::RSA_PSS_SHA256 => (MessageDigest::sha256(), true),
        SignatureScheme::RSA_PSS_SHA384 => (MessageDigest::sha384(), true),
        SignatureScheme::RSA_PSS_SHA512 => (MessageDigest::sha512(), true),
        SignatureScheme::RSA_PKCS1_SHA256 if !tls13 => (MessageDigest::sha256(), false),
        SignatureScheme::RSA_PKCS1_SHA384 if !tls13 => (MessageDigest::sha384(), false),
        SignatureScheme::RSA_PKCS1_SHA512 if !tls13 => (MessageDigest::sha512(), false),
        _ => return Err(bad_signature()),
    };
    let mut verifier = Verifier::new(digest, &key).map_err(|_| bad_signature())?;
    verifier
        .set_rsa_padding(if pss {
            Padding::PKCS1_PSS
        } else {
            Padding::PKCS1
        })
        .map_err(|_| bad_signature())?;
    if pss {
        verifier
            .set_rsa_mgf1_md(digest)
            .map_err(|_| bad_signature())?;
        verifier
            .set_rsa_pss_saltlen(RsaPssSaltlen::DIGEST_LENGTH)
            .map_err(|_| bad_signature())?;
    }
    if verifier
        .verify_oneshot(signature, message)
        .map_err(|_| bad_signature())?
    {
        Ok(Some(HandshakeSignatureValid::assertion()))
    } else {
        Err(bad_signature())
    }
}

// Select only combinations missing from ring. TLS 1.2's ECDSA scheme specifies
// the digest, not the key curve; TLS 1.3 fixes both. This distinction is also
// present in the pinned rustls ring provider's verification mapping. Once a
// key/scheme is selected here, a failed signature never falls through to ring.
fn verify_extended_ecdsa(
    message: &[u8],
    key: &openssl::pkey::PKeyRef<openssl::pkey::Public>,
    scheme: SignatureScheme,
    signature: &[u8],
    tls13: bool,
) -> Result<Option<HandshakeSignatureValid>, Error> {
    use openssl::hash::MessageDigest;
    use openssl::sign::Verifier;
    let bad_signature = || Error::InvalidCertificate(CertificateError::BadSignature);
    let curve = key
        .ec_key()
        .map_err(|_| bad_signature())?
        .group()
        .curve_name();
    let p521 = curve == Some(Nid::SECP521R1);
    let existing_curve = matches!(curve, Some(Nid::X9_62_PRIME256V1 | Nid::SECP384R1));
    if !p521 && !(existing_curve && scheme == SignatureScheme::ECDSA_NISTP521_SHA512) {
        return Ok(None);
    }
    if tls13 && (!p521 || scheme != SignatureScheme::ECDSA_NISTP521_SHA512) {
        return Err(bad_signature());
    }
    let digest = match scheme {
        SignatureScheme::ECDSA_NISTP256_SHA256 if !tls13 => MessageDigest::sha256(),
        SignatureScheme::ECDSA_NISTP384_SHA384 if !tls13 => MessageDigest::sha384(),
        SignatureScheme::ECDSA_NISTP521_SHA512 => MessageDigest::sha512(),
        _ => return Err(bad_signature()),
    };
    if Verifier::new(digest, key)
        .and_then(|mut verifier| verifier.verify_oneshot(signature, message))
        .map_err(|_| bad_signature())?
    {
        Ok(Some(HandshakeSignatureValid::assertion()))
    } else {
        Err(bad_signature())
    }
}

pub fn process_verifier() -> Result<Arc<NodeCertVerifier>, TrustError> {
    static VERIFIER: OnceLock<Result<Arc<NodeCertVerifier>, TrustError>> = OnceLock::new();
    VERIFIER.get_or_init(|| {
        let mode = process_policy()?.mode;
        let extra = std::env::var_os("NODE_EXTRA_CA_CERTS").filter(|value| !value.is_empty());
        let (default_file, default_directory) = default_paths();
        let file = std::env::var_os("SSL_CERT_FILE").map_or(default_file, PathBuf::from);
        let directory = match std::env::var("SSL_CERT_DIR") {
            Ok(directory) => directory,
            Err(std::env::VarError::NotPresent) => default_directory.into(),
            Err(_) if mode == Mode::Bundled => default_directory.into(),
            Err(_) => return Err(TrustError::UnsupportedOptions),
        };
        // The safe upstream path APIs require UTF-8. An unsupported path must
        // never silently select a wider default trust directory.
        if mode == Mode::OpenSsl && file.to_str().is_none() {
            return Err(TrustError::UnsupportedOptions);
        }
        let (store, warning) = load(mode, extra.as_deref().map(Path::new), &file, &directory)?;
        match warning {
            Some(Warning::UnreadableExtra) => eprintln!("Warning: Ignoring extra certificates from NODE_EXTRA_CA_CERTS: file could not be read."),
            Some(Warning::MalformedExtra) => eprintln!("Warning: Ignoring remaining certificates from NODE_EXTRA_CA_CERTS: malformed certificate data."),
            None => {}
        }
        Ok(Arc::new(NodeCertVerifier { store,
            algorithms: rustls::crypto::ring::default_provider().signature_verification_algorithms }))
    }).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_is_the_reviewed_vendored_release() {
        openssl::init_without_config().unwrap();
        assert_eq!(openssl::version::number(), 0x3060_0030);
    }

    #[tokio::test]
    async fn initialization_ignores_external_config_before_loading_crypto() {
        const CHILD: &str = "AUTOROUTER_SYNTHETIC_TLS_INIT";
        if let Ok(mode) = std::env::var(CHILD) {
            if mode == "disabled" {
                openssl::init_without_config().unwrap();
            } else {
                openssl::init();
            }
            let hash = openssl::hash::hash(openssl::hash::MessageDigest::sha256(), b"synthetic");
            assert_eq!(hash.is_ok(), mode == "disabled");
            return;
        }
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "autorouter-tls-init-{}-{nonce}",
            std::process::id()
        )));
        std::fs::create_dir(&scratch.0).unwrap();
        let config = scratch.0.join("synthetic.cnf");
        // No external provider is loaded: this property query excludes the
        // built-in provider. The negative control proves this file is active
        // if initialization accidentally uses normal OpenSSL defaults.
        std::fs::write(&config, "openssl_conf=init\n[init]\nalg_section=algorithms\n[algorithms]\ndefault_properties=fips=yes\n").unwrap();
        for mode in ["disabled", "normal"] {
            let output = tokio::time::timeout(std::time::Duration::from_secs(10),
                tokio::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "tls_roots::tests::initialization_ignores_external_config_before_loading_crypto", "--nocapture"])
                    .env_clear().env(CHILD, mode).env("OPENSSL_CONF", &config)
                    .kill_on_drop(true).output()).await.unwrap().unwrap();
            assert!(
                output.status.success(),
                "{mode}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn bundle_and_missing_extra_preserve_exact_root_set_without_os_union() {
        let (store, warning) =
            load(Mode::Bundled, None, Path::new("/synthetic-unread"), "").unwrap();
        assert_eq!(store.all_certificates().len(), 149);
        assert_eq!(warning, None);
        let absent =
            std::env::temp_dir().join(format!("synthetic-absent-ca-{}", std::process::id()));
        let (store, warning) =
            load(Mode::Bundled, Some(&absent.join("absent.pem")), &absent, "").unwrap();
        assert_eq!(store.all_certificates().len(), 149);
        assert_eq!(warning, Some(Warning::UnreadableExtra));
    }

    #[test]
    fn hostname_policy_preserves_node_partial_wildcards_and_name_boundaries() {
        for (host, pattern, expected) in [
            ("LOCAL.Example.com.", "local.example.com", true),
            ("local.example.com", "lo*.example.com", true),
            ("local.example.com", "*cal.example.com", true),
            ("local.example.com", "l*l.example.com", true),
            ("a.example.com", "*.example.com.", true),
            ("a.b.example.com", "*.example.com", false),
            ("example.com", "*.com", false),
            ("xn--abc.example.com", "xn--*.example.com", false),
            ("a.example.com", "**.example.com", false),
            ("a..com", "a..com", false),
            ("a.example.com", "a.example.com\0", false),
        ] {
            assert_eq!(dns_matches(host, pattern), expected, "{host} {pattern}");
        }
    }
    fn fixture_key() -> openssl::pkey::PKey<openssl::pkey::Private> {
        openssl::init_without_config().unwrap();
        let group = openssl::ec::EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
        openssl::pkey::PKey::from_ec_key(openssl::ec::EcKey::generate(&group).unwrap()).unwrap()
    }

    fn fixture_certificate(
        key: &openssl::pkey::PKey<openssl::pkey::Private>,
        issuer: Option<(&X509, &openssl::pkey::PKey<openssl::pkey::Private>)>,
        common_name: &str,
        ca: bool,
        expired: bool,
        san: Option<&[u8]>,
        client_only: bool,
    ) -> X509 {
        use openssl::asn1::{Asn1Integer, Asn1Object, Asn1OctetString, Asn1Time};
        use openssl::bn::BigNum;
        use openssl::x509::extension::{BasicConstraints, ExtendedKeyUsage, KeyUsage};
        let mut name = openssl::x509::X509NameBuilder::new().unwrap();
        name.append_entry_by_nid(Nid::COMMONNAME, common_name)
            .unwrap();
        let name = name.build();
        let mut certificate = X509::builder().unwrap();
        certificate.set_version(2).unwrap();
        certificate
            .set_serial_number(&Asn1Integer::from_bn(&BigNum::from_u32(1).unwrap()).unwrap())
            .unwrap();
        certificate.set_subject_name(&name).unwrap();
        certificate
            .set_issuer_name(issuer.map_or(&*name, |(cert, _)| cert.subject_name()))
            .unwrap();
        certificate.set_pubkey(key).unwrap();
        certificate
            .set_not_before(&Asn1Time::from_unix(1).unwrap())
            .unwrap();
        let expires = if expired {
            Asn1Time::from_unix(2).unwrap()
        } else {
            Asn1Time::days_from_now(1).unwrap()
        };
        certificate.set_not_after(&expires).unwrap();
        let basic = if ca {
            BasicConstraints::new().critical().ca().build()
        } else {
            BasicConstraints::new().critical().build()
        };
        certificate.append_extension(basic.unwrap()).unwrap();
        let usage = if ca {
            KeyUsage::new()
                .critical()
                .key_cert_sign()
                .crl_sign()
                .build()
        } else {
            KeyUsage::new().critical().digital_signature().build()
        };
        certificate.append_extension(usage.unwrap()).unwrap();
        if !ca {
            let purpose = if client_only {
                ExtendedKeyUsage::new().client_auth().build()
            } else {
                ExtendedKeyUsage::new().server_auth().build()
            };
            certificate.append_extension(purpose.unwrap()).unwrap();
        }
        if let Some(san) = san {
            certificate
                .append_extension(
                    openssl::x509::X509Extension::new_from_der(
                        &Asn1Object::from_str("2.5.29.17").unwrap(),
                        false,
                        &Asn1OctetString::new_from_bytes(san).unwrap(),
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        certificate
            .sign(
                issuer.map_or(key, |(_, key)| key),
                openssl::hash::MessageDigest::sha256(),
            )
            .unwrap();
        certificate.build()
    }

    fn verifier_fixture(certificates: &[X509]) -> NodeCertVerifier {
        let (mut store, _) = {
            let mut store = X509StoreBuilder::new().unwrap();
            let mut params = X509VerifyParam::new().unwrap();
            params.set_purpose(X509PurposeId::SSL_SERVER).unwrap();
            params.set_auth_level(1);
            params.set_depth(100);
            store.set_param(&params).unwrap();
            (store, ())
        };
        for certificate in certificates {
            store.add_cert(certificate.clone()).unwrap();
        }
        NodeCertVerifier {
            store: store.build(),
            algorithms: rustls::crypto::ring::default_provider().signature_verification_algorithms,
        }
    }

    fn verifies(verifier: &NodeCertVerifier, leaf: &X509, chain: &[X509], host: &str) -> bool {
        verifier
            .verify_server_cert(
                &CertificateDer::from(leaf.to_der().unwrap()),
                &chain
                    .iter()
                    .map(|cert| CertificateDer::from(cert.to_der().unwrap()))
                    .collect::<Vec<_>>(),
                &ServerName::try_from(host).unwrap(),
                &[],
                UnixTime::now(),
            )
            .is_ok()
    }

    #[test]
    fn full_chain_expiry_eku_and_ip_validation_do_not_reduce_cas_to_anchors() {
        let root_key = fixture_key();
        let root = fixture_certificate(&root_key, None, "synthetic root", true, false, None, false);
        let intermediate_key = fixture_key();
        let intermediate = fixture_certificate(
            &intermediate_key,
            Some((&root, &root_key)),
            "synthetic intermediate",
            true,
            false,
            None,
            false,
        );
        let leaf_key = fixture_key();
        let san = b"\x30\x06\x87\x04\x7f\x00\x00\x01";
        let leaf = fixture_certificate(
            &leaf_key,
            Some((&intermediate, &intermediate_key)),
            "localhost",
            false,
            false,
            Some(san),
            false,
        );
        assert!(!verifies(
            &verifier_fixture(std::slice::from_ref(&intermediate)),
            &leaf,
            &[],
            "127.0.0.1"
        ));
        let verifier = verifier_fixture(std::slice::from_ref(&root));
        assert!(!verifies(&verifier, &leaf, &[], "127.0.0.1"));
        assert!(verifies(
            &verifier,
            &leaf,
            std::slice::from_ref(&intermediate),
            "127.0.0.1"
        ));
        assert!(!verifies(
            &verifier,
            &leaf,
            std::slice::from_ref(&intermediate),
            "127.0.0.2"
        ));
        // A sole IP SAN doesn't prohibit Node's DNS common-name fallback.
        assert!(verifies(
            &verifier,
            &leaf,
            std::slice::from_ref(&intermediate),
            "localhost"
        ));
        let wrong_purpose = fixture_certificate(
            &leaf_key,
            Some((&root, &root_key)),
            "localhost",
            false,
            false,
            None,
            true,
        );
        assert!(!verifies(&verifier, &wrong_purpose, &[], "localhost"));
        let expired_root =
            fixture_certificate(&root_key, None, "expired root", true, true, None, false);
        let current_leaf = fixture_certificate(
            &leaf_key,
            Some((&expired_root, &root_key)),
            "localhost",
            false,
            false,
            None,
            false,
        );
        assert!(!verifies(
            &verifier_fixture(&[expired_root]),
            &current_leaf,
            &[],
            "localhost"
        ));
    }

    #[test]
    fn malformed_and_empty_dns_names_block_cn_fallback_without_hiding_valid_siblings() {
        let root_key = fixture_key();
        let root = fixture_certificate(&root_key, None, "synthetic root", true, false, None, false);
        let key = fixture_key();
        let verifier = verifier_fixture(std::slice::from_ref(&root));
        for san in [
            b"\x30\x03\x82\x01\xff".as_slice(),
            b"\x30\x02\x82\x00".as_slice(),
        ] {
            let cert = fixture_certificate(
                &key,
                Some((&root, &root_key)),
                "localhost",
                false,
                false,
                Some(san),
                false,
            );
            assert!(!verifies(&verifier, &cert, &[], "localhost"));
        }
        let san = b"\x30\x0e\x82\x01\xff\x82\x09localhost";
        let cert = fixture_certificate(
            &key,
            Some((&root, &root_key)),
            "wrong-cn",
            false,
            false,
            Some(san),
            false,
        );
        assert!(verifies(&verifier, &cert, &[], "localhost"));
    }

    #[test]
    fn pem_partial_results_retain_prefix_and_distinguish_eof_from_parse_errors() {
        let certificate = X509::stack_from_pem(BUNDLED).unwrap().remove(0);
        let pem = certificate.to_pem().unwrap();
        let malformed = b"-----BEGIN CERTIFICATE-----\ninvalid\n-----END CERTIFICATE-----\n";
        let mut prefix = pem.clone();
        prefix.extend_from_slice(malformed);
        prefix.extend_from_slice(&pem);
        let (parsed, error) = X509::stack_from_pem_partial(&prefix).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(error.is_some());
        assert_eq!(parsed[0].to_der().unwrap(), certificate.to_der().unwrap());
        let mut first_bad = malformed.to_vec();
        first_bad.extend_from_slice(&pem);
        let (parsed, error) = X509::stack_from_pem_partial(&first_bad).unwrap();
        assert!(parsed.is_empty());
        assert!(error.is_some());
        for input in [b"".as_slice(), b"synthetic non-PEM text".as_slice()] {
            let (parsed, error) = X509::stack_from_pem_partial(input).unwrap();
            assert!(parsed.is_empty());
            assert!(error.is_none());
        }
        let legacy = String::from_utf8(pem)
            .unwrap()
            .replace("CERTIFICATE", "X509 CERTIFICATE");
        assert_eq!(
            X509::stack_from_pem_partial(legacy.as_bytes())
                .unwrap()
                .0
                .len(),
            1
        );
    }

    #[test]
    fn small_rsa_checks_signatures_without_algorithm_or_key_fallback() {
        openssl::init_without_config().unwrap();
        use openssl::hash::MessageDigest;
        use openssl::pkey::PKey;
        use openssl::rsa::{Padding, Rsa};
        use openssl::sign::{RsaPssSaltlen, Signer};
        let key = PKey::from_rsa(Rsa::generate(1024).unwrap()).unwrap();
        let certificate =
            fixture_certificate(&key, None, "synthetic RSA", true, false, None, false);
        let certificate = CertificateDer::from(certificate.to_der().unwrap());
        let message = b"synthetic CertificateVerify message";
        let sign = |padding, digest, salt| {
            let mut signer = Signer::new(digest, &key).unwrap();
            signer.set_rsa_padding(padding).unwrap();
            if padding == Padding::PKCS1_PSS {
                signer.set_rsa_mgf1_md(digest).unwrap();
                signer.set_rsa_pss_saltlen(salt).unwrap();
            }
            signer.sign_oneshot_to_vec(message).unwrap()
        };
        let pss = sign(
            Padding::PKCS1_PSS,
            MessageDigest::sha256(),
            RsaPssSaltlen::DIGEST_LENGTH,
        );
        for tls13 in [false, true] {
            assert!(
                verify_openssl_signature(
                    message,
                    &certificate,
                    SignatureScheme::RSA_PSS_SHA256,
                    &pss,
                    tls13
                )
                .unwrap()
                .is_some()
            );
            let mut corrupted = pss.clone();
            corrupted[0] ^= 1;
            assert!(
                verify_openssl_signature(
                    message,
                    &certificate,
                    SignatureScheme::RSA_PSS_SHA256,
                    &corrupted,
                    tls13
                )
                .is_err()
            );
            assert!(
                verify_openssl_signature(
                    b"changed message",
                    &certificate,
                    SignatureScheme::RSA_PSS_SHA256,
                    &pss,
                    tls13
                )
                .is_err()
            );
            assert!(
                verify_openssl_signature(
                    message,
                    &certificate,
                    SignatureScheme::RSA_PSS_SHA384,
                    &pss,
                    tls13
                )
                .is_err()
            );
            assert!(
                verify_openssl_signature(
                    message,
                    &certificate,
                    SignatureScheme::ECDSA_NISTP256_SHA256,
                    &pss,
                    tls13
                )
                .is_err()
            );
        }
        let wrong_salt = sign(
            Padding::PKCS1_PSS,
            MessageDigest::sha256(),
            RsaPssSaltlen::custom(0),
        );
        assert!(
            verify_openssl_signature(
                message,
                &certificate,
                SignatureScheme::RSA_PSS_SHA256,
                &wrong_salt,
                true
            )
            .is_err()
        );
        let pkcs1 = sign(
            Padding::PKCS1,
            MessageDigest::sha256(),
            RsaPssSaltlen::DIGEST_LENGTH,
        );
        assert!(
            verify_openssl_signature(
                message,
                &certificate,
                SignatureScheme::RSA_PKCS1_SHA256,
                &pkcs1,
                false
            )
            .unwrap()
            .is_some()
        );
        assert!(
            verify_openssl_signature(
                message,
                &certificate,
                SignatureScheme::RSA_PKCS1_SHA256,
                &pkcs1,
                true
            )
            .is_err()
        );
        assert!(
            verify_openssl_signature(
                message,
                &certificate,
                SignatureScheme::RSA_PSS_SHA256,
                &pkcs1,
                false
            )
            .is_err()
        );
        for bits in [512, 1024] {
            let wrong = PKey::from_rsa(Rsa::generate(bits).unwrap()).unwrap();
            let wrong = fixture_certificate(&wrong, None, "other RSA", true, false, None, false);
            assert!(
                verify_openssl_signature(
                    message,
                    &CertificateDer::from(wrong.to_der().unwrap()),
                    SignatureScheme::RSA_PSS_SHA256,
                    &pss,
                    true
                )
                .is_err()
            );
        }
        let normal = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
        let normal = fixture_certificate(&normal, None, "normal RSA", true, false, None, false);
        // Existing supported keys are delegated, never asserted valid here.
        assert!(
            verify_openssl_signature(
                message,
                &CertificateDer::from(normal.to_der().unwrap()),
                SignatureScheme::RSA_PSS_SHA256,
                &pss,
                true
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn extended_ecdsa_preserves_tls_version_curve_and_digest_boundaries() {
        use openssl::ec::{EcGroup, EcKey};
        use openssl::hash::MessageDigest;
        use openssl::pkey::PKey;
        use openssl::sign::Signer;
        openssl::init_without_config().unwrap();
        let message = b"synthetic ECDSA CertificateVerify message";
        let algorithms = [
            (
                MessageDigest::sha256(),
                SignatureScheme::ECDSA_NISTP256_SHA256,
            ),
            (
                MessageDigest::sha384(),
                SignatureScheme::ECDSA_NISTP384_SHA384,
            ),
            (
                MessageDigest::sha512(),
                SignatureScheme::ECDSA_NISTP521_SHA512,
            ),
        ];
        for curve in [Nid::X9_62_PRIME256V1, Nid::SECP384R1, Nid::SECP521R1] {
            let group = EcGroup::from_curve_name(curve).unwrap();
            let key = PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap();
            let certificate =
                fixture_certificate(&key, None, "synthetic EC", true, false, None, false);
            let certificate = CertificateDer::from(certificate.to_der().unwrap());
            let other_key = PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap();
            let other_certificate =
                fixture_certificate(&other_key, None, "other EC", true, false, None, false);
            let other_certificate = CertificateDer::from(other_certificate.to_der().unwrap());
            for (digest, scheme) in algorithms {
                let mut signer = Signer::new(digest, &key).unwrap();
                let signature = signer.sign_oneshot_to_vec(message).unwrap();
                let extended =
                    curve == Nid::SECP521R1 || scheme == SignatureScheme::ECDSA_NISTP521_SHA512;
                for tls13 in [false, true] {
                    let result =
                        verify_openssl_signature(message, &certificate, scheme, &signature, tls13);
                    if !extended {
                        // Existing hash/curve pairs retain ring's verifier, not
                        // an OpenSSL assertion or an alternate success path.
                        assert!(result.unwrap().is_none());
                        continue;
                    }
                    let allowed = !tls13
                        || (curve == Nid::SECP521R1
                            && scheme == SignatureScheme::ECDSA_NISTP521_SHA512);
                    if !allowed {
                        assert!(result.is_err(), "{curve:?} {scheme:?} TLS1.3");
                        continue;
                    }
                    assert!(result.unwrap().is_some(), "{curve:?} {scheme:?}");
                    let mut corrupted = signature.clone();
                    let last = corrupted.len() - 1;
                    corrupted[last] ^= 1;
                    for invalid in [
                        &corrupted[..],
                        &signature[..signature.len() - 1],
                        b"invalid DER",
                    ] {
                        assert!(
                            verify_openssl_signature(message, &certificate, scheme, invalid, tls13)
                                .is_err()
                        );
                    }
                    assert!(
                        verify_openssl_signature(
                            b"changed message",
                            &certificate,
                            scheme,
                            &signature,
                            tls13
                        )
                        .is_err()
                    );
                    assert!(
                        verify_openssl_signature(
                            message,
                            &other_certificate,
                            scheme,
                            &signature,
                            tls13
                        )
                        .is_err()
                    );
                }
                if curve == Nid::SECP521R1 {
                    let wrong_scheme = if scheme == SignatureScheme::ECDSA_NISTP256_SHA256 {
                        SignatureScheme::ECDSA_NISTP384_SHA384
                    } else {
                        SignatureScheme::ECDSA_NISTP256_SHA256
                    };
                    assert!(
                        verify_openssl_signature(
                            message,
                            &certificate,
                            wrong_scheme,
                            &signature,
                            false
                        )
                        .is_err()
                    );
                    assert!(
                        verify_openssl_signature(
                            message,
                            &certificate,
                            SignatureScheme::RSA_PSS_SHA512,
                            &signature,
                            false
                        )
                        .is_err()
                    );
                }
            }
        }
    }

    #[test]
    fn advertised_p521_retains_existing_signature_preferences() {
        let original = rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes();
        let verifier = verifier_fixture(&[]);
        let advertised = verifier.supported_verify_schemes();
        assert_eq!(&advertised[..original.len()], original);
        assert_eq!(
            &advertised[original.len()..],
            &[SignatureScheme::ECDSA_NISTP521_SHA512]
        );
    }
}
