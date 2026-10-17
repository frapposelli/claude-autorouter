//! Ordered, pattern-based redaction of evaluator and prompt-log excerpts.
//! No captured text is sent to diagnostics. Unsupported regex lookaround and
//! backreferences are explicit bounded scans, never a backtracking engine.

use regex::{Captures, Regex};
use std::borrow::Cow;
use std::sync::LazyLock;

const SECRET: &str = "[REDACTED:secret]";
const PRIVATE_KEY: &str = "[REDACTED:private_key]";
const CREDENTIALS: &str = "[REDACTED:credentials]";
const JS_SPACE: &str = r"\t\n\x0b\x0c\r \u{00a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}";
const SECRET_NAME: &str = "(?:passw(?:or)?d|pwd|passphrase|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|signing[_-]?key|encryption[_-]?key|credentials?)";
const SEPARATOR: &str = r#"(["']?[ \t]{0,8}[:=][ \t]{0,8})"#;
const FLAG_NAME: &str = "(?:password|passwd|pwd|passphrase|pass|token|secret|api-?key|access-?key|private-?key|client-?secret|auth-?token|auth)";

fn whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | ' ' | '\u{00a0}' | '\u{1680}'
        | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}'
        | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

fn word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

// JS non-Unicode /i and \b use ASCII case folding and word characters. Rust's
// defaults also match e.g. the Kelvin sign and long s; expand literal ASCII
// letters rather than broadening the set of recognized secret names.
fn regex(pattern: &str, ignore_case: bool) -> Regex {
    let chars: Vec<char> = pattern.chars().collect();
    let mut output = String::new();
    let mut i = 0;
    let mut in_class = false;
    while i < chars.len() {
        if chars[i..].starts_with(&['[', '\\', 's', '\\', 'S', ']']) {
            output.push_str("(?s:.)");
            i += 6;
            continue;
        }
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            match chars[i + 1] {
                's' => {
                    if !in_class {
                        output.push('[');
                    }
                    output.push_str(JS_SPACE);
                    if !in_class {
                        output.push(']');
                    }
                }
                'S' if !in_class => {
                    output.push_str("[^");
                    output.push_str(JS_SPACE);
                    output.push(']');
                }
                'b' if !in_class => output.push_str(r"(?-u:\b)"),
                _ => {
                    output.push(c);
                    output.push(chars[i + 1]);
                }
            }
            i += 2;
            continue;
        }
        if c == '[' {
            in_class = true;
            output.push(c);
            i += 1;
            continue;
        }
        if c == ']' {
            in_class = false;
            output.push(c);
            i += 1;
            continue;
        }
        if ignore_case && c.is_ascii_alphabetic() {
            if in_class {
                if i + 2 < chars.len() && chars[i + 1] == '-' && chars[i + 2].is_ascii_alphabetic()
                {
                    output.push(c);
                    output.push('-');
                    output.push(chars[i + 2]);
                    output.push(if c.is_ascii_lowercase() {
                        c.to_ascii_uppercase()
                    } else {
                        c.to_ascii_lowercase()
                    });
                    output.push('-');
                    output.push(if chars[i + 2].is_ascii_lowercase() {
                        chars[i + 2].to_ascii_uppercase()
                    } else {
                        chars[i + 2].to_ascii_lowercase()
                    });
                    i += 3;
                    continue;
                }
                output.push(c.to_ascii_lowercase());
                output.push(c.to_ascii_uppercase());
            } else {
                output.push('[');
                output.push(c.to_ascii_lowercase());
                output.push(c.to_ascii_uppercase());
                output.push(']');
            }
        } else {
            output.push(c);
        }
        i += 1;
    }
    Regex::new(&output).expect("reviewed redaction expression must compile")
}

struct Rules {
    private_key: Regex,
    key_end: Regex,
    url_scheme: Regex,
    provider: Vec<Regex>,
    azure: Regex,
    authorization: Regex,
    auth_scheme: Regex,
    cookie: Regex,
    bearer: Regex,
    curl: Regex,
    flag: Regex,
    sshpass: Regex,
    mysql: Regex,
    quoted_assignment: Regex,
    assignment: Regex,
    environment: Regex,
    bare: Regex,
    email: Regex,
    iban: Regex,
    card: Regex,
    tax_code: Regex,
    social: Regex,
    phone: Regex,
}

