//! Extensions for HTTP messages in Hyper.
//!
//! This module provides types and utilities that extend the capabilities of HTTP requests and responses
//! in Hyper. Extensions are additional pieces of information or features that can be attached to HTTP
//! messages via the [`http::Extensions`] map, which is
//! accessible through methods like [`http::Request::extensions`] and [`http::Response::extensions`].
//!
//! # What are extensions?
//!
//! Extensions allow Hyper to associate extra metadata or behaviors with HTTP messages, beyond the standard
//! headers and body. These can be used by advanced users and library authors to access protocol-specific
//! features, track original header casing, handle informational responses, and more.
//!
//! # How to access extensions
//!
//! Extensions are stored in the `Extensions` map of a request or response. You can access them using:
//!
//! ```rust
//! # let response = http::Response::new(());
//! if let Some(ext) = response.extensions().get::<hyper::ext::ReasonPhrase>() {
//!     // use the extension
//! }
//! ```
//!
//! # Extension Groups
//!
//! The extensions in this module can be grouped as follows:
//!
//! - **HTTP/1 Reason Phrase**: [`ReasonPhrase`] — Access non-canonical reason phrases in HTTP/1 responses.
//! - **Informational Responses**: [`on_informational`] — Register callbacks for 1xx HTTP/1 responses on the client.
//! - **Header Case Tracking**: Internal types for tracking the original casing and order of headers as received.
//! - **HTTP/2 Protocol Extensions**: [`Protocol`] — Access the `:protocol` pseudo-header for Extended CONNECT in HTTP/2.
//!
//! Some extensions are only available for specific protocols (HTTP/1 or HTTP/2) or use cases (client, server, FFI).
//!
//! See the documentation on each item for details about its usage and requirements.

#[cfg(all(any(feature = "client", feature = "server"), feature = "http1"))]
use bytes::Bytes;
#[cfg(any(
    all(any(feature = "client", feature = "server"), feature = "http1"),
    feature = "ffi"
))]
use http::header::HeaderName;
#[cfg(all(any(feature = "client", feature = "server"), feature = "http1"))]
use http::header::{HeaderMap, IntoHeaderName, ValueIter};
#[cfg(feature = "ffi")]
use std::collections::HashMap;
#[cfg(feature = "http2")]
use std::fmt;

#[cfg(any(feature = "http1", feature = "ffi"))]
mod h1_reason_phrase;
#[cfg(any(feature = "http1", feature = "ffi"))]
pub use h1_reason_phrase::ReasonPhrase;

#[cfg(all(feature = "http1", feature = "client"))]
mod informational;
#[cfg(all(feature = "http1", feature = "client"))]
pub use informational::on_informational;
#[cfg(all(feature = "http1", feature = "client"))]
pub(crate) use informational::OnInformational;
#[cfg(all(feature = "http1", feature = "client", feature = "ffi"))]
pub(crate) use informational::{on_informational_raw, OnInformationalCallback};

#[cfg(feature = "http2")]
/// Extension type representing the `:protocol` pseudo-header in HTTP/2.
///
/// The `Protocol` extension allows access to the value of the `:protocol` pseudo-header
/// used by the [Extended CONNECT Protocol](https://datatracker.ietf.org/doc/html/rfc8441#section-4).
/// This extension is only sent on HTTP/2 CONNECT requests, most commonly with the value `websocket`.
///
/// # Example
///
/// ```rust
/// use hyper::ext::Protocol;
/// use http::{Request, Method, Version};
///
/// let mut req = Request::new(());
/// *req.method_mut() = Method::CONNECT;
/// *req.version_mut() = Version::HTTP_2;
/// req.extensions_mut().insert(Protocol::from_static("websocket"));
/// // Now the request will include the `:protocol` pseudo-header with value "websocket"
/// ```
#[derive(Clone, Eq, PartialEq)]
pub struct Protocol {
    inner: h2::ext::Protocol,
}

#[cfg(feature = "http2")]
impl Protocol {
    /// Converts a static string to a protocol name.
    pub const fn from_static(value: &'static str) -> Self {
        Self {
            inner: h2::ext::Protocol::from_static(value),
        }
    }

    /// Returns a str representation of the header.
    pub fn as_str(&self) -> &str {
        self.inner.as_str()
    }

    #[cfg(feature = "server")]
    pub(crate) fn from_inner(inner: h2::ext::Protocol) -> Self {
        Self { inner }
    }

    #[cfg(all(feature = "client", feature = "http2"))]
    pub(crate) fn into_inner(self) -> h2::ext::Protocol {
        self.inner
    }
}

