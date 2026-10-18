//! Listener-owned Node HTTP receipt scans, independent of response delivery.
//!
//! Parser progress commits atomically. A scan that wins that commit closes the
//! delivery registry before publishing a terminal state. Observer callbacks and
//! cancellation run outside both locks. No delivery path acquires this lock
//! while holding the completion registry lock.
use crate::transport_completion::{CompletionRegistry, Failure};
use hyper::ext::{
    NodeHttpResponseTiming, NodeHttpServerTimeout, NodeHttpServerTimingControl,
    NodeHttpServerTimingEvent,
};
use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker};
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub(crate) const SCAN_INTERVAL: Duration = Duration::from_secs(30);
const HEADERS_TIMEOUT: Duration = Duration::from_secs(10);
const RECEIVE_TIMEOUT: Duration = Duration::from_secs(30);
const IDLE_TIMEOUT: Duration = Duration::from_secs(6);

type Members = Mutex<BTreeMap<usize, Weak<Receipt>>>;

#[derive(Clone)]
pub(crate) struct ScanRegistry {
    members: Arc<Members>,
    next_scan: Arc<Mutex<Instant>>,
}
impl ScanRegistry {
    pub(crate) fn new() -> Self {
        Self {
            members: Arc::new(Mutex::new(BTreeMap::new())),
            next_scan: Arc::new(Mutex::new(Instant::now() + SCAN_INTERVAL)),
        }
    }
    pub(crate) fn next_scan(&self) -> Instant {
        *self.next_scan.lock().unwrap()
    }
    pub(crate) fn register(
        &self,
        completion: CompletionRegistry,
        requests: CancellationToken,
    ) -> Registration {
        let receipt = Arc::new(Receipt {
            state: Mutex::new(State {
                active: Some((Instant::now(), false)),
                idle: None,
                idle_sleep: None,
                responses: VecDeque::new(),
                terminal: None,
                waker: None,
                next_scan: self.next_scan(),
            }),
            completion,
            requests,
        });
        let id = Arc::as_ptr(&receipt) as usize;
        self.members
            .lock()
            .unwrap()
            .insert(id, Arc::downgrade(&receipt));
        Registration {
            receipt,
            members: Arc::downgrade(&self.members),
            id,
        }
    }
    #[cfg(test)]
    pub(crate) fn scan(&self, now: Instant) {
        self.scan_due(now, now);
    }
    pub(crate) fn scan_due(&self, due: Instant, now: Instant) {
        *self.next_scan.lock().unwrap() = now + SCAN_INTERVAL;
        let live: Vec<_> = {
            let mut members = self.members.lock().unwrap();
            members.retain(|_, receipt| receipt.strong_count() != 0);
            members.values().filter_map(Weak::upgrade).collect()
        };
        for receipt in live {
            receipt.scan(due, now);
        }
    }
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.members.lock().unwrap().len()
    }
}