static RULES: LazyLock<Rules> = LazyLock::new(|| Rules {
    private_key: regex(
        r"-----BEGIN [A-Z0-9 ]{0,40}PRIVATE KEY(?: BLOCK)?-----[\s\S]*?(?:-----END [A-Z0-9 ]{0,40}PRIVATE KEY(?: BLOCK)?-----|$)",
        false,
    ),
    key_end: regex(
        r"-----END [A-Z0-9 ]{0,40}PRIVATE KEY(?: BLOCK)?-----",
        false,
    ),
    url_scheme: regex(r"\b([a-z][a-z0-9+.-]{1,20}://)", true),
    provider: [
        r"\bsk-[A-Za-z0-9_-]{20,}",
        r"\b[rs]k_(?:live|test)_[A-Za-z0-9]{16,}",
        r"\bwhsec_[A-Za-z0-9]{16,}",
        r"\b(?:AKIA|ASIA|ABIA|ACCA)[A-Z0-9]{16}\b",
        r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,})",
        r"\bglpat-[A-Za-z0-9_-]{20,}",
        r"\bxox[abposr]-[A-Za-z0-9-]{10,}",
        r"https://hooks\.slack\.com/services/[A-Za-z0-9/]{8,}",
        r"\bAIza[0-9A-Za-z_-]{35}",
        r"\bya29\.[A-Za-z0-9_-]{20,}",
        r"\bnpm_[A-Za-z0-9]{36}",
        r"\bpypi-[A-Za-z0-9_-]{50,}",
        r"\bSG\.[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}",
        r"\bshp(?:at|ca|pa|ss)_[a-f0-9]{32}\b",
        r"\bhf_[A-Za-z0-9]{30,}",
        r"\bdo[por]_v1_[a-f0-9]{64}\b",
        r"\blin_api_[A-Za-z0-9]{30,}",
        r"\bntn_[A-Za-z0-9]{30,}",
        r"\bdapi[a-f0-9]{32}\b",
        r"\bATATT3[A-Za-z0-9_=-]{20,}",
        r"\bkey-[0-9a-f]{32}\b",
        r"\bSK[0-9a-f]{32}\b",
        r"\b[0-9]{8,10}:[A-Za-z0-9_-]{35}\b",
        r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
    ]
    .iter()
    .map(|pattern| regex(pattern, false))
    .collect(),
    azure: regex(
        r"\b((?:AccountKey|SharedAccessKey|SharedAccessSignature)=)",
        false,
    ),
    authorization: regex(
        r#"\b((?:proxy-)?authorization["']?[ \t]{0,8}[:=][ \t]{0,8}["']?)"#,
        true,
    ),
    auth_scheme: regex(r"^(Bearer|Basic|Token|Digest)[ \t]+", true),
    cookie: regex(
        r#"\b((?:set-)?cookie["']?[ \t]{0,8}[:=][ \t]{0,8}["']?)"#,
        true,
    ),
    bearer: regex(r"\b(Bearer[ \t]+)[A-Za-z0-9._~+/-]{16,}=*", false),
    curl: regex(
        r#"((?:^|\s)(?:-u|--user|--proxy-user)(?:[ \t]+|=)["']?)[^\s:"']{0,64}:[^\s"']+"#,
        false,
    ),
    flag: regex(
        &format!(r#"((?:^|\s)--?[a-z0-9-]{{0,31}}?{FLAG_NAME})([ \t]+|=)(["']?)"#),
        true,
    ),
    sshpass: regex(r#"\b(sshpass[ \t]+-p[ \t]*["']?)[^\s"']+"#, false),
    mysql: regex(
        r"\b((?:mysql|mysqldump|mysqladmin|mariadb)\b[^\n]{0,200}?[ \t]-p)",
        false,
    ),
    quoted_assignment: regex(
        &format!(
            r#"\b([A-Za-z0-9_.-]{{0,40}}{SECRET_NAME}[A-Za-z0-9_.-]{{0,40}}){SEPARATOR}(["'])"#
        ),
        true,
    ),
    assignment: regex(
        &format!(
            r#"\b([A-Za-z0-9_.-]{{0,40}}{SECRET_NAME}[A-Za-z0-9_.-]{{0,40}}){SEPARATOR}(["']?)"#
        ),
        true,
    ),
    environment: regex(
        &format!(
            r#"\b([A-Z][A-Z0-9_]{{0,40}}[_-]KEY|(?:[A-Z][A-Z0-9_]{{0,40}}[_-])?(?:PASS|AUTH)){SEPARATOR}(["']?)"#
        ),
        false,
    ),
    bare: regex(
        r#"((?:pass|auth)["']?[ \t]{0,8}[:=][ \t]{0,8})(["']?)"#,
        true,
    ),
    email: regex(
        r"\b[A-Za-z0-9._%+-]{1,64}@[A-Za-z0-9.-]{1,253}\.[A-Za-z]{2,24}\b",
        false,
    ),
    iban: regex(
        r"\b[A-Z]{2}[0-9]{2}(?: ?[A-Z0-9]{4}){2,7}(?: ?[A-Z0-9]{1,4})?\b",
        false,
    ),
    card: regex(
        r"\b(?:4|5[1-5]|2[2-7]|3[47]|6)(?:[0-9][ -]?){11,17}[0-9]\b",
        false,
    ),
    tax_code: regex(
        r"\b[A-Z]{6}[0-9LMNPQRSTUV]{2}[A-EHLMPRST][0-9LMNPQRSTUV]{2}[A-Z][0-9LMNPQRSTUV]{3}[A-Z]\b",
        false,
    ),
    social: regex(r"\b[0-9]{3}-[0-9]{2}-[0-9]{4}\b", false),
    phone: regex(r"\+[0-9][0-9 .()-]{6,20}[0-9]", false),
});

