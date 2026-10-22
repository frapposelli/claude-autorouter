//! Test-only Node 22.14 option snapshot. Production policy remains unchanged.
use std::sync::atomic::{AtomicBool, Ordering};

use openssl::ssl::SslVersion;

use crate::{http_client::HttpError, tls_policy::Mode};

#[derive(Debug)]
pub(super) struct Options {
    pub mode: Mode,
    pub minimum: SslVersion,
    pub maximum: SslVersion,
    pub ciphers: Option<String>,
    pub openssl_config: Option<String>,
    pub openssl_shared_config: bool,
    config_options: bool,
    unsupported: bool,
}

impl Options {
    pub fn process() -> Result<Self, HttpError> {
        let options = match std::env::var("NODE_OPTIONS") {
            Ok(options) => options,
            Err(std::env::VarError::NotPresent) => String::new(),
            Err(_) => return Err(HttpError::UnsupportedTrustOptions),
        };
        let parsed = Self::parse(&options).map_err(|_| HttpError::UnsupportedTrustOptions)?;
        if parsed.unsupported || parsed.config_options || std::env::var_os("OPENSSL_CONF").is_some()
        {
            return Err(HttpError::UnsupportedTrustOptions);
        }
        Ok(parsed)
    }

    pub fn parse(options: &str) -> Result<Self, String> {
        Self::parse_with_cli(options, &[])
    }

    pub fn configuration_supported(&self) -> bool {
        !self.unsupported
    }

    pub fn parse_with_cli(options: &str, cli: &[String]) -> Result<Self, String> {
        let environment_words = words(options)?;
        let mut mode = Mode::Bundled;
        let (mut openssl, mut bundled) = (false, false);
        let (mut minimum, mut maximum) = ([false; 4], [false; 2]);
        let mut ciphers = None;
        let mut openssl_config = None;
        let mut openssl_shared_config = false;
        let mut config_options = false;
        let mut unsupported = false;
        for words in [environment_words.as_slice(), cli] {
            let mut index = 0;
            while index < words.len() {
                let word = &words[index];
                let (spelling, value) = word
                    .split_once('=')
                    .map_or((word.as_str(), None), |(key, value)| (key, Some(value)));
                let normalized = spelling.replace('_', "-");
                let Some(option) = normalized.strip_prefix("--") else {
                    index += 1;
                    continue;
                };
                let (name, enabled) = option
                    .strip_prefix("no-")
                    .map_or((option, true), |name| (name, false));
                match name {
                    "use-openssl-ca" => {
                        openssl = enabled;
                        if enabled {
                            mode = Mode::OpenSsl;
                        }
                    }
                    "use-bundled-ca" => {
                        bundled = enabled;
                        if enabled {
                            mode = Mode::Bundled;
                        }
                    }
                    "use-system-ca" => {
                        return Err(format!(
                            "{spelling}{} is not allowed in NODE_OPTIONS",
                            if value.is_some() { "=" } else { "" }
                        ));
                    }
                    "tls-min-v1.0" => minimum[0] = enabled,
                    "tls-min-v1.1" => minimum[1] = enabled,
                    "tls-min-v1.2" => minimum[2] = enabled,
                    "tls-min-v1.3" => minimum[3] = enabled,
                    "tls-max-v1.2" => maximum[0] = enabled,
                    "tls-max-v1.3" => maximum[1] = enabled,
                    "tls-cipher-list" => {
                        if !enabled {
                            return Err(format!(
                                "{word} is an invalid negation because it is not a boolean option"
                            ));
                        }
                        let value = if let Some(value) = value {
                            Some(value)
                        } else {
                            index += 1;
                            words
                                .get(index)
                                .map(String::as_str)
                                .filter(|value| !value.starts_with('-'))
                        };
                        let Some(value) = value.filter(|value| !value.is_empty()) else {
                            return Err(format!(
                                "{spelling}{} requires an argument",
                                if word.contains('=') { "=" } else { "" }
                            ));
                        };
                        ciphers = Some(value.to_owned());
                    }
                    "openssl-shared-config" => {
                        config_options = true;
                        openssl_shared_config = enabled;
                    }
                    "openssl-config" => {
                        config_options = true;
                        if !enabled {
                            return Err(format!(
                                "{word} is an invalid negation because it is not a boolean option"
                            ));
                        }
                        let selected = if let Some(value) = value {
                            (!value.is_empty()).then_some(value)
                        } else {
                            index += 1;
                            words
                                .get(index)
                                .map(String::as_str)
                                .filter(|value| !value.starts_with('-'))
                                .map(|value| {
                                    if value.starts_with("\\-") {
                                        &value[1..]
                                    } else {
                                        value
                                    }
                                })
                        };
                        let Some(selected) = selected else {
                            return Err(format!(
                                "{spelling}{} requires an argument",
                                if word.contains('=') { "=" } else { "" }
                            ));
                        };
                        openssl_config = Some(selected.to_owned());
                    }
                    "enable-fips" | "force-fips" => unsupported = true,
                    _ => {}
                }
                index += 1;
            }
            let mut diagnostics = Vec::new();
            if openssl && bundled {
                diagnostics
                    .push("either --use-openssl-ca or --use-bundled-ca can be used, not both");
            }
            if minimum[3] && maximum[0] {
                diagnostics.push("either --tls-min-v1.3 or --tls-max-v1.2 can be used, not both");
            }
            if !diagnostics.is_empty() {
                return Err(diagnostics.join("\n"));
            }
        }
        Ok(Self {
            mode,
            minimum: if minimum[0] {
                SslVersion::TLS1
            } else if minimum[1] {
                SslVersion::TLS1_1
            } else if minimum[2] || !minimum[3] {
                SslVersion::TLS1_2
            } else {
                SslVersion::TLS1_3
            },
            maximum: if maximum[1] || !maximum[0] {
                SslVersion::TLS1_3
            } else {
                SslVersion::TLS1_2
            },
            ciphers,
            openssl_config,
            openssl_shared_config,
            config_options,
            unsupported,
        })
    }
}

