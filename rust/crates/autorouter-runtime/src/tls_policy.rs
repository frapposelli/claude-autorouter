//! Frozen Node 22.14 option semantics, independent of certificate loading.
//!
//! Boolean option values are ignored, negations clear individual flags, and TLS
//! defaults use lib/tls.js's fixed flag priority rather than command-line order.

use std::sync::OnceLock;

const TRUST_CONFLICT: &str = "either --use-openssl-ca or --use-bundled-ca can be used, not both";
const VERSION_CONFLICT: &str = "either --tls-min-v1.3 or --tls-max-v1.2 can be used, not both";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustError {
    InvalidBundle,
    UnsupportedOptions,
    ConflictingSelectors,
    ConflictingVersions,
    UnsupportedSystemSelector,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Mode {
    #[default]
    Bundled,
    OpenSsl,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Versions {
    Tls12,
    Tls13,
    Both,
}

impl Versions {
    pub(crate) fn rustls_versions(self) -> &'static [&'static rustls::SupportedProtocolVersion] {
        static TLS12: [&rustls::SupportedProtocolVersion; 1] = [&rustls::version::TLS12];
        static TLS13: [&rustls::SupportedProtocolVersion; 1] = [&rustls::version::TLS13];
        static BOTH: [&rustls::SupportedProtocolVersion; 2] =
            [&rustls::version::TLS13, &rustls::version::TLS12];
        match self {
            Self::Tls12 => &TLS12,
            Self::Tls13 => &TLS13,
            Self::Both => &BOTH,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TlsPolicy {
    pub(crate) mode: Mode,
    pub(crate) versions: Versions,
}

#[derive(Default)]
struct Options {
    mode: Mode,
    openssl: bool,
    bundled: bool,
    minimum: [bool; 4],
    maximum: [bool; 2],
    system_option: Option<String>,
    unsupported: bool,
}

impl Options {
    fn parse(options: &str) -> Result<Self, TrustError> {
        let mut parsed = Self::default();
        for word in option_words(options)? {
            let option = word.split('=').next().unwrap_or_default().replace('_', "-");
            let Some(option) = option.strip_prefix("--") else {
                continue;
            };
            let (name, enabled) = option
                .strip_prefix("no-")
                .map_or((option, true), |name| (name, false));
            match name {
                "use-openssl-ca" => {
                    parsed.openssl = enabled;
                    if enabled {
                        parsed.mode = Mode::OpenSsl;
                    }
                }
                "use-bundled-ca" => {
                    parsed.bundled = enabled;
                    if enabled {
                        parsed.mode = Mode::Bundled;
                    }
                }
                "use-system-ca" if parsed.system_option.is_none() => {
                    parsed.system_option = Some(
                        word.split_once('=')
                            .map_or_else(|| word.clone(), |(name, _)| format!("{name}=")),
                    );
                }
                "tls-min-v1.0" => parsed.minimum[0] = enabled,
                "tls-min-v1.1" => parsed.minimum[1] = enabled,
                "tls-min-v1.2" => parsed.minimum[2] = enabled,
                "tls-min-v1.3" => parsed.minimum[3] = enabled,
                "tls-max-v1.2" => parsed.maximum[0] = enabled,
                "tls-max-v1.3" => parsed.maximum[1] = enabled,
                "openssl-config"
                | "openssl-shared-config"
                | "tls-cipher-list"
                | "enable-fips"
                | "force-fips" => parsed.unsupported = true,
                _ => {}
            }
        }
        Ok(parsed)
    }

    fn startup_error(&self) -> Option<TrustError> {
        if self.system_option.is_some() {
            Some(TrustError::UnsupportedSystemSelector)
        } else if self.openssl && self.bundled {
            Some(TrustError::ConflictingSelectors)
        } else if self.minimum[3] && self.maximum[0] {
            Some(TrustError::ConflictingVersions)
        } else {
            None
        }
    }

    fn policy(&self, reject: Option<&str>) -> Result<TlsPolicy, TrustError> {
        if let Some(error) = self.startup_error() {
            return Err(error);
        }
        // Rustls cannot negotiate TLS 1.0/1.1. Keep these effective modes
        // explicit, including when a newer minimum flag is also present:
        // Node prioritizes the oldest enabled minimum.
        if self.unsupported || self.minimum[0] || self.minimum[1] || reject == Some("0") {
            return Err(TrustError::UnsupportedOptions);
        }
        let versions = if !self.minimum[2] && self.minimum[3] {
            Versions::Tls13
        } else if self.maximum[0] && !self.maximum[1] {
            Versions::Tls12
        } else {
            Versions::Both
        };
        Ok(TlsPolicy {
            mode: self.mode,
            versions,
        })
    }
}

/// Pure check for errors which Node rejects before application dispatch.
/// Loading roots or reading certificate files is deliberately deferred.
pub fn startup_error(options: &str) -> Option<TrustError> {
    Options::parse(options).ok()?.startup_error()
}

/// Preserves Node's original option spelling and its trailing '=' diagnostic.
pub fn startup_diagnostic(options: &str) -> Option<String> {
    let options = Options::parse(options).ok()?;
    if let Some(option) = options.system_option {
        return Some(format!("{option} is not allowed in NODE_OPTIONS"));
    }
    let mut diagnostics = Vec::new();
    if options.openssl && options.bundled {
        diagnostics.push(TRUST_CONFLICT);
    }
    if options.minimum[3] && options.maximum[0] {
        diagnostics.push(VERSION_CONFLICT);
    }
    (!diagnostics.is_empty()).then(|| diagnostics.join("\n"))
}

// Node's NODE_OPTIONS grammar uses double quotes and ASCII space, with a
// backslash escape only inside quotes. Values on Boolean options are ignored.
fn option_words(options: &str) -> Result<Vec<String>, TrustError> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut chars = options.chars();
    while let Some(character) = chars.next() {
        match character {
            '\\' if quoted => word.push(chars.next().ok_or(TrustError::UnsupportedOptions)?),
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            _ => word.push(character),
        }
    }
    if quoted {
        return Err(TrustError::UnsupportedOptions);
    }
    if !word.is_empty() {
        words.push(word);
    }
    Ok(words)
}

pub(crate) fn process_policy() -> Result<TlsPolicy, TrustError> {
    static POLICY: OnceLock<Result<TlsPolicy, TrustError>> = OnceLock::new();
    *POLICY.get_or_init(|| {
        let options = match std::env::var("NODE_OPTIONS") {
            Ok(options) => options,
            Err(std::env::VarError::NotPresent) => String::new(),
            Err(_) => return Err(TrustError::UnsupportedOptions),
        };
        let policy = Options::parse(&options)?.policy(
            std::env::var("NODE_TLS_REJECT_UNAUTHORIZED")
                .ok()
                .as_deref(),
        )?;
        // NODE_USE_SYSTEM_CA is ignored by the pinned Node 22.14 release.
        if std::env::var_os("OPENSSL_CONF").is_some() {
            return Err(TrustError::UnsupportedOptions);
        }
        Ok(policy)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(options: &str) -> Result<TlsPolicy, TrustError> {
        Options::parse(options)?.policy(None)
    }

    #[test]
    fn selectors_preserve_node_boolean_implications_and_tokenization() {
        for option in [
            "--use-openssl-ca",
            "--use-openssl-ca=false",
            "\"--use_openssl_ca\"",
            "--use-openssl-ca --no-use-openssl-ca",
            "--use-openssl-ca --no-use-bundled-ca",
        ] {
            assert_eq!(
                policy(option).map(|policy| policy.mode),
                Ok(Mode::OpenSsl),
                "{option}"
            );
        }
        for option in [
            "",
            "--no-use-openssl-ca",
            "--use-bundled-ca --no-use-bundled-ca",
            "--title=--use-openssl-ca",
            "--use-openssl-ca --no-use-openssl-ca --use-bundled-ca",
        ] {
            assert_eq!(
                policy(option).map(|policy| policy.mode),
                Ok(Mode::Bundled),
                "{option}"
            );
        }
        assert_eq!(
            policy("--use-openssl-ca --use-bundled-ca"),
            Err(TrustError::ConflictingSelectors)
        );
        assert_eq!(
            policy("--use_system_ca"),
            Err(TrustError::UnsupportedSystemSelector)
        );
        for option in [
            "--tls-cipher-list=DEFAULT",
            "\"unterminated",
            "--tls-min-v1.0",
            "--tls-min-v1.1",
        ] {
            assert_eq!(policy(option), Err(TrustError::UnsupportedOptions));
        }
        assert!(Options::parse("").unwrap().policy(Some("0")).is_err());
    }

    #[test]
    fn versions_follow_fixed_priority_negation_and_boolean_values() {
        for (options, versions) in [
            ("", Versions::Both),
            ("--tls-min-v1.2", Versions::Both),
            ("--tls-min-v1.3", Versions::Tls13),
            ("--tls-max-v1.2", Versions::Tls12),
            ("--tls-max-v1.3", Versions::Both),
            ("\"--tls_min_v1.3=false\"", Versions::Tls13),
            ("--tls-min-v1.2 --tls-min-v1.3", Versions::Both),
            ("--tls-min-v1.3 --tls-min-v1.2", Versions::Both),
            ("--tls-max-v1.3 --tls-max-v1.2", Versions::Both),
            ("--tls-max-v1.2 --tls-max-v1.3", Versions::Both),
            ("--tls-min-v1.3 --no-tls-min-v1.3", Versions::Both),
            ("--tls-max-v1.2 --no-tls-max-v1.2", Versions::Both),
            (
                "--tls-min-v1.3 --tls-min-v1.2 --no-tls-min-v1.2",
                Versions::Tls13,
            ),
            ("--tls-min-v1.0 --no-tls-min-v1.0", Versions::Both),
        ] {
            assert_eq!(policy(options).unwrap().versions, versions, "{options}");
            assert_eq!(startup_diagnostic(options), None, "{options}");
        }
        for options in [
            "--tls-min-v1.3 --tls-max-v1.2",
            "--tls-max-v1.2 --tls-min-v1.3",
            "--tls-min-v1.2 --tls-min-v1.3 --tls-max-v1.2 --tls-max-v1.3",
            "--tls_min_v1.3=false --tls_max_v1.2=0",
        ] {
            assert_eq!(
                policy(options),
                Err(TrustError::ConflictingVersions),
                "{options}"
            );
            assert_eq!(
                startup_diagnostic(options).as_deref(),
                Some(VERSION_CONFLICT)
            );
        }
    }

    #[test]
    fn startup_diagnostics_preserve_spelling_and_do_not_load_files() {
        for (options, diagnostic) in [
            (
                "--use_system_ca",
                "--use_system_ca is not allowed in NODE_OPTIONS",
            ),
            (
                "--no-use-system-ca=false",
                "--no-use-system-ca= is not allowed in NODE_OPTIONS",
            ),
            ("--use-openssl-ca --use-bundled-ca", TRUST_CONFLICT),
            ("--tls-min-v1.3 --tls-max-v1.2", VERSION_CONFLICT),
        ] {
            assert_eq!(startup_diagnostic(options).as_deref(), Some(diagnostic));
        }
        assert_eq!(
            startup_error("--tls-min-v1.3 --tls-max-v1.2"),
            Some(TrustError::ConflictingVersions)
        );
        assert_eq!(
            startup_diagnostic("--openssl-config=/synthetic/missing"),
            None
        );
        assert_eq!(
            startup_diagnostic("--use-openssl-ca --use-bundled-ca --tls-min-v1.3 --tls-max-v1.2"),
            Some(format!("{TRUST_CONFLICT}\n{VERSION_CONFLICT}"))
        );
    }
}
