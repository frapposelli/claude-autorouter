//! Opt-in, append-only session logs. Accepted normalized JSONL stays within a
//! 1 MiB queue and 128 session files. Routing never waits for writes; close
//! joins the writer and releases every descriptor, even after storage failure.
use autorouter_core::telemetry_event::normalize_session_record;
use nix::fcntl::OFlag;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
pub const MAX_PENDING_BYTES: usize = 1024 * 1024;
pub const MAX_SESSIONS: usize = 128;
pub const WARNING: &str = "AutoRouter session logging disabled.";
type IoFuture<'a, T> = Pin<Box<dyn Future<Output = io::Result<T>> + Send + 'a>>;
/// Injectable persistence boundary. Implementations own all I/O until their
/// returned futures resolve, allowing close to prove descriptor/write cleanup.
pub trait SessionSink: Send {
    fn initialize(&mut self, directory: PathBuf) -> IoFuture<'_, ()>;
    fn append(&mut self, name: String, line: Vec<u8>) -> IoFuture<'_, ()>;
    fn close(&mut self) -> IoFuture<'_, ()>;
}
#[derive(Default)]
pub struct NativeSessionSink {
    directory: PathBuf,
    identity: Option<(u64, u64)>,
    handles: HashMap<String, tokio::fs::File>,
}
impl NativeSessionSink {
    async fn check_directory(&self) -> io::Result<std::fs::Metadata> {
        let metadata = tokio::fs::symlink_metadata(&self.directory).await?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || self
                .identity
                .is_some_and(|id| id != (metadata.dev(), metadata.ino()))
        {
            return Err(io::Error::other("Invalid session log directory"));
        }
        Ok(metadata)
    }
}
impl SessionSink for NativeSessionSink {
    fn initialize(&mut self, directory: PathBuf) -> IoFuture<'_, ()> {
        Box::pin(async move {
            self.directory = directory;
            match self.check_directory().await {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            tokio::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.directory)
                .await?;
            let metadata = self.check_directory().await?;
            self.identity = Some((metadata.dev(), metadata.ino()));
            Ok(())
        })
    }
    fn append(&mut self, name: String, line: Vec<u8>) -> IoFuture<'_, ()> {
        Box::pin(async move {
            if !self.handles.contains_key(&name) {
                self.check_directory().await?;
                let file = tokio::fs::OpenOptions::new()
                    .append(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(OFlag::O_NOFOLLOW.bits())
                    .open(self.directory.join(&name))
                    .await?;
                // Own the descriptor before validation so every failure is closed.
                self.handles.insert(name.clone(), file);
                let file = self.handles.get(&name).unwrap();
                let metadata = file.metadata().await?;
                if !metadata.is_file() || metadata.nlink() != 1 {
                    return Err(io::Error::other("Invalid session log file"));
                }
                file.set_permissions(std::fs::Permissions::from_mode(0o600))
                    .await?;
                self.check_directory().await?;
            }
            let file = self.handles.get_mut(&name).unwrap();
            file.write_all(&line).await?;
            file.flush().await?;
            Ok(())
        })
    }
    fn close(&mut self) -> IoFuture<'_, ()> {
        Box::pin(async move {
            let mut result = Ok(());
            for (_, mut file) in self.handles.drain() {
                if let Err(error) = file.flush().await {
                    result = Err(error);
                }
                drop(file);
            }
            result
        })
    }
}
pub fn timestamp() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.millisecond()
    )
}
pub struct SessionLogOptions {
    pub include_prompts: bool,
    pub warn: Arc<dyn Fn(&str) + Send + Sync>,
    pub now: Arc<dyn Fn() -> String + Send + Sync>,
}
impl Default for SessionLogOptions {
    fn default() -> Self {
        Self {
            include_prompts: true,
            warn: Arc::new(|_| {}),
            now: Arc::new(timestamp),
        }
    }
}
struct Item {
    name: String,
    line: Vec<u8>,
}
struct Queue {
    accepting: bool,
    failed: bool,
    warned: bool,
    closing: bool,
    pending_bytes: usize,
    sessions: HashSet<String>,
    items: VecDeque<Item>,
}
struct Inner {
    queue: Mutex<Queue>,
    wake: Notify,
    options: SessionLogOptions,
    launch: String,
}
impl Inner {
    fn warn(&self) {
        let warn = {
            let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            if queue.warned {
                false
            } else {
                queue.warned = true;
                true
            }
        };
        if warn {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (self.options.warn)(WARNING)
            }));
        }
    }
    fn fail(&self) {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        queue.accepting = false;
        queue.failed = true;
        queue.items.clear();
        queue.pending_bytes = 0;
        drop(queue);
        self.warn();
    }
}
pub struct SessionLog {
    inner: Arc<Inner>,
    worker: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}
