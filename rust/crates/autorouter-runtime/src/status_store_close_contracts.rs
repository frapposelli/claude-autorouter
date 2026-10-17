//! Deterministic ownership regressions; included only in status_store's tests.
use super::*;
use std::os::unix::fs::DirBuilderExt;
use std::sync::atomic::AtomicUsize;
use std::task::Poll;
use tokio::sync::{Notify, Semaphore};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "autorouter-status-close-{}",
            random_name().unwrap()
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
struct Removal {
    entered: AtomicUsize,
    active: AtomicUsize,
    completed: AtomicUsize,
    changed: Notify,
    gate: Semaphore,
}
impl Default for Removal {
    fn default() -> Self {
        Self {
            entered: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            completed: AtomicUsize::new(0),
            changed: Notify::new(),
            gate: Semaphore::new(0),
        }
    }
}
impl Removal {
    async fn wait_for(&self, counter: &AtomicUsize, value: usize) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if counter.load(Ordering::SeqCst) >= value {
                    return;
                }
                changed.await;
            }
        })
        .await
        .expect("positive owned removal barrier");
    }
}
struct ActiveRemoval(Arc<Removal>);
impl Drop for ActiveRemoval {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}
struct Release(Arc<Semaphore>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.add_permits(1);
    }
}
struct ReleaseRemoval(Arc<Removal>);
impl Drop for ReleaseRemoval {
    fn drop(&mut self) {
        self.0.gate.add_permits(1);
    }
}
struct HeldRemoveIo(Arc<Removal>);
impl StatusIo for HeldRemoveIo {
    fn create(&self, parent: PathBuf) -> IoFuture<'_, PathBuf> {
        NativeStatusIo.create(parent)
    }
    fn write(&self, directory: PathBuf, bytes: Vec<u8>) -> IoFuture<'_, ()> {
        NativeStatusIo.write(directory, bytes)
    }
    fn remove(&self, directory: PathBuf) -> IoFuture<'_, ()> {
        Box::pin(async move {
            self.0.active.fetch_add(1, Ordering::SeqCst);
            let _active = ActiveRemoval(self.0.clone());
            self.0.entered.fetch_add(1, Ordering::SeqCst);
            self.0.changed.notify_waiters();
            self.0.gate.acquire().await.unwrap().forget();
            let result = NativeStatusIo.remove(directory).await;
            self.0.completed.fetch_add(1, Ordering::SeqCst);
            self.0.changed.notify_waiters();
            result
        })
    }
}
async fn poll_close(future: Pin<&mut impl Future<Output = ()>>) -> bool {
    let mut future = future;
    std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_ready())).await
}
async fn initialized_timer_finished(store: &StatusStore) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if store
                .readiness
                .lock()
                .await
                .as_ref()
                .is_some_and(|task| task.is_finished())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test(flavor = "current_thread")]
async fn cancelled_first_close_retains_worker_until_real_directory_removal_finishes() {
    let root = Directory::new();
    let state = Arc::new(Removal::default());
    let release = ReleaseRemoval(state.clone());
    let store = StatusStore::create(StatusOptions {
        directory: root.0.clone(),
        io: Arc::new(HeldRemoveIo(state.clone())),
        now: Arc::new(|| 1_760_000_000_000),
        ..StatusOptions::default()
    });
    let path = store.ready().await.expect("real private snapshot");
    assert!(path.is_file());
    initialized_timer_finished(&store).await;
    let mut first = Box::pin(store.close());
    let first_ready = poll_close(first.as_mut()).await;
    state.wait_for(&state.entered, 1).await;
    let directory_present_while_held = path.parent().unwrap().is_dir();
    drop(first);
    let retained = store
        .worker
        .lock()
        .await
        .as_ref()
        .is_some_and(|task| !task.is_finished());
    let mut second = Box::pin(store.close());
    let second_ready = poll_close(second.as_mut()).await;
    let live_while_held = state.active.load(Ordering::SeqCst);
    let completed_while_held = state.completed.load(Ordering::SeqCst);
    // Drain before final assertions even on the expected broken implementation.
    drop(release);
    if !second_ready {
        tokio::time::timeout(Duration::from_secs(3), second)
            .await
            .unwrap();
    }
    state.wait_for(&state.completed, 1).await;
    store.close().await;
    assert!(!path.parent().unwrap().exists());
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
    assert_eq!(state.entered.load(Ordering::SeqCst), 1);
    assert_eq!(state.completed.load(Ordering::SeqCst), 1);
    assert_eq!(state.active.load(Ordering::SeqCst), 0);
    assert!(store.worker.lock().await.is_none());
    assert!(store.readiness.lock().await.is_none());
    assert!(!first_ready);
    assert!(directory_present_while_held);
    assert_eq!(live_while_held, 1);
    assert_eq!(completed_while_held, 0);
    assert!(
        !second_ready,
        "second close returned before actual held directory removal"
    );
    assert!(retained, "cancelled close detached the owned worker");
}
#[tokio::test(flavor = "current_thread")]
async fn cancelled_close_retains_readiness_join_owner_until_its_task_finishes() {
    let root = Directory::new();
    let store = StatusStore::create(StatusOptions {
        directory: root.0.clone(),
        ..StatusOptions::default()
    });
    store.ready().await.expect("real initialized store");
    // Finish real I/O and the real readiness timer before installing a held
    // test-only JoinHandle. This isolates readiness-handle cancellation without
    // asserting that the production one-second deadline has been extended.
    store.close().await;
    let gate = Arc::new(Semaphore::new(0));
    let release = Release(gate.clone());
    let entered = Arc::new(AtomicBool::new(false));
    let completed = Arc::new(AtomicBool::new(false));
    let begin = entered.clone();
    let end = completed.clone();
    let task = tokio::spawn(async move {
        begin.store(true, Ordering::SeqCst);
        gate.acquire().await.unwrap().forget();
        end.store(true, Ordering::SeqCst);
    });
    *store.readiness.lock().await = Some(task);
    tokio::time::timeout(Duration::from_secs(3), async {
        while !entered.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut first = Box::pin(store.close());
    let first_ready = poll_close(first.as_mut()).await;
    drop(first);
    let retained = store
        .readiness
        .lock()
        .await
        .as_ref()
        .is_some_and(|task| !task.is_finished());
    let mut second = Box::pin(store.close());
    let second_ready = poll_close(second.as_mut()).await;
    let completed_while_held = completed.load(Ordering::SeqCst);
    drop(release);
    if !second_ready {
        tokio::time::timeout(Duration::from_secs(3), second)
            .await
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        while !completed.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    store.close().await;
    assert!(store.readiness.lock().await.is_none());
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
    assert!(!first_ready);
    assert!(!completed_while_held);
    assert!(
        !second_ready,
        "second close returned before held readiness task completion"
    );
    assert!(retained, "cancelled close detached the readiness owner");
}
