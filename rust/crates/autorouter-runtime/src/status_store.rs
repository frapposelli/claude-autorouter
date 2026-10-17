//! Optional private status snapshots. One owned worker serializes the newest
//! bounded state; routing updates never wait for filesystem I/O. `close` joins
//! all accepted work before deleting the directory, including after readiness
//! timeout. No detached blocking write can recreate files after close returns.
use autorouter_core::status_state::StatusState;
use serde_json::Value;
use std::future::Future;
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio::task::JoinHandle;

type IoFuture<'a, T> = Pin<Box<dyn Future<Output = io::Result<T>> + Send + 'a>>;
/// Injectable I/O boundary for deterministic stalled-storage tests. Each future
/// remains owned until completion; implementations must not detach work.
pub trait StatusIo: Send + Sync {
    fn create(&self, parent: PathBuf) -> IoFuture<'_, PathBuf>;
    fn write(&self, directory: PathBuf, bytes: Vec<u8>) -> IoFuture<'_, ()>;
    fn remove(&self, directory: PathBuf) -> IoFuture<'_, ()>;
}
pub struct NativeStatusIo;
fn random_name() -> io::Result<String> {
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|_| io::Error::other("randomness unavailable"))?;
    Ok(random.iter().map(|b| format!("{b:02x}")).collect())
}
fn joined<T>(result: Result<io::Result<T>, tokio::task::JoinError>) -> io::Result<T> {
    result.map_err(io::Error::other)?
}
impl StatusIo for NativeStatusIo {
    fn create(&self, parent: PathBuf) -> IoFuture<'_, PathBuf> {
        Box::pin(async move {
            joined(
                tokio::task::spawn_blocking(move || {
                    for _ in 0..128 {
                        let path = parent.join(format!("autorouter-status-{}", random_name()?));
                        match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                            Ok(()) => return Ok(path),
                            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                            Err(error) => return Err(error),
                        }
                    }
                    Err(io::Error::other("status directory allocation failed"))
                })
                .await,
            )
        })
    }
    fn write(&self, directory: PathBuf, bytes: Vec<u8>) -> IoFuture<'_, ()> {
        Box::pin(async move {
            joined(
                tokio::task::spawn_blocking(move || {
                    let temporary = directory.join(format!(".state-{}.tmp", random_name()?));
                    let result = (|| {
                        let mut file = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .mode(0o600)
                            .open(&temporary)?;
                        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                        file.write_all(&bytes)?;
                        drop(file);
                        std::fs::rename(&temporary, directory.join("state.json"))
                    })();
                    if result.is_err() {
                        let _ = std::fs::remove_file(&temporary);
                    }
                    result
                })
                .await,
            )
        })
    }
    fn remove(&self, directory: PathBuf) -> IoFuture<'_, ()> {
        Box::pin(async move {
            joined(
                tokio::task::spawn_blocking(move || match std::fs::remove_dir_all(directory) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                    result => result,
                })
                .await,
            )
        })
    }
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
pub struct StatusOptions {
    pub directory: PathBuf,
    pub baseline_model: Option<String>,
    pub io: Arc<dyn StatusIo>,
    pub now: Arc<dyn Fn() -> u64 + Send + Sync>,
}
impl Default for StatusOptions {
    fn default() -> Self {
        Self {
            directory: std::env::temp_dir(),
            baseline_model: None,
            io: Arc::new(NativeStatusIo),
            now: Arc::new(now_ms),
        }
    }
}
struct Inner {
    state: Mutex<StatusState>,
    path: Mutex<Option<PathBuf>>,
    disabled: AtomicBool,
    closing: AtomicBool,
    generation: AtomicU64,
    requested: watch::Sender<u64>,
    completed: watch::Sender<u64>,
    initialized: watch::Sender<bool>,
    now: Arc<dyn Fn() -> u64 + Send + Sync>,
}
impl Inner {
    fn schedule(&self) -> u64 {
        let epoch = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.requested
            .send_modify(|current| *current = (*current).max(epoch));
        epoch
    }
    fn serialized(&self) -> Option<Vec<u8>> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        serde_json::to_vec(&state.snapshot(std::process::id(), (self.now)())).ok()
    }
    fn disable(&self) {
        self.disabled.store(true, Ordering::SeqCst);
        *self.path.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.schedule();
    }
}
pub struct StatusStore {
    inner: Arc<Inner>,
    worker: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    readiness: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    deadline: tokio::time::Instant,
}
impl StatusStore {
    pub fn create(options: StatusOptions) -> Self {
        let (requested, receiver) = watch::channel(0);
        let (completed, _) = watch::channel(0);
        let (initialized, _) = watch::channel(false);
        let state = options
            .baseline_model
            .as_deref()
            .map(StatusState::new)
            .unwrap_or_default();
        let inner = Arc::new(Inner {
            state: Mutex::new(state),
            path: Mutex::new(None),
            disabled: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            requested,
            completed,
            initialized,
            now: options.now,
        });
        let task_inner = inner.clone();
        let worker =
            tokio::spawn(
                async move { run(task_inner, receiver, options.directory, options.io).await },
            );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let timer_inner = inner.clone();
        let readiness = tokio::spawn(async move {
            let mut initialized = timer_inner.initialized.subscribe();
            let wait = async {
                while !*initialized.borrow_and_update() {
                    if initialized.changed().await.is_err() {
                        break;
                    }
                }
            };
            if tokio::time::timeout_at(deadline, wait).await.is_err() {
                timer_inner.disable();
            }
        });
        Self {
            inner,
            worker: tokio::sync::Mutex::new(Some(worker)),
            readiness: tokio::sync::Mutex::new(Some(readiness)),
            deadline,
        }
    }
    pub fn path(&self) -> Option<PathBuf> {
        self.inner
            .path
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub async fn ready(&self) -> Option<PathBuf> {
        let mut initialized = self.inner.initialized.subscribe();
        let wait = async {
            while !*initialized.borrow_and_update() {
                if initialized.changed().await.is_err() {
                    break;
                }
            }
        };
        if tokio::time::timeout_at(self.deadline, wait).await.is_err() {
            self.inner.disable();
        }
        self.path()
    }
    pub fn update(&self, event: &Value) {
        if self.inner.closing.load(Ordering::SeqCst) || self.inner.disabled.load(Ordering::SeqCst) {
            return;
        }
        // Serialize update with close's final state boundary; accepted events
        // are always in the final attempted snapshot.
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if self.inner.closing.load(Ordering::SeqCst)
                || self.inner.disabled.load(Ordering::SeqCst)
            {
                return;
            }
            state.update(event, (self.inner.now)());
            self.inner.schedule();
        }
    }
    pub async fn flush(&self) {
        if self.inner.closing.load(Ordering::SeqCst) {
            self.close().await;
            return;
        }
        if self.inner.disabled.load(Ordering::SeqCst) {
            return;
        }
        let target = {
            // Join the same acceptance boundary as update() and the worker's
            // quiescence check. No filesystem operation runs under this lock.
            let _state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            self.inner.schedule()
        };
        let mut completed = self.inner.completed.subscribe();
        let mut initialized = self.inner.initialized.subscribe();
        loop {
            if self.inner.disabled.load(Ordering::SeqCst)
                || *completed.borrow_and_update() >= target
            {
                return;
            }
            tokio::select! {result=completed.changed()=>if result.is_err(){return;},result=initialized.changed()=>if result.is_err(){return;}}
        }
    }
    pub async fn close(&self) {
        {
            let _state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            self.inner.closing.store(true, Ordering::SeqCst);
            self.inner.schedule();
        }
        let mut task = self.worker.lock().await;
        // Cancellation of a close waiter must retain each owned task for the
        // next close, including a worker still removing its private directory.
        if let Some(task) = task.as_mut() {
            let _ = task.await;
        }
        let _ = task.take();
        let mut readiness = self.readiness.lock().await;
        if let Some(timer) = readiness.as_mut() {
            let _ = timer.await;
        }
        let _ = readiness.take();
    }
}
impl Drop for StatusStore {
    fn drop(&mut self) {
        self.inner.closing.store(true, Ordering::SeqCst);
        self.inner.schedule();
    }
}
async fn run(
    inner: Arc<Inner>,
    mut requested: watch::Receiver<u64>,
    parent: PathBuf,
    io: Arc<dyn StatusIo>,
) {
    let directory = match io.create(parent).await {
        Ok(path) => path,
        Err(_) => {
            inner.disable();
            inner.initialized.send_replace(true);
            return;
        }
    };
    let initial_epoch = *requested.borrow_and_update();
    let initial_ok = if inner.disabled.load(Ordering::SeqCst) {
        false
    } else if let Some(bytes) = inner.serialized() {
        io.write(directory.clone(), bytes).await.is_ok()
    } else {
        false
    };
    let published = {
        let mut path = inner.path.lock().unwrap_or_else(|e| e.into_inner());
        if initial_ok && !inner.disabled.load(Ordering::SeqCst) {
            *path = Some(directory.join("state.json"));
            true
        } else {
            false
        }
    };
    if !published {
        inner.disable();
    }
    inner.initialized.send_replace(true);
    let mut last = initial_epoch;
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(5),
        Duration::from_secs(5),
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    while !inner.disabled.load(Ordering::SeqCst) {
        let epoch = *requested.borrow_and_update();
        if epoch > last {
            if let Some(bytes) = inner.serialized() {
                let _ = io.write(directory.clone(), bytes).await;
            }
            last = epoch;
            continue;
        }
        {
            // A flush owns the whole active drain, including updates accepted
            // during a write. Publish only once that drain is quiescent, and
            // serialize publication with update()/flush()/close() acceptance.
            let _state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if inner.generation.load(Ordering::SeqCst) != last {
                continue;
            }
            inner.completed.send_replace(last);
        }
        if inner.closing.load(Ordering::SeqCst) {
            break;
        }
        tokio::select! {result=requested.changed()=>if result.is_err(){break;},_=heartbeat.tick()=>{inner.schedule();}}
    }
    // Native I/O is never cancelled by timeout: all futures above completed.
    let _ = io.remove(directory).await;
    *inner.path.lock().unwrap_or_else(|e| e.into_inner()) = None;
    inner
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    inner.disabled.store(true, Ordering::SeqCst);
    inner.completed.send_replace(u64::MAX);
}
/// Status files and stdin share the existing 1 MiB bound. Reject nonregular
/// files before open, and recheck bytes to cover growth between stat and read.
pub async fn read_snapshot(path: &Path) -> Option<Value> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        const LIMIT: u64 = 1024 * 1024;
        let metadata = std::fs::metadata(&path).ok()?;
        if !metadata.is_file() || metadata.len() > LIMIT {
            return None;
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > LIMIT {
            return None;
        }
        let document = autorouter_core::js_json::JsDocument::parse(&bytes).ok()?;
        Some(document.to_serde_observation_lossy())
    })
    .await
    .ok()
    .flatten()
}
pub fn process_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    };
    matches!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
        Ok(()) | Err(nix::errno::Errno::EPERM)
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::{Notify, Semaphore};
    struct ControlledIo {
        gate: Semaphore,
        stall: AtomicBool,
        entered: AtomicUsize,
        entered_notice: Notify,
        written_notice: Notify,
        active: AtomicUsize,
        maximum: AtomicUsize,
        removals: AtomicUsize,
        fail_next: AtomicBool,
        native: bool,
        writes: Mutex<Vec<Value>>,
        removed: AtomicBool,
        created: Mutex<Option<PathBuf>>,
    }
    impl ControlledIo {
        fn new() -> Self {
            Self {
                gate: Semaphore::new(0),
                stall: AtomicBool::new(false),
                entered: AtomicUsize::new(0),
                entered_notice: Notify::new(),
                written_notice: Notify::new(),
                active: AtomicUsize::new(0),
                maximum: AtomicUsize::new(0),
                removals: AtomicUsize::new(0),
                fail_next: AtomicBool::new(false),
                native: false,
                writes: Mutex::new(Vec::new()),
                removed: AtomicBool::new(false),
                created: Mutex::new(None),
            }
        }
    }
    impl StatusIo for ControlledIo {
        fn create(&self, parent: PathBuf) -> IoFuture<'_, PathBuf> {
            Box::pin(async move {
                let path = if self.native {
                    NativeStatusIo.create(parent).await?
                } else {
                    parent.join("synthetic-status")
                };
                *self.created.lock().unwrap() = Some(path.clone());
                Ok(path)
            })
        }
        fn write(&self, directory: PathBuf, bytes: Vec<u8>) -> IoFuture<'_, ()> {
            Box::pin(async move {
                self.entered.fetch_add(1, Ordering::SeqCst);
                let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.maximum.fetch_max(active, Ordering::SeqCst);
                self.entered_notice.notify_waiters();
                if self.stall.load(Ordering::SeqCst) {
                    self.gate.acquire().await.unwrap().forget();
                }
                assert!(!self.removed.load(Ordering::SeqCst), "write after removal");
                if self.fail_next.swap(false, Ordering::SeqCst) {
                    self.active.fetch_sub(1, Ordering::SeqCst);
                    return Err(io::Error::other("synthetic storage failure"));
                }
                if self.native {
                    NativeStatusIo.write(directory, bytes.clone()).await?;
                }
                self.writes
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&bytes).unwrap());
                self.active.fetch_sub(1, Ordering::SeqCst);
                self.written_notice.notify_waiters();
                Ok(())
            })
        }
        fn remove(&self, directory: PathBuf) -> IoFuture<'_, ()> {
            Box::pin(async move {
                assert_eq!(
                    self.active.load(Ordering::SeqCst),
                    0,
                    "cleanup overlaps a write"
                );
                self.removals.fetch_add(1, Ordering::SeqCst);
                if self.native {
                    NativeStatusIo.remove(directory).await?;
                }
                self.removed.store(true, Ordering::SeqCst);
                Ok(())
            })
        }
    }
    async fn entered(io: &ControlledIo, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let notified = io.entered_notice.notified();
                if io.entered.load(Ordering::SeqCst) >= count {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("worker did not enter write");
    }
    async fn written(io: &ControlledIo, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let notified = io.written_notice.notified();
                if io.writes.lock().unwrap().len() >= count {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("worker did not complete write");
    }

    #[tokio::test(start_paused = true)]
    async fn flush_retains_shared_drain_until_updates_accepted_during_write_are_saved() {
        use std::future::Future;
        let io = Arc::new(ControlledIo::new());
        let store = Arc::new(StatusStore::create(StatusOptions {
            io: io.clone(),
            ..StatusOptions::default()
        }));
        assert!(store.ready().await.is_some());
        io.stall.store(true, Ordering::SeqCst);
        store.update(&json!({"event":"request_start","request_id":"r","session_id":"s"}));
        let flushed = store.flush();
        tokio::pin!(flushed);
        // Poll flush before yielding to the writer, so the blocked snapshot
        // owns the flush generation rather than an earlier update generation.
        tokio::select! {
            biased;
            () = &mut flushed => panic!("flush completed before its write"),
            () = entered(&io, 2) => {},
        }
        store.update(
            &json!({"event":"route","request_id":"r","session_id":"s","model":"claude-sonnet-5-5"}),
        );
        io.gate.add_permits(1);
        entered(&io, 3).await;
        let returned_before_latest_write =
            std::future::poll_fn(|cx| std::task::Poll::Ready(flushed.as_mut().poll(cx).is_ready()))
                .await;
        io.stall.store(false, Ordering::SeqCst);
        io.gate.add_permits(1);
        if !returned_before_latest_write {
            flushed.await;
        }
        written(&io, 3).await;
        let final_snapshot = io.writes.lock().unwrap().last().unwrap().clone();
        store.close().await;
        assert!(
            !returned_before_latest_write,
            "flush returned while the same drain still owned a blocked accepted update"
        );
        assert_eq!(
            final_snapshot["sessions"]["s"]["selected_model"],
            "claude-sonnet-5-5"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn multiple_flushers_share_one_drain_without_waiting_for_the_next_drain() {
        use std::future::Future;
        let io = Arc::new(ControlledIo::new());
        let store = StatusStore::create(StatusOptions {
            io: io.clone(),
            ..StatusOptions::default()
        });
        assert!(store.ready().await.is_some());
        io.stall.store(true, Ordering::SeqCst);
        store.update(&json!({"event":"request_start","request_id":"first","session_id":"s"}));
        let first = store.flush();
        let second = store.flush();
        tokio::pin!(first, second);
        assert!(
            !std::future::poll_fn(|cx| std::task::Poll::Ready(first.as_mut().poll(cx).is_ready()))
                .await
        );
        assert!(
            !std::future::poll_fn(|cx| std::task::Poll::Ready(second.as_mut().poll(cx).is_ready()))
                .await
        );
        entered(&io, 2).await;
        store.update(&json!({"event":"route","request_id":"first","session_id":"s","model":"claude-sonnet-5-5"}));
        io.gate.add_permits(1);
        entered(&io, 3).await;
        assert!(
            !std::future::poll_fn(|cx| std::task::Poll::Ready(first.as_mut().poll(cx).is_ready()))
                .await
        );
        assert!(
            !std::future::poll_fn(|cx| std::task::Poll::Ready(second.as_mut().poll(cx).is_ready()))
                .await
        );
        io.gate.add_permits(1);
        written(&io, 3).await;
        let mut completed = store.inner.completed.subscribe();
        while *completed.borrow_and_update() < 4 {
            completed.changed().await.unwrap();
        }
        // A later drain must not retroactively extend either completed flush.
        store.update(&json!({"event":"request_start","request_id":"next","session_id":"s"}));
        entered(&io, 4).await;
        tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(&mut first, &mut second);
        })
        .await
        .unwrap();
        assert_eq!(io.writes.lock().unwrap().len(), 3);
        let next = store.flush();
        tokio::pin!(next);
        assert!(
            !std::future::poll_fn(|cx| std::task::Poll::Ready(next.as_mut().poll(cx).is_ready()))
                .await
        );
        store.update(&json!({"event":"route","request_id":"next","session_id":"s","model":"claude-haiku-4-5"}));
        io.gate.add_permits(1);
        entered(&io, 5).await;
        assert!(
            !std::future::poll_fn(|cx| std::task::Poll::Ready(next.as_mut().poll(cx).is_ready()))
                .await
        );
        io.stall.store(false, Ordering::SeqCst);
        io.gate.add_permits(1);
        next.await;
        assert_eq!(io.writes.lock().unwrap().len(), 5);
        let final_snapshot = io.writes.lock().unwrap().last().unwrap().clone();
        assert_eq!(final_snapshot["sessions"]["s"]["request_id"], "next");
        assert_eq!(
            final_snapshot["sessions"]["s"]["selected_model"],
            "claude-haiku-4-5"
        );
        store.close().await;
        assert_eq!(io.maximum.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn flush_during_initialization_waits_for_updates_accepted_during_initial_write() {
        use std::future::Future;
        let io = Arc::new(ControlledIo::new());
        io.stall.store(true, Ordering::SeqCst);
        let store = StatusStore::create(StatusOptions {
            io: io.clone(),
            ..StatusOptions::default()
        });
        let flushed = store.flush();
        tokio::pin!(flushed);
        assert!(
            !std::future::poll_fn(|cx| std::task::Poll::Ready(
                flushed.as_mut().poll(cx).is_ready()
            ))
            .await
        );
        entered(&io, 1).await;
        store.update(
            &json!({"event":"request_start","request_id":"during-initialization","session_id":"s"}),
        );
        io.gate.add_permits(1);
        entered(&io, 2).await;
        assert!(
            !std::future::poll_fn(|cx| std::task::Poll::Ready(
                flushed.as_mut().poll(cx).is_ready()
            ))
            .await
        );
        io.stall.store(false, Ordering::SeqCst);
        io.gate.add_permits(1);
        flushed.await;
        assert!(store.ready().await.is_some());
        assert_eq!(io.writes.lock().unwrap().len(), 2);
        assert_eq!(
            io.writes.lock().unwrap()[1]["sessions"]["s"]["request_id"],
            "during-initialization"
        );
        store.close().await;
    }
    #[tokio::test(start_paused = true)]
    async fn coalesced_updates_and_idle_heartbeat_use_the_injected_clock() {
        let io = Arc::new(ControlledIo::new());
        let now = Arc::new(AtomicU64::new(1_000_000));
        let clock = now.clone();
        let store = StatusStore::create(StatusOptions {
            io: io.clone(),
            now: Arc::new(move || clock.load(Ordering::SeqCst)),
            ..StatusOptions::default()
        });
        assert!(store.ready().await.is_some());
        store.update(&json!({"event":"request_start","request_id":"r","session_id":"s"}));
        store.update(
            &json!({"event":"route","request_id":"r","session_id":"s","model":"claude-sonnet-5"}),
        );
        assert_eq!(
            io.writes.lock().unwrap().last().unwrap()["sessions"],
            json!({})
        );
        store.flush().await;
        assert_eq!(io.writes.lock().unwrap().len(), 2);
        assert_eq!(
            io.writes.lock().unwrap().last().unwrap()["sessions"]["s"]["phase"],
            "connecting"
        );
        now.store(1_005_000, Ordering::SeqCst);
        tokio::time::advance(Duration::from_secs(5)).await;
        entered(&io, 3).await;
        assert_eq!(
            io.writes.lock().unwrap().last().unwrap()["heartbeat_at"],
            1_005_000
        );
        store.close().await;
        assert!(io.removed.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_writer_coalesces_bursts_and_close_owns_accepted_work() {
        let io = Arc::new(ControlledIo::new());
        let store = Arc::new(StatusStore::create(StatusOptions {
            io: io.clone(),
            now: Arc::new(|| 100000),
            ..StatusOptions::default()
        }));
        assert!(store.ready().await.is_some());
        io.stall.store(true, Ordering::SeqCst);
        store.update(&json!({"event":"request_start","request_id":"r","session_id":"s"}));
        entered(&io, 2).await;
        assert_eq!(io.writes.lock().unwrap().len(), 1);
        assert_eq!(io.writes.lock().unwrap()[0]["sessions"], json!({}));
        for index in 0..2000 {
            store.update(&json!({"event":"route","request_id":"r","session_id":"s","model":"claude-sonnet-4-6","routing_latency_ms":index}));
        }
        assert_eq!(io.entered.load(Ordering::SeqCst), 2);
        let delivered = tokio::spawn(async {
            let mut chunks = Vec::new();
            for index in 0..10 {
                tokio::task::yield_now().await;
                chunks.push(format!("synthetic-chunk-{index}"));
            }
            chunks
        })
        .await
        .unwrap();
        assert_eq!(delivered.len(), 10);
        assert_eq!(io.entered.load(Ordering::SeqCst), 2);
        let closing = {
            let store = store.clone();
            tokio::spawn(async move { store.close().await })
        };
        tokio::task::yield_now().await;
        assert!(!closing.is_finished());
        assert!(!io.removed.load(Ordering::SeqCst));
        store.update(&json!({"event":"request_start","request_id":"too-late","session_id":"s"}));
        io.stall.store(false, Ordering::SeqCst);
        io.gate.add_permits(1);
        closing.await.unwrap();
        assert!(io.removed.load(Ordering::SeqCst));
        {
            let writes = io.writes.lock().unwrap();
            assert!(writes.len() <= 3);
            assert_eq!(
                writes.last().unwrap()["sessions"]["s"]["routing_latency_ms"],
                1999
            );
            assert_eq!(writes.last().unwrap()["sessions"]["s"]["request_id"], "r");
        }
        let count = io.entered.load(Ordering::SeqCst);
        tokio::join!(store.close(), store.flush());
        assert_eq!(io.entered.load(Ordering::SeqCst), count);
    }
    #[tokio::test(start_paused = true)]
    async fn readiness_deadline_disables_ui_but_waits_for_writer_before_cleanup() {
        let io = Arc::new(ControlledIo::new());
        io.stall.store(true, Ordering::SeqCst);
        let store = Arc::new(StatusStore::create(StatusOptions {
            io: io.clone(),
            ..StatusOptions::default()
        }));
        entered(&io, 1).await;
        assert!(store.ready().await.is_none());
        let closing = {
            let store = store.clone();
            tokio::spawn(async move { store.close().await })
        };
        tokio::task::yield_now().await;
        assert!(!closing.is_finished());
        assert!(!io.removed.load(Ordering::SeqCst));
        io.gate.add_permits(1);
        closing.await.unwrap();
        assert!(io.removed.load(Ordering::SeqCst));
        assert!(store.path().is_none());
    }
    #[tokio::test]
    async fn native_files_are_private_atomic_bounded_and_removed() {
        let parent =
            std::env::temp_dir().join(format!("autorouter-status-test-{}", random_name().unwrap()));
        std::fs::create_dir(&parent).unwrap();
        let store = StatusStore::create(StatusOptions {
            directory: parent.clone(),
            now: Arc::new(|| 100_000),
            ..StatusOptions::default()
        });
        let path = store.ready().await.unwrap();
        let initial: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(initial["version"], 1);
        assert_eq!(initial["pid"], std::process::id());
        assert_eq!(initial["heartbeat_at"], 100_000);
        assert_eq!(initial["sessions"], json!({}));
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        store.update(&json!({"event":"request_start","request_id":"r","session_id":"s","body":"synthetic-private-canary"}));
        store.flush().await;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let value = read_snapshot(&path).await.unwrap();
        assert_eq!(value["sessions"]["s"]["phase"], "routing");
        assert!(!value.to_string().contains("canary"));
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        assert!(process_alive(std::process::id()));
        assert!(!process_alive(0));
        store.close().await;
        assert!(!path.exists());
        store.update(&json!({"event":"request_start","request_id":"after-close"}));
        tokio::join!(store.close(), store.flush());
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 0);
        std::fs::remove_dir(parent).unwrap();
    }

    #[tokio::test]
    async fn blocked_native_snapshot_preserves_file_and_allows_complete_duplex_stream() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let parent = std::env::temp_dir().join(format!(
            "autorouter-status-stream-{}",
            random_name().unwrap()
        ));
        std::fs::create_dir(&parent).unwrap();
        let io = Arc::new(ControlledIo {
            native: true,
            ..ControlledIo::new()
        });
        let store = Arc::new(StatusStore::create(StatusOptions {
            directory: parent.clone(),
            io: io.clone(),
            now: Arc::new(|| 100_000),
            ..StatusOptions::default()
        }));
        let path = store.ready().await.unwrap();
        let before = std::fs::read(&path).unwrap();
        io.stall.store(true, Ordering::SeqCst);
        store.update(&json!({"event":"request_start","request_id":"r","session_id":"s"}));
        let flushed = {
            let store = store.clone();
            tokio::spawn(async move { store.flush().await })
        };
        entered(&io, 2).await;
        assert_eq!(std::fs::read(&path).unwrap(), before);
        for index in 0..2000 {
            let request = format!("burst-{index}");
            store.update(&json!({"event":"request_start","request_id":request,"session_id":"s"}));
            store.update(&json!({"event":"route","request_id":request,"session_id":"s","model":"claude-sonnet-5-5"}));
        }
        let (mut producer, mut consumer) = tokio::io::duplex(7);
        let delivered = tokio::spawn(async move {
            let mut bytes = Vec::new();
            consumer.read_to_end(&mut bytes).await.unwrap();
            bytes
        });
        let mut expected = Vec::new();
        for index in 0..10 {
            tokio::task::yield_now().await;
            let chunk = format!("chunk-{index}");
            producer.write_all(chunk.as_bytes()).await.unwrap();
            expected.extend_from_slice(chunk.as_bytes());
        }
        producer.shutdown().await.unwrap();
        assert_eq!(delivered.await.unwrap(), expected);
        assert!(!flushed.is_finished());
        assert_eq!(io.entered.load(Ordering::SeqCst), 2);
        assert_eq!(io.writes.lock().unwrap().len(), 1);
        assert_eq!(io.maximum.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        io.stall.store(false, Ordering::SeqCst);
        io.gate.add_permits(1);
        flushed.await.unwrap();
        entered(&io, 3).await;
        assert_eq!(io.writes.lock().unwrap().len(), 3);
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["sessions"]["s"]["request_id"], "burst-1999");
        assert_eq!(
            saved["sessions"]["s"]["selected_model"],
            "claude-sonnet-5-5"
        );
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        store.close().await;
        assert_eq!(io.maximum.load(Ordering::SeqCst), 1);
        assert_eq!(io.removals.load(Ordering::SeqCst), 1);
        std::fs::remove_dir(parent).unwrap();
    }

    #[tokio::test]
    async fn concurrent_close_callers_wait_for_one_native_drain_and_cannot_recreate_files() {
        let parent = std::env::temp_dir().join(format!(
            "autorouter-status-close-{}",
            random_name().unwrap()
        ));
        std::fs::create_dir(&parent).unwrap();
        let io = Arc::new(ControlledIo {
            native: true,
            ..ControlledIo::new()
        });
        let store = Arc::new(StatusStore::create(StatusOptions {
            directory: parent.clone(),
            io: io.clone(),
            ..StatusOptions::default()
        }));
        let path = store.ready().await.unwrap();
        io.stall.store(true, Ordering::SeqCst);
        store.update(&json!({"event":"request_start","request_id":"request-1","session_id":"s"}));
        let flushed = {
            let store = store.clone();
            tokio::spawn(async move { store.flush().await })
        };
        entered(&io, 2).await;
        store.update(&json!({"event":"route","request_id":"request-1","session_id":"s","model":"claude-opus-5-5"}));
        let first = {
            let store = store.clone();
            tokio::spawn(async move { store.close().await })
        };
        let second = {
            let store = store.clone();
            tokio::spawn(async move { store.close().await })
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while !store.inner.closing.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("close did not acquire the final state boundary");
        store.update(&json!({"event":"request_start","request_id":"too-late","session_id":"s"}));
        assert!(!first.is_finished());
        assert!(!second.is_finished());
        assert!(path.parent().unwrap().exists());
        io.stall.store(false, Ordering::SeqCst);
        io.gate.add_permits(1);
        first.await.unwrap();
        second.await.unwrap();
        flushed.await.unwrap();
        let saved = io.writes.lock().unwrap().last().unwrap().clone();
        assert_eq!(saved["sessions"]["s"]["selected_model"], "claude-opus-5-5");
        assert_eq!(saved["sessions"]["s"]["request_id"], "request-1");
        assert_eq!(io.maximum.load(Ordering::SeqCst), 1);
        assert_eq!(io.removals.load(Ordering::SeqCst), 1);
        assert!(!path.parent().unwrap().exists());
        let writes = io.entered.load(Ordering::SeqCst);
        store.flush().await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(10)).await;
        store.update(&json!({"event":"request_start","request_id":"late-again","session_id":"s"}));
        store.close().await;
        assert_eq!(io.entered.load(Ordering::SeqCst), writes);
        assert_eq!(io.removals.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 0);
        std::fs::remove_dir(parent).unwrap();
    }

    #[tokio::test]
    async fn stalled_initial_native_write_times_out_ignores_updates_and_leaves_empty_parent() {
        let parent = std::env::temp_dir().join(format!(
            "autorouter-status-ready-{}",
            random_name().unwrap()
        ));
        std::fs::create_dir(&parent).unwrap();
        let io = Arc::new(ControlledIo {
            native: true,
            ..ControlledIo::new()
        });
        io.stall.store(true, Ordering::SeqCst);
        let store = Arc::new(StatusStore::create(StatusOptions {
            directory: parent.clone(),
            io: io.clone(),
            ..StatusOptions::default()
        }));
        assert!(store.path().is_none());
        entered(&io, 1).await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(store.ready().await.is_none());
        store.update(
            &json!({"event":"request_start","request_id":"after-disabled","session_id":"s"}),
        );
        let closing = {
            let store = store.clone();
            tokio::spawn(async move { store.close().await })
        };
        tokio::task::yield_now().await;
        assert!(!closing.is_finished());
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 1);
        io.gate.add_permits(1);
        closing.await.unwrap();
        assert_eq!(io.entered.load(Ordering::SeqCst), 1);
        assert_eq!(io.writes.lock().unwrap().len(), 1);
        assert_eq!(io.writes.lock().unwrap()[0]["sessions"], json!({}));
        assert_eq!(io.removals.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 0);
        std::fs::remove_dir(parent).unwrap();
    }

    #[tokio::test]
    async fn unavailable_and_removed_storage_accepts_malformed_updates_without_side_effects() {
        let parent = std::env::temp_dir().join(format!(
            "autorouter-status-unavailable-{}",
            random_name().unwrap()
        ));
        std::fs::create_dir(&parent).unwrap();
        let not_directory = parent.join("file");
        std::fs::write(&not_directory, b"not a directory").unwrap();
        let unavailable = StatusStore::create(StatusOptions {
            directory: not_directory.clone(),
            ..StatusOptions::default()
        });
        assert!(unavailable.ready().await.is_none());
        unavailable.update(&json!({"event":"request_start","request_id":"r"}));
        unavailable.flush().await;
        unavailable.close().await;
        assert_eq!(std::fs::read(&not_directory).unwrap(), b"not a directory");
        let store = StatusStore::create(StatusOptions {
            directory: parent.clone(),
            ..StatusOptions::default()
        });
        let path = store.ready().await.unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        for event in [
            Value::Null,
            json!([]),
            json!({"event":{}}),
            json!({"event":"request_start","request_id":"r"}),
        ] {
            store.update(&event);
        }
        store.flush().await;
        store.close().await;
        assert!(store.path().is_none());
        assert!(!path.parent().unwrap().exists());
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[tokio::test]
    async fn failed_native_rename_removes_temporary_file_and_can_retry() {
        let parent = std::env::temp_dir().join(format!(
            "autorouter-status-rename-{}",
            random_name().unwrap()
        ));
        std::fs::create_dir(&parent).unwrap();
        let directory = NativeStatusIo.create(parent.clone()).await.unwrap();
        let path = directory.join("state.json");
        // A directory at the destination forces the actual rename operation
        // to fail after the temporary file has been opened and written.
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("preserved"), b"synthetic destination").unwrap();
        assert!(
            NativeStatusIo
                .write(directory.clone(), b"first".to_vec())
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        assert_eq!(
            std::fs::read(path.join("preserved")).unwrap(),
            b"synthetic destination"
        );
        std::fs::remove_dir_all(&path).unwrap();
        NativeStatusIo
            .write(directory.clone(), b"second".to_vec())
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        NativeStatusIo.remove(directory).await.unwrap();
        std::fs::remove_dir(parent).unwrap();
    }

    #[tokio::test]
    async fn failed_snapshot_keeps_previous_file_and_later_update_recovers() {
        let parent = std::env::temp_dir().join(format!(
            "autorouter-status-retry-{}",
            random_name().unwrap()
        ));
        std::fs::create_dir(&parent).unwrap();
        let io = Arc::new(ControlledIo {
            native: true,
            ..ControlledIo::new()
        });
        let store = StatusStore::create(StatusOptions {
            directory: parent.clone(),
            io: io.clone(),
            ..StatusOptions::default()
        });
        let path = store.ready().await.unwrap();
        let original = std::fs::read(&path).unwrap();
        io.fail_next.store(true, Ordering::SeqCst);
        store.update(&json!({"event":"request_start","request_id":"r","session_id":"s"}));
        store.flush().await;
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        store.update(
            &json!({"event":"route","request_id":"r","session_id":"s","model":"claude-sonnet-5-5"}),
        );
        store.flush().await;
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            saved["sessions"]["s"]["selected_model"],
            "claude-sonnet-5-5"
        );
        assert_eq!(io.maximum.load(Ordering::SeqCst), 1);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        store.close().await;
        std::fs::remove_dir(parent).unwrap();
    }

    #[tokio::test]
    async fn separate_instances_stay_isolated_under_the_requested_parent() {
        let parent = std::env::temp_dir().join(format!(
            "autorouter-status-isolation-{}",
            random_name().unwrap()
        ));
        std::fs::create_dir(&parent).unwrap();
        let first = StatusStore::create(StatusOptions {
            directory: parent.clone(),
            ..StatusOptions::default()
        });
        let second = StatusStore::create(StatusOptions {
            directory: parent.clone(),
            ..StatusOptions::default()
        });
        let (first_path, second_path) = tokio::join!(first.ready(), second.ready());
        let first_path = first_path.unwrap();
        let second_path = second_path.unwrap();
        assert_ne!(first_path, second_path);
        assert_eq!(first_path.parent().unwrap().parent().unwrap(), parent);
        assert_eq!(second_path.parent().unwrap().parent().unwrap(), parent);
        first.update(&json!({"event":"request_start","request_id":"r","session_id":"s"}));
        first.flush().await;
        assert_eq!(
            read_snapshot(&second_path).await.unwrap()["sessions"],
            json!({})
        );
        first.close().await;
        assert!(!first_path.exists());
        assert!(second_path.exists());
        second.close().await;
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 0);
        std::fs::remove_dir(parent).unwrap();
    }
}

#[cfg(test)]
#[path = "status_store_close_contracts.rs"]
mod close_contracts;
