//! Both held phases use the actual Router, evaluator, classifier worker, and
//! GatewayHandle. No result is injected; close is polled after a positive drain
//! entry observation, while the old worker is positively held.
use super::*;
use crate::classifier::shutdown_gate::WorkerPollGate;
use crate::http_client::HttpError;
use crate::server::Gateway;
use crate::server_events::EventSinks;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame};
use hyper::{Request, Response};
use std::{
    future::{Future, poll_fn},
    io,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::Semaphore;

const TEST_BOUND: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug)]
enum HeldPhase {
    Request,
    Body,
}
struct Counted(Arc<AtomicUsize>);
impl Counted {
    fn new(count: Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count)
    }
}
impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
struct HeldBody {
    _owned: Counted,
    entered: Arc<Semaphore>,
    observed: bool,
}
impl Body for HeldBody {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        if !self.observed {
            self.observed = true;
            self.entered.add_permits(1);
        }
        Poll::Pending
    }
}
struct HeldTransport {
    phase: HeldPhase,
    requests: Arc<AtomicUsize>,
    bodies: Arc<AtomicUsize>,
    entered: Arc<Semaphore>,
    calls: AtomicUsize,
}
impl HttpTransport for HeldTransport {
    type ResponseBody = HeldBody;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<HeldBody>, HttpError> {
        assert_eq!(request.uri().path(), "/v1/systemone");
        assert_eq!(self.calls.fetch_add(1, Ordering::SeqCst), 0);
        let _request = Counted::new(self.requests.clone());
        match self.phase {
            HeldPhase::Request => {
                self.entered.add_permits(1);
                std::future::pending().await
            }
            HeldPhase::Body => Ok(Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(HeldBody {
                    _owned: Counted::new(self.bodies.clone()),
                    entered: self.entered.clone(),
                    observed: false,
                })
                .unwrap()),
        }
    }
}

