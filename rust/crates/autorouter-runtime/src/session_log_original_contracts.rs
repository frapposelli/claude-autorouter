//! Exact original session-log fixtures through the real writer and native files.
//! JS getter/Promise/async-warning capabilities remain named API boundaries.
use super::super::*;
use super::{Directory, first_poll};
use serde_json::json;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::sync::Weak;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Semaphore;

const CORPUS: &str = include_str!("../../../parity/cases/session-log-contracts.jsonl");
const CAPTURE: &str = include_str!("../../../parity/cases/session-log-contracts.capture.json");
const GENERATED: &str = "2026-10-02T03:04:05.006Z";
const BOUND: Duration = Duration::from_secs(10);

thread_local! {
    static ASSERTIONS: std::cell::RefCell<std::collections::BTreeMap<(usize,usize),usize>> = const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
}
macro_rules! original {
    ($case:literal, $site:literal, $condition:expr) => {{
        ASSERTIONS.with(|counts| *counts.borrow_mut().entry(($case, $site)).or_default() += 1);
        assert!(
            $condition,
            "original session-log #{} assertion {}",
            $case, $site
        )
    }};
}
fn compact(value: &Value) -> Vec<Value> {
    let entries = value["rows"].as_array().expect("compact rows");
    assert!(entries.len() <= 5002);
    entries
        .iter()
        .map(|entry| {
            if let Some(value) = entry.get("value") {
                return value.clone();
            }
            let mut row = value["base"].clone();
            let row = row.as_object_mut().expect("compact object base");
            for key in entry["remove"].as_array().unwrap() {
                assert!(row.remove(key.as_str().unwrap()).is_some());
            }
            for (key, value) in entry["set"].as_object().unwrap() {
                row.insert(key.clone(), value.clone());
            }
            Value::Object(row.clone())
        })
        .collect()
}
fn case(number: usize) -> Value {
    ASSERTIONS.with(|counts| counts.borrow_mut().clear());
    assert!(CORPUS.len() <= 1024 * 1024 && CAPTURE.len() <= 1024 * 1024);
    assert_eq!(
        format!("{:x}", Sha256::digest(CORPUS)),
        "6a20c5285202fbd496d67f500a5b76405873d070aac5e0eba81f76f2132a4115"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(CAPTURE)),
        "dfca8e29eded9f51e4c22bce05369738b204b3f916bccdb53af53cc416a79218"
    );
    let id = format!("test/session-log.test.mjs#{number}");
    CORPUS
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|row| row["source_test"] == id)
        .unwrap()
}
/// Value has no getters, undefined or non-finite Number. These are explicit,
/// field-specific boundary adaptations, never fake calls to SessionLog::record.
fn input(value: &Value) -> Option<Value> {
    if value.get("selected_model").and_then(|v| v.get("$js")) == Some(&json!("accessor")) {
        return None;
    }
    fn field(value: &Value) -> Option<Value> {
        if let Some(tag) = value.get("$js").and_then(Value::as_str) {
            return match tag {
                "undefined" | "accessor" => None,
                "Infinity" => Some(Value::Null), // finite-Value projection; no Infinity API claim
                "Error" => field(&value["own"]), // unknown Error payload is still discarded by production
                other => panic!("unapproved JS representation {other}"),
            };
        }
        Some(match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .filter_map(|(k, v)| field(v).map(|v| (k.clone(), v)))
                    .collect(),
            ),
            Value::Array(rows) => Value::Array(
                rows.iter()
                    .map(|v| field(v).expect("array field"))
                    .collect(),
            ),
            _ => value.clone(),
        })
    }
    field(value)
}
fn inputs(row: &Value, writer: usize) -> Vec<Option<Value>> {
    compact(&row["writers"][writer]["inputs"])
        .iter()
        .map(input)
        .collect()
}
fn number_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| number_equal(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).is_some_and(|b| number_equal(v, b)))
        }
        _ => a == b,
    }
}
#[derive(Clone, Debug)]
struct FileRows {
    name: String,
    hash: String,
    bytes: usize,
    mode: u32,
    rows: Vec<Value>,
    text: String,
}
fn files(directory: &std::path::Path) -> Vec<FileRows> {
    let mut result = Vec::new();
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert!(metadata.is_file() && !metadata.file_type().is_symlink() && metadata.nlink() == 1);
        assert!(metadata.len() <= 2 * 1024 * 1024);
        use std::io::Read;
        let mut bytes = Vec::new();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(OFlag::O_NOFOLLOW.bits() | OFlag::O_NONBLOCK.bits())
            .open(&path)
            .unwrap();
        assert_eq!(
            (
                file.metadata().unwrap().dev(),
                file.metadata().unwrap().ino()
            ),
            (metadata.dev(), metadata.ino())
        );
        file.take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .unwrap();
        assert!(bytes.len() <= 2 * 1024 * 1024);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.is_empty() || text.ends_with('\n'));
        let rows = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect::<Vec<_>>();
        assert!(rows.len() <= 800);
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        let hash = name
            .strip_suffix(".jsonl")
            .unwrap()
            .rsplit('-')
            .next()
            .unwrap()
            .to_owned();
        assert_eq!(hash.len(), 64);
        assert!(hash.bytes().all(|c| c.is_ascii_hexdigit()));
        result.push(FileRows {
            name,
            hash,
            bytes: text.len(),
            mode: metadata.mode() & 0o777,
            rows,
            text,
        });
    }
    assert!(result.len() <= 128);
    result.sort_by(|a, b| (&a.hash, &a.text).cmp(&(&b.hash, &b.text)));
    result
}
fn row_projection(row: &Value, number: usize) -> Value {
    let mut row = row.clone();
    let actual = row
        .get("timestamp")
        .and_then(Value::as_str)
        .expect("actual timestamp present");
    if (number == 6) || (number == 17 && row["request_id"] == "bad-request") {
        assert_eq!(
            actual, GENERATED,
            "validate injected native clock before projection"
        );
        row["timestamp"] = json!("<generated-iso-timestamp>");
    } else {
        assert!(matches!(
            actual,
            "2026-10-01T12:00:00.000Z" | "2026-10-01T12:00:01.000Z"
        ));
    }
    row
}
fn outputs_match(actual: &[FileRows], expected: &Value, number: usize) -> bool {
    let expected = expected["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["kind"] == "session")
        .collect::<Vec<_>>();
    if actual.len() != expected.len() {
        return false;
    }
    let mut used = vec![false; expected.len()];
    for file in actual {
        let projected = file
            .rows
            .iter()
            .map(|r| row_projection(r, number))
            .collect::<Vec<_>>();
        let Some(index) = expected.iter().enumerate().position(|(i, e)| {
            !used[i]
                && e["session_hash"] == file.hash
                && e["mode"].as_u64() == Some(file.mode as u64)
                && number_equal(
                    &Value::Array(projected.clone()),
                    &Value::Array(compact(&e["rows"])),
                )
        }) else {
            return false;
        };
        used[index] = true;
    }
    true
}
type Producer = (Weak<ObservedLog>, Vec<Value>);
struct OpenedFile {
    fd: i32,
    actual: (u64, u64),
    descriptor_node: (u64, u64),
}
#[derive(Default)]
struct Seen {
    active: AtomicUsize,
    entered: AtomicUsize,
    written: AtomicUsize,
    closes: AtomicUsize,
    max_handles: AtomicUsize,
    opened: Mutex<Vec<OpenedFile>>,
    released: Mutex<Vec<bool>>,
    warnings: Mutex<Vec<String>>,
    changed: Notify,
    gate: Mutex<Option<Arc<Semaphore>>>,
    producer: Mutex<Option<Producer>>,
    produced: Mutex<Vec<bool>>,
    records: Mutex<Vec<bool>>,
}
impl Seen {
    async fn wait(&self, ready: impl Fn() -> bool) {
        tokio::time::timeout(BOUND, async {
            loop {
                let wake = self.changed.notified();
                if ready() {
                    break;
                }
                wake.await;
            }
        })
        .await
        .expect("positive owned-I/O barrier");
    }
}
struct Active(Arc<Seen>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        self.0.changed.notify_waiters();
    }
}
struct Release(Arc<Semaphore>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.add_permits(1000);
    }
}
struct ObserveSink {
    native: NativeSessionSink,
    seen: Arc<Seen>,
    post_open_failure: bool,
}
fn descriptor_identity(fd: i32) -> io::Result<Option<(u64, u64)>> {
    // This bounded fixture observes a kernel descriptor path without reopening
    // or changing access rights on the real append-only file. On macOS the
    // descriptor filesystem's device differs from the underlying file device.
    // Capture its own tuple while ownership is positive; do not conflate them.
    // Metadata does not open a potentially reused pipe or wait for a reader.
    match std::fs::metadata(format!("/dev/fd/{fd}")) {
        Ok(metadata) => Ok(Some((metadata.dev(), metadata.ino()))),
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                || error.raw_os_error() == Some(nix::libc::EBADF) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}
#[test]
fn safe_descriptor_probe_distinguishes_live_closed_and_reused_file_ownership() {
    let directory = Directory::new();
    let open = |name| {
        std::fs::OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.0.join(name))
            .unwrap()
    };
    let original = open("original");
    let original_metadata = original.metadata().unwrap();
    let original_identity = (original_metadata.dev(), original_metadata.ino());
    let mut slot: std::os::fd::OwnedFd = original.into();
    let number = slot.as_raw_fd();
    let flags =
        OFlag::from_bits_retain(nix::fcntl::fcntl(&slot, nix::fcntl::FcntlArg::F_GETFL).unwrap());
    assert_eq!(flags & OFlag::O_ACCMODE, OFlag::O_WRONLY);
    assert!(flags.contains(OFlag::O_APPEND));
    let original_observed = descriptor_identity(number).unwrap().unwrap();
    assert_eq!(original_observed.1, original_identity.1);

    let replacement = open("replacement");
    let replacement_metadata = replacement.metadata().unwrap();
    let replacement_identity = (replacement_metadata.dev(), replacement_metadata.ino());
    assert_ne!(original_identity, replacement_identity);
    // Safe dup2 replaces only our owned descriptor, at the exact same number.
    nix::unistd::dup2(&replacement, &mut slot).unwrap();
    assert_eq!(slot.as_raw_fd(), number);
    let replacement_observed = descriptor_identity(number).unwrap().unwrap();
    assert_eq!(replacement_observed.1, replacement_identity.1);
    assert_ne!(replacement_observed, original_observed);

    drop(slot);
    // Concurrent tests may reuse the number; they cannot own this private file.
    assert_ne!(
        descriptor_identity(number).unwrap(),
        Some(replacement_observed)
    );
}
impl SessionSink for ObserveSink {
    fn initialize(&mut self, directory: PathBuf) -> IoFuture<'_, ()> {
        self.native.initialize(directory)
    }
    fn append(&mut self, name: String, line: Vec<u8>) -> IoFuture<'_, ()> {
        Box::pin(async move {
            self.seen.active.fetch_add(1, Ordering::SeqCst);
            let _active = Active(self.seen.clone());
            let new = !self.native.handles.contains_key(&name);
            // Real create_new/no-follow/private-mode/directory-identity/file validation.
            // Empty append acquires the actual descriptor before the approved mocked
            // payload-write failure; this is not a kernel failure injection.
            self.native.append(name.clone(), Vec::new()).await?;
            if new {
                let file = &self.native.handles[&name];
                let m = file.metadata().await?;
                let observed = descriptor_identity(file.as_raw_fd())?
                    .expect("safe probe positively observes the real append-only native file");
                assert_eq!(
                    observed.1,
                    m.ino(),
                    "descriptor path names the actual file inode"
                );
                self.seen.opened.lock().unwrap().push(OpenedFile {
                    fd: file.as_raw_fd(),
                    actual: (m.dev(), m.ino()),
                    descriptor_node: observed,
                });
            }
            self.seen
                .max_handles
                .fetch_max(self.native.handles.len(), Ordering::SeqCst);
            self.seen.entered.fetch_add(1, Ordering::SeqCst);
            self.seen.changed.notify_waiters();
            if self.post_open_failure {
                assert_eq!(self.native.handles.len(), 1);
                return Err(io::Error::other("synthetic mocked payload write"));
            }
            let gate = self.seen.gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.acquire().await.unwrap().forget();
            }
            let produced = {
                let producer = self.seen.producer.lock().unwrap();
                producer.as_ref().and_then(|(weak, rows)| {
                    rows.get(self.seen.written.load(Ordering::SeqCst) + 1)
                        .map(|row| (weak.clone(), row.clone()))
                })
            };
            if let Some((weak, row)) = produced {
                let accepted = weak.upgrade().expect("live writer").record(&row);
                self.seen.produced.lock().unwrap().push(accepted);
                original!(15, 1, accepted);
            }
            self.native.append(name, line).await?;
            self.seen.written.fetch_add(1, Ordering::SeqCst);
            self.seen.changed.notify_waiters();
            Ok(())
        })
    }
    fn close(&mut self) -> IoFuture<'_, ()> {
        Box::pin(async move {
            self.seen.closes.fetch_add(1, Ordering::SeqCst);
            let result = self.native.close().await;
            assert!(self.native.handles.is_empty());
            // A descriptor number may already be reused by another concurrently
            // running test: compare identity, not merely the integer or EBADF.
            for opened in self.seen.opened.lock().unwrap().iter() {
                assert_eq!(opened.actual.1, opened.descriptor_node.1);
                let same = descriptor_identity(opened.fd)? == Some(opened.descriptor_node);
                self.seen.released.lock().unwrap().push(!same);
            }
            self.seen.changed.notify_waiters();
            result
        })
    }
}
struct ObservedLog {
    native: SessionLog,
    seen: Arc<Seen>,
}
impl std::ops::Deref for ObservedLog {
    type Target = SessionLog;
    fn deref(&self) -> &SessionLog {
        &self.native
    }
}
impl ObservedLog {
    fn record(&self, value: &Value) -> bool {
        let accepted = self.native.record(value);
        self.seen.records.lock().unwrap().push(accepted);
        accepted
    }
}
fn recorded(seen: &Seen, row: &Value, writer: usize) {
    let original = compact(&row["writers"][writer]["inputs"]);
    let expected = original
        .iter()
        .zip(row["writers"][writer]["records"].as_array().unwrap())
        .filter(|(input, _)| self::input(input).is_some())
        .map(|(_, result)| result.as_bool().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        *seen.records.lock().unwrap(),
        expected,
        "complete actual native admission transcript"
    );
    let id = row["source_test"].as_str().unwrap();
    let number = id.rsplit('#').next().unwrap().parse::<usize>().unwrap();
    let capture: Value = serde_json::from_str(CAPTURE).unwrap();
    let definition = capture["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == id)
        .unwrap();
    let mut static_count = 0usize;
    let mut executions = 0usize;
    ASSERTIONS.with(|counts| {
        let counts = counts.borrow();
        for (index, site) in definition["assertions"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            if number == 6 && matches!(index + 1, 3 | 4) {
                continue;
            }
            assert_eq!(
                counts.get(&(number, index + 1)).copied(),
                Some(site["expanded_executions"].as_u64().unwrap() as usize),
                "actual native assertion execution {number}/{}",
                index + 1
            );
            static_count += 1;
            executions += counts[&(number, index + 1)];
        }
        assert_eq!(counts.len(), static_count);
    });
    eprintln!(
        "SESSION_LOG_NATIVE {}",
        json!({"source_test":id,"writer":writer,"record_calls":seen.records.lock().unwrap().len(),"skipped_getter_calls":original.len()-expected.len(),"static_native_sites":static_count,"expanded_native_assertions":executions,
            "file_ownership":seen.opened.lock().unwrap().iter().map(|opened| json!({
                "fd":opened.fd,"actual_file":{"device":opened.actual.0,"inode":opened.actual.1},
                "descriptor_node":{"device":opened.descriptor_node.0,"inode":opened.descriptor_node.1}
            })).collect::<Vec<_>>()})
    );
}
async fn writer(
    directory: PathBuf,
    include_prompts: bool,
    seen: Arc<Seen>,
    fail: bool,
    panic_warning: bool,
) -> Arc<ObservedLog> {
    let warnings = seen.clone();
    let observed = seen.clone();
    Arc::new(ObservedLog {
        native: SessionLog::with_sink(
            directory,
            SessionLogOptions {
                include_prompts,
                now: Arc::new(|| GENERATED.into()),
                warn: Arc::new(move |message| {
                    warnings.warnings.lock().unwrap().push(message.into());
                    warnings.changed.notify_waiters();
                    assert!(!panic_warning, "synthetic warning panic");
                }),
            },
            Box::new(ObserveSink {
                native: NativeSessionSink::default(),
                seen,
                post_open_failure: fail,
            }),
        )
        .await,
        seen: observed,
    })
}
async fn close(writer: &SessionLog) {
    tokio::time::timeout(BOUND, writer.close())
        .await
        .expect("bounded native close");
    assert!(writer.worker.lock().await.is_none());
}
fn cleanup(seen: &Seen, handles: usize) {
    assert_eq!(seen.active.load(Ordering::SeqCst), 0);
    assert_eq!(seen.closes.load(Ordering::SeqCst), 1);
    assert_eq!(seen.opened.lock().unwrap().len(), handles);
    assert_eq!(*seen.released.lock().unwrap(), vec![true; handles]);
}
fn admission(
    writer: &ObservedLog,
    row: &Value,
    index: usize,
    value: &Option<Value>,
) -> Option<bool> {
    let value = value.as_ref()?;
    let accepted = writer.record(value);
    assert_eq!(
        accepted,
        row["writers"][0]["records"][index].as_bool().unwrap(),
        "actual native record {index}"
    );
    Some(accepted)
}
type OrdinaryRun = (Directory, Arc<ObservedLog>, Arc<Seen>, Value, Vec<FileRows>);
async fn ordinary(number: usize) -> OrdinaryRun {
    let row = case(number);
    let root = Directory::new();
    let seen = Arc::new(Seen::default());
    let writer = writer(
        root.0.join("logs"),
        row["writers"][0]["include_prompts"].as_bool().unwrap(),
        seen.clone(),
        false,
        false,
    )
    .await;
    for (index, value) in inputs(&row, 0).iter().enumerate() {
        admission(&writer, &row, index, value);
    }
    close(&writer).await;
    let actual = files(&root.0.join("logs"));
    assert!(
        outputs_match(&actual, &row, number),
        "all original durable rows #{}",
        number
    );
    cleanup(&seen, actual.len());
    (root, writer, seen, row, actual)
}

#[tokio::test(flavor = "current_thread")]
async fn original_01_concurrent_named_and_anonymous_sessions() {
    let row = case(1);
    let root = Directory::new();
    let seen = Arc::new(Seen::default());
    let writer = writer(root.0.join("logs"), true, seen.clone(), false, false).await;
    // All original JS Promise microtasks admit in index order. Independent
    // native futures run after the same explicit first yield, in join order.
    let rows = inputs(&row, 0);
    let mut calls = rows
        .iter()
        .map(|value| {
            let writer = &writer;
            Box::pin(async move {
                tokio::task::yield_now().await;
                original!(1, 1, writer.record(value.as_ref().unwrap()));
            })
        })
        .collect::<Vec<_>>();
    let mut done = vec![false; calls.len()];
    std::future::poll_fn(|cx| {
        for (index, call) in calls.iter_mut().enumerate() {
            if !done[index] && call.as_mut().poll(cx).is_ready() {
                done[index] = true;
            }
        }
        if done.iter().all(|ready| *ready) {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    close(&writer).await;
    let actual = files(&root.0.join("logs"));
    original!(1, 2, actual.len() == 3);
    original!(
        1,
        3,
        std::fs::metadata(root.0.join("logs")).unwrap().mode() & 0o777 == 0o700
    );
    for file in &actual {
        original!(
            1,
            4,
            file.name.starts_with("autorouter-session-")
                && file.name.ends_with(".jsonl")
                && file
                    .name
                    .trim_end_matches(".jsonl")
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        );
        original!(
            1,
            5,
            !file.name.contains("session-0") && !file.name.contains("session-1")
        );
        original!(1, 6, file.mode == 0o600);
        original!(
            1,
            7,
            file.rows
                .iter()
                .all(|r| r.get("session_id") == file.rows[0].get("session_id"))
        );
        original!(1, 8, file.rows.len() == 40);
        let indexes = file
            .rows
            .iter()
            .map(|r| {
                r["request_id"]
                    .as_str()
                    .unwrap()
                    .trim_start_matches("request-")
                    .parse::<usize>()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let mut sorted = indexes.clone();
        sorted.sort();
        original!(1, 9, indexes == sorted);
        original!(1, 10, file.rows.iter().all(|r| r["schema_version"] == 2));
    }
    original!(1, 11, seen.warnings.lock().unwrap().is_empty());
    assert!(outputs_match(&actual, &row, 1));
    cleanup(&seen, 3);
    recorded(&seen, &row, 0);
}
#[tokio::test(flavor = "current_thread")]
async fn original_02_distinct_launches_and_shared_completion() {
    let row = case(2);
    let root = Directory::new();
    let directory = root.0.join("logs");
    std::fs::DirBuilder::new()
        .mode(0o755)
        .create(&directory)
        .unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
    let a = Arc::new(Seen::default());
    let b = Arc::new(Seen::default());
    let (first, second) = tokio::join!(
        writer(directory.clone(), true, a.clone(), false, false),
        writer(directory.clone(), true, b.clone(), false, false)
    );
    assert!(first.record(inputs(&row, 0)[0].as_ref().unwrap()));
    assert!(second.record(inputs(&row, 1)[0].as_ref().unwrap()));
    let mut closing = Box::pin(first.close());
    assert!(!first_poll(closing.as_mut()).await);
    let mut repeated = Box::pin(first.close());
    // #2:1 JS Promise identity is adapted to two admitted Futures joining the
    // same single worker; construction alone does not admit Rust close.
    assert!(!first_poll(repeated.as_mut()).await);
    original!(2, 2, !first.record(inputs(&row, 0)[1].as_ref().unwrap()));
    tokio::time::timeout(BOUND, async {
        tokio::join!(closing, repeated, second.close());
    })
    .await
    .unwrap();
    original!(2, 1, a.closes.load(Ordering::SeqCst) == 1);
    original!(
        2,
        3,
        std::fs::metadata(&directory).unwrap().mode() & 0o777 == 0o755
    );
    let actual = files(&directory);
    original!(2, 4, actual.len() == 2);
    let mut prompts = actual
        .iter()
        .map(|f| f.rows[0]["prompt_excerpt"].as_str().unwrap())
        .collect::<Vec<_>>();
    prompts.sort();
    original!(2, 5, prompts == ["First launch", "Second launch"]);
    original!(2, 6, actual.iter().all(|f| f.rows.len() == 1));
    assert!(outputs_match(&actual, &row, 2));
    close(&first).await;
    close(&second).await;
    cleanup(&a, 1);
    cleanup(&b, 1);
    recorded(&a, &row, 0);
    recorded(&b, &row, 1);
}
#[tokio::test(flavor = "current_thread")]
async fn original_03_detached_private_metadata() {
    let row = case(3);
    let root = Directory::new();
    let seen = Arc::new(Seen::default());
    let writer = writer(root.0.join("logs"), true, seen.clone(), false, false).await;
    let mut entry = inputs(&row, 0)[0].take().unwrap();
    // Unknown throwing raw_response accessor has no Value representation. The
    // frozen JS callback remains the evidence for getter non-invocation.
    original!(3, 1, writer.record(&entry));
    entry["prompt_excerpt"] = json!("Changed after queuing");
    entry["selected_model"] = json!("changed");
    close(&writer).await;
    let actual = files(&root.0.join("logs"));
    original!(3, 2, !actual[0].text.contains("PRIVATE_HEADER_BODY_ERROR"));
    original!(3, 3, outputs_match(&actual, &row, 3));
    cleanup(&seen, 1);
    recorded(&seen, &row, 0);
}
#[tokio::test(flavor = "current_thread")]
async fn original_04_unicode_and_auxiliary_prompt_bounds() {
    let (_root, _writer, _seen, _row, actual) = ordinary(4).await;
    let file = &actual[0];
    original!(4, 1, !file.text.contains("PRIVATE_"));
    original!(
        4,
        2,
        file.rows[0]["prompt_excerpt"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            == 500
    );
    original!(4, 3, file.rows[0]["prompt_excerpt"] == "😀".repeat(500));
    original!(4, 4, file.rows[0]["prompt_truncated"] == true);
    original!(4, 5, file.rows[1]["prompt_truncated"] == true);
    original!(4, 6, file.rows[2]["prompt_truncated"] == false);
    original!(
        4,
        7,
        file.rows[3]["prompt_excerpt"] == "Included foreground text"
    );
    for row in &file.rows[4..] {
        original!(
            4,
            8,
            row["prompt_excerpt"] == "" && row["prompt_truncated"] == false
        );
    }
    recorded(&_seen, &_row, 0);
}
#[tokio::test(flavor = "current_thread")]
async fn original_05_unlimited_finite_latency() {
    let row = case(5);
    let root = Directory::new();
    let seen = Arc::new(Seen::default());
    let writer = writer(root.0.join("logs"), true, seen.clone(), false, false).await;
    original!(5, 1, writer.record(inputs(&row, 0)[0].as_ref().unwrap()));
    close(&writer).await;
    let actual = files(&root.0.join("logs"));
    original!(
        5,
        2,
        actual[0].rows[0]["decision_latency_ms"].as_f64() == Some(7200000.125)
    );
    assert!(outputs_match(&actual, &row, 5));
    cleanup(&seen, 1);
    recorded(&seen, &row, 0);
}
#[tokio::test(flavor = "current_thread")]
async fn original_06_invalid_ids_and_malformed_fields() {
    let row = case(6);
    let root = Directory::new();
    let seen = Arc::new(Seen::default());
    let writer = writer(root.0.join("logs"), true, seen.clone(), false, false).await;
    let values = inputs(&row, 0);
    for value in &values[..5] {
        original!(6, 1, !writer.record(value.as_ref().unwrap()));
    }
    for value in &values[5..12] {
        original!(6, 2, !writer.record(value.as_ref().unwrap()));
    }
    // #6:3 doesNotThrow and #6:4 nested false are JS-only getter boundaries.
    // No fabricated native call/result stands in for the unrepresentable getter.
    assert!(values[12].is_none());
    original!(6, 5, writer.record(values[13].as_ref().unwrap()));
    close(&writer).await;
    let actual = files(&root.0.join("logs"));
    original!(6, 6, std::fs::read_dir(&root.0).unwrap().count() == 1);
    original!(6, 7, actual[0].rows.len() == 1);
    for key in [
        "session_id",
        "agent_id",
        "prompt_id",
        "decision_latency_ms",
        "source",
        "reason",
        "classifier_error",
        "evaluator",
        "classified_tier",
    ] {
        original!(6, 8, actual[0].rows[0].get(key).is_none());
    }
    original!(6, 9, !actual[0].text.contains("PRIVATE"));
    original!(6, 10, actual[0].rows[0]["timestamp"] == GENERATED);
    assert!(outputs_match(&actual, &row, 6));
    cleanup(&seen, 1);
    recorded(&seen, &row, 0);
}
#[tokio::test(flavor = "current_thread")]
async fn original_07_escaped_utf8_queue_bound_and_sticky_overflow() {
    let row = case(7);
    let root = Directory::new();
    let seen = Arc::new(Seen::default());
    let writer = writer(root.0.join("logs"), true, seen.clone(), false, false).await;
    let values = inputs(&row, 0);
    let mut accepted = 0;
    for (i, value) in values[..values.len() - 1].iter().enumerate() {
        assert_eq!(value.as_ref().unwrap()["prompt_excerpt"], "\0".repeat(500));
        let result = admission(&writer, &row, i, value).unwrap();
        if !result {
            break;
        }
        accepted += 1;
    }
    original!(7, 1, accepted > 100 && accepted < 1000);
    original!(
        7,
        2,
        !writer.record(values.last().unwrap().as_ref().unwrap())
    );
    original!(7, 3, *seen.warnings.lock().unwrap() == [WARNING]);
    close(&writer).await;
    let actual = files(&root.0.join("logs"));
    original!(7, 4, actual[0].bytes <= MAX_PENDING_BYTES);
    original!(7, 5, actual[0].rows.len() == accepted);
    original!(
        7,
        6,
        actual[0]
            .rows
            .iter()
            .enumerate()
            .all(|(i, r)| r["request_id"] == format!("request-{i}"))
    );
    assert_eq!(accepted, 308);
    assert!(outputs_match(&actual, &row, 7));
    cleanup(&seen, 1);
    recorded(&seen, &row, 0);
}
#[tokio::test(flavor = "current_thread")]
async fn original_08_session_capacity_and_every_descriptor() {
    let row = case(8);
    let root = Directory::new();
    let seen = Arc::new(Seen::default());
    let writer = writer(root.0.join("logs"), true, seen.clone(), false, false).await;
    let values = inputs(&row, 0);
    for value in &values[..128] {
        original!(8, 1, writer.record(value.as_ref().unwrap()));
    }
    original!(8, 2, !writer.record(values[128].as_ref().unwrap()));
    original!(8, 3, !writer.record(values[129].as_ref().unwrap()));
    close(&writer).await;
    original!(8, 4, seen.opened.lock().unwrap().len() == 128);
    original!(
        8,
        5,
        seen.released.lock().unwrap().iter().all(|closed| *closed)
    );
    let actual = files(&root.0.join("logs"));
    original!(8, 6, actual.len() == 128);
    original!(8, 7, *seen.warnings.lock().unwrap() == [WARNING]);
    assert_eq!(seen.max_handles.load(Ordering::SeqCst), 128);
    assert!(outputs_match(&actual, &row, 8));
    cleanup(&seen, 128);
    recorded(&seen, &row, 0);
}
#[tokio::test(flavor = "current_thread")]
async fn original_11_replaced_directory_fails_before_open() {
    let row = case(11);
    let root = Directory::new();
    let seen = Arc::new(Seen::default());
    let path = root.0.join("logs");
    let writer = writer(path.clone(), true, seen.clone(), false, false).await;
    let target = root.0.join("replacement");
    std::fs::create_dir(&target).unwrap();
    std::fs::rename(&path, root.0.join("original")).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    original!(11, 1, writer.record(inputs(&row, 0)[0].as_ref().unwrap()));
    close(&writer).await;
    original!(11, 2, std::fs::read_dir(target).unwrap().count() == 0);
    original!(11, 3, *seen.warnings.lock().unwrap() == [WARNING]);
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    cleanup(&seen, 0);
    recorded(&seen, &row, 0);
}
#[tokio::test(flavor = "current_thread")]
async fn original_12_factory_and_sync_warning_failure_are_nonrejecting() {
    let row = case(12);
    let root = Directory::new();
    let path = root.0.join("PRIVATE_INVALID_PATH");
    std::fs::write(&path, "not a directory").unwrap();
    let seen = Arc::new(Seen::default());
    let writer = writer(path.clone(), true, seen.clone(), false, true).await;
    original!(12, 1, !writer.record(inputs(&row, 0)[0].as_ref().unwrap()));
    close(&writer).await;
    close(&writer).await;
    original!(12, 2, *seen.warnings.lock().unwrap() == [WARNING]);
    // #12:3 repeats the invalid-directory behavior through the sync callback
    // API; rejecting async warning callbacks are not expressible by Fn(&str).
    let second_seen = Arc::new(Seen::default());
    let second = super::original::writer(path, true, second_seen.clone(), false, false).await;
    original!(12, 3, !second.record(inputs(&row, 1)[0].as_ref().unwrap()));
    close(&second).await;
    assert_eq!(*second_seen.warnings.lock().unwrap(), [WARNING]);
    assert!(writer.worker.lock().await.is_none() && second.worker.lock().await.is_none());
    assert_eq!(seen.active.load(Ordering::SeqCst), 0);
    recorded(&seen, &row, 0);
    recorded(&second_seen, &row, 1);
}
#[tokio::test(flavor = "current_thread")]
async fn original_13_mocked_post_open_failure_closes_real_descriptor() {
    let row = case(13);
    let root = Directory::new();
    let seen = Arc::new(Seen::default());
    let writer = writer(root.0.join("logs"), true, seen.clone(), true, false).await;
    let values = inputs(&row, 0);
    original!(13, 1, writer.record(values[0].as_ref().unwrap()));
    seen.wait(|| !seen.warnings.lock().unwrap().is_empty())
        .await;
    original!(13, 2, !writer.record(values[1].as_ref().unwrap()));
    close(&writer).await;
    original!(13, 3, seen.opened.lock().unwrap().len() == 1);
    original!(13, 4, *seen.released.lock().unwrap() == [true]);
    original!(13, 5, *seen.warnings.lock().unwrap() == [WARNING]);
    assert_eq!(seen.max_handles.load(Ordering::SeqCst), 1);
    let actual = files(&root.0.join("logs"));
    assert_eq!(actual[0].bytes, 0);
    assert!(outputs_match(&actual, &row, 13));
    cleanup(&seen, 1);
    recorded(&seen, &row, 0);
}
#[tokio::test(flavor = "current_thread")]
async fn original_14_two_close_waiters_hold_owned_writes() {
    let row = case(14);
    let root = Directory::new();
    let seen = Arc::new(Seen::default());
    let gate = Arc::new(Semaphore::new(0));
    *seen.gate.lock().unwrap() = Some(gate.clone());
    let release = Release(gate);
    let writer = writer(root.0.join("logs"), true, seen.clone(), false, false).await;
    let values = inputs(&row, 0);
    for value in &values[..3] {
        original!(14, 1, writer.record(value.as_ref().unwrap()));
    }
    seen.wait(|| seen.entered.load(Ordering::SeqCst) == 1).await;
    let mut a = Box::pin(writer.close());
    let mut b = Box::pin(writer.close());
    let a_pending = !first_poll(a.as_mut()).await;
    let b_pending = !first_poll(b.as_mut()).await;
    let late_rejected = !writer.record(values[3].as_ref().unwrap());
    tokio::task::yield_now().await;
    let not_closed = seen.closes.load(Ordering::SeqCst) == 0;
    drop(release);
    tokio::time::timeout(BOUND, async {
        tokio::join!(a, b);
    })
    .await
    .unwrap();
    original!(14, 2, seen.closes.load(Ordering::SeqCst) == 1);
    original!(14, 3, late_rejected);
    original!(14, 4, a_pending && b_pending && not_closed);
    let actual = files(&root.0.join("logs"));
    original!(
        14,
        5,
        actual[0]
            .rows
            .iter()
            .map(|r| r["request_id"].as_str().unwrap())
            .collect::<Vec<_>>()
            == ["request-0", "request-1", "request-2"]
    );
    assert!(outputs_match(&actual, &row, 14));
    cleanup(&seen, 1);
    recorded(&seen, &row, 0);
}
#[tokio::test(flavor = "current_thread")]
async fn original_15_continuous_arrival_at_each_real_write() {
    let row = case(15);
    let root = Directory::new();
    let seen = Arc::new(Seen::default());
    let writer = writer(root.0.join("logs"), true, seen.clone(), false, false).await;
    let values = inputs(&row, 0)
        .into_iter()
        .map(Option::unwrap)
        .collect::<Vec<_>>();
    *seen.producer.lock().unwrap() = Some((Arc::downgrade(&writer), values.clone()));
    original!(15, 2, writer.record(&values[0]));
    seen.wait(|| seen.written.load(Ordering::SeqCst) == 800)
        .await;
    close(&writer).await;
    let produced = seen.produced.lock().unwrap();
    assert_eq!(produced.len(), 799);
    assert!(produced.iter().all(|accepted| *accepted));
    let actual = files(&root.0.join("logs"));
    original!(
        15,
        3,
        actual[0]
            .rows
            .iter()
            .enumerate()
            .all(|(i, r)| r["request_id"] == format!("request-{i}"))
            && actual[0].rows.len() == 800
    );
    original!(15, 4, seen.warnings.lock().unwrap().is_empty());
    assert!(outputs_match(&actual, &row, 15));
    cleanup(&seen, 1);
    recorded(&seen, &row, 0);
}
#[tokio::test(flavor = "current_thread")]
async fn original_16_metadata_mode_correlated_safe_outcome() {
    let row = case(16);
    let root = Directory::new();
    let seen = Arc::new(Seen::default());
    let writer = writer(root.0.join("logs"), false, seen.clone(), false, false).await;
    let values = inputs(&row, 0);
    original!(16, 1, writer.record(values[0].as_ref().unwrap()));
    original!(16, 2, writer.record(values[1].as_ref().unwrap()));
    close(&writer).await;
    let actual = files(&root.0.join("logs"));
    let file = &actual[0];
    original!(16, 3, file.rows.len() == 2);
    original!(
        16,
        4,
        file.rows.iter().all(|r| r["schema_version"] == 2
            && r.get("prompt_excerpt").is_none()
            && r.get("prompt_truncated").is_none())
    );
    original!(
        16,
        5,
        file.rows[0]["request_id"] == file.rows[1]["request_id"]
    );
    original!(
        16,
        6,
        file.rows[1]["confirmed_model"] == "claude-sonnet-5-5"
    );
    original!(16, 7, file.rows[1]["completion_confirmed"] == true);
    original!(
        16,
        8,
        number_equal(
            &file.rows[1]["usage"],
            &json!({"input_tokens":100,"output_tokens":20})
        )
    );
    original!(16, 9, !file.text.contains("PRIVATE_"));
    assert!(outputs_match(&actual, &row, 16));
    cleanup(&seen, 1);
    recorded(&seen, &row, 0);
}
#[tokio::test(flavor = "current_thread")]
async fn original_17_prerouting_absence_and_auxiliary_privacy() {
    let row = case(17);
    let root = Directory::new();
    let seen = Arc::new(Seen::default());
    let writer = writer(root.0.join("logs"), true, seen.clone(), false, false).await;
    let values = inputs(&row, 0);
    original!(17, 1, writer.record(values[0].as_ref().unwrap()));
    original!(17, 2, writer.record(values[1].as_ref().unwrap()));
    close(&writer).await;
    let actual = files(&root.0.join("logs"));
    let file = &actual[0];
    original!(17, 3, file.rows[0].get("selected_model").is_none());
    original!(17, 4, file.rows[0]["status"] == "error");
    original!(17, 5, file.rows[1]["prompt_excerpt"] == "");
    original!(17, 6, !file.text.contains("PRIVATE_"));
    assert!(outputs_match(&actual, &row, 17));
    cleanup(&seen, 1);
    recorded(&seen, &row, 0);
}

#[test]
fn corpus_pins_complete_original_counts_and_language_boundaries() {
    let capture: Value = serde_json::from_str(CAPTURE).unwrap();
    let rows = CORPUS
        .lines()
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 15);
    assert_eq!(capture["static_assertions"], 88);
    assert_eq!(capture["expanded_assertions"], 1168);
    assert_eq!(capture["record_count"], 1399);
    assert_eq!(capture["raw_row_count"], 1375);
    let calls = rows
        .iter()
        .flat_map(|r| r["writers"].as_array().unwrap())
        .map(|w| compact(&w["inputs"]).len())
        .sum::<usize>();
    assert_eq!(calls, 1399);
    let actual_native_inputs = rows
        .iter()
        .flat_map(|r| r["writers"].as_array().unwrap())
        .flat_map(|w| compact(&w["inputs"]))
        .filter_map(|v| input(&v))
        .count();
    assert_eq!(actual_native_inputs, 1398);
    let durable = rows
        .iter()
        .flat_map(|r| r["files"].as_array().unwrap())
        .filter(|f| f["kind"] == "session")
        .map(|f| compact(&f["rows"]).len())
        .sum::<usize>();
    assert_eq!(durable, 1375);
    case(1);
}
#[tokio::test(flavor = "current_thread")]
async fn durable_comparison_rejects_missing_reordered_forged_and_merged_outputs() {
    let (_root, _writer, _seen, row, actual) = ordinary(1).await;
    assert!(outputs_match(&actual, &row, 1));
    let mut changed = actual.clone();
    changed[0].rows.pop();
    assert!(!outputs_match(&changed, &row, 1));
    let mut changed = actual.clone();
    changed[0].rows.swap(0, 1);
    assert!(!outputs_match(&changed, &row, 1));
    let mut changed = actual.clone();
    changed[0].rows[0]["selected_model"] = json!("forged");
    assert!(!outputs_match(&changed, &row, 1));
    let mut changed = actual.clone();
    changed[0].hash = changed[1].hash.clone();
    assert!(!outputs_match(&changed, &row, 1));
    let mut changed = actual.clone();
    changed[0].rows[0]["unexpected"] = json!(null);
    assert!(!outputs_match(&changed, &row, 1));
    let mut changed = actual.clone();
    let latency = changed[0].rows[0]["decision_latency_ms"].as_f64().unwrap();
    changed[0].rows[0]["decision_latency_ms"] = json!(f64::from_bits(latency.to_bits() + 1));
    assert!(
        !outputs_match(&changed, &row, 1),
        "one f64 ULP is not tolerated"
    );
    let mut changed = actual.clone();
    changed[0].rows[0]
        .as_object_mut()
        .unwrap()
        .remove("request_id");
    assert!(!outputs_match(&changed, &row, 1));
    let mut changed = actual.clone();
    changed[0].mode = 0o644;
    assert!(!outputs_match(&changed, &row, 1));
}
