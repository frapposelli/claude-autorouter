//! Frozen finite tool schedules through the actual router/evaluator; no service.
use super::*;
use autorouter_core::js_json::JsDocument;
use autorouter_runtime::http_client::HttpError;
use http_body_util::BodyExt;
use hyper::Response;
use hyper::body::{Body, Frame, SizeHint};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::task::{Context, Poll};
const CORPUS: &str = include_str!("../../parity/cases/ollama-routing-tool-contracts.jsonl");
const CAPTURE: &str = include_str!("../../parity/cases/ollama-routing-tool-contracts.capture.json");
const CORPUS_SHA: &str = "a7c1f9520daafc280723e4f2d33536483e7cd782a5fb557047ff700cebfcea1d";
fn corpus(bytes: &str) -> Result<Vec<Value>, &'static str> {
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("corpus byte bound");
    }
    if format!("{:x}", Sha256::digest(bytes.as_bytes())) != CORPUS_SHA {
        return Err("corpus identity");
    }
    let capture: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(capture["corpus_sha256"], CORPUS_SHA);
    assert_eq!(capture["selected_definitions"], json!([5, 6, 7, 8]));
    assert_eq!(capture["static_assertions"], 25);
    assert_eq!(capture["helper_static_assertions"], 9);
    assert_eq!(capture["expanded_assertions"], 377);
    assert_eq!(capture["requests"], 58);
    let rows: Vec<Value> = bytes
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(rows.len(), 6);
    assert_eq!(
        rows.iter()
            .map(|r| r["requests"].as_array().unwrap().len())
            .sum::<usize>(),
        58
    );
    assert_eq!(
        rows.iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "ollama-routing-tool-5-1",
            "ollama-routing-tool-6-1",
            "ollama-routing-tool-6-2",
            "ollama-routing-tool-7-1",
            "ollama-routing-tool-8-1",
            "ollama-routing-tool-8-2"
        ]
    );
    Ok(rows)
}
fn wire(value: &Value) -> Value {
    JsDocument::parse(value.to_string().as_bytes())
        .unwrap()
        .to_serde_observation_lossy()
}
fn settings(row: &Value) -> RouterConfig {
    let config = read_config(&json!({"AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_OLLAMA_MODEL":row["config"]["ollamaModel"]}),false,Path::new("/synthetic")).unwrap();
    assert_eq!(
        wire(&serde_json::to_value(&config).unwrap()),
        wire(&row["config"])
    );
    assert_eq!(
        row["config_tags"],
        json!([
            {"path":"$.sessionLogDir","kind":"undefined"},
            {"path":"$.stopHookBlockCap","kind":"undefined"},
            {"path":"$.anthropicKey","kind":"undefined"},
            {"path":"$.jevKey","kind":"undefined"},
            {"path":"$.localToken","kind":"undefined"}
        ])
    );
    config
}
struct FiniteBody {
    inner: Full<Bytes>,
    owners: Arc<AtomicUsize>,
}
impl Drop for FiniteBody {
    fn drop(&mut self) {
        self.owners.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Body for FiniteBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        Pin::new(&mut self.get_mut().inner).poll_frame(cx)
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
struct Replay {
    pending: Mutex<VecDeque<Value>>,
    observed: Mutex<Vec<Value>>,
    invalid: AtomicUsize,
    bodies: Arc<AtomicUsize>,
}
impl Replay {
    fn new(row: &Value) -> Arc<Self> {
        let requests = row["requests"].as_array().unwrap();
        assert!(requests.len() <= 20);
        for request in requests {
            assert_eq!(
                request["node_options"],
                json!({"redirect":"error","signal":"nonaborted_AbortSignal"})
            );
            assert!(request["response"]["body"].as_str().unwrap().len() <= 65536);
        }
        Arc::new(Self {
            pending: Mutex::new(requests.clone().into()),
            observed: Mutex::new(Vec::new()),
            invalid: AtomicUsize::new(0),
            bodies: Arc::new(AtomicUsize::new(0)),
        })
    }
    fn verified(&self, row: &Value) -> bool {
        self.pending.lock().unwrap().is_empty()
            && self.invalid.load(Ordering::SeqCst) == 0
            && self.observed.lock().unwrap().len() == row["requests"].as_array().unwrap().len()
            && self.bodies.load(Ordering::SeqCst) == 0
    }
}
impl HttpTransport for Replay {
    type ResponseBody = FiniteBody;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        let expected = self.pending.lock().unwrap().pop_front();
        let Some(expected) = expected else {
            self.invalid.fetch_add(1, Ordering::SeqCst);
            return Err(HttpError::Network);
        };
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        assert!(bytes.len() <= 65536);
        let headers: serde_json::Map<String, Value> = parts
            .headers
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v.to_str().unwrap())))
            .collect();
        let observed = json!({"url":parts.uri.to_string(),"method":parts.method.as_str(),"headers":headers,"body":if bytes.is_empty(){Value::Null}else{json!(std::str::from_utf8(&bytes).unwrap())}});
        let required = json!({"url":expected["url"],"method":expected["method"],"headers":expected["headers"],"body":expected["body"]});
        if observed != required {
            self.invalid.fetch_add(1, Ordering::SeqCst);
            eprintln!(
                "request mismatch index={} url={}",
                self.observed.lock().unwrap().len(),
                parts.uri
            );
        }
        assert!(
            parts.headers.get("authorization").is_none()
                && parts.headers.get("x-api-key").is_none()
        );
        self.observed.lock().unwrap().push(observed);
        // A matching safe fallback must not hide the independent mismatch flag.
        let response = &expected["response"];
        let mut builder = Response::builder().status(response["status"].as_u64().unwrap() as u16);
        for (k, v) in response["headers"].as_object().unwrap() {
            builder = builder.header(k.as_str(), v.as_str().unwrap());
        }
        self.bodies.fetch_add(1, Ordering::SeqCst);
        Ok(builder
            .body(FiniteBody {
                inner: Full::new(Bytes::from(response["body"].as_str().unwrap().to_owned())),
                owners: self.bodies.clone(),
            })
            .unwrap())
    }
}
fn progress(line: &str) -> String {
    if let Some((prefix, duration)) = line.rsplit_once(", ")
        && let Some(number) = duration.strip_suffix("ms")
    {
        let number = number.parse::<f64>().unwrap();
        assert!(number.is_finite() && number >= 0.0);
        return format!("{prefix}, <clock>ms");
    }
    line.into()
}
fn report(value: &Value, row: &Value) -> Value {
    let mut out = value.clone();
    assert!(out.to_string().len() <= 131072);
    assert!(autorouter_core::telemetry_event::valid_timestamp(
        out["timestamp"].as_str().unwrap()
    ));
    out["timestamp"] = json!("<clock>");
    assert!(
        out["warmup_ms"]
            .as_f64()
            .is_some_and(|n| n.is_finite() && n >= 0.0)
    );
    out["warmup_ms"] = json!(0);
    assert_eq!(out["fixture_sha256"], row["fixture_sha256"]);
    if row["fixture_hash_supplied"] != true {
        out.as_object_mut().unwrap().remove("fixture_sha256");
    } else {
        assert_eq!(out["fixture_sha256"], row["supplied_fixture_hash"]);
    }
    for item in out["rows"].as_array_mut().unwrap() {
        assert!(
            item["latency_ms"]
                .as_f64()
                .is_some_and(|n| n.is_finite() && n >= 0.0)
        );
        item["latency_ms"] = json!(0);
    }
    out
}
fn outcome_matches(actual: &Value, row: &Value) -> bool {
    let mut expected = row["outcome"].clone();
    if let Some(tags) = expected.get("tags") {
        for tag in tags.as_array().unwrap() {
            let path = tag["path"].as_str().unwrap();
            let components: Vec<_> = path.split('.').collect();
            let allowed = path == "$.fixture_sha256"
                || (components.len() == 4
                    && components[0] == "$"
                    && components[1] == "rows"
                    && components[2].parse::<usize>().is_ok()
                    && ["classified_tier", "classifier_error", "classifier_status"]
                        .contains(&components[3]));
            if tag["kind"] != "undefined" || !allowed {
                return false;
            }
            let pointer = path[1..].replace('.', "/");
            // JS own-undefined properties map only to absent native wire fields.
            if actual["value"].pointer(&pointer).is_some()
                || expected["value"].pointer(&pointer).is_some()
            {
                return false;
            }
        }
    }
    expected.as_object_mut().unwrap().remove("tags");
    wire(actual) == wire(&expected)
}
fn writes_match(actual: &[String], row: &Value) -> bool {
    json!(actual) == row["writes"]
}
async fn execute(row: &Value) -> (Value, Vec<String>, Arc<Replay>) {
    let transport = Replay::new(row);
    let config = settings(row);
    let token = CancellationToken::new();
    let _guard = token.clone().drop_guard();
    let mut writes = Vec::new();
    assert_eq!(
        digest(row["fixture_bytes"].as_str().unwrap().as_bytes()),
        row["fixture_sha256"].as_str().unwrap()
    );
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        run_tests(
            transport.clone(),
            &config,
            row["fixture_bytes"].as_str().unwrap().as_bytes(),
            &token,
            &mut |line| writes.push(progress(&line)),
        ),
    )
    .await
    .expect("finite tool schedule deadline");
    let value = match result {
        Ok(value) => json!({"kind":"value","value":report(&value,row)}),
        Err(error) => {
            assert_eq!(
                error.to_string(),
                error.message,
                "CLI retains the safe message only"
            );
            json!({"kind":"error","name":"Error","message":error.message,"code":error.code})
        }
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while transport.bodies.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("finite response owner cleanup");
    (value, writes, transport)
}
async fn definition(number: usize) {
    let rows = corpus(CORPUS).unwrap();
    let id = format!("test/ollama-routing.test.mjs#{number}");
    let mut failures = Vec::new();
    for row in rows.iter().filter(|r| r["source_test"] == id) {
        let (value, writes, replay) = execute(row).await;
        if !outcome_matches(&value, row) {
            failures.push(json!({"case":row["id"],"field":"outcome","expected":row["outcome"],"actual":value}));
        }
        if !writes_match(&writes, row) {
            failures.push(
                json!({"case":row["id"],"field":"writes","expected":row["writes"],"actual":writes}),
            );
        }
        assert!(
            replay.verified(row),
            "Complete request sequence and response ownership"
        );
        let serialized = json!({"outcome":value,"writes":writes}).to_string();
        for item in row["cases"].as_array().unwrap() {
            assert!(!serialized.contains(item["prompt"].as_str().unwrap()));
        }
        for excluded in [
            "SYNTHETIC_PRIVATE_RESPONSE",
            "Synthetic coding assistant guidance",
            "SYNTHETIC_REMINDER_ONLY",
            "PRIVATE_THINKING",
        ] {
            assert!(!serialized.contains(excluded));
        }
    }
    assert!(
        failures.is_empty(),
        "{}",
        serde_json::to_string(&failures).unwrap()
    );
}
#[tokio::test]
async fn full_report_warms_once_and_preserves_every_original_metadata_assertion() {
    definition(5).await;
}
#[tokio::test]
async fn collapsed_and_invalid_answers_remain_distinct_failed_reports() {
    definition(6).await;
}
#[tokio::test]
async fn duplicate_tasks_are_reclassified_without_fabricated_tier_coverage() {
    definition(7).await;
}
#[tokio::test]
async fn busy_and_missing_preflight_preserve_exact_errors_and_stop_before_tasks() {
    definition(8).await;
}
#[tokio::test]
async fn independent_io_and_complete_output_controls_reject_matching_fallbacks() {
    let rows = corpus(CORPUS).unwrap();
    let mut altered = rows[2].clone();
    altered["requests"][7]["body"] = json!("{}");
    let (value, writes, replay) = execute(&altered).await;
    assert!(outcome_matches(&value, &altered));
    assert!(writes_match(&writes, &altered));
    assert!(!replay.verified(&altered));
    for suffix in ["", ", error=invalid_response, error=invalid_response"] {
        let mut wrong = writes.clone();
        wrong[1] = wrong[1].replace(", error=invalid_response", suffix);
        assert!(
            !writes_match(&wrong, &altered),
            "missing or duplicate suffix accepted"
        );
    }
    let mut wrong = altered.clone();
    wrong["outcome"]["value"]["passed"] = json!(true);
    assert!(!outcome_matches(&value, &wrong));
    let mut wrong_tag = altered.clone();
    wrong_tag["outcome"]["tags"][0]["kind"] = json!("nonfinite");
    assert!(!outcome_matches(&value, &wrong_tag));
    let mut extra = rows[3].clone();
    let duplicate = extra["requests"][0].clone();
    extra["requests"].as_array_mut().unwrap().push(duplicate);
    let (value, _, replay) = execute(&extra).await;
    assert!(outcome_matches(&value, &extra));
    assert!(!replay.verified(&extra), "unused response queue must fail");
    let (value, _, replay) = execute(&rows[5]).await;
    assert!(replay.verified(&rows[5]));
    let mut wrong_code = rows[5].clone();
    wrong_code["outcome"]["code"] = json!("ROUTING_TEST_ERROR");
    assert!(!outcome_matches(&value, &wrong_code));
    let mut encoded = CORPUS.to_owned();
    encoded.push(' ');
    assert_eq!(corpus(&encoded).unwrap_err(), "corpus identity");
    assert_eq!(
        corpus(&" ".repeat(2 * 1024 * 1024 + 1)).unwrap_err(),
        "corpus byte bound"
    );
}
