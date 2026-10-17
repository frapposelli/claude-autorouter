//! Per-route token-count callbacks for native library embeddings.
//! The product gateway retains its existing HTTP TokenCounter by default.

use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use autorouter_core::js_json::JsDocument;
use tokio_util::sync::CancellationToken;

use crate::callback_privacy::contain_callback;

type CountFuture = Pin<Box<dyn Future<Output = Option<f64>> + Send + 'static>>;
type CountStart = dyn Fn(Arc<JsDocument>, String, CancellationToken) -> CountFuture + Send + Sync;

/// A caller-supplied counter for one route. No callback results are cached here.
///
/// The callback receives the unchanged authoritative request, the target model,
/// and a child cancellation token. Return None for an unavailable estimate.
/// Factories, future polls and destructors must not block or detach owned work.
#[derive(Clone)]
pub struct RouteTokenCounter {
    start: Arc<CountStart>,
}
impl RouteTokenCounter {
    pub fn new<F, Fut>(start: F) -> Self
    where
        F: Fn(Arc<JsDocument>, String, CancellationToken) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Option<f64>> + Send + 'static,
    {
        Self {
            start: Arc::new(move |document, model, cancellation| {
                Box::pin(start(document, model, cancellation))
            }),
        }
    }

    pub(crate) async fn count(
        &self,
        document: Arc<JsDocument>,
        model: String,
        cancellation: &CancellationToken,
        timeout: Duration,
    ) -> Option<u64> {
        if cancellation.is_cancelled() {
            return None;
        }
        // Begin the one deadline before constructing user work. Cooperative
        // timers cannot preempt a blocking factory, poll or destructor.
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let mut work = CountWork {
            future: None,
            cancellation: cancellation.child_token(),
        };
        work.future =
            contain_callback(|| (self.start)(document, model, work.cancellation.clone())).ok();
        work.future.as_ref()?;
        let value = tokio::select! {
            biased;
            _ = cancellation.cancelled() => None,
            _ = &mut deadline => None,
            value = poll_fn(|context| {
                contain_callback(|| {
                    work.future.as_mut().expect("admitted count future").as_mut().poll(context)
                }).unwrap_or(std::task::Poll::Ready(None))
            }) => value,
        };
        let released = work.release();
        if cancellation.is_cancelled() || !released {
            return None;
        }
        safe_count(value)
    }
}

struct CountWork {
    future: Option<CountFuture>,
    cancellation: CancellationToken,
}
impl CountWork {
    fn release(&mut self) -> bool {
        self.cancellation.cancel();
        contain_callback(|| drop(self.future.take())).is_ok()
    }
}
impl Drop for CountWork {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

fn safe_count(value: Option<f64>) -> Option<u64> {
    value
        .filter(|value| {
            value.is_finite()
                && *value >= 0.0
                && *value <= 9_007_199_254_740_991.0
                && value.fract() == 0.0
        })
        .map(|value| value as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_safe_integer_predicate_precedes_every_integer_conversion() {
        for (input, expected) in [
            (None, None),
            (Some(f64::NAN), None),
            (Some(f64::INFINITY), None),
            (Some(f64::NEG_INFINITY), None),
            (Some(-1.0), None),
            (Some(1.5), None),
            (Some(f64::MAX), None),
            (Some(9_007_199_254_740_992.0), None),
            (Some(-0.0), Some(0)),
            (Some(0.0), Some(0)),
            (Some(54_481.0), Some(54_481)),
            (Some(190_001.0), Some(190_001)),
            (Some(9_007_199_254_740_991.0), Some(9_007_199_254_740_991)),
        ] {
            assert_eq!(safe_count(input), expected, "input: {input:?}");
        }
    }
    struct FailedCount {
        mode: &'static str,
        released: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl Future for FailedCount {
        type Output = Option<f64>;
        fn poll(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            if self.mode == "poll" {
                panic!("synthetic counter poll failure");
            }
            std::task::Poll::Ready(Some(54_481.0))
        }
    }
    impl Drop for FailedCount {
        fn drop(&mut self) {
            self.released
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.mode == "drop" {
                panic!("synthetic counter destructor failure");
            }
        }
    }

    #[tokio::test]
    async fn constructor_poll_and_destructor_panics_are_unavailable_and_cancel_child_only() {
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicUsize, Ordering};
        for mode in ["factory", "poll", "drop"] {
            let tokens = Arc::new(Mutex::new(Vec::new()));
            let recorded = tokens.clone();
            let released = Arc::new(AtomicUsize::new(0));
            let counted = released.clone();
            let counter = RouteTokenCounter::new(move |_, _, cancellation| {
                recorded.lock().unwrap().push(cancellation);
                if mode == "factory" {
                    panic!("synthetic counter factory failure");
                }
                FailedCount {
                    mode,
                    released: counted.clone(),
                }
            });
            let caller = CancellationToken::new();
            let result = counter
                .count(
                    Arc::new(JsDocument::parse(b"{}").unwrap()),
                    "synthetic".into(),
                    &caller,
                    Duration::from_secs(1),
                )
                .await;
            assert_eq!(result, None);
            assert_eq!(
                released.load(Ordering::SeqCst),
                if mode == "factory" { 0 } else { 1 }
            );
            let tokens = tokens.lock().unwrap();
            assert_eq!(tokens.len(), 1);
            assert!(tokens[0].is_cancelled());
            assert!(!caller.is_cancelled());
        }
    }
}
