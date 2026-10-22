#![no_main]

use autorouter_runtime::response_observer::{Observation, ResponseObserver};
use libfuzzer_sys::fuzz_target;
use std::sync::{Arc, Mutex};

fn observe(
    bytes: &[u8],
    content_type: &str,
    limit: usize,
    chunk: usize,
    abandon: bool,
) -> Vec<Observation> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&seen);
    let mut observer = ResponseObserver::new(content_type, limit, move |event| {
        captured.lock().unwrap().push(event);
    })
    .unwrap();
    for part in bytes.chunks(chunk.max(1)) {
        observer.push(part);
    }
    if abandon {
        observer.destroy();
        let before = seen.lock().unwrap().clone();
        observer.push(bytes);
        observer.finish();
        assert_eq!(*seen.lock().unwrap(), before);
    } else {
        observer.finish();
    }
    drop(observer);
    Arc::try_unwrap(seen).unwrap().into_inner().unwrap()
}

fuzz_target!(|data: &[u8]| {
    if data.len() > 65_536 || data.len() < 3 {
        return;
    }
    let content_type = [
        "text/event-stream",
        "application/json",
        "application/problem+json",
        "text/plain",
    ][usize::from(data[0]) % 4];
    let limit = 16 + usize::from(data[1]) * 256;
    let bytes = &data[3..];
    let whole = observe(bytes, content_type, limit, bytes.len(), false);
    let fragmented = observe(bytes, content_type, limit, 1 + usize::from(data[2]), false);
    assert_eq!(whole, fragmented);
    let _ = observe(bytes, content_type, limit, 1 + usize::from(data[2]), true);
});