fn replace_literal(text: &mut String, pattern: &Regex, replacement: &str) {
    if let Cow::Owned(replaced) = pattern.replace_all(text, replacement) {
        *text = replaced;
    }
}

fn replace_prefix(text: &mut String, pattern: &Regex) {
    if let Cow::Owned(replaced) =
        pattern.replace_all(text, |caps: &Captures<'_>| format!("{}{SECRET}", &caps[1]))
    {
        *text = replaced;
    }
}

/// A failing guard restarts at the next character, just like an assertion in
/// the original expression. Skipping the whole candidate could miss a valid
/// nested assignment. All header patterns and lookaheads have bounded length.
fn replace_scan(
    text: &mut String,
    pattern: &Regex,
    replacement: impl Fn(&str, &Captures<'_>) -> Option<(usize, String)>,
) {
    let mut cursor = 0;
    let mut copied = 0;
    let mut output = String::new();
    while cursor <= text.len() {
        let Some(caps) = pattern.captures_at(text, cursor) else {
            break;
        };
        let found = caps.get(0).expect("full match");
        if let Some((end, value)) = replacement(text, &caps) {
            output.push_str(&text[copied..found.start()]);
            output.push_str(&value);
            copied = end;
            cursor = end;
        } else {
            cursor = found.start()
                + text[found.start()..]
                    .chars()
                    .next()
                    .map_or(1, char::len_utf8);
        }
    }
    if copied != 0 {
        output.push_str(&text[copied..]);
        *text = output;
    }
}

fn scan_value(
    text: &str,
    start: usize,
    allowed: impl Fn(char, Option<char>) -> bool,
) -> (usize, usize) {
    let mut cursor = start;
    let mut units = 0;
    let mut chars = text[start..].chars().peekable();
    while let Some(c) = chars.next() {
        if !allowed(c, chars.peek().copied()) {
            break;
        }
        cursor += c.len_utf8();
        units += c.len_utf16();
    }
    (cursor, units)
}

fn unquoted(c: char, next: Option<char>) -> bool {
    if c == ',' || c == ';' {
        return next.is_some_and(|next| !whitespace(next) && !matches!(next, '"' | '\'' | '&'));
    }
    !whitespace(c) && !matches!(c, '"' | '\'' | '&')
}

fn redact_key_tails(text: &mut String) {
    let mut output = String::new();
    let mut copied = 0;
    for found in RULES.key_end.find_iter(text) {
        let mut start = found.start();
        while start > copied && text.as_bytes()[start - 1] == b'\n' {
            let mut end = start - 1;
            if end > copied && text.as_bytes()[end - 1] == b'\r' {
                end -= 1;
            }
            let mut begin = end;
            while begin > copied
                && (text.as_bytes()[begin - 1].is_ascii_alphanumeric()
                    || b"+/=".contains(&text.as_bytes()[begin - 1]))
            {
                begin -= 1;
            }
            if end - begin < 16 || (begin > copied && text.as_bytes()[begin - 1] != b'\n') {
                break;
            }
            start = begin;
        }
        if start == found.start() {
            continue;
        }
        output.push_str(&text[copied..start]);
        output.push_str(PRIVATE_KEY);
        copied = found.end();
    }
    if copied != 0 {
        output.push_str(&text[copied..]);
        *text = output;
    }
}

fn iban_valid(text: &str) -> bool {
    let compact: Vec<u8> = text.bytes().filter(|c| *c != b' ').collect();
    if !(15..=34).contains(&compact.len()) {
        return false;
    }
    let mut remainder = 0u32;
    for c in compact[4..].iter().chain(&compact[..4]) {
        if c.is_ascii_digit() {
            remainder = (remainder * 10 + u32::from(c - b'0')) % 97;
        } else {
            remainder = (remainder * 100 + u32::from(c - b'A' + 10)) % 97;
        }
    }
    remainder == 1
}

fn luhn_valid(text: &str) -> bool {
    let digits: Vec<u8> = text.bytes().filter(u8::is_ascii_digit).collect();
    if !(13..=19).contains(&digits.len()) {
        return false;
    }
    digits
        .iter()
        .rev()
        .enumerate()
        .map(|(index, digit)| {
            let mut value = u32::from(digit - b'0');
            if index % 2 == 1 {
                value *= 2;
                if value > 9 {
                    value -= 9;
                }
            }
            value
        })
        .sum::<u32>()
        % 10
        == 0
}

fn tax_code_valid(text: &str) -> bool {
    const ODD: [u32; 26] = [
        1, 0, 5, 7, 9, 13, 15, 17, 19, 21, 2, 4, 18, 20, 11, 3, 6, 8, 12, 14, 16, 10, 22, 25, 24,
        23,
    ];
    let bytes = text.as_bytes();
    if bytes.len() != 16 {
        return false;
    }
    let sum = bytes[..15]
        .iter()
        .enumerate()
        .map(|(index, c)| {
            let value = usize::from(if c.is_ascii_uppercase() {
                c - b'A'
            } else {
                c - b'0'
            });
            if index % 2 == 0 {
                ODD[value]
            } else {
                value as u32
            }
        })
        .sum::<u32>();
    u32::from(bytes[15]) == u32::from(b'A') + sum % 26
}

fn replace_validated(
    text: &mut String,
    pattern: &Regex,
    valid: impl Fn(&str) -> bool,
    marker: &str,
) {
    if let Cow::Owned(replaced) = pattern.replace_all(text, |caps: &Captures<'_>| {
        if valid(&caps[0]) {
            marker.to_owned()
        } else {
            caps[0].to_owned()
        }
    }) {
        *text = replaced;
    }
}

pub fn redact_sensitive(text: &str) -> String {
    let mut text = text.to_owned();
    replace_literal(&mut text, &RULES.private_key, PRIVATE_KEY);
    redact_key_tails(&mut text);
    replace_scan(&mut text, &RULES.url_scheme, |text, caps| {
        let start = caps.get(0)?.end();
        let (colon, units) = scan_value(text, start, |c, _| {
            !whitespace(c) && !matches!(c, ':' | '@' | '/')
        });
        if units > 256 || text.as_bytes().get(colon) != Some(&b':') {
            return None;
        }
        let mut password_units = 0;
        let mut last_at = None;
        for (offset, c) in text[colon + 1..].char_indices() {
            if whitespace(c) || c == '/' || password_units > 256 {
                break;
            }
            if c == '@' && password_units > 0 {
                last_at = Some(colon + 1 + offset + c.len_utf8());
            }
            password_units += c.len_utf16();
        }
        Some((last_at?, format!("{}{CREDENTIALS}@", &caps[1])))
    });
    for pattern in &RULES.provider {
        replace_literal(&mut text, pattern, SECRET);
    }
    replace_scan(&mut text, &RULES.azure, |text, caps| {
        let (end, units) = scan_value(text, caps.get(0)?.end(), |c, _| {
            !whitespace(c) && !matches!(c, ';' | '"' | '\'')
        });
        (units >= 8).then(|| (end, format!("{}{SECRET}", &caps[1])))
    });
    replace_scan(&mut text, &RULES.authorization, |text, caps| {
        let start = caps.get(0)?.end();
        let scheme = RULES.auth_scheme.captures(&text[start..]);
        if let Some(scheme) = scheme {
            let (end, units) = scan_value(text, start + scheme.get(0)?.end(), |c, _| {
                !whitespace(c) && !matches!(c, '"' | '\'' | ',' | ';')
            });
            if units >= 4 {
                return Some((end, format!("{}{} {SECRET}", &caps[1], &scheme[1])));
            }
        }
        let (end, units) = scan_value(text, start, |c, _| {
            !whitespace(c) && !matches!(c, '"' | '\'' | ',' | ';')
        });
        (units >= 4).then(|| (end, format!("{}{SECRET}", &caps[1])))
    });
    replace_scan(&mut text, &RULES.cookie, |text, caps| {
        let mut start = caps.get(0)?.end();
        let (end, mut units) =
            scan_value(text, start, |c, _| !matches!(c, '\r' | '\n' | '"' | '\''));
        // Header spaces are greedy, but the original expression can give
        // them back to a short cookie value to satisfy its eight-unit minimum.
        let mut prefix_end = caps[1].len();
        while units < 8
            && start > caps.get(0)?.start()
            && matches!(text.as_bytes()[start - 1], b' ' | b'\t')
        {
            start -= 1;
            prefix_end -= 1;
            units += 1;
        }
        (units >= 8).then(|| (end, format!("{}{SECRET}", &caps[1][..prefix_end])))
    });
    replace_prefix(&mut text, &RULES.bearer);
    if let Cow::Owned(replaced) = RULES.curl.replace_all(&text, |caps: &Captures<'_>| {
        let username = caps[0][caps[1].len()..].split(':').next().unwrap_or("");
        if username.encode_utf16().count() > 64 {
            caps[0].to_owned()
        } else {
            format!("{}{CREDENTIALS}", &caps[1])
        }
    }) {
        text = replaced;
    }
    replace_scan(&mut text, &RULES.flag, |text, caps| {
        let start = caps.get(0)?.end();
        if caps[3].is_empty() && text[start..].starts_with('-') {
            return None;
        }
        let (end, units) = scan_value(text, start, |c, _| {
            !whitespace(c) && !matches!(c, '"' | '\'')
        });
        (units >= 4).then(|| (end, format!("{}{}{}{SECRET}", &caps[1], &caps[2], &caps[3])))
    });
    replace_prefix(&mut text, &RULES.sshpass);
    replace_scan(&mut text, &RULES.mysql, |text, caps| {
        let prefix = &caps[1];
        let command_end = prefix.find(|c: char| !word(c)).unwrap_or(prefix.len());
        if prefix[command_end..prefix.len() - 3].encode_utf16().count() > 200 {
            return None;
        }
        let (end, units) = scan_value(text, caps.get(0)?.end(), |c, _| !whitespace(c));
        (units >= 3).then(|| (end, format!("{}{SECRET}", &caps[1])))
    });
    replace_scan(&mut text, &RULES.quoted_assignment, |text, caps| {
        let start = caps.get(0)?.end();
        let quote = caps[3].chars().next()?;
        let (end, units) = scan_value(text, start, |c, _| c != quote && c != '\r' && c != '\n');
        if !(4..=512).contains(&units) || !text[end..].starts_with(quote) {
            return None;
        }
        Some((
            end + quote.len_utf8(),
            format!("{}{}{}{SECRET}{}", &caps[1], &caps[2], quote, quote),
        ))
    });
    for pattern in [&RULES.assignment, &RULES.environment] {
        replace_scan(&mut text, pattern, |text, caps| {
            let (end, units) = scan_value(text, caps.get(0)?.end(), unquoted);
            (units >= 4).then(|| (end, format!("{}{}{}{SECRET}", &caps[1], &caps[2], &caps[3])))
        });
    }
    replace_scan(&mut text, &RULES.bare, |text, caps| {
        let matched = caps.get(0)?;
        if text[..matched.start()]
            .chars()
            .next_back()
            .is_some_and(|c| word(c) || c == '-')
        {
            return None;
        }
        let start = matched.end();
        let rest = &text[start..];
        if ["true", "false", "null", "none", "undefined"]
            .iter()
            .any(|value| {
                rest.get(..value.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(value))
                    && rest[value.len()..].chars().next().is_none_or(|c| !word(c))
            })
        {
            return None;
        }
        let (end, units) = scan_value(text, start, unquoted);
        (units >= 4).then(|| (end, format!("{}{}{SECRET}", &caps[1], &caps[2])))
    });
    replace_literal(&mut text, &RULES.email, "[REDACTED:email]");
    replace_validated(&mut text, &RULES.iban, iban_valid, "[REDACTED:iban]");
    replace_validated(&mut text, &RULES.card, luhn_valid, "[REDACTED:card]");
    replace_validated(
        &mut text,
        &RULES.tax_code,
        tax_code_valid,
        "[REDACTED:national_id]",
    );
    replace_validated(
        &mut text,
        &RULES.social,
        |text| {
            !matches!(&text[..3], "000" | "666")
                && !text.starts_with('9')
                && &text[4..6] != "00"
                && &text[7..] != "0000"
        },
        "[REDACTED:national_id]",
    );
    replace_scan(&mut text, &RULES.phone, |text, caps| {
        let found = caps.get(0)?;
        if text[..found.start()]
            .chars()
            .next_back()
            .is_some_and(|c| word(c) || c == '+')
        {
            return None;
        }
        let digits = found.as_str().bytes().filter(u8::is_ascii_digit).count();
        (8..=15)
            .contains(&digits)
            .then(|| (found.end(), "[REDACTED:phone]".into()))
    });
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_context_survive_while_secret_values_are_removed() {
        for (input, expected) in [
            (
                "DB_PASSWORD=synthetic-P4ss",
                "DB_PASSWORD=[REDACTED:secret]",
            ),
            (
                r#""client_secret": "abcd1234""#,
                r#""client_secret": "[REDACTED:secret]""#,
            ),
            ("api_key: zz-synthetic-value", "api_key: [REDACTED:secret]"),
            (
                "export GITHUB_TOKEN=abcdefgh",
                "export GITHUB_TOKEN=[REDACTED:secret]",
            ),
            (
                "Authorization: Basic dXNlcjpwYXNz",
                "Authorization: Basic [REDACTED:secret]",
            ),
            (
                "postgres://admin:hunter2@db.internal:5432/app",
                "postgres://[REDACTED:credentials]@db.internal:5432/app",
            ),
            (
                "REDIS_URL=redis://:Sup3rS3cretPw@cache:6379",
                "REDIS_URL=redis://[REDACTED:credentials]@cache:6379",
            ),
            (
                "postgres://app:p@ssw0rdXYZ@db",
                "postgres://[REDACTED:credentials]@db",
            ),
            ("DB_PASS=Xk29fjqLm3", "DB_PASS=[REDACTED:secret]"),
            ("password=a;bcdefghijklmnop", "password=[REDACTED:secret]"),
            (
                "DB_PASSWORD='hunter2 is my long pass'",
                "DB_PASSWORD='[REDACTED:secret]'",
            ),
            (
                "Cookie: sid=abcdef123456789; theme=dark",
                "Cookie: [REDACTED:secret]",
            ),
            (
                "mysql -u root -pS3cretValue1 appdb",
                "mysql -u root -p[REDACTED:secret] appdb",
            ),
            (
                "curl -u admin:S3cretValue1 https://example.test",
                "curl -u [REDACTED:credentials] https://example.test",
            ),
            (
                "tool --api-key abcdef123456 run",
                "tool --api-key [REDACTED:secret] run",
            ),
            (
                "sshpass -p hunter22x ssh host",
                "sshpass -p [REDACTED:secret] ssh host",
            ),
        ] {
            assert_eq!(redact_sensitive(input), expected, "{input}");
        }
    }

    #[test]
    fn formats_checksum_and_boundaries_match_reviewed_rules() {
        for (input, expected) in [
            (
                "Contact jane.doe@example.com today",
                "Contact [REDACTED:email] today",
            ),
            (
                "IBAN GB82 WEST 1234 5698 7654 32 on file",
                "IBAN [REDACTED:iban] on file",
            ),
            (
                "card 4111 1111 1111 1111 expired",
                "card [REDACTED:card] expired",
            ),
            ("CF RSSMRA85T10A562S ok", "CF [REDACTED:national_id] ok"),
            ("SSN 123-45-6789", "SSN [REDACTED:national_id]"),
            ("call +39 333 1234567 today", "call [REDACTED:phone] today"),
            ("call +1 (415) 555-0132.", "call [REDACTED:phone]."),
        ] {
            assert_eq!(redact_sensitive(input), expected);
        }
        for input in [
            "GB00WEST12345698765432",
            "card 4111 1111 1111 1112",
            "at 1791278893123 ms",
            "order 1234567890123456",
            "RSSMRA85T10A562X",
            "version +1.2.3.4",
            "ssn 000-12-3456",
            "ssn 666-12-3456",
            "build 1.2.3+456789",
            "https://example.com:8080/path",
            "http://[::1]:8080/x",
        ] {
            assert_eq!(redact_sensitive(input), input);
        }
    }

    #[test]
    fn ordinary_text_and_boolean_controls_remain_readable() {
        for text in [
            "Fix the token counter so cached input is included.",
            "Add password reset tests for the login form.",
            "The author: field is missing",
            "passenger: 4242 seats",
            "bypass_cache=true",
            "pass: ok",
            "compass=north-east",
            "oauth_state=abcdef",
            "primary_key=1",
            "monkey=banana",
            "keyboard=qwerty",
            "AUTOROUTER_AUTH_MODE=subscription",
            "export const LOCAL_AUTH_HEADER = 'x-autorouter-token';",
            "env.MAX_THINKING_TOKENS = '0'",
            "checks.tests_pass = results.failed === 0",
            "auth: true",
            "tool --password-stdin file",
            "tool --max-tokens 4096",
            "find . -print0 -prune",
            "ssh -p 2222 host",
            "mysql -u root -p",
        ] {
            assert_eq!(redact_sensitive(text), text, "{text}");
        }
    }

    #[test]
    fn private_key_blocks_and_cut_off_tails_are_redacted() {
        let lines = format!("{}\n{}", "K".repeat(64), "M".repeat(64));
        for input in [
            format!("-----BEGIN RSA PRIVATE KEY-----\n{lines}\n-----END RSA PRIVATE KEY-----"),
            format!("-----BEGIN PRIVATE KEY-----\n{lines}"),
            format!("{lines}\n-----END PRIVATE KEY-----"),
        ] {
            assert_eq!(redact_sensitive(&input), PRIVATE_KEY);
        }
    }

    #[test]
    fn provider_tokens_and_markers_are_redacted_idempotently() {
        for token in [
            format!("sk-ant-api03-{}", "Z".repeat(40)),
            format!("ghp_{}", "B".repeat(36)),
            format!("ya29.{}", "a".repeat(30)),
            format!("SG.{}.{}", "b".repeat(22), "c".repeat(22)),
            format!("shpat_{}", "d".repeat(32)),
            format!("hf_{}", "e".repeat(34)),
            format!("dop_v1_{}", "f".repeat(64)),
            format!("pypi-{}", "g".repeat(60)),
            format!("lin_api_{}", "h".repeat(34)),
            format!("ntn_{}", "i".repeat(34)),
            format!("dapi{}", "a1".repeat(16)),
            format!("ATATT3{}", "j".repeat(30)),
            format!("key-{}", "0a".repeat(16)),
            format!("SK{}", "1b".repeat(16)),
            format!("123456789:{}", "k".repeat(35)),
            format!("whsec_{}", "l".repeat(24)),
        ] {
            let input = format!("Rotate this value: {token} before deploy.");
            assert_eq!(
                redact_sensitive(&input),
                format!("Rotate this value: {SECRET} before deploy.")
            );
        }
        let once = redact_sensitive(
            "REDIS_URL=redis://:pw1234@h\nDB_PASS=abcd1234\nPW_SECRET='a b c d'\nmysql -pabcd1234\ncurl -u a:bcd\nCookie: sid=abcdef123456\n+39 333 1234567",
        );
        assert_eq!(redact_sensitive(&once), once);
    }

    #[test]
    fn utf16_value_lengths_and_ascii_boundaries_do_not_use_rust_defaults() {
        assert_eq!(
            redact_sensitive("password=😀😀"),
            "password=[REDACTED:secret]"
        );
        assert_eq!(
            redact_sensitive("password=\u{0085}abc"),
            "password=[REDACTED:secret]"
        );
        assert_eq!(
            redact_sensitive("password=\u{feff}abc"),
            "password=\u{feff}abc"
        );
        assert_eq!(redact_sensitive("paſſword=abcd"), "paſſword=abcd");
        assert_eq!(
            redact_sensitive("Cookie: trueish"),
            "Cookie:[REDACTED:secret]"
        );
        let oversized_user = format!("curl -u {}:abcd tail", "😀".repeat(33));
        assert_eq!(redact_sensitive(&oversized_user), oversized_user);
    }
}
