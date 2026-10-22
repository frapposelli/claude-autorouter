//! Direct public-field limits must retain frozen source bounds and admission
//! order. All paths, records and private markers in these tests are synthetic.
use autorouter_core::session_history::HistoryLimits;
use autorouter_runtime::session_history::{HistoryOptions, read_session_history};
use serde_json::{Value, json};
use std::future::Future;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

const INVALID: &str = "Invalid session-history read limit.";
const PRIVATE: &str = "SYNTHETIC_PRIVATE_HISTORY_MARKER";
const ID: &str = "autorouter-session-limits";
type Field = (&'static str, usize, fn(&mut HistoryLimits, usize));
// Exact src/session-history.mjs HISTORY_LIMITS from the unchanged baseline;
// these literals are independent of the production Default implementation.
const FIELDS: [Field; 7] = [
    ("maxFiles", 100, |limits, value| limits.max_files = value),
    ("maxDirectoryEntries", 10000, |limits, value| {
        limits.max_directory_entries = value
    }),
    ("maxFileBytes", 4 * 1024 * 1024, |limits, value| {
        limits.max_file_bytes = value
    }),
    ("maxTotalBytes", 16 * 1024 * 1024, |limits, value| {
        limits.max_total_bytes = value
    }),
    ("maxLineBytes", 16384, |limits, value| {
        limits.max_line_bytes = value
    }),
    ("maxRecords", 5000, |limits, value| {
        limits.max_records = value
    }),
    ("maxLines", 10000, |limits, value| limits.max_lines = value),
];

#[test]
fn direct_invalid_fields_reject_before_runtime_worker_path_or_id_admission() {
    assert!(tokio::runtime::Handle::try_current().is_err());
    for (field, ceiling, set) in FIELDS {
        for value in [0, ceiling + 1, usize::MAX] {
            for (directory, id) in [
                (PathBuf::from(format!("/{PRIVATE}")), None),
                (PathBuf::new(), Some(format!("../{PRIVATE}"))),
                (PathBuf::from(format!("/{PRIVATE}")), Some(ID.to_owned())),
            ] {
                let mut limits = HistoryLimits::default();
                set(&mut limits, value);
                let mut reader = Box::pin(read_session_history(
                    directory,
                    HistoryOptions { id, limits },
                ));
                let Poll::Ready(Err(error)) = reader
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                else {
                    panic!(
                        "{field}={value}: invalid public options must reject before any runtime worker"
                    );
                };
                assert_eq!(error, INVALID, "{field}={value}");
                assert!(!error.contains(PRIVATE));
                assert!(!error.contains(field));
            }
        }
    }
}

#[test]
fn default_lowered_and_decoded_limits_share_the_exact_source_ceiling() {
    let defaults = HistoryLimits::default();
    defaults.validate().unwrap();
    let default_json = serde_json::to_value(&defaults).unwrap();
    for (field, ceiling, set) in FIELDS {
        assert_eq!(default_json[field], ceiling);
        for accepted in [1, ceiling] {
            let mut direct = defaults.clone();
            set(&mut direct, accepted);
            direct.validate().unwrap();
            let decoded = HistoryLimits::from_overrides(&json!({field:accepted})).unwrap();
            assert_eq!(
                serde_json::to_value(direct).unwrap(),
                serde_json::to_value(decoded).unwrap()
            );
        }
        for rejected in [
            json!(0),
            json!(ceiling + 1),
            json!(-1),
            json!(1.5),
            json!(PRIVATE),
            Value::Null,
        ] {
            assert_eq!(
                HistoryLimits::from_overrides(&json!({field:rejected}))
                    .err()
                    .as_deref(),
                Some(INVALID)
            );
        }
    }
    assert_eq!(
        HistoryLimits::from_overrides(&json!({PRIVATE:1}))
            .err()
            .as_deref(),
        Some(INVALID)
    );
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let suffix = autorouter_runtime::random_hex::<12>().unwrap();
        let path = std::env::temp_dir().join(format!("autorouter-history-limits-{suffix}"));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
#[tokio::test]
async fn valid_direct_limits_remain_read_only_and_preserve_id_directory_order() {
    use std::io::Write;
    let fixture = Fixture::new();
    let log = fixture.0.join(format!("{ID}.jsonl"));
    let row = json!({"schema_version":2,"event":"decision","timestamp":"2026-10-13T12:00:00.000Z","request_id":"synthetic","requested_model":"claude-opus-5-5","selected_model":"claude-sonnet-5-5","body":PRIVATE});
    let original = format!("{row}\n{row}\n");
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&log)
        .unwrap();
    file.write_all(original.as_bytes()).unwrap();
    drop(file);
    for limits in [
        HistoryLimits::default(),
        HistoryLimits {
            max_records: 1,
            ..HistoryLimits::default()
        },
    ] {
        let expected_records = if limits.max_records == 1 { 1 } else { 2 };
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            read_session_history(
                fixture.0.clone(),
                HistoryOptions {
                    id: Some(ID.into()),
                    limits,
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            result["records"].as_array().unwrap().len(),
            expected_records
        );
        assert!(!result.to_string().contains(PRIVATE));
        assert_eq!(std::fs::read_to_string(&log).unwrap(), original);
    }
    let minimum = HistoryLimits {
        max_files: 1,
        max_directory_entries: 1,
        max_file_bytes: 1,
        max_total_bytes: 1,
        max_line_bytes: 1,
        max_records: 1,
        max_lines: 1,
    };
    let shown = read_session_history(
        fixture.0.clone(),
        HistoryOptions {
            id: Some(ID.into()),
            limits: minimum.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(shown["summary"]["coverage"]["bytes_read"], 1);
    assert_eq!(shown["summary"]["coverage"]["truncated"], true);
    assert_eq!(shown["limits"], serde_json::to_value(&minimum).unwrap());
    let missing = fixture.0.join("not-created");
    let result = read_session_history(
        missing.clone(),
        HistoryOptions {
            id: None,
            limits: minimum,
        },
    )
    .await
    .unwrap();
    assert_eq!(result["coverage"]["directory_missing"], true);
    assert!(!missing.exists());
    assert_eq!(std::fs::read_to_string(log).unwrap(), original);
    assert_eq!(
        read_session_history(
            PathBuf::new(),
            HistoryOptions {
                id: Some(format!("../{PRIVATE}")),
                ..HistoryOptions::default()
            }
        )
        .await
        .unwrap_err(),
        "Use an exact session ID from sessions list."
    );
    assert_eq!(
        read_session_history(PathBuf::new(), HistoryOptions::default())
            .await
            .unwrap_err(),
        "A regular session log directory is required."
    );
}

#[test]
fn dropping_an_unpolled_reader_does_not_start_filesystem_or_runtime_work() {
    assert!(tokio::runtime::Handle::try_current().is_err());
    drop(read_session_history(
        PathBuf::from(format!("/{PRIVATE}")),
        HistoryOptions::default(),
    ));
    assert!(tokio::runtime::Handle::try_current().is_err());
}