#[cfg(feature = "http2")]
impl<'proto> From<&'proto str> for Protocol {
    fn from(value: &'proto str) -> Self {
        Self {
            inner: h2::ext::Protocol::from(value),
        }
    }
}

#[cfg(feature = "http2")]
impl AsRef<[u8]> for Protocol {
    fn as_ref(&self) -> &[u8] {
        self.inner.as_ref()
    }
}

#[cfg(feature = "http2")]
impl fmt::Debug for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}

/// A map from header names to their original casing as received in an HTTP message.
///
/// If an HTTP/1 response `res` is parsed on a connection whose option
/// [`preserve_header_case`] was set to true and the response included
/// the following headers:
///
/// ```ignore
/// x-Bread: Baguette
/// X-BREAD: Pain
/// x-bread: Ficelle
/// ```
///
/// Then `res.extensions().get::<HeaderCaseMap>()` will return a map with:
///
/// ```ignore
/// HeaderCaseMap({
///     "x-bread": ["x-Bread", "X-BREAD", "x-bread"],
/// })
/// ```
///
/// [`preserve_header_case`]: /client/struct.Client.html#method.preserve_header_case
#[cfg(all(any(feature = "client", feature = "server"), feature = "http1"))]
#[derive(Clone, Debug)]
pub(crate) struct HeaderCaseMap(HeaderMap<Bytes>);

#[cfg(all(any(feature = "client", feature = "server"), feature = "http1"))]
impl HeaderCaseMap {
    /// Returns a view of all spellings associated with that header name,
    /// in the order they were found.
    #[cfg(feature = "client")]
    pub(crate) fn get_all<'hdr>(
        &'hdr self,
        name: &HeaderName,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + 'hdr> + 'hdr {
        self.get_all_internal(name)
    }

    /// Returns a view of all spellings associated with that header name,
    /// in the order they were found.
    #[cfg(any(feature = "client", feature = "server"))]
    pub(crate) fn get_all_internal(&self, name: &HeaderName) -> ValueIter<'_, Bytes> {
        self.0.get_all(name).into_iter()
    }

    #[cfg(any(feature = "client", feature = "server"))]
    pub(crate) fn default() -> Self {
        Self(HeaderMap::default())
    }

    #[cfg(any(test, feature = "ffi"))]
    pub(crate) fn insert(&mut self, name: HeaderName, orig: Bytes) {
        self.0.insert(name, orig);
    }

    #[cfg(any(feature = "client", feature = "server"))]
    pub(crate) fn append<N>(&mut self, name: N, orig: Bytes)
    where
        N: IntoHeaderName,
    {
        self.0.append(name, orig);
    }
}

#[cfg(feature = "ffi")]
#[derive(Clone, Debug)]
/// Hashmap<Headername, numheaders with that name>
pub(crate) struct OriginalHeaderOrder {
    /// Stores how many entries a Headername maps to. This is used
    /// for accounting.
    num_entries: HashMap<HeaderName, usize>,
    /// Stores the ordering of the headers. ex: `vec[i] = (headerName, idx)`,
    /// The vector is ordered such that the ith element
    /// represents the ith header that came in off the line.
    /// The `HeaderName` and `idx` are then used elsewhere to index into
    /// the multi map that stores the header values.
    entry_order: Vec<(HeaderName, usize)>,
}

#[cfg(all(feature = "http1", feature = "ffi"))]
impl OriginalHeaderOrder {
    pub(crate) fn default() -> Self {
        OriginalHeaderOrder {
            num_entries: HashMap::new(),
            entry_order: Vec::new(),
        }
    }

    pub(crate) fn insert(&mut self, name: HeaderName) {
        if !self.num_entries.contains_key(&name) {
            let idx = 0;
            self.num_entries.insert(name.clone(), 1);
            self.entry_order.push((name, idx));
        }
        // Replacing an already existing element does not
        // change ordering, so we only care if its the first
        // header name encountered
    }

    pub(crate) fn append<N>(&mut self, name: N)
    where
        N: IntoHeaderName + Into<HeaderName> + Clone,
    {
        let name: HeaderName = name.into();
        let idx = match self.num_entries.entry(name.clone()) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let idx = *entry.get();
                *entry.get_mut() += 1;
                idx
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(1);
                0
            }
        };
        self.entry_order.push((name, idx));
    }

    // No doc test is run here because `RUSTFLAGS='--cfg hyper_unstable_ffi'`
    // is needed to compile. Once ffi is stabilized `no_run` should be removed
    // here.
    /// This returns an iterator that provides header names and indexes
    /// in the original order received.
    pub(crate) fn get_in_order(&self) -> impl Iterator<Item = &(HeaderName, usize)> {
        self.entry_order.iter()
    }
}