impl SessionLog {
    pub async fn create(directory: PathBuf, options: SessionLogOptions) -> Self {
        Self::with_sink(directory, options, Box::<NativeSessionSink>::default()).await
    }
    pub async fn with_sink(
        directory: PathBuf,
        options: SessionLogOptions,
        mut sink: Box<dyn SessionSink>,
    ) -> Self {
        let mut random = [0u8; 12];
        let random_ok = getrandom::fill(&mut random).is_ok();
        let launch = format!(
            "{}-{}",
            (options.now)().replace(['-', ':', '.'], ""),
            random
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        let inner = Arc::new(Inner {
            queue: Mutex::new(Queue {
                accepting: true,
                failed: false,
                warned: false,
                closing: false,
                pending_bytes: 0,
                sessions: HashSet::new(),
                items: VecDeque::new(),
            }),
            wake: Notify::new(),
            options,
            launch,
        });
        if !random_ok
            || autorouter_core::config::js_trim(&directory.to_string_lossy()).is_empty()
            || sink.initialize(directory).await.is_err()
        {
            inner.fail();
        }
        let task_inner = inner.clone();
        let worker = tokio::spawn(async move { run(task_inner, sink).await });
        Self {
            inner,
            worker: tokio::sync::Mutex::new(Some(worker)),
        }
    }
    pub fn record(&self, entry: &Value) -> bool {
        let Some(row) = normalize_session_record(
            entry,
            self.inner.options.include_prompts,
            &(self.inner.options.now)(),
        ) else {
            return false;
        };
        let session_key = row["session_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| format!("session:{s}"))
            .unwrap_or("anonymous".into());
        let Ok(mut line) = serde_json::to_vec(&row) else {
            return false;
        };
        line.push(b'\n');
        let bytes = line.len();
        let mut queue = self.inner.queue.lock().unwrap_or_else(|e| e.into_inner());
        if !queue.accepting {
            return false;
        };
        if queue.pending_bytes.saturating_add(bytes) > MAX_PENDING_BYTES
            || (!queue.sessions.contains(&session_key) && queue.sessions.len() >= MAX_SESSIONS)
        {
            queue.accepting = false;
            drop(queue);
            self.inner.warn();
            return false;
        }
        let name = format!(
            "autorouter-session-{}-{:x}.jsonl",
            self.inner.launch,
            Sha256::digest(session_key.as_bytes())
        );
        queue.sessions.insert(session_key);
        queue.items.push_back(Item { name, line });
        queue.pending_bytes += bytes;
        drop(queue);
        self.inner.wake.notify_one();
        true
    }
    pub async fn close(&self) {
        {
            let mut queue = self.inner.queue.lock().unwrap_or_else(|e| e.into_inner());
            queue.accepting = false;
            queue.closing = true;
        }
        self.inner.wake.notify_one();
        let mut worker = self.worker.lock().await;
        if let Some(worker) = worker.take() {
            let _ = worker.await;
        }
    }
}
impl Drop for SessionLog {
    fn drop(&mut self) {
        let mut queue = self.inner.queue.lock().unwrap_or_else(|e| e.into_inner());
        queue.accepting = false;
        queue.closing = true;
        drop(queue);
        self.inner.wake.notify_one();
    }
}
async fn run(inner: Arc<Inner>, mut sink: Box<dyn SessionSink>) {
    loop {
        let notified = inner.wake.notified();
        let (item, done) = {
            let mut queue = inner.queue.lock().unwrap_or_else(|e| e.into_inner());
            let item = if queue.failed {
                None
            } else {
                queue.items.pop_front()
            };
            let done = queue.failed || (queue.closing && item.is_none());
            (item, done)
        };
        if done {
            break;
        }
        if let Some(item) = item {
            let bytes = item.line.len();
            if sink.append(item.name, item.line).await.is_err() {
                inner.fail();
                break;
            }
            let mut queue = inner.queue.lock().unwrap_or_else(|e| e.into_inner());
            queue.pending_bytes -= bytes;
        } else {
            notified.await;
        }
    }
    if sink.close().await.is_err() {
        inner.warn();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::sync::Semaphore;
    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            let mut nonce = [0u8; 8];
            getrandom::fill(&mut nonce).unwrap();
            let path = std::env::temp_dir().join(format!(
                "autorouter-log-regression-{:x}",
                u64::from_le_bytes(nonce)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn decision(index: usize, session: &str) -> Value {
        json!({"event":"decision","request_id":format!("request-{index}"),"session_id":session,"requested_model":"claude-opus-5-5","selected_model":"claude-sonnet-4-6","prompt_excerpt":"synthetic task","body":"private-canary"})
    }
    struct State {
        gate: Semaphore,
        entered: AtomicBool,
        closed: AtomicBool,
        rows: Mutex<Vec<(String, Value)>>,
        failure: AtomicBool,
    }
    struct ControlledSink(Arc<State>);
    impl SessionSink for ControlledSink {
        fn initialize(&mut self, _: PathBuf) -> IoFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn append(&mut self, name: String, line: Vec<u8>) -> IoFuture<'_, ()> {
            Box::pin(async move {
                self.0.entered.store(true, Ordering::SeqCst);
                self.0.gate.acquire().await.unwrap().forget();
                assert!(!self.0.closed.load(Ordering::SeqCst));
                if self.0.failure.load(Ordering::SeqCst) {
                    return Err(io::Error::other("synthetic-private-error"));
                }
                self.0
                    .rows
                    .lock()
                    .unwrap()
                    .push((name, serde_json::from_slice(&line).unwrap()));
                Ok(())
            })
        }
        fn close(&mut self) -> IoFuture<'_, ()> {
            Box::pin(async move {
                self.0.closed.store(true, Ordering::SeqCst);
                Ok(())
            })
        }
    }
    fn state() -> Arc<State> {
        Arc::new(State {
            gate: Semaphore::new(0),
            entered: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            rows: Mutex::new(Vec::new()),
            failure: AtomicBool::new(false),
        })
    }

    #[tokio::test]
    async fn existing_and_dangling_directory_links_fail_open_without_touching_targets() {
        use std::os::unix::fs::symlink;
        let root = TestDirectory::new();
        let target = root.0.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("unchanged"), "synthetic-untouched").unwrap();
        let missing = root.0.join("missing");
        for (name, destination) in [("existing", &target), ("dangling", &missing)] {
            let path = root.0.join(name);
            symlink(destination, &path).unwrap();
            let warnings = Arc::new(Mutex::new(Vec::new()));
            let observed = warnings.clone();
            let log = SessionLog::create(
                path.clone(),
                SessionLogOptions {
                    warn: Arc::new(move |message| {
                        observed.lock().unwrap().push(message.to_owned());
                    }),
                    ..SessionLogOptions::default()
                },
            )
            .await;
            assert!(!log.record(&decision(0, "s")));
            log.close().await;
            assert_eq!(*warnings.lock().unwrap(), vec![WARNING]);
            assert!(
                std::fs::symlink_metadata(path)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 1);
        assert_eq!(
            std::fs::read_to_string(target.join("unchanged")).unwrap(),
            "synthetic-untouched"
        );
        assert!(!missing.exists());
    }

    #[tokio::test]
    async fn inserted_file_symlink_never_changes_target_contents_or_permissions() {
        use std::os::unix::fs::symlink;
        let root = TestDirectory::new();
        let target = root.0.join("synthetic-PRIVATE-target");
        std::fs::write(&target, "synthetic-untouched").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let observed = warnings.clone();
        let directory = root.0.join("logs");
        let log = SessionLog::create(
            directory.clone(),
            SessionLogOptions {
                warn: Arc::new(move |message| {
                    observed.lock().unwrap().push(message.to_owned());
                }),
                ..SessionLogOptions::default()
            },
        )
        .await;
        let inserted = directory.join(format!(
            "autorouter-session-{}-{:x}.jsonl",
            log.inner.launch,
            Sha256::digest(b"session:s")
        ));
        symlink(&target, &inserted).unwrap();
        assert!(log.record(&decision(0, "s")));
        log.close().await;
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "synthetic-untouched"
        );
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(*warnings.lock().unwrap(), vec![WARNING]);
        assert!(
            std::fs::symlink_metadata(inserted)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_dir(directory).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn separate_launches_never_append_to_each_other_or_change_existing_directory_mode() {
        let root = TestDirectory::new();
        let directory = root.0.join("logs");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (first, second) = tokio::join!(
            SessionLog::create(directory.clone(), SessionLogOptions::default()),
            SessionLog::create(directory.clone(), SessionLogOptions::default())
        );
        let mut entry = decision(0, "s");
        entry["prompt_excerpt"] = json!("First launch");
        assert!(first.record(&entry));
        entry["prompt_excerpt"] = json!("Second launch");
        assert!(second.record(&entry));
        tokio::join!(first.close(), first.close(), second.close());
        assert!(!first.record(&entry));
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let mut prompts = Vec::new();
        for file in std::fs::read_dir(&directory).unwrap() {
            let text = std::fs::read_to_string(file.unwrap().path()).unwrap();
            assert_eq!(text.lines().count(), 1);
            let row: Value = serde_json::from_str(text.trim()).unwrap();
            prompts.push(row["prompt_excerpt"].as_str().unwrap().to_owned());
        }
        prompts.sort();
        assert_eq!(prompts, vec!["First launch", "Second launch"]);
    }

    #[tokio::test]
    async fn close_owns_stalled_writes_and_later_records_are_rejected() {
        let state = state();
        let log = Arc::new(
            SessionLog::with_sink(
                PathBuf::from("/synthetic"),
                SessionLogOptions::default(),
                Box::new(ControlledSink(state.clone())),
            )
            .await,
        );
        for i in 0..3 {
            assert!(log.record(&decision(i, "s")));
        }
        while !state.entered.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        let close = {
            let log = log.clone();
            tokio::spawn(async move { log.close().await })
        };
        tokio::task::yield_now().await;
        assert!(!close.is_finished());
        assert!(!log.record(&decision(4, "s")));
        state.gate.add_permits(3);
        close.await.unwrap();
        assert!(state.closed.load(Ordering::SeqCst));
        let rows = state.rows.lock().unwrap();
        assert_eq!(rows.len(), 3);
        for (i, (_, row)) in rows.iter().enumerate() {
            assert_eq!(row["request_id"], format!("request-{i}"));
            assert!(!row.to_string().contains("canary"));
        }
    }
    #[tokio::test]
    async fn pending_bytes_and_sessions_are_bounded_without_losing_accepted_rows() {
        for session_limit in [false, true] {
            let state = state();
            let warnings = Arc::new(AtomicUsize::new(0));
            let observed = warnings.clone();
            let log = SessionLog::with_sink(
                PathBuf::from("/synthetic"),
                SessionLogOptions {
                    warn: Arc::new(move |message| {
                        assert_eq!(message, WARNING);
                        observed.fetch_add(1, Ordering::SeqCst);
                    }),
                    ..SessionLogOptions::default()
                },
                Box::new(ControlledSink(state.clone())),
            )
            .await;
            let mut accepted = 0;
            for i in 0..10000 {
                let session = if session_limit {
                    format!("s{i}")
                } else {
                    "s".into()
                };
                if log.record(&decision(i, &session)) {
                    accepted += 1;
                } else {
                    break;
                }
            }
            assert!(accepted > 0);
            if session_limit {
                assert_eq!(accepted, MAX_SESSIONS);
            }
            assert_eq!(warnings.load(Ordering::SeqCst), 1);
            assert!(!log.record(&decision(99999, "s")));
            state.gate.add_permits(accepted);
            log.close().await;
            assert_eq!(state.rows.lock().unwrap().len(), accepted);
            assert!(state.closed.load(Ordering::SeqCst));
        }
    }
    #[tokio::test]
    async fn storage_failure_warns_once_closes_and_does_not_echo_error() {
        let state = state();
        state.failure.store(true, Ordering::SeqCst);
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let sink = warnings.clone();
        let log = SessionLog::with_sink(
            PathBuf::from("/synthetic"),
            SessionLogOptions {
                warn: Arc::new(move |s| sink.lock().unwrap().push(s.to_owned())),
                ..SessionLogOptions::default()
            },
            Box::new(ControlledSink(state.clone())),
        )
        .await;
        assert!(log.record(&decision(0, "s")));
        state.gate.add_permits(1);
        log.close().await;
        assert_eq!(*warnings.lock().unwrap(), vec![WARNING]);
        assert!(state.closed.load(Ordering::SeqCst));
        assert!(!log.record(&decision(1, "s")));
    }
    #[tokio::test]
    async fn native_files_are_exclusive_private_and_metadata_only() {
        let directory = std::env::temp_dir().join(format!(
            "autorouter-log-test-{}",
            timestamp().replace([':', '.'], "")
        ));
        let log = SessionLog::create(
            directory.clone(),
            SessionLogOptions {
                include_prompts: false,
                ..SessionLogOptions::default()
            },
        )
        .await;
        for i in 0..120 {
            assert!(log.record(&decision(i, &format!("s{}", i % 3))));
        }
        log.close().await;
        let files = std::fs::read_dir(&directory)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for file in files {
            assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
            let bytes = std::fs::read_to_string(file.path()).unwrap();
            assert!(!bytes.contains("synthetic task"));
            assert!(!bytes.contains("canary"));
            assert_eq!(bytes.lines().count(), 40);
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn native_writer_rejects_links_replaced_directory_and_existing_file() {
        use std::os::unix::fs::symlink;
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let root = std::env::temp_dir().join(format!(
            "autorouter-log-safety-{:x}",
            u64::from_le_bytes(random)
        ));
        std::fs::create_dir(&root).unwrap();
        let target = root.join("unrelated");
        std::fs::create_dir(&target).unwrap();
        let link = root.join("linked");
        symlink(&target, &link).unwrap();
        let linked = SessionLog::create(link, SessionLogOptions::default()).await;
        assert!(!linked.record(&decision(0, "s")));
        linked.close().await;
        let directory = root.join("logs");
        let replaced = SessionLog::create(directory.clone(), SessionLogOptions::default()).await;
        std::fs::rename(&directory, root.join("moved")).unwrap();
        symlink(&target, &directory).unwrap();
        assert!(replaced.record(&decision(1, "s")));
        replaced.close().await;
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(root.join("moved")).unwrap().count(), 0);
        let directory = root.join("exclusive");
        let existing = SessionLog::create(directory.clone(), SessionLogOptions::default()).await;
        let path = directory.join(format!(
            "autorouter-session-{}-{:x}.jsonl",
            existing.inner.launch,
            Sha256::digest(b"session:s")
        ));
        std::fs::write(&path, "synthetic-existing-file").unwrap();
        assert!(existing.record(&decision(2, "s")));
        existing.close().await;
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "synthetic-existing-file"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
