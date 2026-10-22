//! Private B3 bootstrap. No shipping admission or arbitrary-config parity claim.
use std::ffi::CString;

use super::options::Options;

pub(super) struct Configured {
    policy: Options,
}
impl Configured {
    pub(super) fn into_policy(self) -> Options {
        self.policy
    }
}

#[derive(Debug)]
pub(super) enum StartupError {
    Unsupported,
    Configuration(i32),
}
impl StartupError {
    pub(super) fn exit(self) -> ! {
        match self {
            Self::Unsupported => {
                eprintln!("Unsupported synthetic OpenSSL configuration options");
                std::process::exit(1)
            }
            Self::Configuration(reason) => {
                // Deliberately do not print OpenSSL file, section or error data.
                // Diagnostic bytes are a separate unqualified B3 contract.
                eprintln!("OpenSSL configuration error: reason {reason}");
                std::process::exit(reason)
            }
        }
    }
}

pub(super) fn initialize(policy: Options) -> Result<Configured, StartupError> {
    if !policy.configuration_supported() {
        return Err(StartupError::Unsupported);
    }
    let environment = match std::env::var("OPENSSL_CONF") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(_) => return Err(StartupError::Unsupported),
    };
    let filename = policy
        .openssl_config
        .as_deref()
        .filter(|value| !value.is_empty())
        .or(environment.as_deref().filter(|value| !value.is_empty()));
    // The supplier's compiled default config path is not the frozen Node path.
    // This private phase requires an explicit path or explicit empty env value.
    if filename.is_none() && environment.is_none() {
        return Err(StartupError::Unsupported);
    }
    let filename = filename
        .map(CString::new)
        .transpose()
        .map_err(|_| StartupError::Unsupported)?;
    openssl::init_with_config_first_use(filename.as_deref(), policy.openssl_shared_config)
        .map_err(|errors| {
            let reason = errors
                .errors()
                .first()
                .map_or(1, |error| error.reason_code());
            StartupError::Configuration(if reason == 0 { 1 } else { reason })
        })?;
    Ok(Configured { policy })
}

// Only the differential driver invokes this as a sole test in a fresh process.
// Ordinary cargo test does not load configuration or mutate the environment.
#[test]
fn first_use_child() {
    let Ok(mode) = std::env::var("AUTOROUTER_SYNTHETIC_OPENSSL_INIT") else {
        return;
    };
    let filename =
        CString::new(std::env::var("AUTOROUTER_SYNTHETIC_OPENSSL_FILE").unwrap()).unwrap();
    match mode.as_str() {
        "no-config-first" => {
            openssl::init_without_config().unwrap();
            let rejected = openssl::init_with_config_first_use(Some(&filename), false).is_err();
            assert!(rejected);
            eprintln!("OpenSSL first-use fixture: conflict rejected");
        }
        "cached-success" | "cached-failure" | "cached-failure-bytes" => {
            let first = openssl::init_with_config_first_use(Some(&filename), false);
            assert_eq!(first.is_ok(), mode == "cached-success");
            // Mutation uses only this driver's owned synthetic file. Identical
            // initialization must retain its first outcome instead of rereading.
            let replacement = if mode == "cached-success" {
                "nodejs_conf=missing\n"
            } else {
                ""
            };
            std::fs::write(filename.to_str().unwrap(), replacement).unwrap();
            let second = openssl::init_with_config_first_use(Some(&filename), false);
            assert_eq!(first.is_ok(), second.is_ok());
            assert!(openssl::init_without_config().is_err());
            assert!(openssl::init_with_config_first_use(Some(&filename), true).is_err());
            eprintln!("OpenSSL first-use fixture: cached outcome and conflicts verified");
        }
        _ => panic!("unknown synthetic initialization mode"),
    }
}
