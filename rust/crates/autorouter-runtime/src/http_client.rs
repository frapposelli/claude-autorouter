//! Pooled raw HTTP transport. No payloads, credentials, or URLs appear in errors.
//! Redirect and content decoding policy belongs to callers, never the transport.

use std::fmt;
use std::future::Future;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::{Body, Incoming};
use hyper::{Request, Response};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::TokioExecutor;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpError {
    Network,
    InvalidRequest,
    CertificateRoots,
    UnsupportedTrustOptions,
    ConflictingTrustOptions,
    UnsupportedSystemTrustOption,
}

impl fmt::Display for HttpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ConflictingTrustOptions => "either --use-openssl-ca or --use-bundled-ca can be used, not both",
            Self::UnsupportedSystemTrustOption => "--use-system-ca is not allowed in NODE_OPTIONS",
            Self::Network => "HTTP transport failed",
            Self::InvalidRequest => "Invalid HTTP request",
            Self::CertificateRoots => "Could not load bundled certificate roots",
            Self::UnsupportedTrustOptions => "Unsupported Node TLS trust options; this native build supports bundled roots, NODE_EXTRA_CA_CERTS, and --use-openssl-ca",
        })
    }
}
impl std::error::Error for HttpError {}

/// Fetch's Headers constructor uses WebIDL ByteString, then trims HTTP
/// whitespace. Encoding a Rust string as UTF-8 changes Latin-1 credentials.
pub fn fetch_header_value(value: &str) -> Result<hyper::header::HeaderValue, HttpError> {
    let bytes: Result<Vec<u8>, _> = value
        .chars()
        .map(|value| u8::try_from(u32::from(value)))
        .collect();
    let bytes = bytes.map_err(|_| HttpError::InvalidRequest)?;
    let start = bytes
        .iter()
        .position(|byte| !matches!(byte, b'\t' | b'\r' | b'\n' | b' '))
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !matches!(byte, b'\t' | b'\r' | b'\n' | b' '))
        .map_or(start, |index| index + 1);
    hyper::header::HeaderValue::from_bytes(&bytes[start..end])
        .map_err(|_| HttpError::InvalidRequest)
}

/// Injectable transport with owned request and response bodies. Dropping its
/// future/body must release that request; no unbounded response collection.
pub trait HttpTransport: Send + Sync {
    type ResponseBody: Body<Data = Bytes> + Send + 'static;

    fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> impl Future<Output = Result<Response<Self::ResponseBody>, HttpError>> + Send;

    /// Native Node HTTP forwarding has a distinct response parser from fetch.
    fn request_raw(
        &self,
        request: Request<Full<Bytes>>,
    ) -> impl Future<Output = Result<Response<Self::ResponseBody>, HttpError>> + Send {
        self.request(request)
    }
}

#[derive(Clone)]
pub struct NativeHttpClient {
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
}

impl NativeHttpClient {
    pub fn new() -> Result<Self, HttpError> {
        let verifier = crate::tls_roots::process_verifier().map_err(|error| match error {
            crate::tls_roots::TrustError::InvalidBundle => HttpError::CertificateRoots,
            crate::tls_roots::TrustError::UnsupportedOptions => HttpError::UnsupportedTrustOptions,
            crate::tls_roots::TrustError::ConflictingSelectors => {
                HttpError::ConflictingTrustOptions
            }
            crate::tls_roots::TrustError::UnsupportedSystemSelector => {
                HttpError::UnsupportedSystemTrustOption
            }
        })?;
        let tls = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        let connector = HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_or_http()
            .enable_http1()
            .build();
        let mut builder = Client::builder(TokioExecutor::new());
        // Hyper-util retries cancelled requests on reused connections by
        // default. An inference/evaluator request must never be replayed here.
        builder.retry_canceled_requests(false);
        Ok(Self {
            client: builder.build(connector),
        })
    }
}

impl HttpTransport for NativeHttpClient {
    type ResponseBody = Incoming;

    async fn request_raw(
        &self,
        mut request: Request<Full<Bytes>>,
    ) -> Result<Response<Incoming>, HttpError> {
        request
            .extensions_mut()
            .insert(hyper::ext::NodeHttpResponsePolicy);
        self.request(request).await
    }

