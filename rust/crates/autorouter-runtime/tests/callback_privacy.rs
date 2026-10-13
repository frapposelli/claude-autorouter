//! Actual process stderr proves hook ownership and private failure containment.
use autorouter_runtime::callback_privacy::{contain_callback, initialize_observer_privacy};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PRIVATE: &str = "synthetic-private-observer-payload-271828";
const PUBLIC: &str = "unrelated-host-hook-public-marker";

#[test]
fn callback_privacy_preserves_host_hook_on_other_threads() {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "callback_hook_child", "--ignored", "--nocapture"])
        .env("RUST_BACKTRACE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let read = |stream: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stream.take(65_537).read_to_end(&mut bytes).unwrap();
            bytes
        })
    };
    let stdout = read(Box::new(child.stdout.take().unwrap()));
    let stderr = read(Box::new(child.stderr.take().unwrap()));
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Privacy subprocess exceeded its deadline");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = stdout.join().unwrap();
    let stderr = stderr.join().unwrap();
    assert!(status.success());
    assert!(stdout.len() <= 65_536 && stderr.len() <= 65_536);
    let stdout = String::from_utf8(stdout).unwrap();
    let stderr = String::from_utf8(stderr).unwrap();
    assert!(!stdout.contains(PRIVATE));
    assert!(!stderr.contains(PRIVATE));
    assert_eq!(stderr.matches(PUBLIC).count(), 2);
}

#[test]
#[ignore = "explicit subprocess child; run by callback_privacy_preserves_host_hook_on_other_threads"]
fn callback_hook_child() {
    std::panic::set_hook(Box::new(|info| {
        if let Some(payload) = info.payload().downcast_ref::<&str>() {
            eprintln!("{payload}");
        } else if let Some(payload) = info.payload().downcast_ref::<String>() {
            eprintln!("{payload}");
        }
    }));
    initialize_observer_privacy();
    initialize_observer_privacy(); // Initialization must not discard the host hook.
    assert!(contain_callback(|| panic!("{PRIVATE}")).is_err());
    let sink: autorouter_runtime::server_events::EventSink =
        std::sync::Arc::new(|_| panic!("{PRIVATE}"));
    autorouter_runtime::server_events::emit(
        &Some(sink),
        serde_json::json!({"event":"request_start"}),
    );
    let mut observer = autorouter_runtime::response_observer::ResponseObserver::new(
        "text/event-stream",
        4096,
        |_| panic!("{PRIVATE}"),
    )
    .unwrap();
    observer.push(b"event: message_start\ndata: {\"message\":{\"model\":\"claude-sonnet-5\"}}\n\n");
    observer.finish();
    let called = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let recorded = called.clone();
    let lifecycle = autorouter_runtime::server_events::RequestEvents::new(
        serde_json::json!({"request_id":"synthetic-request","session_id":"synthetic-session"}),
        "claude-opus-4-8",
        autorouter_core::config::SessionLogMode::Metadata,
        autorouter_runtime::server_events::EventSinks {
            record: Some(std::sync::Arc::new(move |_| {
                recorded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                panic!("{PRIVATE}");
            })),
            ..Default::default()
        },
    );
    lifecycle.status("request_complete", serde_json::json!({}));
    assert_eq!(called.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        contain_callback(|| {
            assert!(contain_callback(|| panic!("{PRIVATE}")).is_err());
            7
        }),
        Ok(7)
    );
    let entered = std::sync::Arc::new(std::sync::Barrier::new(2));
    let released = entered.clone();
    let worker = std::thread::spawn(move || {
        contain_callback(|| {
            released.wait();
            released.wait();
            panic!("{PRIVATE}")
        })
        .unwrap_err();
    });
    entered.wait(); // The other thread is inside a protected callback now.
    assert!(std::panic::catch_unwind(|| panic!("{PUBLIC}")).is_err());
    entered.wait();
    worker.join().unwrap();
    // The callback's restoration must also preserve this same-thread host hook.
    assert!(std::panic::catch_unwind(|| panic!("{PUBLIC}")).is_err());
}
