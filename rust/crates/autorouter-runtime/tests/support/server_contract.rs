//! Exact shared server-contract helpers; distinct original metadata policies stay separate.
use super::peer::Observation;
use autorouter_runtime::{http_client::HttpTransport, server_events::EventSink};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{HeaderMap, Request};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

const LIMIT: usize = 2 * 1024 * 1024;
type Events = Arc<Mutex<Vec<Value>>>;

pub fn parsed(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

pub fn headers(value: &Value) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (key, value) in value.as_object().unwrap() {
        out.insert(
            hyper::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
            value.as_str().unwrap().parse().unwrap(),
        );
    }
    out
}

pub fn header_is(map: &HeaderMap, name: &str, value: Option<&str>) -> bool {
    map.get(name).and_then(|value| value.to_str().ok()) == value
}

pub fn decode(value: &Value) -> Vec<u8> {
    let value = value.as_str().unwrap();
    assert!(value.len() < LIMIT * 2);
    openssl::base64::decode_block(value).unwrap()
}

pub fn chunks(row: &Value) -> Vec<Bytes> {
    row["response_writes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| Bytes::from(decode(&row["base64"])))
        .collect()
}

pub fn response_bytes(row: &Value) -> Vec<u8> {
    chunks(row)
        .iter()
        .flat_map(|chunk| chunk.iter().copied())
        .collect()
}

pub fn upstream_rows(row: &Value) -> Vec<Value> {
    row["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["role"] == "upstream")
        .unwrap()["requests"]
        .as_array()
        .unwrap()
        .clone()
}

pub fn events(rows: &[Value], name: &str) -> Vec<Value> {
    rows.iter()
        .filter(|row| row["event"] == name)
        .cloned()
        .collect()
}

pub fn event_names(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .map(|row| row["event"].as_str().unwrap().to_owned())
        .collect()
}

pub fn sink(active: &Events, history: &Events) -> EventSink {
    let (active, history) = (active.clone(), history.clone());
    Arc::new(move |event| {
        let value = event.to_serde_observation_lossy();
        for rows in [&active, &history] {
            let mut rows = rows.lock().unwrap();
            if rows.len() >= 128 {
                if rows.len() == 128 {
                    rows.push(json!({"event":"fixture_observer_overflow"}));
                }
                continue;
            }
            rows.push(value.clone());
        }
    })
}

pub fn same_request(observed: &Observation, wanted: &Value) -> bool {
    observed.method == wanted["method"].as_str().unwrap()
        && observed.path == wanted["path"].as_str().unwrap()
        && header_is(
            &observed.headers,
            "x-claude-code-session-id",
            wanted["headers"]["x-claude-code-session-id"].as_str(),
        )
}

pub fn compare_request(observed: &Observation, wanted: &Value) -> Result<(), String> {
    if !same_request(observed, wanted) {
        return Err("unexpected peer method/path/session".into());
    }
    for name in [
        "authorization",
        "x-api-key",
        "x-autorouter-token",
        "cookie",
        "anthropic-beta",
        "anthropic-version",
        "accept-encoding",
        "x-claude-code-session-id",
        "x-claude-code-agent-id",
        "x-claude-code-prompt-id",
        "x-claude-code-request-class",
    ] {
        if !header_is(
            &observed.headers,
            name,
            wanted["headers"].get(name).and_then(Value::as_str),
        ) {
            return Err(format!("semantic header mismatch: {name}"));
        }
    }
    let expected = wanted["request_body_utf8"].as_str().unwrap().as_bytes();
    if expected.is_empty() {
        if !observed.body.is_empty() {
            return Err("unexpected discovery body".into());
        }
    } else if parsed(&observed.body) != parsed(expected) {
        return Err("complete forwarded request differs".into());
    }
    Ok(())
}

pub fn javascript_number_equality(wanted: &Value, got: &Value) -> bool {
    match (wanted, got) {
        (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(a, b)| javascript_number_equality(a, b))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, value)| {
                    right
                        .get(key)
                        .is_some_and(|got| javascript_number_equality(value, got))
                })
        }
        _ => wanted == got,
    }
}