    async fn request(
        &self,
        mut request: Request<Full<Bytes>>,
    ) -> Result<Response<Incoming>, HttpError> {
        if request
            .extensions()
            .get::<hyper::ext::NodeHttpResponsePolicy>()
            .is_none()
        {
            request
                .extensions_mut()
                .insert(hyper::ext::NodeFetchResponsePolicy);
        }
        if !matches!(request.uri().scheme_str(), Some("http" | "https")) {
            return Err(HttpError::InvalidRequest);
        }
        self.client
            .request(request)
            .await
            .map_err(|_| HttpError::Network)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn fetch_headers_use_latin1_and_reject_non_byte_strings() {
        assert_eq!(
            fetch_header_value("\t Bearer synthetic-é\r\n")
                .unwrap()
                .as_bytes(),
            b"Bearer synthetic-\xe9"
        );
        assert!(fetch_header_value("Bearer synthetic-Ā").is_err());
        assert!(fetch_header_value("Bearer synthetic-�").is_err());
        assert!(fetch_header_value("Bearer synthetic\ninside").is_err());
    }

    #[tokio::test]
    async fn raw_provider_response_policy_checks_framing_bounds_and_retained_headers() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let cases = [
            (format!("HTTP/1.1 200 OK\r\n{}content-length: 4\r\nx-visible: last\r\nconnection: close\r\n\r\nBODY", "z:\r\n".repeat(1001)), true, false),
            ("HTTP/1.1 200 OK\r\ncontent-length: 4\r\ncontent-length: 4\r\n\r\nBODY".into(), false, false),
            ("HTTP/1.1 200 OK\r\ncontent-length: 4\r\ntransfer-encoding: chunked\r\n\r\n4\r\nBODY\r\n0\r\n\r\n".into(), false, false),
            ("HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\ncontent-length: 4\r\n\r\n4\r\nBODY\r\n0\r\n\r\n".into(), false, false),
            ("HTTP/1.1 200 OK\ncontent-length: 4\n\nBODY".into(), false, false),
            (format!("HTTP/1.1 200 OK\r\nx-padding: {}\r\ncontent-length: 4\r\n\r\nBODY", "x".repeat(16384)), false, false),
            (format!("HTTP/1.1 100 Continue\r\nx-info: initial\r\n\r\nHTTP/1.1 200 OK\r\nx-padding: {}\r\ncontent-length: 4\r\n\r\nBODY", "x".repeat(16384)), false, false),
            ("HTTP/1.1 100 Continue\r\nx-info: initial\r\n\r\nHTTP/1.1 200 OK\r\ncontent-length: 4\r\nx-visible: last\r\n\r\nBODY".into(), true, true),
        ];
        let client = NativeHttpClient::new().unwrap();
        for (wire, accepted, visible) in cases {
            for native in [true, false] {
                let accepted = accepted && (native || !wire.starts_with("HTTP/1.1 100"));
                let visible = visible || (!native && wire.contains("x-visible: last"));
                let wire = wire.clone();
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let server = tokio::spawn(async move {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut received = Vec::new();
                    let mut chunk = [0; 2048];
                    while !received.windows(4).any(|part| part == b"\r\n\r\n") {
                        let count = socket.read(&mut chunk).await.unwrap();
                        if count == 0 {
                            return;
                        }
                        received.extend_from_slice(&chunk[..count]);
                    }
                    for part in wire.as_bytes().chunks(97) {
                        if socket.write_all(part).await.is_err() {
                            return;
                        }
                        tokio::task::yield_now().await;
                    }
                });
                let request = Request::get(format!("http://{address}/synthetic"))
                    .body(Full::new(Bytes::new()))
                    .unwrap();
                let response = if native {
                    client.request_raw(request).await
                } else {
                    client.request(request).await
                };
                assert_eq!(response.is_ok(), accepted);
                if let Ok(response) = response {
                    assert_eq!(response.headers().contains_key("x-visible"), visible);
                    assert_eq!(
                        response.into_body().collect().await.unwrap().to_bytes(),
                        "BODY"
                    );
                }
                server.await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn chunked_provider_trailers_and_extensions_keep_node_bounds_and_grammar() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let cases = [
            (
                format!("4\r\nBODY\r\n0\r\n{}\r\n", "z:\r\n".repeat(1001)),
                true,
            ),
            (
                format!(
                    "2;x={}\r\nBO\r\n2;x={}\r\nDY\r\n0\r\n\r\n",
                    "a".repeat(10000),
                    "a".repeat(10000)
                ),
                true,
            ),
            ("4;x=\r\nBODY\r\n0\r\n\r\n".into(), true),
            ("4;x=unquoted space\r\nBODY\r\n0\r\n\r\n".into(), false),
            (
                format!("4;x=\"{}\"\r\nBODY\r\n0\r\n\r\n", "a".repeat(16382)),
                false,
            ),
            (
                format!("4\r\nBODY\r\n0\r\nx:{}\r\n\r\n", "a".repeat(16384)),
                false,
            ),
        ];
        let client = NativeHttpClient::new().unwrap();
        for (chunked, clean) in cases {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut received = Vec::new();
                let mut buffer = [0; 2048];
                while !received.windows(4).any(|part| part == b"\r\n\r\n") {
                    let count = socket.read(&mut buffer).await.unwrap();
                    if count == 0 {
                        return;
                    }
                    received.extend_from_slice(&buffer[..count]);
                }
                let wire = format!(
                    "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n{chunked}"
                );
                let _ = socket.write_all(wire.as_bytes()).await;
            });
            let response = client
                .request_raw(
                    Request::get(format!("http://{address}/synthetic"))
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
                .await;
            let body = match response {
                Ok(response) => response
                    .into_body()
                    .collect()
                    .await
                    .ok()
                    .map(|body| body.to_bytes()),
                Err(_) => None,
            };
            assert_eq!(body.is_some(), clean);
            if let Some(body) = body {
                assert_eq!(body, "BODY");
            }
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn local_http_reuses_connections_preserves_credentials_and_never_decodes_or_redirects() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let connections = accepted.clone();
        let observed = requests.clone();
        let server = tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                connections.fetch_add(1, Ordering::SeqCst);
                let observed = observed.clone();
                children.spawn(async move {
                    let service = service_fn(move |request: Request<Incoming>| {
                        observed.lock().unwrap().push((
                            request.uri().path().to_owned(),
                            request.headers().get("authorization").cloned(),
                        ));
                        let redirect = request.uri().path() == "/redirect";
                        let response = if redirect {
                            Response::builder()
                                .status(307)
                                .header("location", format!("http://{address}/must-not-follow"))
                                .body(Full::new(Bytes::from_static(b"synthetic redirect")))
                                .unwrap()
                        } else {
                            Response::builder()
                                .header("content-encoding", "gzip")
                                .body(Full::new(Bytes::from_static(&[31, 139, 8, 0, 1, 2, 3, 4])))
                                .unwrap()
                        };
                        async { Ok::<_, Infallible>(response) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(socket), service)
                        .await;
                });
            }
        });
        let client = NativeHttpClient::new().unwrap();
        for credential in ["Bearer synthetic-first", "Bearer synthetic-refreshed"] {
            let request = Request::post(format!("http://{address}/compressed"))
                .header("authorization", credential)
                .body(Full::new(Bytes::new()))
                .unwrap();
            let response = client.request(request).await.unwrap();
            assert_eq!(response.headers()["content-encoding"], "gzip");
            assert_eq!(
                response
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .as_ref(),
                &[31, 139, 8, 0, 1, 2, 3, 4]
            );
            tokio::task::yield_now().await;
        }
        let response = client
            .request(
                Request::get(format!("http://{address}/redirect"))
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 307);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "synthetic redirect"
        );
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
        {
            let recorded = requests.lock().unwrap();
            assert_eq!(recorded.len(), 3);
            assert_eq!(recorded[0].1.as_ref().unwrap(), "Bearer synthetic-first");
            assert_eq!(
                recorded[1].1.as_ref().unwrap(),
                "Bearer synthetic-refreshed"
            );
            assert!(recorded.iter().all(|(path, _)| path != "/must-not-follow"));
        }
        server.abort();
        let _ = server.await;
    }
}
