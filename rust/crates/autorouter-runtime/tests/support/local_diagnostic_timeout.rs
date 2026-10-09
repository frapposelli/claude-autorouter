//! Timed diagnostic replay. Only the production evaluator generates timeouts.
use autorouter_core::config::{RouterConfig, read_config};
use autorouter_core::js_json::JsDocument;
use autorouter_runtime::http_client::{HttpError, HttpTransport};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

pub const CORPUS: &str =
    include_str!("../../../../parity/cases/local-diagnostic-timeout-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../../parity/cases/local-diagnostic-timeout-contracts.capture.json");
const PIN: &str = "e2bb7874fef4ab2282bb0cd093a198ed9e50c4d1b6bdb89bedf1d97a7d250798";

pub fn validate(corpus: &str, capture: &Value) -> Result<Vec<Value>, &'static str> {
    if corpus.len() > 1024 * 1024 {
        return Err("corpus bound");
    }
    let digest = format!("{:x}", Sha256::digest(corpus));
    if digest != PIN || capture["cases_sha256"] != digest {
        return Err("immutable corpus identity");
    }
    if capture["selected_definitions"] != json!([7, 8])
        || capture["cases"] != 2
        || capture["requests"] != 48
        || capture["progress_events"] != 30
        || capture["static_assertions"] != 11
        || capture["helper_static_assertions"] != 11
        || capture["executed_assertions"] != 261
        || capture["executed_assertion_ids"].as_array().map(Vec::len) != Some(261)
    {
        return Err("capture inventory");
    }
    let rows: Vec<Value> = corpus
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .map_err(|_| "case JSON")?;
    if rows.len() != 2 {
        return Err("case count");
    }
    for (row, number) in rows.iter().zip([7, 8]) {
        if row["source_tests"] != json!([format!("test/local-diagnostic.test.mjs#{number}")])
            || row["requests"].as_array().map(Vec::len) != Some(24)
            || row["progress"].as_array().map(Vec::len) != Some(15)
            || row["config"]["ollamaTimeoutMs"] != if number == 7 { 0 } else { 2 }
        {
            return Err("timed schedule");
        }
    }
    Ok(rows)
}
pub fn captured() -> (Vec<Value>, Value) {
    let capture = serde_json::from_str(CAPTURE).unwrap();
    (validate(CORPUS, &capture).unwrap(), capture)
}
pub fn wire(value: &Value) -> Value {
    JsDocument::parse(value.to_string().as_bytes())
        .unwrap()
        .to_serde_observation_lossy()
}
pub fn settings(row: &Value) -> RouterConfig {
    let config = read_config(
        &json!({"AUTOROUTER_EVALUATOR":"ollama", "AUTOROUTER_OLLAMA_MODEL":row["config"]["ollamaModel"],
            "AUTOROUTER_OLLAMA_TIMEOUT_MS":row["config"]["ollamaTimeoutMs"].to_string()}),
        false,
        std::path::Path::new("/synthetic"),
    )
    .unwrap();
    assert_eq!(
        wire(&serde_json::to_value(&config).unwrap()),
        wire(&row["config"])
    );
    verify_undefined(&row["config"], &row["config_tags"]);
    config
}
pub fn verify_undefined(value: &Value, tags: &Value) {
    for tag in tags.as_array().unwrap() {
        assert_eq!(tag["kind"], "undefined");
        let path = tag["path"].as_str().unwrap();
        let allowed = [
            "sessionLogDir",
            "stopHookBlockCap",
            "localToken",
            "anthropicKey",
            "jevKey",
            "classified_tier",
            "classifier_error",
            "classifier_status",
        ];
        assert!(allowed.contains(&path.rsplit('.').next().unwrap()));
        let pointer = format!("/{}", path.strip_prefix("$.").unwrap().replace('.', "/"));
        assert!(
            value.pointer(&pointer).is_none(),
            "Own undefined must stay omitted: {path}"
        );
    }
}

/// Checks the original >=4ms predicate BEFORE removing nondeterministic values.
pub fn comparison(
    report: &Value,
    progress: &[Value],
    elapsed_bound: bool,
) -> Result<Value, &'static str> {
    let mut report = report.clone();
    let mut progress = progress.to_vec();
    fn timing(value: &mut Value, minimum: f64) -> Result<(), &'static str> {
        let elapsed = value["latency_ms"]
            .as_f64()
            .ok_or("missing numeric elapsed time")?;
        if !elapsed.is_finite() || elapsed < minimum {
            return Err("elapsed predicate failed");
        }
        value
            .as_object_mut()
            .ok_or("timing object")?
            .remove("latency_ms");
        Ok(())
    }
    timing(report.get_mut("startup").ok_or("startup")?, 0.0)?;
    for row in report
        .get_mut("rows")
        .and_then(Value::as_array_mut)
        .ok_or("rows")?
    {
        timing(row, if elapsed_bound { 4.0 } else { 0.0 })?;
    }
    for event in &mut progress {
        if matches!(
            event["event"].as_str(),
            Some("startup_complete" | "case_complete")
        ) {
            // The source's predicate is on report rows. Event elapsed values
            // are additionally required to equal their corresponding rows below.
            timing(event, 0.0)?;
        }
    }
    Ok(wire(&json!({"report":report,"progress":progress})))
}
pub fn expected_progress(row: &Value) -> Vec<Value> {
    row["progress"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["value"].clone())
        .collect()
}
pub fn request_observation(request: &Value) -> Value {
    json!({"url":request["url"],"method":request["method"],"headers":request["headers"],
        "body":request["body"],"body_text":request["body_text"]})
}