/// Opt-in Node native HTTP response parsing for AutoRouter's raw provider path.
/// Other clients retain upstream Hyper behavior unless this request marker is set.
#[cfg(all(feature = "node-http1-compat", feature = "client", feature = "http1"))]
#[derive(Clone, Copy, Debug)]
pub struct NodeHttpResponsePolicy;

/// Callback immediately before a response head is encoded, after ordered service
/// dispatch. This only identifies submission; it does not prove a successful flush.
#[cfg(all(feature = "node-http1-compat", feature = "server", feature = "http1"))]
#[derive(Clone)]
pub struct NodeHttpResponseSubmission(std::sync::Arc<dyn Fn() + Send + Sync>);
#[cfg(all(feature = "node-http1-compat", feature = "server", feature = "http1"))]
impl NodeHttpResponseSubmission {
    /// Construct a synchronous callback. It must never panic or block.
    pub fn new(callback: impl Fn() + Send + Sync + 'static) -> Self {
        Self(std::sync::Arc::new(callback))
    }
    pub(crate) fn call(&self) { (self.0)(); }
}
#[cfg(all(feature = "node-http1-compat", feature = "server", feature = "http1"))]
impl std::fmt::Debug for NodeHttpResponseSubmission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NodeHttpResponseSubmission")
    }
}

/// Node's bundled fetch response parser: semantic header bound without status
/// text accounting or the native HTTP API's 1000-field observation truncation.
#[cfg(all(feature = "node-http1-compat", feature = "client", feature = "http1"))]
#[derive(Clone, Copy, Debug)]
pub struct NodeFetchResponsePolicy;


/// Result of polling the HTTP/1 connection's existing flush future.
///
/// These observations are not delivery or application-completion authority.
/// `Pending` can mean a write or the underlying flush is pending. `Ready`
/// describes this poll only; with pipeline flush enabled it can be an optimized
/// return without a physical write. Callers must retain that distinction.
#[cfg(feature = "node-http1-body-handoff")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeHttpFlushPoll {
    /// The existing flush poll returned successfully.
    Ready,
    /// A write or flush poll is pending.
    Pending,
    /// The existing flush poll returned an error.
    Failed,
}

/// Per-response observations for an experimental producer handoff.
#[cfg(feature = "node-http1-body-handoff")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeHttpBodyHandoffEvent {
    /// The completed response is waiting behind earlier response work.
    /// This can repeat; it does not mean this response's body was polled.
    Queued,
    /// Ordered dispatch selected this response, immediately before its head.
    Active,
    /// A nonempty Data frame was passed through the existing body encoder.
    /// These are input bytes; a fixed Content-Length encoder may clip them.
    /// No physical write or successful delivery is implied.
    DataSubmitted {
        /// Nonempty input bytes supplied to this encoder call.
        input_bytes: usize,
        /// Checked cumulative input bytes supplied for this response.
        total_input_bytes: u64,
    },
    /// A flush poll after the recorded input bytes were submitted returned.
    /// The count belongs only to this response, including across Pending polls.
    FlushPolled {
        /// Input boundary captured before polling the connection flush.
        total_input_bytes: u64,
        /// Result of that poll, distinct from delivery completion.
        outcome: NodeHttpFlushPoll,
    },
}

/// Optional server-response extension for bounded, request-owned handoff
/// observation. No callback is created or installed by Hyper itself.
///
/// Callers must construct a distinct observer for each response, retain their
/// own request/generation ownership, and never equate an event with successful
/// forwarding. The callback is synchronous and must not block or panic.
#[cfg(feature = "node-http1-body-handoff")]
#[derive(Clone)]
pub struct NodeHttpBodyHandoff(
    std::sync::Arc<dyn Fn(NodeHttpBodyHandoffEvent) + Send + Sync>,
);
#[cfg(feature = "node-http1-body-handoff")]
impl NodeHttpBodyHandoff {
    /// Construct a synchronous observation callback. It must not panic or block.
    pub fn new(callback: impl Fn(NodeHttpBodyHandoffEvent) + Send + Sync + 'static) -> Self {
        Self(std::sync::Arc::new(callback))
    }
    pub(crate) fn call(&self, event: NodeHttpBodyHandoffEvent) { (self.0)(event); }
}
#[cfg(feature = "node-http1-body-handoff")]
impl std::fmt::Debug for NodeHttpBodyHandoff {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NodeHttpBodyHandoff")
    }
}
