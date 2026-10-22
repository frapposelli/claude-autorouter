//! This qualifies the Stub owner-to-adapter drain contract with actual Tokio
//! tasks. Classifier request/body semantics are qualified separately in runtime.
use super::*;
use std::future::{Future, poll_fn};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::Poll;
use tokio::sync::{Semaphore, oneshot};
use tokio_util::task::TaskTracker;

const BOUND: Duration = Duration::from_secs(3);
struct Resource(Arc<AtomicUsize>);
impl Resource {
    fn acquire(count: Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count)
    }
}
impl Drop for Resource {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
struct ReleaseOnDrop {
    stop: CancellationToken,
    release: Arc<Semaphore>,
}
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.stop.cancel();
        self.release.add_permits(1);
    }
}
struct OwnedAdapter {
    stop: CancellationToken,
    tasks: TaskTracker,
    close_entered: Semaphore,
    closes: AtomicUsize,
}
impl GatewayRouter for OwnedAdapter {
    async fn route(
        &self,
        _: Arc<JsDocument>,
        _: RouteOptions,
        _: &HeaderMap,
        _: &CancellationToken,
        _: &str,
    ) -> Result<Value, EvaluationError> {
        panic!("the synthetic ownership control sends no route request")
    }
    fn complete(&self, _: &str, _: &Value) -> bool {
        false
    }
    fn shutdown(&self) {
        self.stop.cancel();
    }
    async fn close(&self) {
        self.closes.fetch_add(1, Ordering::SeqCst);
        self.stop.cancel();
        self.tasks.close();
        self.close_entered.add_permits(1);
        self.tasks.wait().await;
    }
}
async fn ownership_schedule(cancel_close_waiter: bool) {
    let adapter = Arc::new(OwnedAdapter {
        stop: CancellationToken::new(),
        tasks: TaskTracker::new(),
        close_entered: Semaphore::new(0),
        closes: AtomicUsize::new(0),
    });
    let owned_live = Arc::new(AtomicUsize::new(0));
    let owned_entered = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let release_guard = ReleaseOnDrop {
        stop: adapter.stop.clone(),
        release: release.clone(),
    };
    let (stop, held, entered, live) = (
        adapter.stop.clone(),
        release,
        owned_entered.clone(),
        owned_live.clone(),
    );
    // Track the complete task before spawning, including resource destruction.
    let worker = adapter.tasks.track_future(async move {
        let _resource = Resource::acquire(live);
        entered.add_permits(1);
        stop.cancelled().await;
        held.acquire().await.unwrap().forget();
    });
    let worker_handle = tokio::spawn(worker);
    tokio::time::timeout(BOUND, owned_entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let connection_live = Arc::new(AtomicUsize::new(0));
    let connection_entered = Arc::new(Semaphore::new(0));
    let mut connections = tokio::task::JoinSet::new();
    let (live, entered) = (connection_live.clone(), connection_entered.clone());
    connections.spawn(async move {
        let _resource = Resource::acquire(live);
        entered.add_permits(1);
        std::future::pending::<()>().await;
    });
    tokio::time::timeout(BOUND, connection_entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let stopped = CancellationToken::new();
    let finished = Arc::new(AtomicBool::new(false));
    let (finished_tx, mut finished_rx) = oneshot::channel();
    let (signal, owner, complete) = (stopped.clone(), adapter.clone(), finished.clone());
    let task = tokio::spawn(async move {
        signal.cancelled().await;
        // Exact production cleanup helper, held by the same independent task
        // ownership used by the real Stub::close. No socket/provider is needed.
        finish_stub_connections(connections, owner).await;
        complete.store(true, Ordering::SeqCst);
        let _ = finished_tx.send(());
    });
    let handle = Stub {
        address: (std::net::Ipv4Addr::LOCALHOST, 0).into(),
        cancel: stopped,
        task,
    };
    let mut close = Box::pin(handle.close());
    assert!(
        poll_fn(|cx| Poll::Ready(close.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    let entered_drain = tokio::time::timeout(BOUND, async {
        tokio::select! {
            biased;
            p = adapter.close_entered.acquire() => { p.unwrap().forget(); true },
            _ = &mut finished_rx => false,
        }
    })
    .await
    .unwrap();
    let observed = poll_fn(|cx| Poll::Ready(close.as_mut().poll(cx))).await;
    let returned_while_owned = observed.is_ready();
    let live_while_held = owned_live.load(Ordering::SeqCst);
    let connections_after_join = connection_live.load(Ordering::SeqCst);
    let before_release_complete = finished.load(Ordering::SeqCst);
    // Retain every result only after releasing and joining all test ownership.
    // This also cleans up the deliberately failing cancellation-only candidate.
    if cancel_close_waiter || returned_while_owned {
        drop(close);
        drop(release_guard);
        if !before_release_complete {
            tokio::time::timeout(BOUND, &mut finished_rx)
                .await
                .unwrap()
                .unwrap();
        }
    } else {
        drop(release_guard);
        tokio::time::timeout(BOUND, close).await.unwrap();
    }
    adapter.tasks.close();
    tokio::time::timeout(BOUND, worker_handle)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(BOUND, adapter.tasks.wait())
        .await
        .unwrap();
    assert_eq!(owned_live.load(Ordering::SeqCst), 0);
    assert_eq!(connection_live.load(Ordering::SeqCst), 0);
    assert!(finished.load(Ordering::SeqCst));
    assert!(adapter.tasks.is_empty());
    eprintln!(
        "STUB_OWNERSHIP entered={entered_drain} early={returned_while_owned} held={live_while_held} connections={connections_after_join} waiter_cancelled={cancel_close_waiter}"
    );
    assert!(
        entered_drain,
        "stub cleanup never entered its adapter's asynchronous drain"
    );
    assert!(
        !returned_while_owned,
        "stub close returned while adapter-owned task was held"
    );
    assert!(!before_release_complete);
    assert_eq!(live_while_held, 1);
    assert_eq!(connections_after_join, 0);
    assert_eq!(adapter.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn stub_close_awaits_adapter_after_joining_connection_tasks() {
    ownership_schedule(false).await;
}
#[tokio::test(flavor = "current_thread")]
async fn cancelled_stub_close_waiter_preserves_owned_adapter_drain() {
    ownership_schedule(true).await;
}
