//! Isolated real storage APIs. Wrappers own every injected wait and delegate
//! successful operations to the production backend; no product state is read.
use super::{SCENARIOS, normalize_snapshot, sha};
use autorouter_runtime::session_log::{
    NativeSessionSink, SessionLog, SessionLogOptions, SessionSink,
};
use autorouter_runtime::status_store::{NativeStatusIo, StatusIo, StatusOptions, StatusStore};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::future::{Future, poll_fn};
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::task::Poll;
use std::time::{Duration, Instant};
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;

type IoFuture<'a, T> = Pin<Box<dyn Future<Output = io::Result<T>> + Send + 'a>>;
fn need(ok: bool, stage: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(format!("Storage check failed: {stage}"))
    }
}
fn io_error() -> io::Error {
    io::Error::other("Synthetic storage failure")
}
const TIME: &str = "2026-10-01T12:00:00.000Z";

pub(super) fn decision(index: usize, session: Option<&str>) -> Value {
    let mut row = json!({"schema_version":2,"event":"decision","timestamp":TIME,"request_id":format!("request-{index}"),"request_class":"main","prompt_excerpt":"Fix a typo","prompt_truncated":false,"requested_model":"claude-haiku-4-5-20251001","selected_model":"claude-sonnet-5-5","decision_latency_ms":12.5,"source":"jev","reason":"low_confidence","evaluator":"jev","classified_tier":"haiku","body":"PRIVATE_BODY","headers":{"authorization":"PRIVATE_AUTH"},"error":"PRIVATE_ERROR"});
    if let Some(session) = session {
        row["session_id"] = json!(session);
    }
    row
}
fn outcome(index: usize, session: Option<&str>) -> Value {
    let mut row = json!({"schema_version":2,"event":"outcome","timestamp":TIME,"request_id":format!("request-{index}"),"status":"completed","http_status":200,"requested_model":"claude-haiku-4-5-20251001","selected_model":"claude-sonnet-5-5","confirmed_model":"claude-sonnet-5-5","completion_confirmed":true,"usage_complete":true,"usage":{"input_tokens":100,"output_tokens":20,"private":"PRIVATE_USAGE"},"baseline_model":"claude-opus-5-5","pricing_version":"2026-09-29.1","total_latency_ms":650,"body":"PRIVATE_BODY","prompt_excerpt":"PRIVATE_PROMPT"});
    if let Some(session) = session {
        row["session_id"] = json!(session);
    }
    row
}
pub(super) fn burst(store: &StatusStore, index: usize) {
    for (event, fields) in [
        ("request_start", json!({})),
        (
            "route",
            json!({"model":"claude-sonnet-5-5","source":"jev","evaluation_latency_ms":200}),
        ),
        ("upstream_response", json!({"status":200})),
        ("upstream_model", json!({"model":"claude-sonnet-5-5"})),
        (
            "upstream_usage",
            json!({"usage":{"input_tokens":1000,"output_tokens":100}}),
        ),
        ("request_complete", json!({})),
    ] {
        let mut value = json!({"event":event,"request_id":format!("r-{index}"),"session_id":format!("s-{}",index%20)});
        value
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        store.update(&value);
    }
}
struct State {
    held: AtomicBool,
    gate: Semaphore,
    entered: AtomicUsize,
    active: AtomicUsize,
    maximum: AtomicUsize,
    writes: AtomicUsize,
    closes: AtomicUsize,
    removals: AtomicUsize,
    fail_next: AtomicBool,
    fail_init: AtomicBool,
    notice: Notify,
    cancel: CancellationToken,
    delay: Duration,
    continuous: Mutex<Option<Weak<SessionLog>>>,
    warnings: AtomicUsize,
    warning_messages: Mutex<Vec<String>>,
    last: Mutex<Option<Vec<u8>>>,
    hold_marker: Mutex<Option<PathBuf>>,
}
impl State {
    fn new(cancel: CancellationToken, delay: u64) -> Arc<Self> {
        Arc::new(Self {
            held: AtomicBool::new(false),
            gate: Semaphore::new(0),
            entered: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            maximum: AtomicUsize::new(0),
            writes: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
            removals: AtomicUsize::new(0),
            fail_next: AtomicBool::new(false),
            fail_init: AtomicBool::new(false),
            notice: Notify::new(),
            cancel,
            delay: Duration::from_millis(delay),
            continuous: Mutex::new(None),
            warnings: AtomicUsize::new(0),
            warning_messages: Mutex::new(Vec::new()),
            last: Mutex::new(None),
            hold_marker: Mutex::new(None),
        })
    }
    fn release(&self) {
        let marker = self.hold_marker.lock().unwrap().take();
        if let Some(path) = marker {
            let _ = std::fs::remove_file(path);
        }
        self.held.store(false, Ordering::SeqCst);
        self.gate.add_permits(8);
    }
    async fn entered(&self, count: usize) -> Result<(), String> {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let notice = self.notice.notified();
                if self.entered.load(Ordering::SeqCst) >= count {
                    break;
                }
                notice.await;
            }
        })
        .await
        .map_err(|_| "Storage entry barrier timed out".to_owned())
    }
    async fn before(&self) -> io::Result<Active<'_>> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum.fetch_max(active, Ordering::SeqCst);
        let guard = Active(self);
        self.entered.fetch_add(1, Ordering::SeqCst);
        self.notice.notify_waiters();
        if self.held.load(Ordering::SeqCst) {
            let marker = self.hold_marker.lock().unwrap().clone();
            if let Some(path) = marker {
                use std::io::Write;
                use std::os::unix::fs::OpenOptionsExt;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(path)?;
                file.write_all(b"{\"stage\":\"initial_write_held\",\"active\":1}\n")?;
            }
            tokio::select! { permit=self.gate.acquire()=>{permit.map_err(|_|io_error())?.forget();},_=self.cancel.cancelled()=>return Err(io_error())}
        }
        if !self.delay.is_zero() {
            tokio::select! {_=tokio::time::sleep(self.delay)=>{},_=self.cancel.cancelled()=>return Err(io_error())}
        }
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(io_error());
        }
        Ok(guard)
    }
}
struct Active<'a>(&'a State);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}
struct StatusBackend(Arc<State>);
impl StatusIo for StatusBackend {
    fn create(&self, parent: PathBuf) -> IoFuture<'_, PathBuf> {
        Box::pin(async move {
            if self.0.fail_init.load(Ordering::SeqCst) {
                Err(io_error())
            } else {
                NativeStatusIo.create(parent).await
            }
        })
    }
    fn write(&self, directory: PathBuf, bytes: Vec<u8>) -> IoFuture<'_, ()> {
        Box::pin(async move {
            let _active = self.0.before().await?;
            NativeStatusIo.write(directory, bytes.clone()).await?;
            *self.0.last.lock().unwrap() = Some(bytes);
            self.0.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn remove(&self, directory: PathBuf) -> IoFuture<'_, ()> {
        Box::pin(async move {
            if self.0.active.load(Ordering::SeqCst) != 0 {
                return Err(io_error());
            }
            self.0.removals.fetch_add(1, Ordering::SeqCst);
            NativeStatusIo.remove(directory).await
        })
    }
}
struct LogBackend {
    state: Arc<State>,
    native: NativeSessionSink,
}
impl SessionSink for LogBackend {
    fn initialize(&mut self, directory: PathBuf) -> IoFuture<'_, ()> {
        Box::pin(async move {
            if self.state.fail_init.load(Ordering::SeqCst) {
                Err(io_error())
            } else {
                self.native.initialize(directory).await
            }
        })
    }
    fn append(&mut self, name: String, line: Vec<u8>) -> IoFuture<'_, ()> {
        Box::pin(async move {
            let _active = self.state.before().await?;
            let index = self.state.writes.load(Ordering::SeqCst);
            let writer = self
                .state
                .continuous
                .lock()
                .unwrap()
                .as_ref()
                .and_then(Weak::upgrade);
            if let Some(writer) = writer
                && index < 799
                && !writer.record(&decision(index + 1, Some("session-a")))
            {
                return Err(io_error());
            }
            self.native.append(name, line).await?;
            self.state.writes.fetch_add(1, Ordering::SeqCst);
            self.state.notice.notify_waiters();
            Ok(())
        })
    }
    fn close(&mut self) -> IoFuture<'_, ()> {
        Box::pin(async move {
            self.state.closes.fetch_add(1, Ordering::SeqCst);
            self.native.close().await
        })
    }
}
#[derive(Default)]
struct Owners {
    stores: Vec<Arc<StatusStore>>,
    logs: Vec<Arc<SessionLog>>,
    states: Vec<Arc<State>>,
    flush_tasks: tokio::task::JoinSet<f64>,
    progress_tasks: tokio::task::JoinSet<Vec<f64>>,
}
impl Owners {
    fn store(&mut self, root: &Path, state: Arc<State>, fixed_clock: bool) -> Arc<StatusStore> {
        let store = Arc::new(StatusStore::create(StatusOptions {
            directory: root.to_owned(),
            io: Arc::new(StatusBackend(state.clone())),
            now: if fixed_clock {
                Arc::new(|| 1_760_000_000_000)
            } else {
                StatusOptions::default().now
            },
            ..Default::default()
        }));
        self.stores.push(store.clone());
        self.states.push(state);
        store
    }
    async fn log(
        &mut self,
        root: &Path,
        state: Arc<State>,
        metadata: bool,
        panic_warn: bool,
    ) -> Arc<SessionLog> {
        let warnings = state.clone();
        let writer = Arc::new(
            SessionLog::with_sink(
                root.join("logs"),
                SessionLogOptions {
                    include_prompts: !metadata,
                    now: Arc::new(|| TIME.into()),
                    warn: Arc::new(move |message| {
                        warnings
                            .warning_messages
                            .lock()
                            .unwrap()
                            .push(message.to_owned());
                        warnings.warnings.fetch_add(1, Ordering::SeqCst);
                        warnings.notice.notify_waiters();
                        assert!(!panic_warn, "Synthetic warning callback failure");
                    }),
                },
                Box::new(LogBackend {
                    state: state.clone(),
                    native: NativeSessionSink::default(),
                }),
            )
            .await,
        );
        self.logs.push(writer.clone());
        self.states.push(state);
        writer
    }
    async fn close(&mut self) {
        for state in &self.states {
            state.release();
        }
        self.flush_tasks.abort_all();
        self.progress_tasks.abort_all();
        while self.flush_tasks.join_next().await.is_some() {}
        while self.progress_tasks.join_next().await.is_some() {}
        for store in &self.stores {
            store.close().await;
        }
        for log in &self.logs {
            log.close().await;
        }
    }
    fn clean(&self) -> bool {
        self.flush_tasks.is_empty()
            && self.progress_tasks.is_empty()
            && self
                .states
                .iter()
                .all(|state| state.active.load(Ordering::SeqCst) == 0)
    }
}
async fn first_poll<F: Future>(future: Pin<&mut F>) -> bool {
    let mut future = future;
    poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_ready())).await
}
async fn snapshot(store: &StatusStore, fixed_clock: bool) -> Result<Value, String> {
    let path = store.path().ok_or("Missing status path")?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| "Cannot read synthetic status")?;
    need(bytes.len() <= 1024 * 1024, "snapshot bound")?;
    let mut value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid synthetic status")?;
    normalize_snapshot(&mut value, fixed_clock)?;
    Ok(value)
}
fn modes(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    need(
        std::fs::metadata(path)
            .map_err(|_| "Missing private file")?
            .permissions()
            .mode()
            & 0o777
            == 0o600,
        "private file",
    )?;
    need(
        std::fs::metadata(path.parent().unwrap())
            .map_err(|_| "Missing private directory")?
            .permissions()
            .mode()
            & 0o777
            == 0o700,
        "private directory",
    )
}
fn log_files(root: &Path) -> Result<Value, String> {
    let mut files = BTreeMap::new();
    let path = root.join("logs");
    if !path.exists() {
        return Ok(json!(files));
    }
    let mut total = 0;
    let mut count = 0;
    for entry in std::fs::read_dir(path).map_err(|_| "Cannot read synthetic logs")? {
        let path = entry.map_err(|_| "Cannot inspect synthetic log")?.path();
        modes(&path)?;
        let bytes = super::read(&path, 2 * 1024 * 1024)?;
        total += bytes.len();
        need(total <= 2 * 1024 * 1024, "log byte bound")?;
        let mut rows = Vec::new();
        for line in bytes.split(|b| *b == b'\n').filter(|v| !v.is_empty()) {
            let mut row: Value =
                serde_json::from_slice(line).map_err(|_| "Invalid synthetic log row")?;
            need(row["timestamp"] == TIME, "original log timestamp")?;
            row["timestamp"] = json!("<timestamp>");
            rows.push(row);
            count += 1;
            need(count <= 3000, "log row bound")?;
        }
        let key = rows
            .first()
            .and_then(|r| r["session_id"].as_str())
            .unwrap_or("<anonymous>")
            .to_owned();
        need(!files.contains_key(&key), "unique session file")?;
        files.insert(key, rows);
    }
    Ok(json!(files))
}
async fn status_case(
    id: &str,
    root: &Path,
    cancel: CancellationToken,
    owners: &mut Owners,
) -> Result<Value, String> {
    let state = State::new(cancel, 0);
    if id == "status_create_failure" {
        state.fail_init.store(true, Ordering::SeqCst);
    }
    if id == "status_initial_failure" {
        state.fail_next.store(true, Ordering::SeqCst);
    }
    if id == "status_readiness_timeout" {
        *state.hold_marker.lock().unwrap() = Some(root.join("held-io.json"));
        state.held.store(true, Ordering::SeqCst);
    }
    let store = owners.store(root, state.clone(), true);
    let ready = store.ready().await;
    if [
        "status_create_failure",
        "status_initial_failure",
        "status_readiness_timeout",
    ]
    .contains(&id)
    {
        need(ready.is_none(), "failed readiness")?;
        burst(&store, 0);
        let closing = store.close();
        tokio::pin!(closing);
        let completed = first_poll(closing.as_mut()).await;
        if id == "status_readiness_timeout" {
            need(!completed, "close retains initial I/O")?;
        }
        state.release();
        if !completed {
            closing.await;
        }
        need(
            std::fs::read_dir(root)
                .map_err(|_| "Missing scratch")?
                .count()
                == 0,
            "failed status cleanup",
        )?;
        return Ok(
            json!({"ready":false,"late_update_ignored":true,"owned_initial_write":id=="status_readiness_timeout"}),
        );
    }
    modes(ready.as_ref().ok_or("Status readiness missing")?)?;
    let mut checks = json!({"ready":true,"private":true});
    if id == "status_normal" {
        for i in 0..60 {
            burst(&store, i);
            store.flush().await;
        }
        checks["snapshot"] = snapshot(&store, true).await?;
    } else if id == "status_held" || id == "status_concurrent_close" {
        state.held.store(true, Ordering::SeqCst);
        let before = state.entered.load(Ordering::SeqCst);
        burst(&store, 0);
        let first = store.flush();
        tokio::pin!(first);
        need(!first_poll(first.as_mut()).await, "held flush pending")?;
        state.entered(before + 1).await?;
        for i in 1..2000 {
            burst(&store, i);
        }
        let second = store.flush();
        tokio::pin!(second);
        need(!first_poll(second.as_mut()).await, "second flush pending")?;
        for _ in 0..4 {
            tokio::time::sleep(Duration::from_millis(1)).await;
            need(
                state.active.load(Ordering::SeqCst) == 1,
                "held writer ownership",
            )?;
        }
        need(!first_poll(first.as_mut()).await, "no premature flush")?;
        if id == "status_concurrent_close" {
            let one = store.close();
            let two = store.close();
            tokio::pin!(one, two);
            need(
                !first_poll(one.as_mut()).await && !first_poll(two.as_mut()).await,
                "admitted close waits",
            )?;
            burst(&store, 9999);
            state.release();
            tokio::join!(one, two, first, second);
            checks["late_update_ignored"] = json!(true);
            checks["close_waited"] = json!(true);
            let bytes = state
                .last
                .lock()
                .unwrap()
                .clone()
                .ok_or("Missing final accepted snapshot")?;
            let mut saved: Value =
                serde_json::from_slice(&bytes).map_err(|_| "Invalid final snapshot")?;
            normalize_snapshot(&mut saved, true)?;
            need(!saved.to_string().contains("r-9999"), "late status ignored")?;
            checks["snapshot"] = saved;
        } else {
            state.release();
            tokio::join!(first, second);
            checks["snapshot"] = snapshot(&store, true).await?;
        }
        need(state.maximum.load(Ordering::SeqCst) == 1, "one writer")?;
        checks["bursts"] = json!(2000);
        checks["progress_while_held"] = json!(4);
        checks["single_writer"] = json!(true);
    } else {
        burst(&store, 0);
        store.flush().await;
        let before = snapshot(&store, true).await?;
        let path = store.path().unwrap();
        if id == "status_write_recovery" {
            state.fail_next.store(true, Ordering::SeqCst);
        } else if id == "status_rename_recovery" {
            std::fs::remove_file(&path).map_err(|_| "Cannot stage rename failure")?;
            std::fs::create_dir(&path).map_err(|_| "Cannot stage rename failure")?;
        } else {
            return Err("Unknown status scenario".into());
        }
        burst(&store, 1);
        store.flush().await;
        if id == "status_write_recovery" {
            need(
                snapshot(&store, true).await? == before,
                "previous snapshot retained",
            )?;
        } else {
            need(
                std::fs::read_dir(path.parent().unwrap())
                    .map_err(|_| "Cannot inspect temporary files")?
                    .count()
                    == 1,
                "failed rename temporary cleanup",
            )?;
            std::fs::remove_dir(&path).map_err(|_| "Cannot restore rename destination")?;
        }
        burst(&store, 2);
        store.flush().await;
        checks["snapshot"] = snapshot(&store, true).await?;
        checks["recovered"] = json!(true);
    }
    store.close().await;
    need(store.path().is_none(), "closed status path")?;
    need(
        state.active.load(Ordering::SeqCst) == 0 && state.removals.load(Ordering::SeqCst) == 1,
        "status close cleanup",
    )?;
    need(
        std::fs::read_dir(root)
            .map_err(|_| "Cannot inspect status scratch")?
            .count()
            == 0,
        "status directory removed",
    )?;
    checks["removed"] = json!(true);
    Ok(checks)
}
async fn log_case(
    id: &str,
    root: &Path,
    cancel: CancellationToken,
    owners: &mut Owners,
) -> Result<Value, String> {
    let state = State::new(cancel, 0);
    let held = ["log_queue_bound", "log_session_bound", "log_close"].contains(&id);
    state.held.store(held, Ordering::SeqCst);
    if id == "log_init_warning_failure" {
        state.fail_init.store(true, Ordering::SeqCst);
    }
    if id == "log_append_failure" {
        state.fail_next.store(true, Ordering::SeqCst);
    }
    let writer = owners
        .log(
            root,
            state.clone(),
            id == "log_metadata",
            id == "log_init_warning_failure",
        )
        .await;
    let mut checks = json!({});
    let mut accepted = 0usize;
    match id {
        "log_normal" | "log_metadata" => {
            for i in 0..4 {
                let s = (i > 0).then(|| format!("session-{}", i % 2));
                need(
                    writer.record(&decision(i, s.as_deref())),
                    "decision accepted",
                )?;
                need(writer.record(&outcome(i, s.as_deref())), "outcome accepted")?;
                accepted += 2;
            }
        }
        "log_queue_bound" => {
            for i in 0..3000 {
                let mut row = decision(i, Some("session-a"));
                row["prompt_excerpt"] = json!(format!("{}😀", "\0".repeat(499)));
                if writer.record(&row) {
                    accepted += 1;
                } else {
                    break;
                }
            }
            need((100..=1000).contains(&accepted), "escaped pending byte cap")?;
            need(!writer.record(&decision(9999, None)), "overflow sticky")?;
            state.release();
        }
        "log_session_bound" => {
            for i in 0..129 {
                let ok = writer.record(&decision(i, Some(&format!("session-{i}"))));
                need(ok == (i < 128), "session cap")?;
                accepted += usize::from(ok);
            }
            state.release();
        }
        "log_continuous" => {
            *state.continuous.lock().unwrap() = Some(Arc::downgrade(&writer));
            need(
                writer.record(&decision(0, Some("session-a"))),
                "continuous first row",
            )?;
            tokio::time::timeout(Duration::from_secs(8), async {
                loop {
                    let notice = state.notice.notified();
                    if state.writes.load(Ordering::SeqCst) == 800 {
                        break;
                    }
                    notice.await;
                }
            })
            .await
            .map_err(|_| "Continuous arrival deadline")?;
            accepted = 800;
        }
        "log_append_failure" => {
            need(
                writer.record(&decision(0, Some("session-a"))),
                "initial failing append accepted",
            )?;
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let notice = state.notice.notified();
                    if state.warnings.load(Ordering::SeqCst) > 0 {
                        break;
                    }
                    notice.await;
                }
            })
            .await
            .map_err(|_| "Append failure warning deadline")?;
            need(
                state.warnings.load(Ordering::SeqCst) == 1,
                "append failure warning",
            )?;
            need(!writer.record(&decision(1, None)), "failed writer disabled")?;
            accepted = 1;
        }
        "log_close" => {
            for i in 0..3 {
                need(
                    writer.record(&decision(i, Some("session-a"))),
                    "close record",
                )?;
            }
            accepted = 3;
            state.entered(1).await?;
            let first = writer.close();
            let second = writer.close();
            tokio::pin!(first, second);
            need(
                !first_poll(first.as_mut()).await && !first_poll(second.as_mut()).await,
                "two admitted closes",
            )?;
            need(
                !writer.record(&decision(9999, None)),
                "late admission rejected",
            )?;
            state.release();
            tokio::join!(first, second);
            checks["close_waited"] = json!(true);
        }
        "log_init_warning_failure" => {
            need(
                !writer.record(&decision(0, None)),
                "initial failure rejects record",
            )?;
        }
        _ => return Err("Unknown log scenario".into()),
    }
    writer.close().await;
    writer.close().await;
    need(
        state.closes.load(Ordering::SeqCst) == 1 && state.active.load(Ordering::SeqCst) == 0,
        "one log close after owned append",
    )?;
    need(
        !writer.record(&decision(9999, None)),
        "closed record rejected",
    )?;
    let files = log_files(root)?;
    let rows = files
        .as_object()
        .unwrap()
        .values()
        .map(|rows| rows.as_array().unwrap().len())
        .sum::<usize>();
    need(
        rows == if id == "log_append_failure" {
            0
        } else {
            accepted
        },
        "all accepted rows drain unless write failed",
    )?;
    if id == "log_session_bound" {
        need(files.as_object().unwrap().len() == 128, "128 actual files")?;
    }
    if ["log_continuous", "log_close", "log_queue_bound"].contains(&id) {
        let rows = files["session-a"]
            .as_array()
            .ok_or("Missing ordered rows")?;
        for (i, row) in rows.iter().enumerate() {
            need(
                row["request_id"] == format!("request-{i}"),
                "ordered accepted rows",
            )?;
        }
    }
    let encoded = files.to_string();
    need(
        !encoded.contains("PRIVATE_BODY")
            && !encoded.contains("PRIVATE_AUTH")
            && !encoded.contains("PRIVATE_ERROR")
            && !encoded.contains("PRIVATE_USAGE"),
        "private log fields",
    )?;
    if id == "log_metadata" {
        need(
            !encoded.contains("prompt_excerpt")
                && !encoded.contains("prompt_truncated")
                && !encoded.contains("PRIVATE_PROMPT"),
            "metadata omits excerpts",
        )?;
    }
    checks["accepted"] = json!(accepted);
    checks["files"] = files;
    checks["warnings"] = json!(state.warnings.load(Ordering::SeqCst));
    checks["warning_messages"] = json!(*state.warning_messages.lock().unwrap());
    checks["close_calls"] = json!(1);
    checks["late_rejected"] = json!(true);
    Ok(checks)
}

