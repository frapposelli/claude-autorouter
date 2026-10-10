//! Original session-log contracts and independently owned shutdown controls.
use super::*;
use serde_json::json;
use std::os::unix::fs::DirBuilderExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use std::time::Duration;
use tokio::sync::Semaphore;

const TIME: &str = "2026-10-01T12:00:00.000Z";

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!(
            "autorouter-session-contract-{}",
            nonce
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn decision(index: usize) -> Value {
    json!({"schema_version":2,"event":"decision","timestamp":TIME,
        "request_id":format!("request-{index}"),"session_id":"session-a",
        "request_class":"main","prompt_excerpt":"Fix a typo","prompt_truncated":false,
        "requested_model":"claude-haiku-4-5-20251001","selected_model":"claude-sonnet-5-5",
        "decision_latency_ms":12.5,"source":"jev","reason":"low_confidence",
        "evaluator":"jev","classified_tier":"haiku"})
}

struct HeldIo {
    gate: Semaphore,
    entered: AtomicUsize,
    active: AtomicUsize,
    closes: AtomicUsize,
    closed: AtomicUsize,
    changed: Notify,
}
impl HeldIo {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            gate: Semaphore::new(0),
            entered: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
            changed: Notify::new(),
        })
    }
    async fn wait_for(&self, value: &AtomicUsize, minimum: usize) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let changed = self.changed.notified();
                if value.load(Ordering::SeqCst) >= minimum {
                    break;
                }
                changed.await;
            }
        })
        .await
        .expect("owned session I/O barrier");
    }
}

struct ActiveIo(Arc<HeldIo>);
impl Drop for ActiveIo {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        self.0.changed.notify_waiters();
    }
}
struct ReleaseOnDrop(Option<Arc<HeldIo>>);
impl ReleaseOnDrop {
    fn release(&mut self) {
        if let Some(state) = self.0.take() {
            state.gate.add_permits(3);
        }
    }
}
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.release();
    }
}

struct HeldNativeSink {
    native: NativeSessionSink,
    state: Arc<HeldIo>,
}
impl SessionSink for HeldNativeSink {
    fn initialize(&mut self, directory: PathBuf) -> IoFuture<'_, ()> {
        self.native.initialize(directory)
    }
    fn append(&mut self, name: String, line: Vec<u8>) -> IoFuture<'_, ()> {
        Box::pin(async move {
            self.state.active.fetch_add(1, Ordering::SeqCst);
            let _active = ActiveIo(self.state.clone());
            self.state.entered.fetch_add(1, Ordering::SeqCst);
            self.state.changed.notify_waiters();
            self.state.gate.acquire().await.unwrap().forget();
            self.native.append(name, line).await
        })
    }
    fn close(&mut self) -> IoFuture<'_, ()> {
        Box::pin(async move {
            self.state.closes.fetch_add(1, Ordering::SeqCst);
            let result = self.native.close().await;
            self.state.closed.fetch_add(1, Ordering::SeqCst);
            self.state.changed.notify_waiters();
            result
        })
    }
}

async fn first_poll(future: Pin<&mut impl Future<Output = ()>>) -> bool {
    let mut future = future;
    std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_ready())).await
}

#[tokio::test]
async fn cancelled_first_close_retains_worker_until_real_writes_and_second_close_finish() {
    let root = Directory::new();
    let directory = root.0.join("logs");
    let state = HeldIo::new();
    let mut release = ReleaseOnDrop(Some(state.clone()));
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let observed = warnings.clone();
    let writer = SessionLog::with_sink(
        directory.clone(),
        SessionLogOptions {
            warn: Arc::new(move |warning| observed.lock().unwrap().push(warning.to_owned())),
            now: Arc::new(|| TIME.into()),
            ..SessionLogOptions::default()
        },
        Box::new(HeldNativeSink {
            native: NativeSessionSink::default(),
            state: state.clone(),
        }),
    )
    .await;
    for index in 0..3 {
        assert!(writer.record(&decision(index)));
    }
    state.wait_for(&state.entered, 1).await;

    let mut first = Box::pin(writer.close());
    let first_ready = first_poll(first.as_mut()).await;
    drop(first); // Drop the actual future, not merely a Pin<&mut Future> wrapper.
    let retained_after_cancel = writer
        .worker
        .lock()
        .await
        .as_ref()
        .is_some_and(|worker| !worker.is_finished());
    let mut second = Box::pin(writer.close());
    let second_ready = first_poll(second.as_mut()).await;
    let late_accepted = writer.record(&decision(9999));
    let active_while_held = state.active.load(Ordering::SeqCst);
    let closes_while_held = state.closes.load(Ordering::SeqCst);

    // Complete actual I/O even when testing the old broken implementation, so
    // the intended early-close failure does not leave a held worker behind.
    release.release();
    if !second_ready {
        tokio::time::timeout(Duration::from_secs(3), second)
            .await
            .expect("second close joins the retained worker");
    }
    state.wait_for(&state.closed, 1).await;
    writer.close().await;
    let files = std::fs::read_dir(&directory)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(files.len(), 1);
    let rows: Vec<Value> = std::fs::read_to_string(files[0].path())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows, (0..3).map(decision).collect::<Vec<_>>());
    assert_eq!(state.entered.load(Ordering::SeqCst), 3);
    assert_eq!(state.active.load(Ordering::SeqCst), 0);
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    assert_eq!(state.closed.load(Ordering::SeqCst), 1);
    assert!(writer.worker.lock().await.is_none());
    assert!(warnings.lock().unwrap().is_empty());
    assert!(!first_ready);
    assert!(!late_accepted);
    assert_eq!(active_while_held, 1);
    assert_eq!(closes_while_held, 0);
    assert!(
        !second_ready,
        "a cancelled close waiter must not detach the worker and let the next close return before I/O"
    );
    assert!(
        retained_after_cancel,
        "the worker JoinHandle remains owned after waiter cancellation"
    );
}
