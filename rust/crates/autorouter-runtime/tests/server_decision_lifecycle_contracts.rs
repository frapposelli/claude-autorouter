//! Complete frozen server #4–#7 decision privacy, callback failure and client cancellation schedules.
#[allow(dead_code)] // Shared helpers retain separate policies used by sibling targets.
#[path = "support/server_contract.rs"]
mod contract;
use contract::{
    Reply, chunks, compare_request, decode, events, fixture_case, headers, parsed, response_bytes,
    sink, upstream_rows,
};
#[path = "support/server_audit_router.rs"]
mod audit;
#[allow(dead_code)]
#[path = "support/server_stream_failure.rs"]
mod cancellation_peer;
#[allow(dead_code)] // Shared peer also supplies streaming interfaces to other targets.
#[path = "support/server_streaming_routing.rs"]
mod peer;
#[allow(dead_code)] // Shared observer fixture also provides earlier continuity interfaces.
#[path = "support/server_outcome_history.rs"]
mod real;
use audit::RouterImpl;
use autorouter_core::{config::read_config, js_json::JsDocument};
use autorouter_runtime::{
    http_client::{HttpTransport, NativeHttpClient},
    observer_tasks::{ObserverFuture, ObserverLimits, ObserverRejected, ObserverTasks},
    server::{Gateway, GatewayHandle},
    server_events::{EventSink, EventSinks},
};
use bytes::Bytes;
use http_body_util::Full;
use hyper::Request;
use peer::{Observation, Peer, ResponsePlan};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const CORPUS: &str =
    include_str!("../../../parity/cases/server-decision-lifecycle-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../parity/cases/server-decision-lifecycle-contracts.capture.json");