fn retain_progress(
    rows: &mut Vec<f64>,
    value: f64,
    maximum: usize,
    cancel: &CancellationToken,
) -> bool {
    if rows.len() >= maximum {
        cancel.cancel();
        false
    } else {
        rows.push(value);
        true
    }
}
async fn measure(
    root: &Path,
    delay: u64,
    profile: &str,
    cancel: CancellationToken,
    owners: &mut Owners,
) -> Result<Value, String> {
    let (warmup, samples) = if profile == "paired" {
        (200, 2000)
    } else {
        (0, 60)
    };
    let state = State::new(cancel.clone(), delay);
    let started = Instant::now();
    let store = owners.store(root, state.clone(), false);
    need(store.ready().await.is_some(), "measurement readiness")?;
    let ready = started.elapsed().as_secs_f64() * 1000.;
    for i in 0..warmup {
        burst(&store, i);
        store.flush().await;
    }
    let progress_cancel = cancel.child_token();
    let progress_stop = progress_cancel.clone();
    let overflow_cancel = cancel.clone();
    owners.progress_tasks.spawn(async move {
        let mut rows = Vec::new();
        let mut previous = Instant::now();
        let mut timer = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_millis(5),
            Duration::from_millis(5),
        );
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {_=progress_stop.cancelled()=>break,_=timer.tick()=>{let now=Instant::now();if !retain_progress(&mut rows,(now.duration_since(previous).as_secs_f64()*1000.-5.).max(0.),10000,&overflow_cancel){break;}previous=now;}}
        }
        rows
    });
    let mut updates = Vec::with_capacity(samples);
    let mut admissions = Vec::with_capacity(samples);
    for i in 0..samples {
        if cancel.is_cancelled() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
        let start = Instant::now();
        burst(&store, i);
        updates.push(start.elapsed().as_secs_f64() * 1000.);
        let owned = store.clone();
        let mut flush = Box::pin(async move {
            owned.flush().await;
        });
        let start = Instant::now();
        let done = first_poll(flush.as_mut()).await;
        admissions.push(start.elapsed().as_secs_f64() * 1000.);
        owners.flush_tasks.spawn(async move {
            if !done {
                flush.await;
            }
            start.elapsed().as_secs_f64() * 1000.
        });
    }
    let mut completion = Vec::with_capacity(samples);
    while let Some(task) = owners.flush_tasks.join_next().await {
        completion.push(task.map_err(|_| "Measurement flush owner failed")?);
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
    progress_cancel.cancel();
    let progress = owners
        .progress_tasks
        .join_next()
        .await
        .ok_or("Missing measurement progress owner")?
        .map_err(|_| "Measurement progress owner failed")?;
    let final_state = snapshot(&store, false).await?;
    let start = Instant::now();
    store.close().await;
    let close = start.elapsed().as_secs_f64() * 1000.;
    need(updates.len() == samples, "measurement sample count")?;
    Ok(
        json!({"profile":profile,"warmup_bursts":warmup,"measured_bursts":samples,"injected_write_delay_ms":delay,"raw":{"update_ms":updates,"flush_admission_ms":admissions,"flush_completion_ms":completion,"timer_lateness_ms":progress,"ready_ms":[ready],"close_ms":[close]},"snapshot":final_state,"snapshot_writes":state.writes.load(Ordering::SeqCst),"acceptance_qualified":false,"allocations":null,"open_file_descriptors":null,"cpu":null,"peak_rss":null}),
    )
}
async fn owner_closed(cancel: CancellationToken) {
    use std::io::Read;
    use std::os::fd::AsFd;
    let Ok(fd) = std::io::stdin().as_fd().try_clone_to_owned() else {
        cancel.cancel();
        return;
    };
    let Ok(flags) = nix::fcntl::fcntl(&fd, nix::fcntl::FcntlArg::F_GETFL) else {
        cancel.cancel();
        return;
    };
    if nix::fcntl::fcntl(
        &fd,
        nix::fcntl::FcntlArg::F_SETFL(
            nix::fcntl::OFlag::from_bits_truncate(flags) | nix::fcntl::OFlag::O_NONBLOCK,
        ),
    )
    .is_err()
    {
        cancel.cancel();
        return;
    }
    let Ok(fd) = tokio::io::unix::AsyncFd::new(std::fs::File::from(fd)) else {
        cancel.cancel();
        return;
    };
    loop {
        let Ok(mut ready) = fd.readable().await else {
            break;
        };
        let mut byte = [0];
        match ready.try_io(|inner| {
            let mut file = inner.get_ref();
            file.read(&mut byte)
        }) {
            Ok(Ok(0)) | Ok(Err(_)) => break,
            Ok(Ok(_)) => break,
            Err(_) => {}
        }
    }
    cancel.cancel();
}
pub(super) fn run(args: &[String]) -> Result<(), String> {
    if args.len() != 4 {
        return Err("Invalid storage child arguments".into());
    }
    let id = &args[0];
    let root = Path::new(&args[1]);
    let mode = &args[2];
    let profile = &args[3];
    if (!SCENARIOS.contains(&id.as_str())
        && !["measure_normal", "measure_slow"].contains(&id.as_str()))
        || !["validate", "measure"].contains(&mode.as_str())
        || !["paired", "legacy-status-workload"].contains(&profile.as_str())
        || ((mode == "measure") != id.starts_with("measure_"))
    {
        return Err("Invalid storage child scenario".into());
    }
    need(
        std::fs::read_dir(root)
            .map_err(|_| "Invalid storage child scratch")?
            .count()
            == 0,
        "empty child scratch",
    )?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Cannot start storage child")?;
    let report=runtime.block_on(async {
        let signals=crate::tool_process::Signals::new()?;let cancel=signals.token.clone();let owner=tokio::spawn(owner_closed(cancel.clone()));let mut owners=Owners::default();
        let limit=if mode=="measure"{60}else{15};
        let result=tokio::time::timeout(Duration::from_secs(limit),async {tokio::select!{result=async{if mode=="measure"{measure(root,if id=="measure_slow"{20}else{0},profile,cancel.clone(),&mut owners).await}else if id.starts_with("status_"){status_case(id,root,cancel.clone(),&mut owners).await}else{log_case(id,root,cancel.clone(),&mut owners).await}}=>result,_=cancel.cancelled()=>Err("Storage child lost owner or was cancelled".into())}}).await.unwrap_or_else(|_|Err("Storage child scenario deadline".into()));
        let cancelled_before_cleanup=cancel.is_cancelled();cancel.cancel();let cleanup=tokio::time::timeout(Duration::from_secs(5),owners.close()).await.is_ok()&&owners.clean();owner.abort();let _=owner.await;
        let _=std::fs::remove_dir_all(root.join("logs"));let empty=std::fs::read_dir(root).is_ok_and(|mut rows|rows.next().is_none());
        Ok::<_, String>(json!({"schema_version":1,"kind":"storage_child","implementation":"native","scenario":id,"mode":mode,"protocol_sha256":sha(super::PROTOCOL.as_bytes()),"passed":result.is_ok()&&!cancelled_before_cleanup&&cleanup&&empty,"semantics":result.as_ref().ok(),"error":result.err(),"cleanup":{"active_io":owners.states.iter().map(|s|s.active.load(Ordering::SeqCst)).sum::<usize>(),"joined":cleanup,"scratch_empty":empty},"numerical_values_retained":mode=="measure"}))
    })?;
    let passed = report["passed"] == true;
    let bytes = super::encode(&report, super::CHILD_LIMIT)?;
    use std::io::Write;
    let mut output = std::io::stdout().lock();
    output
        .write_all(&bytes)
        .and_then(|_| output.write_all(b"\n"))
        .and_then(|_| output.flush())
        .map_err(|_| "Cannot emit bounded storage report")?;
    if passed {
        Ok(())
    } else {
        Err("Storage scenario failed; bounded report retained".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timer_sample_cap_cancels_instead_of_accepting_truncation() {
        let cancel = CancellationToken::new();
        let mut rows = Vec::new();
        assert!(retain_progress(&mut rows, 0., 1, &cancel));
        assert!(!retain_progress(&mut rows, 0., 1, &cancel));
        assert!(cancel.is_cancelled());
        assert_eq!(rows.len(), 1);
    }
    #[tokio::test]
    async fn cancelled_held_work_joins_retained_flush_and_progress_tasks() {
        let scratch = crate::tool_process::Scratch::new("storage-owned-work").unwrap();
        let cancel = CancellationToken::new();
        let state = State::new(cancel.clone(), 0);
        let mut owners = Owners::default();
        let store = owners.store(&scratch.0, state.clone(), true);
        assert!(store.ready().await.is_some());
        state.held.store(true, Ordering::SeqCst);
        burst(&store, 0);
        let owned = store.clone();
        let mut flush = Box::pin(async move {
            owned.flush().await;
        });
        assert!(!first_poll(flush.as_mut()).await);
        owners.flush_tasks.spawn(async move {
            flush.await;
            0.
        });
        owners.progress_tasks.spawn(std::future::pending());
        tokio::time::timeout(Duration::from_secs(2), state.entered(2))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.active.load(Ordering::SeqCst), 1);
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(3), owners.close())
            .await
            .unwrap();
        assert!(owners.clean());
        assert_eq!(std::fs::read_dir(&scratch.0).unwrap().count(), 0);
    }
}
