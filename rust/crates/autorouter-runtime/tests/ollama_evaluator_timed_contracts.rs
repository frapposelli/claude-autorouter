//! First regression for original Ollama evaluator definition 9.
//! The body must be released at cancellation return, before any cleanup yield.
//! No assertion equates a Rust cancellation enum with a JavaScript Error object.
use autorouter_core::config::read_config;
use autorouter_core::js_json::JsDocument;
use autorouter_runtime::classifier::Classifier;
use autorouter_runtime::evaluator::EvaluationError;
use autorouter_runtime::http_client::{HttpError, HttpTransport};
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::{Body, Frame};
use hyper::{Request, Response};
use serde_json::json;
use std::convert::Infallible;
use std::future::{Future, poll_fn};
use std::path::Path;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct BodyOwners {
    active: AtomicUsize,
    pending_entered: AtomicBool,
    pending_dropped: AtomicUsize,
    entered: Notify,
}
struct ObservedBody {
    bytes: Option<Bytes>,
    pending: bool,
    owners: Arc<BodyOwners>,
}
impl Drop for ObservedBody {
    fn drop(&mut self) {
        self.owners.active.fetch_sub(1, Ordering::SeqCst);
        if self.pending {
            self.owners.pending_dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
}
impl Body for ObservedBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let this = self.get_mut();
        if this.pending {
            this.owners.pending_entered.store(true, Ordering::SeqCst);
            this.owners.entered.notify_waiters();
            Poll::Pending
        } else {
            Poll::Ready(this.bytes.take().map(|bytes| Ok(Frame::data(bytes))))
        }
    }
    fn is_end_stream(&self) -> bool {
        !self.pending && self.bytes.is_none()
    }
}
struct StreamReplay {
    phase: &'static str,
    first: AtomicBool,
    paths: Mutex<Vec<String>>,
    model: String,
    owners: Arc<BodyOwners>,
}
impl StreamReplay {
    async fn wait_reading(&self) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let entered = self.owners.entered.notified();
                tokio::pin!(entered);
                entered.as_mut().enable();
                if self.owners.pending_entered.load(Ordering::SeqCst) {
                    break;
                }
                entered.await;
            }
        })
        .await
        .expect("positive Pending body-read barrier");
    }
}
impl HttpTransport for StreamReplay {
    type ResponseBody = ObservedBody;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<ObservedBody>, HttpError> {
        assert_eq!(request.method(), hyper::Method::POST);
        let path = request.uri().path();
        assert!(matches!(path, "/api/show" | "/v1/systemone"));
        self.paths.lock().unwrap().push(path.to_owned());
        let pending = path == self.phase && self.first.swap(false, Ordering::SeqCst);
        let payload = if path == "/api/show" {
            json!({"details":{"parameter_size":"9B"},"capabilities":["completion"]})
        } else {
            json!({"model":self.model,"answers":{"tier":{"type":"choice","choice":"haiku","probabilities":{"haiku":0.8,"sonnet":0.1,"opus":0.1},"confidence":0.418}},"usage":{"input_tokens":1234,"output_tokens":1}})
        };
        self.owners.active.fetch_add(1, Ordering::SeqCst);
        Ok(Response::new(ObservedBody {
            bytes: (!pending).then(|| Bytes::from(payload.to_string())),
            pending,
            owners: self.owners.clone(),
        }))
    }
}
struct OwnedClassifier(Classifier<StreamReplay>);
impl Drop for OwnedClassifier {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}
async fn cancellation_return_releases_pending_reader(phase: &'static str) {
    let mut config = read_config(
        &json!({"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_AUTH_MODE":"subscription"}),
        false,
        Path::new("/synthetic"),
    )
    .unwrap();
    config.ollama_timeout_ms = 0;
    let replay = Arc::new(StreamReplay {
        phase,
        first: AtomicBool::new(true),
        paths: Mutex::new(Vec::new()),
        model: config.ollama_model.clone(),
        owners: Arc::new(BodyOwners::default()),
    });
    let classifier = OwnedClassifier(Classifier::new(replay.clone(), &config));
    let body = JsDocument::parse(br#"{"model":"claude-haiku-4-5-20251001","max_tokens":4096,"messages":[{"role":"user","content":"Fix one typo"}]}"#).unwrap();
    let token = CancellationToken::new();
    let _cancel_on_exit = token.clone().drop_guard();
    {
        let mut pending = Box::pin(classifier.0.classify(&body, &config, &token));
        assert!(
            poll_fn(|cx| Poll::Ready(pending.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        replay.wait_reading().await;
        assert_eq!(replay.owners.active.load(Ordering::SeqCst), 1);
        assert_eq!(replay.owners.pending_dropped.load(Ordering::SeqCst), 0);
        token.cancel();
        // No await that can yield between cancellation and this manual poll.
        // The current implementation returns Ready while its worker owns the
        // body. A corrected implementation may return Pending for owned cleanup.
        let at_cancel = poll_fn(|cx| Poll::Ready(pending.as_mut().poll(cx))).await;
        let result = match at_cancel {
            Poll::Ready(result) => result,
            Poll::Pending => tokio::time::timeout(Duration::from_secs(2), pending.as_mut())
                .await
                .expect("cancelled last subscriber must release its owned worker"),
        };
        assert!(matches!(result, Err(EvaluationError::Cancelled)));
        // This is the original immediate body-cancel observation boundary.
        // Do not add a post-return yield or drain before either assertion.
        assert_eq!(
            replay.owners.pending_dropped.load(Ordering::SeqCst),
            1,
            "classifier rejected before its cancelled {phase} body was released"
        );
        assert_eq!(replay.owners.active.load(Ordering::SeqCst), 0);
    }
    let first_paths = if phase == "/api/show" {
        vec!["/api/show"]
    } else {
        vec!["/api/show", "/v1/systemone"]
    };
    assert_eq!(*replay.paths.lock().unwrap(), first_paths);
    let retry = classifier
        .0
        .classify(&body, &config, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(retry.source, "ollama");
    assert_eq!(retry.tier.as_str(), "haiku");
    let mut expected = first_paths;
    expected.extend(["/api/show", "/v1/systemone"]);
    assert_eq!(*replay.paths.lock().unwrap(), expected);
    assert_eq!(replay.owners.active.load(Ordering::SeqCst), 0);
}
#[tokio::test(flavor = "current_thread")]
async fn cancelled_metadata_reader_is_released_before_classify_returns() {
    cancellation_return_releases_pending_reader("/api/show").await;
}
#[tokio::test(flavor = "current_thread")]
async fn cancelled_inference_reader_is_released_before_classify_returns() {
    cancellation_return_releases_pending_reader("/v1/systemone").await;
}

#[path = "support/ollama_evaluator_timed.rs"]
mod timed;
use autorouter_core::router::RouteOptions;
use autorouter_runtime::evaluator::evaluate_serialized_answer;
use autorouter_runtime::router::Router;
use serde_json::Value;
use tokio::task::JoinSet;

struct ClassifierOwner(Arc<Classifier<timed::Replay>>);
impl Drop for ClassifierOwner {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}
struct RouterOwner(Arc<Router<timed::Replay>>);
impl Drop for RouterOwner {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}
const BOUND: Duration = Duration::from_secs(2);
fn classify_task(
    row: &Value,
    replay: Arc<timed::Replay>,
) -> (ClassifierOwner, JoinSet<Result<Value, EvaluationError>>) {
    let config = timed::config(row);
    let owner = ClassifierOwner(Arc::new(Classifier::new(replay, &config)));
    let worker = owner.0.clone();
    let document = JsDocument::parse(row["input"].to_string().as_bytes()).unwrap();
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        worker
            .classify(&document, &config, &CancellationToken::new())
            .await
            .map(|value| serde_json::to_value(value).unwrap())
    });
    (owner, tasks)
}
async fn take_result(
    tasks: &mut JoinSet<Result<Value, EvaluationError>>,
) -> Result<Value, EvaluationError> {
    tokio::time::timeout(BOUND, tasks.join_next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}
#[tokio::test(start_paused = true)]
async fn original_seven_one_forty_ms_deadline_cancels_bodies_and_rejects_65537_bytes() {
    let cases = timed::cases();
    for (index, row) in cases[..3].iter().enumerate() {
        let replay = Arc::new(timed::Replay::new(std::slice::from_ref(row)));
        let (_owner, mut tasks) = classify_task(row, replay.clone());
        replay.wait_calls(1).await;
        let start = replay.clock();
        if index > 0 {
            tokio::time::advance(Duration::from_millis(10)).await;
            replay.wait_calls(2).await;
        }
        if index < 2 {
            replay.wait_pending(1).await;
            assert_eq!(
                start.elapsed(),
                Duration::from_millis(if index == 0 { 0 } else { 10 })
            );
            tokio::time::advance(Duration::from_millis(if index == 0 { 39 } else { 29 })).await;
            tokio::task::yield_now().await;
            assert!(tasks.try_join_next().is_none(), "40 ms budget fired early");
            assert_eq!(replay.owners.bodies.load(Ordering::SeqCst), 1);
            tokio::time::advance(Duration::from_millis(1)).await;
        }
        let result = take_result(&mut tasks).await.unwrap();
        assert_eq!(
            start.elapsed(),
            Duration::from_millis(if index == 2 { 10 } else { 40 })
        );
        // Original #7 assert1/2: complete fallback and exact error category.
        assert!(timed::matches(&result, row));
        // Original #7 assert3: body released at completed evaluation boundary.
        assert!(replay.idle());
        assert_eq!(
            replay.owners.body_dropped.load(Ordering::SeqCst),
            if index == 0 { 1 } else { 2 }
        );
        // Original #7 assert4/5: full captured I/O and shared production deadline.
        assert_eq!(replay.calls(), if index == 0 { 1 } else { 2 });
        assert!(replay.verified());
        assert!(tasks.is_empty());
    }
}
#[tokio::test(start_paused = true)]
async fn original_eight_zero_accepts_15_15_30_and_positive_twenty_expires_in_headers() {
    let cases = timed::cases();
    for pair in cases[3..7].as_chunks::<2>().0 {
        assert_eq!(pair[0]["signal"], pair[1]["signal"]);
        // Reuse the same caller token for both original sequential evaluations.
        let token = CancellationToken::new();
        let _cancel = token.clone().drop_guard();
        for row in pair {
            let replay = Arc::new(timed::Replay::new(std::slice::from_ref(row)));
            let config = timed::config(row);
            let finite = config.ollama_timeout_ms != 0;
            let state = row["input"].to_string();
            // An absent JS caller signal maps to an uncancelled owned token; the
            // supplied live signal maps to a separately owned uncancelled token.
            let (work, cancellation) = (replay.clone(), token.clone());
            let mut tasks = JoinSet::new();
            tasks.spawn(async move {
                evaluate_serialized_answer(work.as_ref(), &config, &state, &cancellation)
                    .await
                    .map(|value| serde_json::to_value(value).unwrap())
            });
            replay.wait_calls(1).await;
            let start = replay.clock();
            tokio::time::advance(Duration::from_millis(15)).await;
            replay.wait_calls(2).await;
            assert_eq!(start.elapsed(), Duration::from_millis(15));
            if finite {
                tokio::time::advance(Duration::from_millis(4)).await;
                tokio::task::yield_now().await;
                assert!(tasks.try_join_next().is_none());
                assert_eq!(replay.owners.request_live.load(Ordering::SeqCst), 1);
                tokio::time::advance(Duration::from_millis(1)).await;
                // Original #8 assert2: actual production timer, not a mocked error.
                assert!(matches!(
                    take_result(&mut tasks).await,
                    Err(EvaluationError::Timeout)
                ));
                assert_eq!(start.elapsed(), Duration::from_millis(20));
                assert_eq!(replay.owners.body_dropped.load(Ordering::SeqCst), 1);
            } else {
                tokio::time::advance(Duration::from_millis(15)).await;
                replay.wait_pending(1).await;
                assert_eq!(start.elapsed(), Duration::from_millis(30));
                tokio::time::advance(Duration::from_millis(29)).await;
                tokio::task::yield_now().await;
                assert!(tasks.try_join_next().is_none());
                tokio::time::advance(Duration::from_millis(1)).await;
                // Original #8 assert1: complete Opus answer and token metrics.
                assert!(timed::matches(&take_result(&mut tasks).await.unwrap(), row));
                assert_eq!(start.elapsed(), Duration::from_millis(60));
                assert_eq!(replay.owners.body_dropped.load(Ordering::SeqCst), 2);
            }
            assert!(!token.is_cancelled());
            assert!(tasks.is_empty());
            assert_eq!(replay.calls(), 2);
            assert_eq!(replay.owners.request_dropped.load(Ordering::SeqCst), 2);
            assert!(replay.verified());
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn original_nine_exact_cancel_return_and_same_classifier_retry_for_both_readers() {
    let cases = timed::cases();
    for (first, retry) in [(7, 8), (9, 10)] {
        let row = &cases[first];
        let retry_row = &cases[retry];
        assert_eq!(row["input"], retry_row["input"]);
        assert_eq!(row["router"], retry_row["router"]);
        let replay = Arc::new(timed::Replay::new(&[row.clone(), retry_row.clone()]));
        let config = timed::config(row);
        assert_eq!(config.ollama_timeout_ms, 0);
        let classifier = ClassifierOwner(Arc::new(Classifier::new(replay.clone(), &config)));
        let document = JsDocument::parse(row["input"].to_string().as_bytes()).unwrap();
        let token = CancellationToken::new();
        let _cancel = token.clone().drop_guard();
        {
            let mut pending = Box::pin(classifier.0.classify(&document, &config, &token));
            assert!(
                poll_fn(|cx| Poll::Ready(pending.as_mut().poll(cx)))
                    .await
                    .is_pending()
            );
            replay.wait_pending(1).await;
            assert_eq!(replay.owners.bodies.load(Ordering::SeqCst), 1);
            token.cancel();
            let immediate = poll_fn(|cx| Poll::Ready(pending.as_mut().poll(cx))).await;
            let result = match immediate {
                Poll::Ready(value) => value,
                Poll::Pending => tokio::time::timeout(BOUND, pending.as_mut()).await.unwrap(),
            };
            // Original #9 assert1: typed API adaptation of original Error identity.
            assert!(matches!(result, Err(EvaluationError::Cancelled)));
            // Original #9 assert2: no cleanup yield between rejection and check.
            assert!(
                replay.idle(),
                "cancelled reader retained after classify returns"
            );
        }
        // Original #9 assert3: exact path count, with sticky full I/O verification.
        assert_eq!(replay.calls(), if first == 7 { 1 } else { 2 });
        let result = classifier
            .0
            .classify(&document, &config, &CancellationToken::new())
            .await
            .unwrap();
        // Original #9 assert4/5: full successful uncached Haiku retry.
        assert!(timed::matches(
            &serde_json::to_value(result).unwrap(),
            retry_row
        ));
        assert!(replay.verified());
    }
}
#[tokio::test]
async fn original_ten_and_fifteen_precancelled_tokens_perform_zero_io() {
    let cases = timed::cases();
    for row in &cases[11..13] {
        let replay = timed::Replay::new(std::slice::from_ref(row));
        let config = timed::config(row);
        let token = CancellationToken::new();
        token.cancel();
        let error = evaluate_serialized_answer(&replay, &config, &row["input"].to_string(), &token)
            .await
            .unwrap_err();
        // Original #10 assert1 and #15 assert1 have an explicit typed Error API
        // adaptation. #10's nested fail guard still has zero actual executions.
        assert_eq!(error, EvaluationError::Cancelled);
        assert!(error.to_string().contains("cancelled"));
        assert_eq!(replay.calls(), 0);
        assert!(replay.verified());
    }
}
#[tokio::test]
async fn original_fifteen_one_router_three_routes_keep_continuity_and_adaptive_guard() {
    let cases = timed::cases();
    let rows = &cases[13..16];
    let replay = Arc::new(timed::Replay::new(rows));
    let router = RouterOwner(Arc::new(Router::new(
        replay.clone(),
        timed::config(&rows[0]),
    )));
    let token = CancellationToken::new();
    let _cancel = token.clone().drop_guard();
    for (index, row) in rows.iter().enumerate() {
        let document = Arc::new(JsDocument::parse(row["input"].to_string().as_bytes()).unwrap());
        let value = tokio::time::timeout(
            BOUND,
            router.0.route(
                document,
                RouteOptions::default(),
                &hyper::HeaderMap::new(),
                &token,
                "",
            ),
        )
        .await
        .unwrap()
        .unwrap();
        // Original #15 assert2..6: complete decisions, including classified
        // Haiku with selected Opus during tool continuation, then Sonnet guard.
        assert!(timed::matches(&value, row));
        assert_eq!(
            replay.calls(),
            (index + 1) * 2,
            "continuation still evaluates before policy pinning"
        );
        assert!(replay.idle());
    }
    assert!(replay.verified());
}
#[tokio::test(start_paused = true)]
async fn matching_timeout_cannot_hide_wrong_timed_request_or_response_category() {
    let cases = timed::cases();
    for mode in ["url", "body"] {
        let mut row = cases[1].clone();
        if mode == "url" {
            row["requests"][1]["url"] = json!("http://127.0.0.1:11434/wrong");
        } else {
            row["requests"][1]["parsed_body"]["state"]["current_task"] = json!("wrong task");
        }
        let replay = Arc::new(timed::Replay::new(std::slice::from_ref(&row)));
        let (_owner, mut tasks) = classify_task(&row, replay.clone());
        let output = take_result(&mut tasks).await.unwrap();
        assert!(timed::matches(&output, &row));
        assert!(!replay.verified());
        let mut wrong = row.clone();
        wrong["output"]["classifier_error"] = json!("network_error");
        assert!(!timed::matches(&output, &wrong));
    }
}