const LIMIT: usize = 2 * 1024 * 1024;
const BOUND: Duration = Duration::from_secs(5);
type Events = Arc<Mutex<Vec<Value>>>;
fn case(number: usize) -> Value {
    assert_eq!(
        format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
        "83672378eb92cc0e9f883cc609a90b6f1fe146e111b81243a14a06a3440808c9"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(CAPTURE.as_bytes())),
        "9073d61fae36dc240d416f1dc1a7c46c669232aab3acd4d92ceba92c4317f2fc"
    );
    let metadata: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(metadata["static_assertions"], 48);
    assert_eq!(metadata["expanded_assertions"], 103);
    assert_eq!(metadata["callbacks_executed"], 4);
    CORPUS
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|row| row["source_test"] == format!("test/server.test.mjs#{number}"))
        .unwrap()
}
type AuditRouter = audit::AuditRouter<Mutex<Vec<(Arc<JsDocument>, String)>>>;
struct Fixture {
    client: Arc<NativeHttpClient>,
    router: Arc<AuditRouter>,
    gateway: Option<GatewayHandle>,
    peer: Option<Peer>,
    statuses: Events,
    history: Events,
    logs: Events,
    log_history: Events,
    decisions: Events,
    records: Events,
    callback_outcomes: Events,
    real: bool,
    observer_owner: Option<ObserverTasks>,
    never_polled: Arc<std::sync::atomic::AtomicUsize>,
    never_dropped: Arc<std::sync::atomic::AtomicUsize>,
}
struct Seen {
    requests: Vec<Observation>,
    routes: Vec<Value>,
    completions: Vec<Value>,
    statuses: Vec<Value>,
    history: Vec<Value>,
    logs: Vec<Value>,
    decisions: Vec<Value>,
    records: Vec<Value>,
    callback_outcomes: Vec<Value>,
    evaluator_calls: Vec<Value>,
    real: bool,
    provider_closed_before_owner_close: Option<bool>,
    manufactured_provider_disconnect: Option<bool>,
    continuity_before_close: Option<Value>,
    input_documents_unchanged: Vec<bool>,
    observer_before_close: Option<Value>,
    observer_after_close: Option<Value>,
    never_polled_before_close: usize,
    never_dropped_after_close: usize,
}
impl Fixture {
    async fn new(number: usize, row: &Value) -> Self {
        let script = upstream_rows(row);
        let peer = Peer::start(move |request, index| {
            let wanted = script.get(index).ok_or("unregistered peer request")?;
            compare_request(request, wanted)?;
            Ok(ResponsePlan::chunks(
                wanted["response_status"].as_u64().unwrap() as u16,
                headers(&wanted["response_headers"]),
                chunks(wanted),
            ))
        })
        .await
        .unwrap();
        let client = Arc::new(NativeHttpClient::new().unwrap());
        let mut config = read_config(
            &row["config_arguments"].as_array().unwrap().last().unwrap()[0],
            false,
            Path::new("/tmp"),
        )
        .unwrap();
        config.local_token = row["gateway_config"]["localToken"]
            .as_str()
            .map(str::to_owned);
        config.upstream = format!("http://{}", peer.address);
        config.session_log_mode = if row["gateway_config"]["sessionLogMode"] == "prompts" {
            autorouter_core::config::SessionLogMode::Prompts
        } else {
            autorouter_core::config::SessionLogMode::Metadata
        };
        let inner = if number == 33 {
            RouterImpl::Real(Box::new(real::OriginalRouter::new(row)))
        } else {
            RouterImpl::Mock(
                row["route_calls"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| r["decision"].clone())
                    .collect(),
            )
        };
        let router = Arc::new(AuditRouter {
            inner,
            calls: Mutex::new(Vec::new()),
            completions: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
            witnesses: Mutex::new(Vec::new()),
        });
        let statuses = Events::default();
        let history = Events::default();
        let logs = Events::default();
        let log_history = Events::default();
        let decisions = Events::default();
        let records = Events::default();
        let callback_outcomes = Events::default();
        let observer_owner =
            (number == 5).then(|| ObserverTasks::new(ObserverLimits::default()).unwrap());
        let never_polled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let never_dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let d = decisions.clone();
        let outcomes = callback_outcomes.clone();
        let decision: EventSink = if let Some(owner) = &observer_owner {
            let polled = never_polled.clone();
            let dropped = never_dropped.clone();
            owner.handle().event_sink(move |document|->ObserverFuture{
                let event=document.to_serde_observation_lossy();let invocation={let mut rows=d.lock().unwrap();assert!(rows.len()<32);rows.push(event);rows.len()};
                {let mut rows=outcomes.lock().unwrap();assert!(rows.len()<32);rows.push(json!({"invocation":invocation,"outcome":if invocation==1{"synchronous_unwind"}else if invocation==2{"returned_future_rejection"}else{"returned_never_ready_future"}}));}
                match invocation{1=>panic!("PRIVATE_SYNC_LOG_FAILURE"),2=>Box::pin(async{Err(ObserverRejected)}),3=>Box::pin(real::NeverReady::new(polled.clone(),dropped.clone())),_=>unreachable!()}
            })
        } else {
            Arc::new(move |document| {
                let mut rows = d.lock().unwrap();
                assert!(rows.len() < 32);
                rows.push(document.to_serde_observation_lossy());
            })
        };
        let gateway = Gateway::with_router(
            config,
            client.clone(),
            router.clone(),
            EventSinks {
                status: Some(sink(&statuses, &history)),
                log: Some(sink(&logs, &log_history)),
                decision: (number != 7).then_some(decision),
                record: None,
            },
        )
        .unwrap()
        .listen(0)
        .await
        .unwrap();
        Self {
            client,
            router,
            gateway: Some(gateway),
            peer: Some(peer),
            statuses,
            history,
            logs,
            log_history,
            decisions,
            records,
            callback_outcomes,
            real: number == 33,
            observer_owner,
            never_polled,
            never_dropped,
        }
    }
    async fn send(&self, row: &Value) -> Result<Reply, String> {
        contract::send_buffered(&self.client, self.gateway.as_ref(), row).await
    }
    async fn wait_for_status(&self, event: &str) -> Result<(), String> {
        contract::wait_for_status(&self.statuses, event).await
    }
    async fn finish(mut self) -> Result<Seen, String> {
        let continuity_before_close = match &self.router.inner {
            RouterImpl::Real(real) => Some(real.continuity()),
            RouterImpl::Mock(_) => None,
        };
        let observer_before_close = self
            .observer_owner
            .as_ref()
            .map(|owner| real::snapshot(owner.snapshot()));
        let never_polled_before_close = self.never_polled.load(Ordering::SeqCst);
        let requests = contract::close_gateway(&mut self.gateway, &mut self.peer, BOUND).await?;
        if let Some(owner) = &self.observer_owner {
            tokio::time::timeout(BOUND, owner.close())
                .await
                .map_err(|_| "observer owner close timed out")?;
        }
        let observer_after_close = self
            .observer_owner
            .as_ref()
            .map(|owner| real::snapshot(owner.snapshot()));
        let never_dropped_after_close = self.never_dropped.load(Ordering::SeqCst);
        if !self.router.closed.load(Ordering::SeqCst) {
            return Err("mock router closure not observed".into());
        }
        contract::check_recorders([&self.statuses, &self.history, &self.logs, &self.log_history])?;
        Ok(Seen {
            requests,
            routes: self.router.calls.lock().unwrap().clone(),
            completions: self.router.completions.lock().unwrap().clone(),
            statuses: self.statuses.lock().unwrap().clone(),
            history: self.history.lock().unwrap().clone(),
            logs: self.logs.lock().unwrap().clone(),
            decisions: self.decisions.lock().unwrap().clone(),
            records: self.records.lock().unwrap().clone(),
            callback_outcomes: self.callback_outcomes.lock().unwrap().clone(),
            evaluator_calls: match &self.router.inner {
                RouterImpl::Real(r) => r.calls(),
                RouterImpl::Mock(_) => Vec::new(),
            },
            real: self.real,
            provider_closed_before_owner_close: None,
            manufactured_provider_disconnect: None,
            continuity_before_close,
            input_documents_unchanged: self
                .router
                .witnesses
                .lock()
                .unwrap()
                .iter()
                .map(|(d, s)| d.stringify() == *s)
                .collect(),
            observer_before_close,
            observer_after_close,
            never_polled_before_close,
            never_dropped_after_close,
        })
    }
}
use contract::SharedAssertions as Assertions;
fn supplemental_transcripts(row: &Value, replies: &[Reply], seen: &Seen) {
    contract::verify_transcript(
        row,
        replies,
        &contract::Transcript {
            requests: &seen.requests,
            routes: &seen.routes,
            completions: &seen.completions,
            history: &seen.history,
            logs: &seen.logs,
            real: seen.real,
        },
    );
    if let Some(directory) = std::env::var_os("AUTOROUTER_NATIVE_PARITY_CAPTURE_DIR") {
        let raw = json!({"schema_version":1,"status":"passed","source_test":row["source_test"],"upstream_requests":seen.requests.iter().map(contract::capture_request).collect::<Vec<_>>(),"route_calls":seen.routes,"complete_calls":seen.completions,"statuses":seen.statuses,"all_emitted_statuses":seen.history,"logs":seen.logs,"records":seen.records,"decisions":seen.decisions,"callback_outcomes":seen.callback_outcomes,"evaluator_calls":seen.evaluator_calls,"continuity_before_close":seen.continuity_before_close,"provider_closed_before_owner_close":seen.provider_closed_before_owner_close,"manufactured_provider_disconnect":seen.manufactured_provider_disconnect,"input_documents_unchanged":seen.input_documents_unchanged,"observer_before_close":seen.observer_before_close,"observer_after_close":seen.observer_after_close,"never_polled_before_close":seen.never_polled_before_close,"never_dropped_after_close":seen.never_dropped_after_close,"downstream":replies.iter().map(contract::capture_reply).collect::<Vec<_>>(),"projection":"None. Actual generated IDs, measured times, deterministic routing times, headers and complete bytes retained."});
        contract::write_row_capture(&directory, &row["id"], &raw, LIMIT, true);
    }
}
async fn wait_complete_count(f: &Fixture, count: usize) -> Result<(), String> {
    contract::wait_complete_count(&f.statuses, count).await
}
fn compare_record_callbacks(row: &Value, seen: &Seen) {
    contract::compare_record_callbacks(
        row,
        &contract::RecordCallbacks {
            records: &seen.records,
            decisions: &seen.decisions,
            history: &seen.history,
            logs: &seen.logs,
            real: seen.real,
        },
    );
}
fn valid_utc_timestamp(value: &str) -> bool {
    let Ok(r) = regex::Regex::new(r"^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})\.(\d{3})Z$")
    else {
        return false;
    };
    let Some(c) = r.captures(value) else {
        return false;
    };
    let number = |i: usize| c[i].parse::<u16>().ok();
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second), Some(millis)) = (
        number(1),
        number(2),
        number(3),
        number(4),
        number(5),
        number(6),
        number(7),
    ) else {
        return false;
    };
    let Ok(month) = time::Month::try_from(month as u8) else {
        return false;
    };
    time::Date::from_calendar_date(i32::from(year), month, day as u8).is_ok_and(|date| {
        date.with_hms_milli(hour as u8, minute as u8, second as u8, millis)
            .is_ok()
    })
}
#[tokio::test]
async fn original_4_all_bounded_prompt_privacy_and_internal_request_variants() {
    let row = fixture_case(&case(4), 0);
    let f = Fixture::new(4, &row).await;
    let a = Assertions::new(4);
    let outcome = tokio::time::timeout(BOUND, async {
        let mut replies = Vec::new();
        for (index, request) in row["downstream"].as_array().unwrap().iter().enumerate() {
            let reply = f.send(request).await?;
            wait_complete_count(&f, index + 1).await?;
            a.check(1, reply.status == 200);
            a.check(
                2,
                reply.bytes == response_bytes(&upstream_rows(&row)[index]),
            );
            replies.push(reply);
        }
        Ok::<_, String>(replies)
    })
    .await;
    let seen = f.finish().await.unwrap();
    let replies = outcome.unwrap().unwrap();
    let first = &seen.decisions[0];
    let prompt = "DECISION_ONLY_TASK: inspect this fixture.\nKeep the supplied emoji 😀.";
    let private = [
        "PRIVATE_SYSTEM",
        "PRIVATE_SCHEMA",
        "PRIVATE_THINKING",
        "PRIVATE_IMAGE",
        "PRIVATE_TOOL_RESULT",
        "PRIVATE_REMINDER",
    ];
    a.check(3, first["schema_version"].as_f64() == Some(2.));
    a.check(4, first["event"] == "decision");
    a.check(5, valid_utc_timestamp(first["timestamp"].as_str().unwrap()));
    a.check(
        6,
        regex::Regex::new("^[a-f0-9-]{36}$")
            .unwrap()
            .is_match(first["request_id"].as_str().unwrap()),
    );
    a.check(7, first["session_id"] == "decision-session");
    a.check(8, first["agent_id"] == "decision-agent");
    a.check(9, first["prompt_id"] == "decision-prompt");
    a.check(10, first["request_class"] == "main");
    a.check(11, first["prompt_excerpt"] == prompt);
    a.check(12, first["prompt_truncated"] == false);
    a.check(
        13,
        first["requested_model"] == row["route_calls"][0]["document"]["model"],
    );
    a.check(
        14,
        first["selected_model"] == row["route_calls"][0]["decision"]["model"],
    );
    a.check(15, first["decision_latency_ms"].as_f64() == Some(12.25));
    a.check(16, first["source"] == "jev");
    a.check(17, first["evaluator"] == "jev");
    a.check(18, first["classified_tier"] == "haiku");
    a.check(19, first["reason"] == "classified");
    for value in private
        .iter()
        .chain(["PRIVATE_SIGNATURE", "PRIVATE_PATH"].iter())
    {
        a.check(20, !first.to_string().contains(value));
    }
    a.check(
        21,
        seen.input_documents_unchanged[0]
            && seen.routes[0]["document"]
                == parsed(row["downstream"][0]["body"].as_str().unwrap().as_bytes()),
    );
    let bounded = &seen.decisions[1];
    a.check(22, bounded["prompt_excerpt"] == "😀".repeat(500));
    a.check(
        23,
        bounded["prompt_excerpt"].as_str().unwrap().chars().count() == 500,
    );
    a.check(24, bounded["prompt_truncated"] == true);
    a.check(25, bounded["source"] == "cache");
    a.check(26, seen.decisions[2]["prompt_truncated"] == false);
    for (index, class) in ["auxiliary", "compaction", "subagent", "workflow"]
        .iter()
        .enumerate()
    {
        let internal = &seen.decisions[index + 3];
        a.check(27, internal["prompt_excerpt"] == "");
        a.check(28, internal["prompt_truncated"] == false);
        a.check(29, internal["source"] == "passthrough");
        a.check(30, internal["request_class"] == *class);
    }
    a.check(31, seen.decisions.len() == 7);
    a.check(
        32,
        seen.decisions
            .iter()
            .map(|r| r["request_id"].as_str().unwrap())
            .collect::<BTreeSet<_>>()
            .len()
            == 7,
    );
    for channel in [&seen.logs, &seen.statuses] {
        let serialized = json!(channel).to_string();
        for value in std::iter::once(prompt)
            .chain(private)
            .chain(["😀", "OMITTED_TAIL"])
        {
            a.check(33, !serialized.contains(value));
        }
        a.check(34, !serialized.contains("prompt_excerpt"));
    }
    a.finish();
    assert!(
        seen.input_documents_unchanged
            .iter()
            .all(|unchanged| *unchanged)
    );
    compare_record_callbacks(&row, &seen);
    supplemental_transcripts(&row, &replies, &seen);
}
#[tokio::test]
async fn original_5_actual_decision_failure_returns_never_delay_or_change_stream() {
    let row = fixture_case(&case(5), 0);
    let f = Fixture::new(5, &row).await;
    let a = Assertions::new(5);
    let outcome = tokio::time::timeout(BOUND, async {
        let mut replies = Vec::new();
        for (index, request) in row["downstream"].as_array().unwrap().iter().enumerate() {
            let reply = tokio::time::timeout(Duration::from_millis(1000), f.send(request))
                .await
                .map_err(|_| "original client deadline")??;
            wait_complete_count(&f, index + 1).await?;
            a.check(1, reply.status == 200);
            a.check(
                2,
                reply
                    .headers
                    .get("request-id")
                    .and_then(|v| v.to_str().ok())
                    == Some("decision-sink-fixture"),
            );
            a.check(
                3,
                reply.bytes == response_bytes(&upstream_rows(&row)[index]),
            );
            replies.push(reply);
        }
        Ok::<_, String>(replies)
    })
    .await;
    for _ in 0..100 {
        if f.observer_owner.as_ref().unwrap().snapshot().rejected == 1
            && f.never_polled.load(Ordering::SeqCst) == 1
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(f.observer_owner.as_ref().unwrap().snapshot().rejected, 1);
    assert_eq!(
        f.observer_owner
            .as_ref()
            .unwrap()
            .snapshot()
            .factory_panicked,
        1
    );
    assert_eq!(f.observer_owner.as_ref().unwrap().snapshot().pending, 1);
    assert_eq!(f.never_polled.load(Ordering::SeqCst), 1);
    assert_eq!(f.never_dropped.load(Ordering::SeqCst), 0);
    let seen = f.finish().await.unwrap();
    let replies = outcome.unwrap().unwrap();
    a.check(4, seen.callback_outcomes.len() == 3);
    a.check(
        5,
        !json!({"logs":seen.logs,"statuses":seen.statuses})
            .to_string()
            .contains("PRIVATE_"),
    );
    a.finish();
    let after = seen.observer_after_close.as_ref().unwrap();
    assert_eq!(after["cancelled"], 1);
    assert_eq!(after["pending"], 0);
    assert_eq!(after["charged_bytes"], 0);
    assert_eq!(after["worker_finished"], true);
    assert_eq!(seen.never_dropped_after_close, 1);
    compare_record_callbacks(&row, &seen);
    supplemental_transcripts(&row, &replies, &seen);
}
#[tokio::test]
async fn original_6_fallback_decision_remains_logged_when_provider_returns_429() {
    let row = fixture_case(&case(6), 0);
    let f = Fixture::new(6, &row).await;
    let outcome = tokio::time::timeout(BOUND, async {
        let reply = f.send(&row["downstream"][0]).await?;
        f.wait_for_status("request_complete").await?;
        Ok::<_, String>(reply)
    })
    .await;
    let seen = f.finish().await.unwrap();
    let reply = outcome.unwrap().unwrap();
    let a = Assertions::new(6);
    a.check(1, reply.status == 429);
    a.check(2, reply.bytes == response_bytes(&upstream_rows(&row)[0]));
    a.check(3, seen.decisions.len() == 1);
    let decision = &seen.decisions[0];
    a.check(4, decision["source"] == "fallback");
    a.check(5, decision["classifier_error"] == "timeout");
    a.check(
        6,
        decision["selected_model"] == row["route_calls"][0]["document"]["model"],
    );
    a.check(7, decision["decision_latency_ms"].as_f64() == Some(1501.));
    a.check(8, decision["prompt_excerpt"] == "Fix a typo");
    a.finish();
    compare_record_callbacks(&row, &seen);
    supplemental_transcripts(&row, &[reply], &seen);
}
#[tokio::test]
async fn original_7_actual_downstream_body_drop_closes_provider_before_owner_shutdown() {
    let row = fixture_case(&case(7), 0);
    let peer = cancellation_peer::Peer::start(7, upstream_rows(&row)).await;
    let client = Arc::new(NativeHttpClient::new().unwrap());
    let mut config = read_config(&row["config_arguments"][0][0], false, Path::new("/tmp")).unwrap();
    config.local_token = row["gateway_config"]["localToken"]
        .as_str()
        .map(str::to_owned);
    config.upstream = format!("http://{}", peer.address);
    let router = Arc::new(AuditRouter {
        inner: RouterImpl::Mock(vec![row["route_calls"][0]["decision"].clone()]),
        calls: Mutex::new(Vec::new()),
        completions: Mutex::new(Vec::new()),
        closed: AtomicBool::new(false),
        witnesses: Mutex::new(Vec::new()),
    });
    let statuses = Events::default();
    let history = Events::default();
    let logs = Events::default();
    let log_history = Events::default();
    let gateway = Gateway::with_router(
        config,
        client.clone(),
        router.clone(),
        EventSinks {
            status: Some(sink(&statuses, &history)),
            log: Some(sink(&logs, &log_history)),
            ..Default::default()
        },
    )
    .unwrap()
    .listen(0)
    .await
    .unwrap();
    let request = &row["downstream"][0];
    let mut native = Request::builder()
        .method(request["method"].as_str().unwrap())
        .uri(format!("http://{}/v1/messages", gateway.address));
    *native.headers_mut().unwrap() = headers(&request["headers"]);
    let outcome = tokio::time::timeout(BOUND, async {
        let response = client
            .request(
                native
                    .body(Full::new(Bytes::from(
                        request["body"].as_str().unwrap().to_owned(),
                    )))
                    .unwrap(),
            )
            .await
            .map_err(|_| "downstream request failed")?;
        let (parts, body) = response.into_parts();
        // Source response.body.cancel discards its owned unread stream. Drop the
        // real native body; never read, abort the provider, or close an owner.
        drop(body);
        tokio::time::timeout(Duration::from_millis(2000), async {
            while !peer.peer_closed() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .map_err(|_| "upstream not physically closed by client cancellation")?;
        for _ in 0..100 {
            if statuses
                .lock()
                .unwrap()
                .iter()
                .any(|r| r["event"] == "request_cancelled")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Ok::<_, String>(Reply {
            status: parts.status.as_u16(),
            headers: parts.headers,
            bytes: Vec::new(),
        })
    })
    .await;
    let provider_closed_before_close = peer.peer_closed();
    let manufactured_disconnect = peer.destroyed();
    let seen = Seen {
        requests: peer
            .observations()
            .into_iter()
            .map(|r| Observation {
                method: r.method,
                path: r.path,
                headers: r.headers,
                body: r.body,
            })
            .collect(),
        routes: router.calls.lock().unwrap().clone(),
        completions: router.completions.lock().unwrap().clone(),
        statuses: statuses.lock().unwrap().clone(),
        history: history.lock().unwrap().clone(),
        logs: logs.lock().unwrap().clone(),
        decisions: Vec::new(),
        records: Vec::new(),
        callback_outcomes: Vec::new(),
        evaluator_calls: Vec::new(),
        real: false,
        provider_closed_before_owner_close: Some(provider_closed_before_close),
        manufactured_provider_disconnect: Some(manufactured_disconnect),
        continuity_before_close: None,
        input_documents_unchanged: router
            .witnesses
            .lock()
            .unwrap()
            .iter()
            .map(|(d, s)| d.stringify() == *s)
            .collect(),
        observer_before_close: None,
        observer_after_close: None,
        never_polled_before_close: 0,
        never_dropped_after_close: 0,
    };
    let closed = tokio::time::timeout(BOUND, gateway.close()).await;
    let peer_closed = peer.close().await;
    closed.unwrap();
    peer_closed.unwrap();
    let reply = outcome.unwrap().unwrap();
    let a = Assertions::new(7);
    a.check(1, events(&seen.statuses, "request_error").is_empty());
    a.finish();
    assert!(
        provider_closed_before_close && !manufactured_disconnect,
        "actual response.body cancellation closes the provider before owner cleanup"
    );
    assert_eq!(events(&seen.statuses, "request_cancelled").len(), 1);
    supplemental_transcripts(&row, &[reply], &seen);
}