#[derive(Clone, Copy)]
pub enum Schedule {
    RuntimeDelay,
    PendingStartup,
    PendingRuntime,
}
#[derive(Default)]
struct State {
    observed: Vec<Value>,
    invalid: bool,
    active: usize,
    delayed: usize,
    completed: usize,
    dropped: usize,
    pending: bool,
}
pub struct Replay {
    expected: Vec<Value>,
    responses: Vec<Value>,
    schedule: Schedule,
    state: Mutex<State>,
    entered: Notify,
}
impl Replay {
    pub fn new(row: &Value, successful: &Value, schedule: Schedule) -> Arc<Self> {
        let expected = row["requests"].as_array().unwrap().clone();
        let responses = successful["requests"].as_array().unwrap().clone();
        assert_eq!(expected.len(), 24);
        assert_eq!(responses.len(), 24);
        for response in &responses {
            assert!(response["response"]["body"]["text"].as_str().unwrap().len() <= 65536);
        }
        Arc::new(Self {
            expected,
            responses,
            schedule,
            state: Mutex::new(State::default()),
            entered: Notify::new(),
        })
    }
    pub fn observed(&self) -> Vec<Value> {
        self.state.lock().unwrap().observed.clone()
    }
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let state = self.state.lock().unwrap();
        (state.active, state.delayed, state.completed, state.dropped)
    }
    pub fn verified(&self) -> bool {
        let state = self.state.lock().unwrap();
        !state.invalid && state.observed.len() == self.expected.len() && state.active == 0
    }
    pub async fn wait_pending(&self) {
        loop {
            let entered = self.entered.notified();
            tokio::pin!(entered);
            entered.as_mut().enable();
            if self.state.lock().unwrap().pending {
                return;
            }
            entered.await;
        }
    }
}
struct Active<'a> {
    owner: &'a Replay,
    delayed: bool,
    completed: bool,
}
impl Drop for Active<'_> {
    fn drop(&mut self) {
        let mut state = self.owner.state.lock().unwrap();
        state.active -= 1;
        if self.delayed {
            if self.completed {
                state.completed += 1;
            } else {
                state.dropped += 1;
            }
        }
    }
}
impl HttpTransport for Replay {
    type ResponseBody = Full<Bytes>;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        assert!(bytes.len() <= 65536);
        let headers: serde_json::Map<String, Value> = parts
            .headers
            .iter()
            .map(|(key, value)| (key.to_string(), json!(value.to_str().unwrap())))
            .collect();
        let observed = json!({"url":parts.uri.to_string(),"method":parts.method.as_str(),"headers":headers,
            "body":if bytes.is_empty(){Value::Null}else{serde_json::from_slice::<Value>(&bytes).unwrap()},
            "body_text":std::str::from_utf8(&bytes).unwrap()});
        let index = {
            let mut state = self.state.lock().unwrap();
            let index = state.observed.len();
            if index >= self.expected.len() {
                state.invalid = true;
                return Err(HttpError::Network);
            }
            state.invalid |= observed != request_observation(&self.expected[index]);
            state.observed.push(observed);
            state.active += 1;
            index
        };
        let mut active = Active {
            owner: self,
            delayed: false,
            completed: false,
        };
        let inference = parts.uri.path() == "/v1/systemone";
        let startup = index == 5;
        let pending = inference
            && match self.schedule {
                Schedule::PendingStartup => startup,
                Schedule::PendingRuntime => !startup,
                Schedule::RuntimeDelay => false,
            };
        if pending || inference && !startup {
            active.delayed = true;
            {
                let mut state = self.state.lock().unwrap();
                state.delayed += 1;
                state.pending |= pending;
            }
            self.entered.notify_waiters();
            if pending {
                std::future::pending::<()>().await;
            }
            // Even timeout rows would return a valid original response if the
            // evaluator failed to enforce its independent two-millisecond bound.
            tokio::time::sleep(Duration::from_millis(6)).await;
        }
        let response = &self.responses[index]["response"];
        let mut builder = Response::builder().status(response["status"].as_u64().unwrap() as u16);
        for (key, value) in response["headers"].as_object().unwrap() {
            builder = builder.header(key.as_str(), value.as_str().unwrap());
        }
        active.completed = true;
        Ok(builder
            .body(Full::new(Bytes::from(
                response["body"]["text"].as_str().unwrap().to_owned(),
            )))
            .unwrap())
    }
}