async fn gateway_retired_owner_schedule(cancel_close_waiter: bool) {
    let mut observations = Vec::new();
    for phase in [HeldPhase::Request, HeldPhase::Body] {
        let transport = Arc::new(HeldTransport {
            phase,
            requests: Arc::new(AtomicUsize::new(0)),
            bodies: Arc::new(AtomicUsize::new(0)),
            entered: Arc::new(Semaphore::new(0)),
            calls: AtomicUsize::new(0),
        });
        let settings = autorouter_core::config::read_config(
            &json!({
                "AUTOROUTER_EVALUATOR": "jev",
                "TYPESAFE_API_KEY": "synthetic-evaluator",
                "ANTHROPIC_API_KEY": "synthetic-provider",
                "AUTOROUTER_TOKEN": "synthetic-shutdown-regression-token",
                "AUTOROUTER_JEV_TIMEOUT_MS": "10000"
            }),
            false,
            std::path::Path::new("/synthetic"),
        )
        .unwrap();
        let router = Arc::new(Router::new(transport.clone(), settings.clone()));
        let gate = Arc::new(WorkerPollGate::default());
        router.classifier.test_worker_poll_gate(gate.clone());
        let gateway = Gateway::with_router(
            settings,
            transport.clone(),
            router.clone(),
            EventSinks::default(),
        )
        .unwrap();
        let handle = gateway.listen(0).await.unwrap();
        let document = Arc::new(
            JsDocument::parse(br#"{"model":"claude-sonnet-5","messages":[{"role":"user","content":"Synthetic shutdown ownership"}],"max_tokens":4096}"#)
                .unwrap(),
        );
        let caller = CancellationToken::new();
        let headers = HeaderMap::new();
        let mut route = Box::pin(router.route_exact(
            document.clone(),
            RouteOptions {
                scope: "synthetic-owner".into(),
                prompt_id: "synthetic-task".into(),
                request_id: Some("synthetic-request".into()),
                count_tokens: false,
                ..RouteOptions::default()
            },
            &headers,
            &caller,
            "",
        ));
        assert!(
            poll_fn(|cx| Poll::Ready(route.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        tokio::time::timeout(TEST_BOUND, transport.entered.acquire())
            .await
            .expect("actual evaluator phase was never entered")
            .unwrap()
            .forget();
        assert_eq!(router.classifier.pending_counts(), (1, 1));
        let release = gate.hold();
        drop(route);
        assert_eq!(router.classifier.pending_counts(), (0, 0));
        tokio::time::timeout(TEST_BOUND, gate.wait_blocked())
            .await
            .expect("cancelled real worker never reached the scheduling gate");
        let mut close_task = tokio::spawn(handle.close());
        tokio::time::timeout(TEST_BOUND, async {
            while !router.advisory_tasks.is_closed() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("router close never entered its persistent drain");
        let close = poll_fn(|cx| Poll::Ready(Pin::new(&mut close_task).poll(cx))).await;
        let close_returned_while_held = close.is_ready();
        let request_live = transport.requests.load(Ordering::SeqCst);
        let body_live = transport.bodies.load(Ordering::SeqCst);
        if cancel_close_waiter && close.is_pending() {
            close_task.abort();
            assert!(
                tokio::time::timeout(TEST_BOUND, &mut close_task)
                    .await
                    .unwrap()
                    .unwrap_err()
                    .is_cancelled()
            );
        }
        // Always release and await owned teardown before asserting the gap.
        drop(release);
        tokio::time::timeout(TEST_BOUND, gate.wait_finished())
            .await
            .expect("held worker did not complete bounded cleanup");
        if close.is_pending() && !cancel_close_waiter {
            tokio::time::timeout(TEST_BOUND, &mut close_task)
                .await
                .expect("gateway cleanup exceeded the released test bound")
                .expect("gateway close task panicked");
        }
        if cancel_close_waiter {
            // The consuming close waiter is gone; the listener still owns its
            // drain and must release its final Gateway Arc after joining it.
            tokio::time::timeout(TEST_BOUND, async {
                while Arc::strong_count(&gateway) != 1 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("cancelled close waiter stranded listener cleanup");
        }
        assert!(matches!(
            router
                .route_exact(
                    document,
                    RouteOptions {
                        request_class: "auxiliary".into(),
                        ..RouteOptions::default()
                    },
                    &headers,
                    &caller,
                    ""
                )
                .await,
            Err(EvaluationError::Cancelled)
        ));
        assert_eq!(transport.requests.load(Ordering::SeqCst), 0);
        assert_eq!(transport.bodies.load(Ordering::SeqCst), 0);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert!(!caller.is_cancelled());
        observations.push((phase, close_returned_while_held, request_live, body_live));
    }
    eprintln!("GATEWAY_SHUTDOWN_OWNERSHIP {observations:?}");
    assert!(
        observations
            .iter()
            .all(|(_, returned, requests, bodies)| { !*returned && *requests + *bodies == 1 }),
        "gateway close must remain pending while its retired worker owns request/body"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn gateway_close_must_join_retired_classifier_request_and_body() {
    gateway_retired_owner_schedule(false).await;
}
#[tokio::test(flavor = "current_thread")]
async fn cancelling_consuming_gateway_close_keeps_listener_drain_ownership() {
    gateway_retired_owner_schedule(true).await;
}

struct AdvisoryTransport {
    count_live: Arc<AtomicUsize>,
    count_calls: AtomicUsize,
    evaluation_calls: AtomicUsize,
    count_entered: Semaphore,
}
impl Default for AdvisoryTransport {
    fn default() -> Self {
        Self {
            count_live: Arc::new(AtomicUsize::new(0)),
            count_calls: AtomicUsize::new(0),
            evaluation_calls: AtomicUsize::new(0),
            count_entered: Semaphore::new(0),
        }
    }
}
impl HttpTransport for AdvisoryTransport {
    type ResponseBody = Full<Bytes>;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Full<Bytes>>, HttpError> {
        let is_count = request.uri().path().ends_with("/count_tokens");
        let body = request.into_body().collect().await.unwrap().to_bytes();
        let text = std::str::from_utf8(&body).unwrap();
        let value = if is_count {
            self.count_calls.fetch_add(1, Ordering::SeqCst);
            let _owned = Counted::new(self.count_live.clone());
            self.count_entered.add_permits(1);
            if text.contains("unused-held")
                || text.contains("cancel-inline")
                || text.contains("drop-route")
            {
                return std::future::pending().await;
            }
            if text.contains("unavailable") {
                return Err(HttpError::Network);
            }
            json!({"input_tokens": if text.contains("over-budget") { 200000 } else { 1000 }})
        } else {
            self.evaluation_calls.fetch_add(1, Ordering::SeqCst);
            if text.contains("drop-route") {
                return std::future::pending().await;
            }
            json!({"answers":{"tier":{"choice":if text.contains("unused-held") || text.contains("no-count") { "opus" } else { "haiku" },"confidence":0.9}}})
        };
        Ok(Response::new(Full::new(Bytes::from(
            serde_json::to_vec(&value).unwrap(),
        ))))
    }
}
fn advisory_document(label: &str) -> Arc<JsDocument> {
    Arc::new(
        JsDocument::parse(
            &serde_json::to_vec(&json!({
                "model":"claude-haiku-4-5-20251001",
                "messages":[{"role":"user","content":format!("{label} {}", "a".repeat(160000))}],
                "max_tokens":4096,"thinking":{"type":"disabled"}
            }))
            .unwrap(),
        )
        .unwrap(),
    )
}
fn advisory_options(label: &str) -> RouteOptions {
    RouteOptions {
        scope: label.into(),
        prompt_id: label.into(),
        request_id: Some(label.into()),
        count_tokens: true,
        ..RouteOptions::default()
    }
}
fn advisory_router(transport: Arc<AdvisoryTransport>) -> Router<AdvisoryTransport> {
    let config = autorouter_core::config::read_config(
        &json!({"AUTOROUTER_EVALUATOR":"jev","TYPESAFE_API_KEY":"synthetic-evaluator","ANTHROPIC_API_KEY":"synthetic-provider","AUTOROUTER_JEV_TIMEOUT_MS":"10000","AUTOROUTER_TOKEN_COUNT_TIMEOUT_MS":"10000"}),
        false, std::path::Path::new("/synthetic")
    ).unwrap();
    Router::with_clock_and_limits(
        transport,
        config,
        Arc::new(|| 0),
        Limits {
            pending: 2,
            subscribers: 1,
        },
    )
}
async fn count_entered(transport: &AdvisoryTransport) {
    tokio::time::timeout(TEST_BOUND, transport.count_entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
}
async fn bounded_route(
    future: impl Future<Output = Result<RouteDecision, EvaluationError>>,
) -> RouteDecision {
    tokio::time::timeout(TEST_BOUND, future)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn omitted_speculation_at_capacity_keeps_required_counts_and_exact_eligibility() {
    let transport = Arc::new(AdvisoryTransport::default());
    let router = advisory_router(transport.clone());
    assert_eq!(router.advisory_limit, 1);
    let gate = Arc::new(WorkerPollGate::default());
    *router.advisory_gate.lock().unwrap() = Some(gate.clone());
    let headers = HeaderMap::new();
    let caller = CancellationToken::new();
    let unused = bounded_route(router.route_exact(
        advisory_document("unused-held"),
        advisory_options("unused-held"),
        &headers,
        &caller,
        "",
    ))
    .await;
    assert_eq!(unused.decision["model"], router.config.models.opus);
    count_entered(&transport).await;
    assert_eq!(router.advisory_tasks.len(), 1);
    let release = gate.hold();
    for (label, expected_count, expected_model, expected_check) in [
        (
            "within-budget",
            1000,
            router.config.models.haiku.as_str(),
            "within_budget",
        ),
        (
            "over-budget",
            200000,
            router.config.models.sonnet.as_str(),
            "over_budget",
        ),
    ] {
        let result = bounded_route(router.route_exact(
            advisory_document(label),
            advisory_options(label),
            &headers,
            &caller,
            "",
        ))
        .await;
        assert_eq!(result.decision["model"], expected_model);
        assert_eq!(result.decision["counted_input_tokens"], expected_count);
        assert_eq!(result.decision["context_check"], expected_check);
        assert_eq!(
            router.advisory_tasks.len(),
            1,
            "required count stayed inline"
        );
        count_entered(&transport).await;
    }
    let before = transport.count_calls.load(Ordering::SeqCst);
    let result = bounded_route(router.route_exact(
        advisory_document("no-count"),
        advisory_options("no-count"),
        &headers,
        &caller,
        "",
    ))
    .await;
    assert_eq!(result.decision["model"], router.config.models.opus);
    assert_eq!(transport.count_calls.load(Ordering::SeqCst), before);
    assert_eq!(transport.count_live.load(Ordering::SeqCst), 1);
    let mut close = Box::pin(router.close());
    assert!(
        poll_fn(|cx| Poll::Ready(close.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    tokio::time::timeout(TEST_BOUND, gate.wait_blocked())
        .await
        .unwrap();
    drop(close);
    let mut second = Box::pin(router.close());
    assert!(
        poll_fn(|cx| Poll::Ready(second.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    drop(release);
    tokio::time::timeout(TEST_BOUND, second).await.unwrap();
    assert_eq!(transport.count_live.load(Ordering::SeqCst), 0);
    assert_eq!(router.advisory_tasks.len(), 0);
    assert_eq!(transport.count_calls.load(Ordering::SeqCst), 3);
}

#[tokio::test(flavor = "current_thread")]
async fn a_started_count_returning_none_is_never_retried_and_its_slot_recovers() {
    let transport = Arc::new(AdvisoryTransport::default());
    let router = advisory_router(transport.clone());
    let caller = CancellationToken::new();
    let headers = HeaderMap::new();
    let result = bounded_route(router.route_exact(
        advisory_document("unavailable"),
        advisory_options("unavailable"),
        &headers,
        &caller,
        "",
    ))
    .await;
    assert_eq!(result.decision["context_check"], "count_unavailable");
    assert_eq!(transport.count_calls.load(Ordering::SeqCst), 1);
    assert_eq!(transport.count_live.load(Ordering::SeqCst), 0);
    assert_eq!(router.advisory_tasks.len(), 0);
    let result = bounded_route(router.route_exact(
        advisory_document("within-after-none"),
        advisory_options("within-after-none"),
        &headers,
        &caller,
        "",
    ))
    .await;
    assert_eq!(result.decision["context_check"], "within_budget");
    assert_eq!(transport.count_calls.load(Ordering::SeqCst), 2);
    tokio::time::timeout(TEST_BOUND, router.close())
        .await
        .unwrap();
    assert_eq!(router.advisory_tasks.len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn required_inline_count_at_capacity_remains_cancellable() {
    let transport = Arc::new(AdvisoryTransport::default());
    let router = advisory_router(transport.clone());
    let headers = HeaderMap::new();
    let owner = CancellationToken::new();
    bounded_route(router.route_exact(
        advisory_document("unused-held"),
        advisory_options("unused-held"),
        &headers,
        &owner,
        "",
    ))
    .await;
    count_entered(&transport).await;
    let caller = CancellationToken::new();
    let mut route = Box::pin(router.route_exact(
        advisory_document("cancel-inline"),
        advisory_options("cancel-inline"),
        &headers,
        &caller,
        "",
    ));
    tokio::time::timeout(TEST_BOUND, async {
        tokio::select! {
            _ = transport.count_entered.acquire() => {},
            _ = &mut route => panic!("held inline route unexpectedly finished"),
        }
    })
    .await
    .unwrap();
    assert_eq!(transport.count_live.load(Ordering::SeqCst), 2);
    assert_eq!(router.advisory_tasks.len(), 1);
    caller.cancel();
    assert!(matches!(
        tokio::time::timeout(TEST_BOUND, route).await.unwrap(),
        Err(EvaluationError::Cancelled)
    ));
    assert_eq!(transport.count_live.load(Ordering::SeqCst), 1);
    tokio::time::timeout(TEST_BOUND, router.close())
        .await
        .unwrap();
    assert_eq!(transport.count_live.load(Ordering::SeqCst), 0);
    assert_eq!(router.advisory_tasks.len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn owner_close_interrupts_required_inline_count_without_caller_cancellation() {
    let transport = Arc::new(AdvisoryTransport::default());
    let router = advisory_router(transport.clone());
    let headers = HeaderMap::new();
    let caller = CancellationToken::new();
    bounded_route(router.route_exact(
        advisory_document("unused-held"),
        advisory_options("unused-held"),
        &headers,
        &caller,
        "",
    ))
    .await;
    count_entered(&transport).await;
    let mut route = Box::pin(router.route_exact(
        advisory_document("cancel-inline"),
        advisory_options("cancel-inline"),
        &headers,
        &caller,
        "",
    ));
    tokio::time::timeout(TEST_BOUND, async {
        tokio::select! {
            permit = transport.count_entered.acquire() => permit.unwrap().forget(),
            _ = &mut route => panic!("held inline route unexpectedly finished"),
        }
    })
    .await
    .unwrap();
    assert_eq!(transport.count_live.load(Ordering::SeqCst), 2);
    assert_eq!(router.advisory_tasks.len(), 1);
    let mut close = Box::pin(router.close());
    assert!(
        poll_fn(|cx| Poll::Ready(close.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    assert!(!caller.is_cancelled());
    assert!(matches!(
        tokio::time::timeout(TEST_BOUND, route).await.unwrap(),
        Err(EvaluationError::Cancelled)
    ));
    tokio::time::timeout(TEST_BOUND, close).await.unwrap();
    assert!(!caller.is_cancelled());
    assert_eq!(transport.count_live.load(Ordering::SeqCst), 0);
    assert_eq!(router.advisory_tasks.len(), 0);
    assert_eq!(transport.count_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn aborted_advisory_retains_capacity_until_its_owned_future_is_destroyed() {
    let transport = Arc::new(AdvisoryTransport::default());
    let router = advisory_router(transport.clone());
    let caller = CancellationToken::new();
    let headers = HeaderMap::new();
    let mut route = Box::pin(router.route_exact(
        advisory_document("drop-route"),
        advisory_options("drop-route"),
        &headers,
        &caller,
        "",
    ));
    tokio::time::timeout(TEST_BOUND, async {
        tokio::select! {
            permit = transport.count_entered.acquire() => permit.unwrap().forget(),
            _ = &mut route => panic!("held route unexpectedly finished"),
        }
    })
    .await
    .unwrap();
    assert_eq!(router.advisory_tasks.len(), 1);
    assert_eq!(transport.count_live.load(Ordering::SeqCst), 1);
    drop(route);
    // No executor yield: abort is a request, not destruction of owned I/O.
    assert_eq!(router.advisory_tasks.len(), 1);
    assert_eq!(transport.count_live.load(Ordering::SeqCst), 1);
    assert!(
        router
            .start_early_count(
                advisory_document("after-drop"),
                &router.config.models.haiku,
                &headers,
                &caller,
                ""
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(transport.count_calls.load(Ordering::SeqCst), 1);
    tokio::time::timeout(TEST_BOUND, async {
        while !router.advisory_tasks.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(transport.count_live.load(Ordering::SeqCst), 0);
    let admitted = router
        .start_early_count(
            advisory_document("after-drop"),
            &router.config.models.haiku,
            &headers,
            &caller,
            "",
        )
        .unwrap()
        .expect("released slot must admit real work");
    assert_eq!(
        tokio::time::timeout(TEST_BOUND, admitted)
            .await
            .unwrap()
            .unwrap(),
        Some(1000)
    );
    tokio::time::timeout(TEST_BOUND, router.close())
        .await
        .unwrap();
    assert_eq!(transport.count_live.load(Ordering::SeqCst), 0);
    assert_eq!(router.advisory_tasks.len(), 0);
    assert_eq!(transport.count_calls.load(Ordering::SeqCst), 2);
    assert!(!caller.is_cancelled());
}
