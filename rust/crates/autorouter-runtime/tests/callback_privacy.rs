//! Actual process stderr proves hook ownership and private failure containment.
use autorouter_runtime::callback_privacy::{contain_callback, initialize_observer_privacy};
#[path = "support/privacy_child.rs"]
mod privacy_child;

const PRIVATE: &str = "synthetic-private-observer-payload-271828";
const PUBLIC: &str = "unrelated-host-hook-public-marker";

#[test]
fn callback_privacy_preserves_host_hook_on_other_threads() {
    privacy_child::assert_private_child(
        "callback_hook_child",
        PRIVATE,
        PUBLIC,
        2,
        "Privacy subprocess exceeded its deadline",
    );
}

#[test]
#[ignore = "explicit subprocess child; run by callback_privacy_preserves_host_hook_on_other_threads"]
fn callback_hook_child() {
    privacy_child::initialize_privacy();
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