pub(crate) struct Registration {
    receipt: Arc<Receipt>,
    members: Weak<Members>,
    id: usize,
}
impl Registration {
    pub(crate) fn control(&self) -> Arc<dyn NodeHttpServerTimingControl> {
        self.receipt.clone()
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(members) = self.members.upgrade() {
            members.lock().unwrap().remove(&self.id);
        }
    }
}
struct State {
    active: Option<(Instant, bool)>,
    idle: Option<Instant>,
    idle_sleep: Option<Pin<Box<tokio::time::Sleep>>>,
    responses: VecDeque<NodeHttpResponseTiming>,
    terminal: Option<NodeHttpServerTimeout>,
    waker: Option<Waker>,
    next_scan: Instant,
}
struct Receipt {
    state: Mutex<State>,
    completion: CompletionRegistry,
    requests: CancellationToken,
}
impl std::fmt::Debug for Receipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HttpReceipt")
    }
}
impl Receipt {
    fn scan(&self, due: Instant, now: Instant) {
        let action = {
            let mut state = self.state.lock().unwrap();
            if state.terminal.is_some() {
                return;
            }
            state.next_scan = now + SCAN_INTERVAL;
            let request_expired = state.active.is_some_and(|(start, headers)| {
                let age = now.saturating_duration_since(start);
                (!headers && age > HEADERS_TIMEOUT) || age > RECEIVE_TIMEOUT
            });
            // The listener timer was registered first. An older idle callback
            // would have run before it in Node, even if both are polled late.
            let older_idle = state
                .idle
                .is_some_and(|deadline| deadline < due && deadline <= now);
            let timeout = if older_idle {
                Some(NodeHttpServerTimeout::Idle)
            } else if request_expired {
                Some(NodeHttpServerTimeout::Request {
                    write_raw_408: state
                        .responses
                        .front()
                        .is_none_or(|response| !response.is_prepared()),
                })
            } else if state.idle.is_some_and(|deadline| now >= deadline) {
                Some(NodeHttpServerTimeout::Idle)
            } else {
                None
            };
            let action = timeout.map(|timeout| {
                // Lock order: receipt -> completion. Taking records does not run
                // callbacks. All subsequent flushes see a closed registry.
                let notify = self.completion.close_take(Failure::Deadline);
                state.terminal = Some(timeout);
                (notify, state.waker.take())
            });
            let deferred = if action.is_none() {
                state.waker.take()
            } else {
                None
            };
            drop(state);
            if let Some(waker) = deferred {
                waker.wake();
            }
            action
        };
        if let Some((notify, waker)) = action {
            self.requests.cancel();
            if let Some(waker) = waker {
                waker.wake();
            }
            notify();
        }
    }
}
impl NodeHttpServerTimingControl for Receipt {
    fn commit(&self, events: &[NodeHttpServerTimingEvent]) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.terminal.is_some() {
            return false;
        }
        let now = Instant::now();
        for event in events {
            match event {
                NodeHttpServerTimingEvent::MessageBegin => {
                    state.active = Some((now, false));
                    state.idle = None;
                    state.idle_sleep = None;
                }
                NodeHttpServerTimingEvent::HeadersComplete {
                    message_complete,
                    response,
                } => {
                    if let Some((_, headers)) = &mut state.active {
                        *headers = true;
                    }
                    if *message_complete {
                        state.active = None;
                    }
                    state.responses.push_back(response.clone());
                }
                NodeHttpServerTimingEvent::MessageComplete => state.active = None,
                NodeHttpServerTimingEvent::ParserAborted => {
                    state.active = None;
                    state.idle = None;
                    state.idle_sleep = None;
                }
                NodeHttpServerTimingEvent::PositiveInput => {
                    state.idle = None;
                    state.idle_sleep = None;
                }
                NodeHttpServerTimingEvent::ResponseFinished { queued_responses } => {
                    state.responses.pop_front();
                    // Node arms this even if the next request head is partial;
                    // its independent receipt age must remain active.
                    state.idle = (!queued_responses).then_some(now + IDLE_TIMEOUT);
                    state.idle_sleep = state
                        .idle
                        .map(|deadline| Box::pin(tokio::time::sleep_until(deadline)));
                }
            }
        }
        true
    }
    fn poll_timeout(&self, cx: &mut Context<'_>) -> Poll<NodeHttpServerTimeout> {
        let action = {
            let mut state = self.state.lock().unwrap();
            if let Some(timeout) = state.terminal {
                return Poll::Ready(timeout);
            }
            // A due listener scan registered earlier must run before a later
            // idle callback, even if this connection happens to be polled first.
            let scan_precedes_idle = state.idle.is_some_and(|idle| state.next_scan <= idle)
                && Instant::now() >= state.next_scan;
            if !scan_precedes_idle
                && state
                    .idle_sleep
                    .as_mut()
                    .is_some_and(|timer| timer.as_mut().poll(cx).is_ready())
            {
                let notify = self.completion.close_take(Failure::Deadline);
                state.terminal = Some(NodeHttpServerTimeout::Idle);
                Some((notify, state.waker.take()))
            } else {
                if state
                    .waker
                    .as_ref()
                    .is_none_or(|waker| !waker.will_wake(cx.waker()))
                {
                    state.waker = Some(cx.waker().clone());
                }
                None
            }
        };
        if let Some((notify, waker)) = action {
            self.requests.cancel();
            if let Some(waker) = waker {
                waker.wake();
            }
            notify();
            Poll::Ready(NodeHttpServerTimeout::Idle)
        } else {
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pending(control: &dyn NodeHttpServerTimingControl) -> bool {
        control
            .poll_timeout(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    }
    fn result(control: &dyn NodeHttpServerTimingControl) -> NodeHttpServerTimeout {
        match control.poll_timeout(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("receipt must be terminal"),
        }
    }
    fn zero(response: &NodeHttpResponseTiming) -> NodeHttpServerTimingEvent {
        NodeHttpServerTimingEvent::HeadersComplete {
            message_complete: true,
            response: response.clone(),
        }
    }
    #[tokio::test(start_paused = true)]
    async fn strict_header_scan_tie_and_latched_progress() {
        let scans = ScanRegistry::new();
        let requests = CancellationToken::new();
        let registry = CompletionRegistry::new(16);
        let owner = scans.register(registry.clone(), requests.clone());
        let control = owner.control();
        let start = Instant::now();
        scans.scan(start + HEADERS_TIMEOUT);
        assert!(pending(control.as_ref()));
        assert!(!requests.is_cancelled());
        scans.scan(start + HEADERS_TIMEOUT + Duration::from_nanos(1));
        assert!(matches!(
            result(control.as_ref()),
            NodeHttpServerTimeout::Request {
                write_raw_408: true
            }
        ));
        assert!(requests.is_cancelled());
        assert!(!control.commit(&[
            NodeHttpServerTimingEvent::MessageBegin,
            zero(&NodeHttpResponseTiming::new())
        ]));
        assert!(
            registry
                .track_empty_response(
                    hyper::Response::new(http_body_util::Empty::<bytes::Bytes>::new()),
                    |_| {}
                )
                .is_err()
        );
        drop(owner);
        assert_eq!(scans.len(), 0);
    }
    #[tokio::test(start_paused = true)]
    async fn body_age_includes_headers_but_completed_zero_body_is_inactive() {
        let scans = ScanRegistry::new();
        let owner = scans.register(CompletionRegistry::new(16), CancellationToken::new());
        let control = owner.control();
        let start = Instant::now();
        tokio::time::advance(Duration::from_secs(1)).await;
        let body = NodeHttpResponseTiming::new();
        assert!(control.commit(&[
            NodeHttpServerTimingEvent::MessageBegin,
            NodeHttpServerTimingEvent::HeadersComplete {
                message_complete: false,
                response: body
            }
        ]));
        scans.scan(start + Duration::from_secs(30));
        assert!(pending(control.as_ref()));
        scans.scan(start + Duration::from_secs(31));
        assert!(
            pending(control.as_ref()),
            "request age exactly30s is not expired"
        );
        scans.scan(start + Duration::from_secs(31) + Duration::from_nanos(1));
        assert!(matches!(
            result(control.as_ref()),
            NodeHttpServerTimeout::Request { .. }
        ));
        drop(owner);
        let owner = scans.register(CompletionRegistry::new(16), CancellationToken::new());
        let control = owner.control();
        assert!(control.commit(&[
            NodeHttpServerTimingEvent::MessageBegin,
            zero(&NodeHttpResponseTiming::new())
        ]));
        scans.scan(start + Duration::from_secs(600));
        assert!(pending(control.as_ref()));
    }
    #[tokio::test(start_paused = true)]
    async fn promotion_keeps_the_queued_responses_own_prepared_flag() {
        for prepared in [false, true] {
            let scans = ScanRegistry::new();
            let owner = scans.register(CompletionRegistry::new(16), CancellationToken::new());
            let control = owner.control();
            let a = NodeHttpResponseTiming::new();
            a.mark_prepared();
            let b = NodeHttpResponseTiming::new();
            if prepared {
                b.mark_prepared();
            }
            assert!(control.commit(&[
                NodeHttpServerTimingEvent::MessageBegin,
                zero(&a),
                NodeHttpServerTimingEvent::MessageBegin,
                zero(&b),
                NodeHttpServerTimingEvent::MessageBegin
            ]));
            assert!(
                control.commit(&[NodeHttpServerTimingEvent::ResponseFinished {
                    queued_responses: true
                }])
            );
            scans.scan(Instant::now() + Duration::from_secs(30));
            assert!(
                matches!(result(control.as_ref()), NodeHttpServerTimeout::Request { write_raw_408 } if write_raw_408 != prepared)
            );
            // The scan captures its decision. Later queued work cannot change it.
            b.mark_prepared();
            assert!(
                matches!(result(control.as_ref()), NodeHttpServerTimeout::Request { write_raw_408 } if write_raw_408 != prepared)
            );
        }
    }
    #[tokio::test(start_paused = true)]
    async fn finished_prior_response_allows_a_new_raw_timeout() {
        let scans = ScanRegistry::new();
        let owner = scans.register(CompletionRegistry::new(16), CancellationToken::new());
        let control = owner.control();
        let a = NodeHttpResponseTiming::new();
        a.mark_prepared();
        assert!(control.commit(&[
            NodeHttpServerTimingEvent::MessageBegin,
            zero(&a),
            NodeHttpServerTimingEvent::ResponseFinished {
                queued_responses: false
            },
            NodeHttpServerTimingEvent::PositiveInput,
            NodeHttpServerTimingEvent::MessageBegin
        ]));
        scans.scan(Instant::now() + Duration::from_secs(30));
        assert!(matches!(
            result(control.as_ref()),
            NodeHttpServerTimeout::Request {
                write_raw_408: true
            }
        ));
    }
    #[tokio::test(start_paused = true)]
    async fn idle_buffer_and_raw_activity_do_not_erase_partial_receipt_age() {
        let scans = ScanRegistry::new();
        let owner = scans.register(CompletionRegistry::new(16), CancellationToken::new());
        let control = owner.control();
        assert!(control.commit(&[
            NodeHttpServerTimingEvent::MessageBegin,
            zero(&NodeHttpResponseTiming::new()),
            NodeHttpServerTimingEvent::MessageBegin,
            NodeHttpServerTimingEvent::ResponseFinished {
                queued_responses: false
            }
        ]));
        assert!(pending(control.as_ref()));
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(
            pending(control.as_ref()),
            "idle buffer must extend past5000ms"
        );
        assert!(control.commit(&[NodeHttpServerTimingEvent::PositiveInput]));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(
            pending(control.as_ref()),
            "positiveCRLF clears idle without beginning another request"
        );
        scans.scan(Instant::now() + Duration::from_secs(24));
        assert!(matches!(
            result(control.as_ref()),
            NodeHttpServerTimeout::Request {
                write_raw_408: true
            }
        ));
        drop(owner);
        let owner = scans.register(CompletionRegistry::new(16), CancellationToken::new());
        let control = owner.control();
        assert!(control.commit(&[
            NodeHttpServerTimingEvent::MessageBegin,
            zero(&NodeHttpResponseTiming::new()),
            NodeHttpServerTimingEvent::ResponseFinished {
                queued_responses: false
            }
        ]));
        assert!(pending(control.as_ref()));
        tokio::time::advance(Duration::from_secs(6)).await;
        assert!(matches!(
            result(control.as_ref()),
            NodeHttpServerTimeout::Idle
        ));
    }
    #[tokio::test(start_paused = true)]
    async fn registrations_retire_immediately_instead_of_waiting_for_scan() {
        let scans = ScanRegistry::new();
        for _ in 0..1000 {
            let registration =
                scans.register(CompletionRegistry::new(16), CancellationToken::new());
            assert_eq!(scans.len(), 1);
            drop(registration);
            assert_eq!(scans.len(), 0);
        }
    }
}

#[cfg(test)]
mod delayed_scan_tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn overdue_idle_and_scanner_follow_their_due_order_not_poll_order() {
        for finish_at in [
            Duration::ZERO,
            Duration::from_secs(24),
            Duration::from_secs(29),
        ] {
            let scans = ScanRegistry::new();
            let owner = scans.register(CompletionRegistry::new(16), CancellationToken::new());
            let control = owner.control();
            let start = Instant::now();
            let response = NodeHttpResponseTiming::new();
            response.mark_prepared();
            assert!(control.commit(&[
                NodeHttpServerTimingEvent::MessageBegin,
                NodeHttpServerTimingEvent::HeadersComplete {
                    message_complete: true,
                    response
                },
                NodeHttpServerTimingEvent::MessageBegin
            ]));
            tokio::time::advance(finish_at).await;
            assert!(
                control.commit(&[NodeHttpServerTimingEvent::ResponseFinished {
                    queued_responses: false
                }])
            );
            // Neither connection nor listener is driven until37s. The nominal
            // listener callback was due30s; its actual start is37s. Idle6 runs
            // first, equal30 yields to the earlier-registered scanner, and idle35
            // is later than the scanner even though both are now overdue.
            tokio::time::advance(Duration::from_secs(37) - finish_at).await;
            scans.scan_due(start + Duration::from_secs(30), Instant::now());
            let timeout = control.poll_timeout(&mut Context::from_waker(Waker::noop()));
            if finish_at.is_zero() {
                assert!(matches!(timeout, Poll::Ready(NodeHttpServerTimeout::Idle)));
            } else {
                assert!(matches!(
                    timeout,
                    Poll::Ready(NodeHttpServerTimeout::Request {
                        write_raw_408: true
                    })
                ));
            }
        }
    }
}

#[cfg(test)]
mod reverse_poll_tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn a_late_idle_poll_defers_to_an_earlier_due_listener_scan() {
        let scans = ScanRegistry::new();
        let owner = scans.register(CompletionRegistry::new(16), CancellationToken::new());
        let control = owner.control();
        let start = Instant::now();
        let a = NodeHttpResponseTiming::new();
        a.mark_prepared();
        assert!(control.commit(&[
            NodeHttpServerTimingEvent::MessageBegin,
            NodeHttpServerTimingEvent::HeadersComplete {
                message_complete: true,
                response: a
            },
            NodeHttpServerTimingEvent::MessageBegin
        ]));
        tokio::time::advance(Duration::from_secs(29)).await;
        assert!(
            control.commit(&[NodeHttpServerTimingEvent::ResponseFinished {
                queued_responses: false
            }])
        );
        tokio::time::advance(Duration::from_secs(8)).await;
        let mut cx = Context::from_waker(Waker::noop());
        assert!(
            control.poll_timeout(&mut cx).is_pending(),
            "idle35 must not overtake scan30"
        );
        scans.scan_due(start + Duration::from_secs(30), Instant::now());
        assert!(matches!(
            control.poll_timeout(&mut cx),
            Poll::Ready(NodeHttpServerTimeout::Request {
                write_raw_408: true
            })
        ));
    }
}