// Same double-quote/ASCII-space grammar as Node's ParseNodeOptionsEnvVar.
fn words(options: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut characters = options.chars();
    while let Some(character) = characters.next() {
        match character {
            '\\' if quoted => word.push(
                characters
                    .next()
                    .ok_or("invalid value for NODE_OPTIONS (invalid escape)")?,
            ),
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
        return Err("invalid value for NODE_OPTIONS (unterminated string)".into());
    }
    if !word.is_empty() {
        words.push(word);
    }
    Ok(words)
}

pub(super) const REJECTION_WARNING: &str = "Setting the NODE_TLS_REJECT_UNAUTHORIZED environment variable to '0' makes TLS connections and HTTPS requests insecure by disabling certificate verification.";

pub(super) fn reject_unauthorized() -> bool {
    static WARNED: AtomicBool = AtomicBool::new(false);
    let reject = std::env::var("NODE_TLS_REJECT_UNAUTHORIZED").as_deref() != Ok("0");
    if !reject && !WARNED.swap(true, Ordering::SeqCst) {
        eprintln!("Warning: {REJECTION_WARNING}");
    }
    reject
}

#[test]
fn parsing_preserves_priority_negation_and_string_operand_boundaries() {
    for (input, minimum, maximum) in [
        (
            "--tls-min-v1.0 --tls-min-v1.2",
            SslVersion::TLS1,
            SslVersion::TLS1_3,
        ),
        (
            "--tls_min_v1.1=false --tls-min-v1.0 --no-tls-min-v1.0",
            SslVersion::TLS1_1,
            SslVersion::TLS1_3,
        ),
        (
            "--tls-max-v1.2 --tls-max-v1.3",
            SslVersion::TLS1_2,
            SslVersion::TLS1_3,
        ),
        (
            "--tls-min-v1.1 --tls-max-v1.2",
            SslVersion::TLS1_1,
            SslVersion::TLS1_2,
        ),
    ] {
        let parsed = Options::parse(input).unwrap();
        assert_eq!(
            (parsed.minimum, parsed.maximum),
            (minimum, maximum),
            "{input}"
        );
    }
    assert_eq!(
        Options::parse("--tls-cipher-list DEFAULT --tls_cipher_list=\"HIGH:!aNULL\"")
            .unwrap()
            .ciphers
            .as_deref(),
        Some("HIGH:!aNULL")
    );
    assert!(
        Options::parse("--tls-cipher-list=--use-openssl-ca")
            .unwrap()
            .mode
            == Mode::Bundled
    );
    for input in [
        "--tls-cipher-list",
        "--tls-cipher-list=",
        "--tls-cipher-list --tls-min-v1.3",
        "--no-tls-cipher-list",
        "\"unterminated",
        "--tls-min-v1.3 --tls-max-v1.2",
    ] {
        assert!(Options::parse(input).is_err(), "{input}");
    }
}

#[test]
fn configuration_parsing_preserves_cli_precedence_empty_operands_and_appname_negation() {
    let parsed = Options::parse_with_cli(
        "--openssl-config=first --openssl-shared-config=false",
        &[
            "--openssl-config".into(),
            "path with spaces".into(),
            "--no-openssl-shared-config=true".into(),
        ],
    )
    .unwrap();
    assert_eq!(parsed.openssl_config.as_deref(), Some("path with spaces"));
    assert!(!parsed.openssl_shared_config);
    assert!(parsed.configuration_supported());
    let empty = Options::parse_with_cli(
        "--openssl-config=first",
        &["--openssl-config".into(), String::new()],
    )
    .unwrap();
    assert_eq!(empty.openssl_config.as_deref(), Some(""));
    assert!(
        Options::parse("--openssl-shared-config=false")
            .unwrap()
            .openssl_shared_config
    );
    assert!(
        !Options::parse("--enable-fips")
            .unwrap()
            .configuration_supported()
    );
}

#[test]
fn configuration_string_options_retain_missing_and_negated_diagnostics() {
    assert!(Options::parse_with_cli("--openssl-config", &["owned-file".into()]).is_err());
    assert!(
        Options::parse_with_cli(
            "--tls-min-v1.3 --tls-max-v1.2",
            &["--no-tls-min-v1.3".into()]
        )
        .is_err()
    );
    assert_eq!(
        Options::parse_with_cli("", &["--openssl-config".into(), "\\-file".into()])
            .unwrap()
            .openssl_config
            .as_deref(),
        Some("-file")
    );
    for input in [
        "--openssl-config",
        "--openssl-config=",
        "--openssl-config --tls-min-v1.3",
        "--no-openssl-config=foo",
        "--openssl-config \"\"",
    ] {
        assert!(Options::parse(input).is_err(), "{input}");
    }
    assert_eq!(
        Options::parse("--openssl_config=\"path with spaces\"")
            .unwrap()
            .openssl_config
            .as_deref(),
        Some("path with spaces")
    );
    assert_eq!(
        Options::parse("--openssl-config=--literal")
            .unwrap()
            .openssl_config
            .as_deref(),
        Some("--literal")
    );
}
