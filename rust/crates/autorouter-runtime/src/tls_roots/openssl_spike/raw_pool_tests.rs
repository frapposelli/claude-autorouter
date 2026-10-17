//! Ownership regressions use bodies that forbid a convenient extra EOF poll.
use super::*;
use http_body_util::BodyExt;
use std::convert::Infallible;

struct LastData {
    ended: bool,
}
impl Body for LastData {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        assert!(!self.ended, "consumer must not need an extra EOF poll");
        self.ended = true;
        Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"x")))))
    }
    fn is_end_stream(&self) -> bool {
        self.ended
    }
}
fn completion(rows: &Arc<Mutex<Vec<bool>>>) -> Option<Completion> {
    let rows = rows.clone();
    Some(Box::new(move |clean| rows.lock().unwrap().push(clean)))
}
#[tokio::test]
async fn final_data_retires_without_further_none_poll() {
    let rows = Arc::new(Mutex::new(Vec::new()));
    let mut body = OwnedBody::new(LastData { ended: false }, completion(&rows), None);
    assert!(rows.lock().unwrap().is_empty());
    assert_eq!(
        body.frame().await.unwrap().unwrap().into_data().unwrap(),
        Bytes::from_static(b"x")
    );
    assert_eq!(*rows.lock().unwrap(), vec![true]);
    drop(body);
    assert_eq!(*rows.lock().unwrap(), vec![true], "one retirement only");
}
#[test]
fn header_only_handoff_retires_without_any_body_poll() {
    let rows = Arc::new(Mutex::new(Vec::new()));
    let body = OwnedBody::new(LastData { ended: true }, completion(&rows), None);
    assert!(body.is_end_stream());
    assert_eq!(*rows.lock().unwrap(), vec![true]);
    drop(body);
    assert_eq!(*rows.lock().unwrap(), vec![true]);
}
#[test]
fn held_decoder_eof_drop_is_not_consumer_completion() {
    let rows = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new(Gate(Mutex::new(GateState {
        held: true,
        waker: None,
    })));
    let body = OwnedBody::new(LastData { ended: true }, completion(&rows), Some(gate));
    assert!(!body.is_end_stream());
    assert!(rows.lock().unwrap().is_empty());
    drop(body);
    assert_eq!(*rows.lock().unwrap(), vec![false]);
}
#[test]
fn unconsumed_body_drop_is_not_consumer_completion() {
    let rows = Arc::new(Mutex::new(Vec::new()));
    drop(OwnedBody::new(
        LastData { ended: false },
        completion(&rows),
        None,
    ));
    assert_eq!(*rows.lock().unwrap(), vec![false]);
}
#[test]
fn joined_leading_decimal_hint_policy() {
    for (hint, expected) in [
        ("timeout=0", None),
        ("timeout=1", None),
        ("timeout=1.5", None),
        ("timeout=2", Some(1000)),
        ("timeout=2, max=7", Some(1000)),
        ("timeout=6", Some(5000)),
        ("Timeout=2", Some(5000)),
        ("max=7, timeout=2", Some(5000)),
        ("timeout=-2", Some(5000)),
    ] {
        let mut headers = hyper::HeaderMap::new();
        headers.insert("keep-alive", hint.parse().unwrap());
        assert_eq!(
            idle_timeout(&headers).map(|value| value.as_millis()),
            expected,
            "{hint}"
        );
    }
    let mut headers = hyper::HeaderMap::new();
    headers.append("keep-alive", "timeout=2".parse().unwrap());
    headers.append("keep-alive", "max=7".parse().unwrap());
    assert_eq!(idle_timeout(&headers), Some(Duration::from_secs(1)));
    headers.clear();
    headers.insert(
        "keep-alive",
        format!("timeout={}", "9".repeat(100)).parse().unwrap(),
    );
    assert_eq!(idle_timeout(&headers), Some(Duration::from_secs(5)));
}

#[test]
fn empty_first_keep_alive_field_preserves_join_delimiter() {
    let mut headers = hyper::HeaderMap::new();
    headers.append("keep-alive", "".parse().unwrap());
    headers.append("keep-alive", "timeout=2".parse().unwrap());
    assert_eq!(idle_timeout(&headers), Some(Duration::from_secs(5)));
}

#[test]
fn node_url_pool_identity_normalizes_host_and_effective_port() {
    for (a, b) in [
        ("http://LOCALHOST/path", "http://localhost:80/other"),
        ("https://LOCALHOST/path", "https://localhost:00443/other"),
        ("http://LOCALHOST:01234/path", "http://localhost:1234/other"),
    ] {
        assert_eq!(
            pool_key(&a.parse().unwrap()).unwrap(),
            pool_key(&b.parse().unwrap()).unwrap()
        );
    }
    assert_ne!(
        pool_key(&"http://localhost:1234/".parse().unwrap()).unwrap(),
        pool_key(&"https://localhost:1234/".parse().unwrap()).unwrap()
    );
}

#[tokio::test]
async fn upgrade_connection_token_without_attachment_cannot_dial() {
    let (trust, _) = super::super::lifecycle_tests::isolated_tls(None);
    let client = RawPoolClient::new(&trust).unwrap();
    for attached in [false, true] {
        let mut request = Request::get("http://127.0.0.1:9/synthetic")
            .body(Full::new(Bytes::new()))
            .unwrap();
        request
            .headers_mut()
            .append("connection", "keep-alive".parse().unwrap());
        request
            .headers_mut()
            .append("connection", "other, UpGrAdE".parse().unwrap());
        let capture = attached.then(|| capture_http1_assignment(&mut request));
        assert!(matches!(
            client.request_raw(request).await,
            Err(HttpError::InvalidRequest)
        ));
        assert!(capture.is_none_or(|capture| capture.assignment().is_none()));
    }
    assert_eq!(client.raw_counts.attempts.load(Ordering::SeqCst), 0);
    assert_eq!(client.raw_counts.tasks.load(Ordering::SeqCst), 0);
}