pub fn compare_metadata(wanted: &Value, got: &Value) {
    // This is a semantic comparison, not a rewritten observation. Undefined
    // source fields must be absent in the Rust Value interface. Number values
    // use JavaScript equality; only actual measured timings have no equality
    // oracle. Deterministic injected routing times remain exact.
    let expected = wanted.as_object().unwrap();
    let actual = got.as_object().unwrap();
    let excluded = ["request_id", "first_response_ms", "total_latency_ms"];
    let keys = expected
        .iter()
        .filter(|(key, value)| {
            !excluded.contains(&key.as_str()) && value["$js_type"] != "undefined"
        })
        .map(|(key, _)| key)
        .collect::<BTreeSet<_>>();
    let actual_keys = actual
        .keys()
        .filter(|key| !excluded.contains(&key.as_str()))
        .collect::<BTreeSet<_>>();
    assert_eq!(keys, actual_keys, "complete safe event field set");
    for key in keys {
        assert!(
            javascript_number_equality(&expected[key], &actual[key]),
            "safe event field {key}: expected {:?}, observed {:?}",
            expected[key],
            actual[key]
        );
    }
    for key in ["first_response_ms", "total_latency_ms"] {
        if expected
            .get(key)
            .is_some_and(|value| value["$js_type"] != "undefined")
        {
            assert!(
                actual[key]
                    .as_f64()
                    .is_some_and(|value| value.is_finite() && value >= 0.0),
                "validated actual measured timing {key}"
            );
        } else {
            assert!(
                !actual.contains_key(key),
                "source timing absence stays absent: {key}"
            );
        }
    }
}

pub fn fixture_case(row: &Value, index: usize) -> Value {
    let mut out = row.clone();
    let all = row["downstream"].as_array().unwrap();
    let selected = all
        .iter()
        .enumerate()
        .filter(|(_, r)| r["fixture_index"] == index)
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    assert!(!selected.is_empty());
    out["downstream"] = Value::Array(selected.iter().map(|i| all[*i].clone()).collect());
    let starts = events(row["statuses"].as_array().unwrap(), "request_start");
    let ids = selected
        .iter()
        .map(|i| starts[*i]["request_id"].clone())
        .collect::<Vec<_>>();
    out["route_calls"] = Value::Array(
        selected
            .iter()
            .map(|i| row["route_calls"][*i].clone())
            .collect(),
    );
    for name in ["statuses", "records", "decisions"] {
        out[name] = Value::Array(
            row[name]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| ids.contains(&r["request_id"]))
                .cloned()
                .collect(),
        );
    }
    out["completions"] = Value::Array(
        row["completions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| ids.contains(&r["request_id"]))
            .cloned()
            .collect(),
    );
    out["peers"] = Value::Array(
        row["peers"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["fixture_index"] == index)
            .cloned()
            .collect(),
    );
    out["gateway_config"] = row["gateway_configs"][index].clone();
    if row["gateway_configs"].as_array().unwrap().len() > 1 {
        out["id"] = format!("{}-fixture-{}", row["id"].as_str().unwrap(), index + 1).into();
    }
    out
}

pub fn compare_real_metadata(wanted: &Value, got: &Value, real: bool) {
    // This is a semantic comparison, not a rewritten observation. Undefined
    // source fields must be absent in the Rust Value interface. Number values
    // use JavaScript equality; only actual measured timings have no equality
    // oracle. Deterministic injected routing times remain exact.
    let expected = wanted.as_object().unwrap();
    let actual = got.as_object().unwrap();
    if wanted.get("timestamp").is_some() {
        assert!(
            regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$")
                .unwrap()
                .is_match(got["timestamp"].as_str().unwrap())
        );
    } else {
        assert!(got.get("timestamp").is_none());
    }
    let mut excluded = vec![
        "request_id",
        "first_response_ms",
        "total_latency_ms",
        "timestamp",
    ];
    if real {
        excluded.extend(["latency_ms", "evaluation_latency_ms", "routing_latency_ms"]);
    }
    let keys = expected
        .iter()
        .filter(|(key, value)| {
            !excluded.contains(&key.as_str()) && value["$js_type"] != "undefined"
        })
        .map(|(key, _)| key)
        .collect::<BTreeSet<_>>();
    let actual_keys = actual
        .keys()
        .filter(|key| !excluded.contains(&key.as_str()))
        .collect::<BTreeSet<_>>();
    assert_eq!(keys, actual_keys, "complete safe event field set");
    for key in keys {
        assert!(
            javascript_number_equality(&expected[key], &actual[key]),
            "safe event field {key}: expected {:?}, observed {:?}",
            expected[key],
            actual[key]
        );
    }
    let mut timings = vec!["first_response_ms", "total_latency_ms"];
    if real {
        timings.extend(["latency_ms", "evaluation_latency_ms", "routing_latency_ms"]);
    }
    for key in timings {
        if expected
            .get(key)
            .is_some_and(|value| value["$js_type"] != "undefined")
        {
            assert!(
                actual[key]
                    .as_f64()
                    .is_some_and(|value| value.is_finite() && value >= 0.0),
                "validated actual measured timing {key}"
            );
        } else {
            assert!(
                !actual.contains_key(key),
                "source timing absence stays absent: {key}"
            );
        }
    }
}

