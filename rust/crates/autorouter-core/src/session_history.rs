//! Bounded legacy/current session parsing and historical attribution. A
//! selection never implies successful completion; conflicting duplicates and
//! unsupported historical prices remain explicitly unpriced.
use crate::js_json::JsDocument;
use crate::savings::{PRICING_DATE, PRICING_SOURCE, PRICING_VERSION, estimate_outcome_savings};
use crate::telemetry_event::{normalization_projection, normalize_session_record, valid_timestamp};
use icu_collator::{Collator, CollatorBorrowed};
use icu_locale_core::Locale;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryLimits {
    pub max_files: usize,
    pub max_directory_entries: usize,
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
    pub max_line_bytes: usize,
    pub max_records: usize,
    pub max_lines: usize,
}
impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            max_files: 100,
            max_directory_entries: 10000,
            max_file_bytes: 4 * 1024 * 1024,
            max_total_bytes: 16 * 1024 * 1024,
            max_line_bytes: 16384,
            max_records: 5000,
            max_lines: 10000,
        }
    }
}
impl HistoryLimits {
    pub fn from_overrides(value: &Value) -> Result<Self, String> {
        let mut limits = serde_json::to_value(Self::default()).unwrap();
        if let Some(values) = value.as_object() {
            for (key, value) in values {
                let Some(max) = limits.get(key).and_then(Value::as_u64) else {
                    return Err("Invalid session-history read limit.".into());
                };
                let Some(number) = value
                    .as_f64()
                    .filter(|n| n.is_finite() && n.fract() == 0.0 && *n >= 1.0 && *n <= max as f64)
                else {
                    return Err("Invalid session-history read limit.".into());
                };
                limits[key] = json!(number as u64);
            }
        } else if !value.is_null() {
            return Err("Invalid session-history read limit.".into());
        }
        serde_json::from_value(limits).map_err(|_| "Invalid session-history read limit.".into())
    }
}
pub fn valid_id(id: &str) -> bool {
    id.strip_prefix("autorouter-session-")
        .is_some_and(|suffix| {
            (1..=160).contains(&suffix.len())
                && suffix
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
}
fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
#[derive(Default)]
struct Counts {
    entries: Vec<(String, u64)>,
    indexes: HashMap<String, usize>,
}
impl Counts {
    fn get(&self, key: &str) -> Option<&u64> {
        self.indexes.get(key).map(|index| &self.entries[*index].1)
    }
}
fn increment(counts: &mut Counts, key: &str) {
    if key.is_empty() {
        return;
    }
    if let Some(index) = counts.indexes.get(key) {
        counts.entries[*index].1 += 1;
    } else {
        counts.indexes.insert(key.into(), counts.entries.len());
        counts.entries.push((key.into(), 1));
    }
}
fn integer_key(key: &str) -> Option<u32> {
    key.parse::<u32>()
        .ok()
        .filter(|value| *value < u32::MAX && value.to_string() == key)
}
fn integer_key_order(a: &str, b: &str) -> std::cmp::Ordering {
    match (integer_key(a), integer_key(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    }
}
fn sorted_counts(mut counts: Counts, collator: &CollatorBorrowed<'_>) -> Value {
    // Object.entries puts integer keys first; stable locale sorting preserves
    // insertion order among equivalent keys (for example Thai punctuation).
    counts
        .entries
        .sort_by(|(a, _), (b, _)| integer_key_order(a, b));
    counts
        .entries
        .sort_by(|(a, _), (b, _)| collator.compare(a, b));
    // Object.fromEntries exposes integer keys first again on serialization.
    counts
        .entries
        .sort_by(|(a, _), (b, _)| integer_key_order(a, b));
    Value::Object(
        counts
            .entries
            .into_iter()
            .map(|(key, value)| (key, json!(value)))
            .collect(),
    )
}
/// ICU's POSIX default locale uses LC_ALL, then LC_MESSAGES, then LANG.
/// Empty values are present overrides; LC_COLLATE is not consulted by Node.
pub fn locale_from_environment(env: &Value) -> String {
    let raw = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|key| env.get(*key))
        .and_then(Value::as_str)
        .unwrap_or("en_US");
    let base = raw.split(['.', '@']).next().unwrap_or("");
    if matches!(base, "" | "C" | "POSIX") {
        return "en-US".into();
    }
    let locale = if raw.rsplit('@').next() == Some("nynorsk") {
        "nn-NO".into()
    } else {
        base.replace('_', "-")
    };
    locale
        .parse::<Locale>()
        .map(|value| value.to_string())
        .unwrap_or_else(|_| "en-US".into())
}
fn collator(locale: &str) -> CollatorBorrowed<'static> {
    let locale = locale
        .parse::<Locale>()
        .unwrap_or_else(|_| "en-US".parse().expect("fixed locale"));
    Collator::try_new(locale.into(), Default::default()).unwrap_or_else(|_| {
        Collator::try_new(Default::default(), Default::default())
            .expect("embedded root collation data")
    })
}
fn statistics(mut values: Vec<f64>) -> Value {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if n == 0 {
        json!({"samples":0})
    } else {
        json!({"samples":n,"p50":values[(n as f64*0.5).ceil() as usize-1],"p95":values[(n as f64*0.95).ceil() as usize-1],"max":values[n-1]})
    }
}
fn canonical(value: &Value) -> String {
    JsDocument::parse(value.to_string().as_bytes())
        .expect("serialized normalized JSON")
        .stringify()
}
#[derive(Default)]
struct Request {
    decision: Option<Value>,
    outcome: Option<Value>,
    conflicting: bool,
}
pub fn summarize(id: &str, rows: &[Value], coverage: Value) -> Value {
    summarize_with_locale(id, rows, coverage, "en-US")
}
pub fn summarize_with_locale(id: &str, rows: &[Value], mut coverage: Value, locale: &str) -> Value {
    let collator = collator(locale);
    let (mut selected, mut confirmed, mut sources, mut reasons, mut errors) = (
        Counts::default(),
        Counts::default(),
        Counts::default(),
        Counts::default(),
        Counts::default(),
    );
    let (mut decision_latencies, mut total_latencies, mut baselines, mut versions) =
        (Vec::new(), Vec::new(), BTreeSet::new(), BTreeSet::new());
    let (mut decisions, mut outcomes, mut legacy, mut duplicates, mut unversioned) =
        (0, 0, 0, 0, 0);
    let mut requests: Vec<Request> = Vec::new();
    let mut indexes = HashMap::new();
    for row in rows {
        let index = *indexes
            .entry(text(row, "request_id").to_owned())
            .or_insert_with(|| {
                requests.push(Request::default());
                requests.len() - 1
            });
        let request = &mut requests[index];
        let target = if row["event"] == "decision" {
            &mut request.decision
        } else {
            &mut request.outcome
        };
        if let Some(previous) = target {
            duplicates += 1;
            if canonical(previous) != canonical(row) {
                request.conflicting = true;
            }
            continue;
        }
        *target = Some(row.clone());
        if row["event"] == "decision" {
            decisions += 1;
            if row["schema_version"].as_f64() == Some(1.0) {
                legacy += 1;
            }
            increment(&mut selected, text(row, "selected_model"));
            increment(&mut sources, text(row, "source"));
            increment(&mut reasons, text(row, "reason"));
            increment(&mut errors, text(row, "classifier_error"));
            if let Some(n) = row["decision_latency_ms"]
                .as_f64()
                .filter(|n| n.is_finite())
            {
                decision_latencies.push(n);
            }
        } else {
            outcomes += 1;
            increment(&mut confirmed, text(row, "confirmed_model"));
            if !text(row, "baseline_model").is_empty() {
                baselines.insert(text(row, "baseline_model").to_owned());
            }
            if !text(row, "pricing_version").is_empty() {
                versions.insert(text(row, "pricing_version").to_owned());
            } else {
                unversioned += 1;
            }
            if let Some(n) = row["total_latency_ms"].as_f64().filter(|n| n.is_finite()) {
                total_latencies.push(n);
            }
        }
    }
    let (
        mut completed,
        mut failed,
        mut cancelled,
        mut pending,
        mut unconfirmed,
        mut outcome_only,
        mut conflicting,
    ) = (0, 0, 0, 0, 0, 0, 0);
    let (mut actual, mut baseline, mut saved, mut priced, mut unpriced) = (0.0, 0.0, 0.0, 0, 0);
    let mut unpriced_reasons = Counts::default();
    for request in &requests {
        if request.conflicting {
            conflicting += 1;
        }
        let estimate = if request.conflicting {
            json!({"priced":false,"unpriced_reason":"invalid_telemetry"})
        } else if let Some(outcome) = &request.outcome {
            estimate_outcome_savings(outcome)
        } else {
            json!({"priced":false,"unpriced_reason":"missing_outcome"})
        };
        if estimate["priced"] == true {
            priced += 1;
            actual += estimate["actual_usd"].as_f64().unwrap_or(0.0);
            baseline += estimate["baseline_usd"].as_f64().unwrap_or(0.0);
            saved += estimate["saved_usd"].as_f64().unwrap_or(0.0);
        } else {
            unpriced += 1;
            increment(&mut unpriced_reasons, text(&estimate, "unpriced_reason"));
        }
        let Some(outcome) = &request.outcome else {
            pending += 1;
            continue;
        };
        if request.decision.is_none() {
            outcome_only += 1;
        }
        if outcome["status"] == "cancelled" {
            cancelled += 1;
        } else if outcome["status"] == "error"
            || outcome["http_status"]
                .as_f64()
                .is_some_and(|n| !(200.0..300.0).contains(&n))
        {
            failed += 1;
        } else if outcome["completion_confirmed"] == true && !request.conflicting {
            completed += 1;
        } else {
            unconfirmed += 1;
        }
    }
    let mut timestamps = rows
        .iter()
        .filter_map(|r| r["timestamp"].as_str())
        .collect::<Vec<_>>();
    timestamps.sort();
    let fallback = *sources.get("fallback").unwrap_or(&0);
    coverage["legacy_decisions"] = json!(legacy);
    coverage["duplicate_records"] = json!(duplicates);
    coverage["conflicting_requests"] = json!(conflicting);
    coverage["partial"] = json!(coverage["partial"] == true || duplicates > 0);
    let facts = if versions.contains(PRICING_VERSION) {
        json!([{"version":PRICING_VERSION,"date":PRICING_DATE,"source":PRICING_SOURCE}])
    } else {
        json!([])
    };
    let selected = sorted_counts(selected, &collator);
    let confirmed = sorted_counts(confirmed, &collator);
    let sources = sorted_counts(sources, &collator);
    let reasons = sorted_counts(reasons, &collator);
    let errors = sorted_counts(errors, &collator);
    let unpriced_reasons = sorted_counts(unpriced_reasons, &collator);
    let mut result = json!({"id":id,"started_at":timestamps.first(),"updated_at":timestamps.last(),"requests":requests.len(),"decisions":decisions,"outcomes":outcomes,"completed":completed,"failed":failed,"cancelled":cancelled,"pending":pending,"unconfirmed":unconfirmed,"outcome_only":outcome_only,"selected_models":selected,"confirmed_models":confirmed,"sources":sources,"routing_reasons":reasons,"classifier_errors":errors,"fallbacks":fallback,"fallback_rate":if decisions>0{fallback as f64/decisions as f64}else{0.0},"decision_latency_ms":statistics(decision_latencies),"total_latency_ms":statistics(total_latencies),"mixed_baselines":baselines.len()>1,"baseline_models":baselines,"mixed_pricing_versions":versions.len()>1,"pricing_versions":versions,"pricing_facts":facts,"unversioned_outcomes":unversioned,"savings":{"basis":"API-equivalent estimate, not subscription charges","actual_usd":actual,"baseline_usd":baseline,"saved_usd":saved,"percent":if baseline==0.0{0.0}else{saved/baseline*100.0},"priced_requests":priced,"unpriced_requests":unpriced,"unpriced_reasons":unpriced_reasons},"coverage":coverage});
    if let Some(session) = rows
        .first()
        .and_then(|r| r["session_id"].as_str())
        .filter(|s| !s.is_empty())
    {
        result["session_id"] = json!(session);
    }
    result
}
pub fn parse_session(bytes: &[u8], file_bytes: u64, id: &str, limits: &HistoryLimits) -> Value {
    parse_session_with_locale(bytes, file_bytes, id, limits, "en-US")
}
pub fn parse_session_with_locale(
    bytes: &[u8],
    file_bytes: u64,
    id: &str,
    limits: &HistoryLimits,
    locale: &str,
) -> Value {
    let mut rows = Vec::new();
    let (mut offset, mut lines, mut invalid, mut oversized, mut mixed) = (0, 0, 0, 0, 0);
    let mut session_identity = None;
    let mut truncated = (bytes.len() as u64) < file_bytes;
    let mut incomplete = false;
    while offset < bytes.len() {
        if rows.len() >= limits.max_records || lines >= limits.max_lines {
            truncated = true;
            break;
        }
        let Some(relative) = bytes[offset..].iter().position(|b| *b == b'\n') else {
            incomplete = true;
            break;
        };
        let end = offset + relative;
        lines += 1;
        if end - offset > limits.max_line_bytes {
            oversized += 1;
            offset = end + 1;
            continue;
        }
        let row = (|| {
            let doc = JsDocument::parse(&bytes[offset..end]).ok()?;
            let entry = normalization_projection(&doc);
            if !entry.is_object()
                || !matches!(entry["schema_version"].as_f64(), Some(1.0 | 2.0))
                || (entry["schema_version"].as_f64() == Some(1.0) && entry["event"] != "decision")
                || !entry["timestamp"].as_str().is_some_and(valid_timestamp)
            {
                return None;
            }
            let mut row =
                normalize_session_record(&entry, entry.get("prompt_excerpt").is_some(), "")?;
            row["schema_version"] = entry["schema_version"].clone();
            Some(row)
        })();
        if let Some(row) = row {
            let session = text(&row, "session_id").to_owned();
            let identity = session_identity.get_or_insert_with(|| session.clone());
            if *identity != session {
                mixed += 1;
            } else {
                rows.push(row);
            }
        } else {
            invalid += 1;
        }
        offset = end + 1;
    }
    let coverage = json!({"bytes_read":bytes.len(),"file_bytes":file_bytes,"lines_read":lines,"invalid_records":invalid,"oversized_lines":oversized,"mixed_session_records":mixed,"truncated":truncated,"incomplete_tail":incomplete,"partial":truncated||incomplete||invalid>0||oversized>0||mixed>0});
    json!({"summary":summarize_with_locale(id,&rows,coverage,locale),"records":rows})
}
fn models_text(models: &Value) -> String {
    let result = models
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(name, count)| format!("{name} ({count})"))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    if result.is_empty() {
        "none".into()
    } else {
        result
    }
}
fn n(v: &Value, key: &str) -> String {
    v.get(key)
        .map(|v| {
            if let Some(n) = v.as_f64() {
                ryu_js::Buffer::new().format(n).to_owned()
            } else {
                v.to_string()
            }
        })
        .unwrap_or_default()
}
fn fixed(value: &Value, digits: u8) -> String {
    ryu_js::Buffer::new()
        .format_to_fixed(value.as_f64().unwrap_or(0.0), digits)
        .into()
}
fn joined_values(value: &Value) -> String {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}
pub fn summary_lines(summary: &Value) -> Vec<String> {
    let mut lines = vec![
        format!("ID: {}", text(summary, "id")),
        format!(
            "Session: {}; {} to {}",
            summary["session_id"].as_str().unwrap_or("anonymous"),
            summary["started_at"].as_str().unwrap_or("no valid records"),
            summary["updated_at"].as_str().unwrap_or("unknown")
        ),
        format!(
            "Observed: {} selections; {} confirmed completed; {} failed; {} cancelled; {} without outcome; {} unconfirmed outcomes.",
            n(summary, "decisions"),
            n(summary, "completed"),
            n(summary, "failed"),
            n(summary, "cancelled"),
            n(summary, "pending"),
            n(summary, "unconfirmed")
        ),
        format!(
            "Selected models: {}",
            models_text(&summary["selected_models"])
        ),
        format!(
            "Response models observed: {}",
            models_text(&summary["confirmed_models"])
        ),
        format!(
            "Fallbacks: {} of {} selections ({}%); routing reasons: {}",
            n(summary, "fallbacks"),
            n(summary, "decisions"),
            fixed(
                &json!(summary["fallback_rate"].as_f64().unwrap_or(0.0) * 100.0),
                1
            ),
            models_text(&summary["routing_reasons"])
        ),
    ];
    let savings = &summary["savings"];
    let saved = if savings["priced_requests"].as_f64().is_some_and(|n| n > 0.0) {
        format!("${}", fixed(&savings["saved_usd"], 4))
    } else {
        "unavailable".into()
    };
    lines.push(format!("API-equivalent savings: {saved}; priced {}/{} observed requests; {} unpriced. These are not subscription charges.",n(savings,"priced_requests"),n(summary,"requests"),n(savings,"unpriced_requests")));
    let baselines = joined_values(&summary["baseline_models"]);
    if !baselines.is_empty() {
        lines.push(format!(
            "Recorded Opus baseline{}: {baselines}",
            if summary["mixed_baselines"] == true {
                "s (mixed)"
            } else {
                ""
            }
        ));
    }
    let versions = joined_values(&summary["pricing_versions"]);
    let unversioned = summary["unversioned_outcomes"].as_f64().unwrap_or(0.0);
    lines.push(format!(
        "Recorded pricing version{}: {}{}.",
        if summary["mixed_pricing_versions"] == true {
            "s (mixed)"
        } else {
            ""
        },
        if versions.is_empty() {
            "not recorded"
        } else {
            &versions
        },
        if unversioned > 0.0 {
            format!(
                "; {} outcomes without a recorded version",
                n(summary, "unversioned_outcomes")
            )
        } else {
            String::new()
        }
    ));
    if let Some(facts) = summary["pricing_facts"].as_array() {
        for fact in facts {
            lines.push(format!(
                "Pricing facts {}, reviewed {}: {}",
                text(fact, "version"),
                text(fact, "date"),
                text(fact, "source")
            ));
        }
    }
    if savings["unpriced_requests"]
        .as_f64()
        .is_some_and(|n| n > 0.0)
    {
        lines.push(format!(
            "Unpriced reasons: {}",
            models_text(&savings["unpriced_reasons"])
        ));
    }
    let latency = &summary["decision_latency_ms"];
    if latency["samples"].as_f64().is_some_and(|n| n > 0.0) {
        lines.push(format!(
            "Decision latency: p50 {} ms; p95 {} ms ({} samples).",
            n(latency, "p50"),
            n(latency, "p95"),
            n(latency, "samples")
        ));
    }
    if summary["coverage"]["partial"] == true {
        lines.push("Partial history: limits, incomplete writes, duplicate or unreadable records affect these observed counts.".into());
    }
    if summary["coverage"]["legacy_decisions"]
        .as_f64()
        .is_some_and(|n| n > 0.0)
    {
        lines.push(format!(
            "{} legacy decisions record selection only; no successful response is implied.",
            n(&summary["coverage"], "legacy_decisions")
        ));
    }
    lines
}
pub fn report_lines(report: &Value, show: bool) -> Vec<String> {
    let mut lines = Vec::new();
    if !show {
        if let Some(sessions) = report["sessions"].as_array() {
            if sessions.is_empty() {
                lines.push("No readable session logs found.".into());
            }
            for session in sessions {
                lines.extend(summary_lines(session));
            }
        }
        if report["coverage"]["partial"] == true {
            lines.push("Partial scan: displayed sessions or counts are limited; use sessions show ID for an individual file.".into());
        }
        return lines;
    }
    lines.extend(summary_lines(&report["summary"]));
    if let Some(rows) = report["records"].as_array() {
        for row in rows {
            let description = if row["event"] == "decision" {
                format!(
                    "selected {}; {}; {}",
                    text(row, "selected_model"),
                    row["source"].as_str().unwrap_or("unknown source"),
                    row["reason"].as_str().unwrap_or("unspecified reason")
                )
            } else {
                format!(
                    "{}{}{}; response model {}; completion {}",
                    text(row, "status"),
                    row.get("http_status")
                        .map(|v| format!(" (HTTP {})", n(&json!({"status":v}), "status")))
                        .unwrap_or_default(),
                    row["error_type"]
                        .as_str()
                        .map(|s| format!("; {s}"))
                        .unwrap_or_default(),
                    row["confirmed_model"].as_str().unwrap_or("not observed"),
                    if row["completion_confirmed"] == true {
                        "confirmed"
                    } else {
                        "unconfirmed"
                    }
                )
            };
            lines.push(format!(
                "{} {}: {description}",
                text(row, "timestamp"),
                text(row, "request_id")
            ));
            if !text(row, "prompt_excerpt").is_empty() {
                let safe = text(row, "prompt_excerpt")
                    .chars()
                    .filter(
                        |c| !matches!(*c as u32,0..=31|127..=159|0x202a..=0x202e|0x2066..=0x2069),
                    )
                    .collect::<String>();
                lines.push(format!(
                    "  Prompt excerpt: {safe}{}",
                    if row["prompt_truncated"] == true {
                        "… [truncated]"
                    } else {
                        ""
                    }
                ));
            }
        }
    }
    lines
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_collation_preserves_locale_contractions_case_and_stable_ties() {
        let order = |locale: &str, keys: &[&str]| {
            let mut counts = Counts::default();
            for key in keys {
                increment(&mut counts, key);
            }
            sorted_counts(counts, &collator(locale))
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(order("cs", &["ch", "i", "h"]), ["h", "ch", "i"]);
        assert_eq!(order("da", &["a", "A"]), ["A", "a"]);
        assert_eq!(order("tr", &["i", "I"]), ["I", "i"]);
        assert_eq!(order("th", &["a", "_a", "-a"]), ["a", "_a", "-a"]);
        assert_eq!(order("th", &["-a", "_a", "a"]), ["-a", "_a", "a"]);
        assert_eq!(order("en", &["10", "2", "a"]), ["2", "10", "a"]);
    }
    #[test]
    fn locale_environment_honors_present_empty_overrides() {
        assert_eq!(
            locale_from_environment(&json!({"LANG":"cs_CZ.UTF-8"})),
            "cs-CZ"
        );
        assert_eq!(
            locale_from_environment(&json!({"LANG":"cs_CZ.UTF-8","LC_MESSAGES":"da_DK.UTF-8"})),
            "da-DK"
        );
        assert_eq!(
            locale_from_environment(
                &json!({"LANG":"cs_CZ.UTF-8","LC_MESSAGES":"da_DK.UTF-8","LC_ALL":""})
            ),
            "en-US"
        );
        assert_eq!(
            locale_from_environment(&json!({"LC_COLLATE":"cs_CZ.UTF-8","LANG":"C.UTF-8"})),
            "en-US"
        );
    }

    #[test]
    fn conflicting_duplicates_and_legacy_selections_never_imply_completed_execution() {
        let row = json!({"schema_version":1,"event":"decision","timestamp":"2026-10-05T12:00:00.000Z","request_id":"r","requested_model":"claude-opus-5-5","selected_model":"claude-sonnet-5-5"});
        let outcome = json!({"schema_version":2,"event":"outcome","timestamp":"2026-10-05T12:00:00.000Z","request_id":"r","status":"completed","completion_confirmed":true,"confirmed_model":"claude-sonnet-5-5"});
        let mut conflicting = outcome.clone();
        conflicting["confirmed_model"] = json!("claude-opus-5-5");
        let bytes = format!("{row}\n{outcome}\n{conflicting}\n");
        let result = parse_session(
            bytes.as_bytes(),
            bytes.len() as u64,
            "autorouter-session-test",
            &HistoryLimits::default(),
        );
        assert_eq!(result["summary"]["completed"], 0);
        assert_eq!(result["summary"]["unconfirmed"], 1);
        assert_eq!(result["summary"]["coverage"]["legacy_decisions"], 1);
        assert_eq!(
            result["summary"]["savings"]["unpriced_reasons"]["invalid_telemetry"],
            1
        );
    }
    #[test]
    fn truncated_tail_bad_schema_and_mixed_identity_are_explicit() {
        let row = json!({"schema_version":2,"event":"decision","timestamp":"2026-10-05T12:00:00.000Z","request_id":"r","session_id":"s","requested_model":"claude-opus-5-5","selected_model":"claude-sonnet-5-5"});
        let mut other = row.clone();
        other["session_id"] = json!("other");
        let bytes = format!("{row}\n{other}\n{{}}\nunfinished");
        let result = parse_session(
            bytes.as_bytes(),
            bytes.len() as u64,
            "autorouter-session-test",
            &HistoryLimits::default(),
        );
        assert_eq!(result["records"].as_array().unwrap().len(), 1);
        assert_eq!(result["summary"]["coverage"]["mixed_session_records"], 1);
        assert_eq!(result["summary"]["coverage"]["invalid_records"], 1);
        assert_eq!(result["summary"]["coverage"]["incomplete_tail"], true);
    }
}
