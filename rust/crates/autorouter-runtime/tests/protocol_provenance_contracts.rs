//! Original protocol-fixtures #1 authoring/provenance assertions, separately
//! from the actual synthetic HTTP replay and historical provider observations.
use serde_json::{Value, json};
use std::collections::HashSet;

const CORPUS: &str = include_str!("../../../../test/fixtures/claude-protocol-v1.json");
// Validate the actual ISO forms in the pinned corpus, not a general Date.parse API.
fn finite_fixture_timestamp(value: &str, shape: &regex::Regex) -> bool {
    let Some(parts) = shape.captures(value) else {
        return false;
    };
    let Some(year) = parts[1].parse::<i32>().ok() else {
        return false;
    };
    let Some(month) = parts[2]
        .parse::<u8>()
        .ok()
        .and_then(|month| time::Month::try_from(month).ok())
    else {
        return false;
    };
    let Some(day) = parts[3].parse::<u8>().ok() else {
        return false;
    };
    let Ok(date) = time::Date::from_calendar_date(year, month, day) else {
        return false;
    };
    let clock = if parts.get(4).is_some() {
        let (Some(hour), Some(minute), Some(second)) = (
            parts[4].parse::<u8>().ok(),
            parts[5].parse::<u8>().ok(),
            parts[6].parse::<u8>().ok(),
        ) else {
            return false;
        };
        let fraction = parts.get(7).map_or("", |part| part.as_str());
        let nanos = if fraction.is_empty() {
            0
        } else {
            format!("{fraction:0<9}").parse::<u32>().unwrap()
        };
        let Ok(clock) = time::Time::from_hms_nano(hour, minute, second, nanos) else {
            return false;
        };
        clock
    } else {
        time::Time::MIDNIGHT
    };
    (time::PrimitiveDateTime::new(date, clock)
        .assume_utc()
        .unix_timestamp_nanos() as f64)
        .is_finite()
}
fn validate(corpus: &Value) -> Result<(), &'static str> {
    if corpus["fixture_version"] != 1 || corpus["synthetic"] != true {
        return Err("version or synthetic declaration");
    }
    let date = regex::Regex::new(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$").unwrap();
    if !corpus["reviewed_at"]
        .as_str()
        .is_some_and(|date_text| date.is_match(date_text))
    {
        return Err("review date shape");
    }
    if !corpus["description"]
        .as_str()
        .is_some_and(|description| description.contains("not captured traffic"))
    {
        return Err("synthetic description");
    }
    let limits = corpus["limits"]
        .as_array()
        .ok_or("limits")?
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    if !limits.contains("not evaluator quality or downstream task success") {
        return Err("quality scope");
    }
    let historical = corpus["historical_evidence"]
        .as_array()
        .ok_or("historical evidence")?;
    let mut seen = HashSet::new();
    let versions: Vec<_> = historical
        .iter()
        .filter_map(|row| row["claude_version"].as_str())
        .filter(|version| seen.insert(*version))
        .collect();
    if versions != ["2.1.284", "2.1.285"] {
        return Err("historical client versions");
    }
    let checksum = regex::Regex::new(r"^[a-f0-9]{64}$").unwrap();
    let artifact = regex::Regex::new(r"^artifacts/[A-Za-z0-9_-]+\.json$").unwrap();
    let timestamp_shape = regex::Regex::new(r"^([0-9]{4})-([0-9]{2})-([0-9]{2})(?:T([0-9]{2}):([0-9]{2}):([0-9]{2})(?:\.([0-9]{1,9}))?Z)?$").unwrap();
    for row in historical {
        if !row["sha256"]
            .as_str()
            .is_some_and(|value| checksum.is_match(value))
        {
            return Err("historical checksum");
        }
        if !row["artifact"]
            .as_str()
            .is_some_and(|value| artifact.is_match(value))
        {
            return Err("historical artifact");
        }
        if !row["observed_at"].is_null() {
            let observed = row["observed_at"].as_str().ok_or("historical timestamp")?;
            if !finite_fixture_timestamp(observed, &timestamp_shape) {
                return Err("historical timestamp");
            }
        }
    }
    if !historical
        .iter()
        .find(|row| row["kind"] == "historical_synthetic_auto_permission_probe")
        .is_some_and(|row| row.get("observed_at") == Some(&Value::Null))
    {
        return Err("unknown historical timestamp must remain null");
    }
    let contracts = corpus["official_contracts"]
        .as_array()
        .ok_or("official contracts")?;
    if contracts
        .iter()
        .filter_map(|row| row["area"].as_str())
        .collect::<Vec<_>>()
        != ["messages", "streaming", "fallback", "tool_search"]
    {
        return Err("official areas");
    }
    for row in contracts {
        let url = url::Url::parse(row["url"].as_str().ok_or("official URL")?)
            .map_err(|_| "official URL")?;
        if url.origin().ascii_serialization() != "https://platform.claude.com" {
            return Err("official origin");
        }
    }
    let cases = corpus["cases"].as_array().ok_or("cases")?;
    let ids: HashSet<_> = cases.iter().filter_map(|row| row["id"].as_str()).collect();
    if ids.len() != cases.len() {
        return Err("unique case IDs");
    }
    let coverage: HashSet<_> = cases
        .iter()
        .flat_map(|row| row["covers"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .collect();
    for required in [
        "haiku_floor",
        "opus",
        "thinking",
        "deferred_tools",
        "compaction",
        "goal_feedback",
        "agent_scope",
        "auxiliary_verdict",
        "provider_fallback",
        "mixed_usage",
        "incomplete_response",
    ] {
        if !coverage.contains(required) {
            return Err("required protocol coverage");
        }
    }
    for scenario in cases {
        for step in scenario["steps"].as_array().ok_or("scenario steps")? {
            if ["request", "response", "expected"]
                .iter()
                .any(|name| step.get(*name).is_none_or(Value::is_null))
            {
                return Err("complete synthetic step");
            }
            if !step["evaluator_tier"].is_null()
                && !["haiku", "sonnet", "opus"]
                    .contains(&step["evaluator_tier"].as_str().unwrap_or(""))
            {
                return Err("evaluator tier");
            }
            if step["expected_request_overrides"]
                .as_object()
                .ok_or("request overrides")?
                .keys()
                .any(|key| !["model", "thinking"].contains(&key.as_str()))
            {
                return Err("only reviewed request adaptations");
            }
            if step["request"].to_string().contains("\"signature\"") {
                return Err("synthetic signatures prohibited");
            }
        }
    }
    Ok(())
}
#[test]
fn original_synthetic_protocol_metadata_retains_every_provenance_boundary() {
    use sha2::{Digest, Sha256};
    assert_eq!(
        format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
        "4cdaa7716255ec17032e26437f65c22aea170d8abb4d259c35c61e2709a1b41f"
    );
    let corpus: Value = serde_json::from_str(CORPUS).unwrap();
    validate(&corpus).unwrap();
}
#[test]
fn protocol_provenance_validation_rejects_quality_laundering_and_invented_history() {
    let corpus: Value = serde_json::from_str(CORPUS).unwrap();
    for (path, value, reason) in [
        (
            "/synthetic",
            json!(false),
            "version or synthetic declaration",
        ),
        (
            "/official_contracts/0/url",
            json!("https://example.invalid/private"),
            "official origin",
        ),
        (
            "/cases/0/steps/0/expected_request_overrides/system",
            json!("invented"),
            "only reviewed request adaptations",
        ),
        (
            "/cases/0/steps/0/request/signature",
            json!("synthetic-forged-signature"),
            "synthetic signatures prohibited",
        ),
    ] {
        let mut changed = corpus.clone();
        let (parent, field) = path.rsplit_once('/').unwrap();
        changed.pointer_mut(parent).unwrap()[field] = value;
        assert_eq!(validate(&changed), Err(reason));
    }
    let mut changed = corpus;
    let historical = changed["historical_evidence"].as_array_mut().unwrap();
    historical
        .iter_mut()
        .find(|row| row["kind"] == "historical_synthetic_auto_permission_probe")
        .unwrap()["observed_at"] = json!("2026-10-13");
    assert_eq!(
        validate(&changed),
        Err("unknown historical timestamp must remain null")
    );
}