pub fn identity_headers(value: &Value) -> HeaderMap {
    let mut result = HeaderMap::new();
    for (key, value) in value.as_object().unwrap() {
        result.insert(
            hyper::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
            value.as_str().unwrap().parse().unwrap(),
        );
    }
    result
}

pub fn single_sink(rows: &Events) -> EventSink {
    let rows = rows.clone();
    Arc::new(move |event| {
        let mut rows = rows.lock().unwrap();
        if rows.len() >= 128 {
            if rows.len() == 128 {
                rows.push(json!({"event":"fixture_observer_overflow"}));
            }
            return;
        }
        rows.push(event.to_serde_observation_lossy());
    })
}

pub fn finish_counts(number: usize, counts: &BTreeMap<usize, usize>, capture: &str) {
    let capture: Value = serde_json::from_str(capture).unwrap();
    let row = capture["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["number"] == number)
        .unwrap();
    for (index, site) in row["assertions"].as_array().unwrap().iter().enumerate() {
        assert_eq!(
            counts.get(&(index + 1)).copied().unwrap_or(0),
            site["executions"].as_u64().unwrap() as usize,
            "server#{}:assert-{} execution count",
            number,
            index + 1
        );
    }
    assert_eq!(counts.len(), row["assertions"].as_array().unwrap().len());
}

pub struct Reply {
    pub status: u16,
    pub headers: HeaderMap,
    pub bytes: Vec<u8>,
}

pub async fn send_buffered(
    client: &autorouter_runtime::http_client::NativeHttpClient,
    gateway: Option<&autorouter_runtime::server::GatewayHandle>,
    row: &Value,
) -> Result<Reply, String> {
    let url = url::Url::parse(row["url"].as_str().unwrap()).unwrap();
    let mut request = Request::builder()
        .method(row["method"].as_str().unwrap())
        .uri(format!("http://{}{}", gateway.unwrap().address, url.path()));
    *request.headers_mut().unwrap() = headers(&row["headers"]);
    let response = client
        .request(
            request
                .body(Full::new(Bytes::from(
                    row["body"].as_str().unwrap_or_default().to_owned(),
                )))
                .unwrap(),
        )
        .await
        .map_err(|e| e.to_string())?;
    let (parts, body) = response.into_parts();
    let bytes = Limited::new(body, LIMIT)
        .collect()
        .await
        .map_err(|e| e.to_string())?
        .to_bytes()
        .to_vec();
    Ok(Reply {
        status: parts.status.as_u16(),
        headers: parts.headers,
        bytes,
    })
}

pub async fn wait_for_status(statuses: &Events, event: &str) -> Result<(), String> {
    for _ in 0..100 {
        if statuses
            .lock()
            .unwrap()
            .iter()
            .any(|row| row["event"] == event)
        {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    Err(format!("missing status event: {event}"))
}

pub struct Transcript<'a> {
    pub requests: &'a [Observation],
    pub routes: &'a [Value],
    pub completions: &'a [Value],
    pub history: &'a [Value],
    pub logs: &'a [Value],
    pub real: bool,
}

pub fn verify_transcript(row: &Value, replies: &[Reply], seen: &Transcript<'_>) {
    let expected = upstream_rows(row);
    assert_eq!(seen.requests.len(), expected.len());
    for (request, wanted) in seen.requests.iter().zip(&expected) {
        compare_request(request, wanted).unwrap();
    }
    let source_routes = row["route_calls"].as_array().unwrap();
    assert_eq!(seen.routes.len(), source_routes.len());
    for (route, wanted) in seen.routes.iter().zip(source_routes) {
        assert_eq!(route["document"], wanted["document"]);
        assert_eq!(route["signal_live"], true);
        assert_eq!(route["count_tokens"], true);
        for (native, source) in [
            ("scope", "scope"),
            ("prompt_id", "promptId"),
            ("request_class", "requestClass"),
        ] {
            if wanted["options"][source]["$js_type"] == "undefined" {
                assert_eq!(route[native], "");
            } else {
                assert_eq!(route[native], wanted["options"][source]);
            }
        }
        assert!(
            seen.history.iter().any(
                |event| event["event"] == "route" && event["request_id"] == route["request_id"]
            )
        );
    }
    // All four #32 clients are sequential and fully consumed before the next
    // request; preserve original request order even when scopes are identical.
    let source = row["statuses"].as_array().unwrap();
    let source_starts = events(source, "request_start");
    let starts = events(seen.history, "request_start");
    assert_eq!(starts.len(), source_starts.len());
    assert_eq!(seen.history.len(), source.len());
    for (wanted_start, start) in source_starts.iter().zip(&starts) {
        let wanted = source
            .iter()
            .filter(|e| e["request_id"] == wanted_start["request_id"])
            .collect::<Vec<_>>();
        let actual = seen
            .history
            .iter()
            .filter(|e| e["request_id"] == start["request_id"])
            .collect::<Vec<_>>();
        assert_eq!(actual.len(), wanted.len());
        for (got, wanted) in actual.iter().zip(&wanted) {
            compare_real_metadata(wanted, got, seen.real);
        }
    }
    for (index, reply) in replies.iter().enumerate() {
        let expected = &row["downstream"][index];
        assert_eq!(reply.status, expected["status"]);
        let body = expected["reads"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|read| read["kind"] == "read" && read["done"] == false)
            .flat_map(|read| decode(&read["base64"]))
            .collect::<Vec<_>>();
        assert_eq!(
            reply.bytes, body,
            "every original delivered byte remains unchanged for text and JSON readers"
        );
    }
    let source_completions = row["completions"].as_array().unwrap();
    let starts = events(seen.history, "request_start");
    assert_eq!(seen.completions.len(), starts.len());
    for completion in seen.completions {
        let index = starts
            .iter()
            .position(|event| event["request_id"] == completion["id"])
            .unwrap();
        let source_id = source_starts[index]["request_id"].clone();
        let wanted = source_completions
            .iter()
            .find(|r| r["request_id"] == source_id)
            .unwrap();
        assert_eq!(
            completion["accepted"],
            seen.real && !completion["evidence"].is_null()
        );
        if wanted["evidence"]["$js_type"] == "undefined" {
            assert!(completion["evidence"].is_null());
        } else {
            assert_eq!(completion["evidence"], wanted["evidence"]);
        }
    }
    for secret in [
        "PRIVATE_PROMPT_METADATA_MODE",
        "PRIVATE_RESPONSE_TEXT",
        "PRIVATE_ERROR",
        "Fix a typo",
        "local-test-token-123456789",
        "upstream-secret",
        "classifier-secret",
        "PRIVATE_SPEED",
        "PRIVATE_GEO",
        "PRIVATE_TIER",
        "PRIVATE_TOKEN",
    ] {
        assert!(
            !json!({"logs":seen.logs,"statuses":seen.history})
                .to_string()
                .contains(secret)
        );
    }
}

pub fn write_row_capture(
    directory: &std::ffi::OsStr,
    id: &Value,
    raw: &Value,
    limit: usize,
    inclusive: bool,
) {
    let bytes = serde_json::to_vec_pretty(raw).unwrap();
    if inclusive {
        assert!(bytes.len() <= limit);
    } else {
        assert!(bytes.len() < limit);
    }
    let path =
        std::path::Path::new(directory).join(format!("{}.native.json", id.as_str().unwrap()));
    write_capture_file(&path, &bytes);
}

pub fn write_capture_file(path: &std::path::Path, bytes: &[u8]) {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).unwrap();
    file.write_all(bytes).unwrap();
    file.write_all(b"\n").unwrap();
}
