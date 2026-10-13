//! Exact bounded payload admission for asynchronous event and record callbacks.
use crate::observer_tasks::{ObserverHandle, ObserverRejected};
use crate::server_events::{EventSink, RecordSink};
use autorouter_core::js_json::JsDocument;
use serde_json::Value;
use std::future::Future;
use std::io::{self, Write};
use std::sync::Arc;

struct Count {
    bytes: usize,
    limit: usize,
}
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let total = self
            .bytes
            .checked_add(bytes.len())
            .filter(|total| *total <= self.limit)
            .ok_or_else(|| io::Error::other("Observer payload exceeds its byte limit"))?;
        self.bytes = total;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn string_bytes(value: &str, limit: usize) -> Option<usize> {
    // Only a scalar is serialized here. Container traversal below is iterative,
    // so a deep custom record cannot overflow serde's recursive visitor stack.
    let mut count = Count { bytes: 0, limit };
    serde_json::to_writer(&mut count, value).ok()?;
    Some(count.bytes)
}
fn record_bytes(value: &Value, limit: usize) -> Option<usize> {
    enum Cursor<'a> {
        Value(&'a Value),
        Array(std::slice::Iter<'a, Value>, bool),
        Object(serde_json::map::Iter<'a>, bool),
    }
    let mut bytes = 0usize;
    let mut stack = vec![Cursor::Value(value)];
    while let Some(cursor) = stack.pop() {
        let remaining = limit.checked_sub(bytes)?;
        let extra = match cursor {
            Cursor::Value(value) => match value {
                Value::Null => 4,
                Value::Bool(value) => {
                    if *value {
                        4
                    } else {
                        5
                    }
                }
                Value::Number(value) => value.to_string().len(),
                Value::String(value) => string_bytes(value, remaining)?,
                Value::Array(values) => {
                    stack.push(Cursor::Array(values.iter(), false));
                    2
                }
                Value::Object(values) => {
                    stack.push(Cursor::Object(values.iter(), false));
                    2
                }
            },
            Cursor::Array(mut values, comma) => {
                if let Some(value) = values.next() {
                    stack.push(Cursor::Array(values, true));
                    stack.push(Cursor::Value(value));
                    usize::from(comma)
                } else {
                    0
                }
            }
            Cursor::Object(mut values, comma) => {
                if let Some((key, value)) = values.next() {
                    stack.push(Cursor::Object(values, true));
                    stack.push(Cursor::Value(value));
                    string_bytes(key, remaining)?.checked_add(1 + usize::from(comma))?
                } else {
                    0
                }
            }
        };
        bytes = bytes.checked_add(extra)?;
        if bytes > limit {
            return None;
        }
    }
    Some(bytes)
}
impl ObserverHandle {
    /// Adapt a returned future to the ordinary event callback boundary.
    /// Keep its ObserverTasks owner and explicitly close it during shutdown.
    pub fn event_sink<F, Fut>(&self, factory: F) -> EventSink
    where
        F: Fn(JsDocument) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), ObserverRejected>> + Send + 'static,
    {
        let handle = self.clone();
        let factory = Arc::new(factory);
        Arc::new(move |event| {
            let limit = handle.payload_limit();
            let bytes = event.serialized_len_up_to(limit).unwrap_or(limit + 1);
            let factory = factory.clone();
            let _ = handle.submit(bytes, move || factory(event));
        })
    }
    /// Normalized record callbacks keep their original values and omissions.
    /// Oversized records are declined before invoking the callback factory.
    pub fn record_sink<F, Fut>(&self, factory: F) -> RecordSink
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), ObserverRejected>> + Send + 'static,
    {
        let handle = self.clone();
        let factory = Arc::new(factory);
        Arc::new(move |record| {
            let limit = handle.payload_limit();
            let bytes = record_bytes(&record, limit).unwrap_or(limit + 1);
            let factory = factory.clone();
            let _ = handle.submit(bytes, move || factory(record));
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observer_tasks::{ObserverLimits, ObserverTasks};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn record_charge_matches_serializer_without_recursive_container_traversal() {
        for value in [
            serde_json::json!({"event":"decision","confidence":1.0,"model":"é😀","extra":"\n\u{0}"}),
            serde_json::json!([false, true, null, u64::MAX, 1e-300]),
        ] {
            let bytes = serde_json::to_vec(&value).unwrap().len();
            assert_eq!(record_bytes(&value, bytes), Some(bytes));
            assert_eq!(record_bytes(&value, bytes - 1), None);
        }
        let mut deep = Value::Null;
        for _ in 0..2000 {
            deep = Value::Array(vec![deep]);
        }
        assert_eq!(record_bytes(&deep, 4004), Some(4004));
        assert_eq!(record_bytes(&deep, 1024), None);
        // Avoid testing Value's unrelated recursive Drop at this depth.
        while let Value::Array(mut values) = deep {
            deep = values.pop().unwrap();
        }
    }

    #[tokio::test]
    async fn callbacks_start_synchronously_preserve_exact_values_and_do_not_wait_for_futures() {
        let owner = ObserverTasks::new(ObserverLimits::default()).unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let called = count.clone();
        let sink = owner.handle().event_sink(move |event| {
            assert_eq!(event.stringify(), r#"{"model":"\ud800","event":"route"}"#);
            called.fetch_add(1, Ordering::SeqCst);
            async { std::future::pending::<Result<(), ObserverRejected>>().await }
        });
        sink(JsDocument::parse(br#"{"model":"\ud800","event":"route"}"#).unwrap());
        assert_eq!(count.load(Ordering::SeqCst), 1);
        tokio::time::timeout(Duration::from_secs(2), owner.close())
            .await
            .unwrap();
        assert_eq!(owner.snapshot().pending, 0);
        assert!(owner.snapshot().worker_finished);
    }

    #[tokio::test]
    async fn rejected_records_and_oversized_events_never_change_the_producer() {
        let owner = ObserverTasks::new(ObserverLimits {
            max_payload_bytes: 128,
            ..ObserverLimits::default()
        })
        .unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let called = count.clone();
        let record = owner.handle().record_sink(move |value| {
            assert_eq!(
                value,
                serde_json::json!({"event":"outcome","completion_confirmed":false})
            );
            called.fetch_add(1, Ordering::SeqCst);
            async { Err(ObserverRejected) }
        });
        record(serde_json::json!({"event":"outcome","completion_confirmed":false}));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        record(serde_json::json!({"padding":"x".repeat(1024)}));
        let event = owner
            .handle()
            .event_sink(|_| async { panic!("oversized event factory must never run") });
        event(JsDocument::parse(format!("\"{}\"", "x".repeat(1024)).as_bytes()).unwrap());
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(owner.snapshot().declined_payload, 2);
        tokio::time::timeout(Duration::from_secs(2), async {
            while owner.snapshot().pending != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(owner.snapshot().rejected, 1);
        owner.close().await;
    }
}
