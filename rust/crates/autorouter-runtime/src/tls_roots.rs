//! Explicit trust dependency: frozen Node 22.14.0 bundled Mozilla roots.
//! NODE_EXTRA_CA_CERTS extends this store once, without an implicit OS union.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, pem::PemObject};

const BUNDLED: &[u8] = include_bytes!("../../../vendor/node-ca/node-v22.14.0.pem");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustError {
    InvalidBundle,
    UnsupportedOptions,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Warning {
    UnreadableExtra,
    MalformedExtra,
}

fn unsupported_options(options: &str, system: Option<&str>, reject: Option<&str>) -> bool {
    // Node flags accept underscores as aliases for dashes. Quote characters do
    // not hide a trust flag from this conservative unsupported-mode diagnostic.
    let options = options.replace('_', "-");
    system == Some("1")
        || reject == Some("0")
        || [
            "--use-openssl-ca",
            "--use-system-ca",
            "--openssl-config",
            "--openssl-shared-config",
            "--tls-cipher-list",
            "--tls-min-v1.0",
            "--tls-min-v1.1",
            "--tls-min-v1.2",
            "--tls-min-v1.3",
            "--tls-max-v1.2",
            "--tls-max-v1.3",
            "--enable-fips",
            "--force-fips",
        ]
        .iter()
        .any(|option| options.contains(option))
}

fn load(extra: Option<&Path>) -> Result<(RootCertStore, Option<Warning>), TrustError> {
    let mut roots = RootCertStore::empty();
    for certificate in CertificateDer::pem_slice_iter(BUNDLED) {
        roots
            .add(certificate.map_err(|_| TrustError::InvalidBundle)?)
            .map_err(|_| TrustError::InvalidBundle)?;
    }
    let Some(extra) = extra else {
        return Ok((roots, None));
    };
    let certificates = match CertificateDer::pem_file_iter(extra) {
        Ok(certificates) => certificates,
        Err(_) => return Ok((roots, Some(Warning::UnreadableExtra))),
    };
    // Node retains certificates parsed before the first malformed PEM record,
    // then stops processing that file and emits one warning.
    for certificate in certificates {
        match certificate {
            Ok(certificate) => {
                if roots.add(certificate).is_err() {
                    return Ok((roots, Some(Warning::MalformedExtra)));
                }
            }
            Err(_) => return Ok((roots, Some(Warning::MalformedExtra))),
        }
    }
    Ok((roots, None))
}

pub fn process_roots() -> Result<Arc<RootCertStore>, TrustError> {
    static ROOTS: OnceLock<Result<Arc<RootCertStore>, TrustError>> = OnceLock::new();
    ROOTS.get_or_init(|| {
        if unsupported_options(
            &std::env::var("NODE_OPTIONS").unwrap_or_default(),
            std::env::var("NODE_USE_SYSTEM_CA").ok().as_deref(),
            std::env::var("NODE_TLS_REJECT_UNAUTHORIZED").ok().as_deref(),
        ) {
            return Err(TrustError::UnsupportedOptions);
        }
        let extra = std::env::var_os("NODE_EXTRA_CA_CERTS").filter(|value| !value.is_empty());
        let (roots, warning) = load(extra.as_deref().map(Path::new))?;
        match warning {
            Some(Warning::UnreadableExtra) => eprintln!("Warning: Ignoring extra certificates from NODE_EXTRA_CA_CERTS: file could not be read."),
            Some(Warning::MalformedExtra) => eprintln!("Warning: Ignoring remaining certificates from NODE_EXTRA_CA_CERTS: malformed certificate data."),
            None => {}
        }
        Ok(Arc::new(roots))
    }).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_is_complete_and_missing_extra_file_preserves_default_trust() {
        let (roots, warning) = load(None).unwrap();
        assert_eq!(roots.len(), 149);
        assert_eq!(warning, None);
        let isolated =
            std::env::temp_dir().join(format!("synthetic-absent-ca-{}", std::process::id()));
        let (roots, warning) = load(Some(&isolated.join("absent.pem"))).unwrap();
        assert_eq!(roots.len(), 149);
        assert_eq!(warning, Some(Warning::UnreadableExtra));
    }

    #[test]
    fn unsupported_trust_overrides_are_explicit_instead_of_silently_broadening_trust() {
        assert!(!unsupported_options(
            "--use-bundled-ca --max-old-space-size=256",
            None,
            None
        ));
        for flag in [
            "--use-openssl-ca",
            "--use_system_ca",
            "--tls-cipher-list=DEFAULT",
        ] {
            assert!(unsupported_options(flag, None, None));
        }
        assert!(unsupported_options("", Some("1"), None));
        assert!(unsupported_options("", None, Some("0")));
    }
}
