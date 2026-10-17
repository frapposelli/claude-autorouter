//! AutoRouter's opt-in Node 22/24 server header policy. This is not a second
//! HTTP decoder: httparse still validates grammar and Hyper still frames bodies.
//! The incremental counter mirrors Node's on_url/on_header_field/on_header_value
//! byte accounting; raw fields are bounded before normalization or truncation.
use crate::error::Parse;
use bytes::BytesMut;

pub(super) const HEADER_BYTES: usize = 16 * 1024;
pub(super) const RETAINED_FIELDS: usize = 1000;
const METHODS: &[&str] = &[
    "ACL",
    "BIND",
    "CHECKOUT",
    "CONNECT",
    "COPY",
    "DELETE",
    "GET",
    "HEAD",
    "LINK",
    "LOCK",
    "M-SEARCH",
    "MERGE",
    "MKACTIVITY",
    "MKCALENDAR",
    "MKCOL",
    "MOVE",
    "NOTIFY",
    "OPTIONS",
    "PATCH",
    "POST",
    "PROPFIND",
    "PROPPATCH",
    "PURGE",
    "PUT",
    "QUERY",
    "REBIND",
    "REPORT",
    "SEARCH",
    "SOURCE",
    "SUBSCRIBE",
    "TRACE",
    "UNBIND",
    "UNLINK",
    "UNLOCK",
    "UNSUBSCRIBE",
];

#[derive(Default, Debug, Clone, PartialEq)]
pub(super) struct Counter {
    offset: usize,
    bytes: usize,
    fields: usize,
    state: State,
    method: [u8; 16],
    method_len: usize,
    fetch_response: bool,
}
#[derive(Default, Debug, Clone, PartialEq)]
enum State {
    #[default]
    Method,
    ResponseVersion,
    ResponseCode,
    ResponseReason,
    TargetStart,
    Target,
    VersionStart,
    Version,
    RequestLf,
    Name,
    BeforeValue,
    Value,
    HeaderLf,
    EndLf,
    Done,
}
impl Counter {
    pub(super) fn trailers() -> Self {
        Self {
            state: State::Name,
            ..Self::default()
        }
    }
    pub(super) fn response() -> Self {
        Self {
            state: State::ResponseVersion,
            ..Self::default()
        }
    }

    pub(super) fn fetch_response() -> Self {
        Self {
            fetch_response: true,
            ..Self::response()
        }
    }
    pub(super) fn is_native_response(&self) -> bool {
        !self.fetch_response
    }
    pub(super) fn observe(&mut self, input: &mut BytesMut) -> Result<(), Parse> {
        let mut read = self.offset;
        let mut written = self.offset;
        while read < input.len() {
            if matches!(self.state, State::Done) {
                break;
            }
            let byte = input[read];
            read += 1;
            // Node does not retain leading header OWS or repeated request-line
            // delimiters. Discard them incrementally so arbitrarily long OWS
            // cannot grow Hyper's buffered head beyond the semantic byte bound.
            let discard = (matches!(self.state, State::BeforeValue)
                && matches!(byte, b' ' | b'\t'))
                || (matches!(self.state, State::TargetStart | State::VersionStart) && byte == b' ');
            if !discard {
                if written != read - 1 {
                    input[written] = byte;
                }
                written += 1;
            }
            if byte == b'\n'
                && !matches!(
                    self.state,
                    State::RequestLf | State::HeaderLf | State::EndLf | State::Method
                )
            {
                return Err(Parse::Header(crate::error::Header::Token));
            }
            match self.state {
                State::Method => {
                    if self.method_len == 0 && matches!(byte, b'\r' | b'\n') {
                        continue;
                    }
                    if byte == b' ' {
                        if !METHODS
                            .iter()
                            .any(|method| method.as_bytes() == &self.method[..self.method_len])
                        {
                            return Err(Parse::Method);
                        }
                        self.state = State::TargetStart;
                    } else if self.method_len < self.method.len() {
                        self.method[self.method_len] = byte;
                        self.method_len += 1;
                        if !METHODS.iter().any(|method| {
                            method
                                .as_bytes()
                                .starts_with(&self.method[..self.method_len])
                        }) {
                            return Err(Parse::Method);
                        }
                    } else {
                        return Err(Parse::Method);
                    }
                }
                State::ResponseVersion => {
                    if byte == b' ' {
                        self.state = State::ResponseCode;
                    }
                }
                State::ResponseCode => {
                    if byte == b' ' {
                        self.state = State::ResponseReason;
                    } else if byte == b'\r' {
                        self.state = State::RequestLf;
                    }
                }
                State::ResponseReason => {
                    if byte == b'\r' {
                        self.state = State::RequestLf;
                    } else if !self.fetch_response {
                        self.count(1)?;
                    }
                }
                State::TargetStart => {
                    if byte != b' ' {
                        self.count(1)?;
                        self.state = State::Target;
                    }
                }
                State::Target => {
                    if byte == b' ' {
                        self.state = State::VersionStart;
                    } else {
                        self.count(1)?;
                    }
                }
                State::VersionStart => {
                    if byte != b' ' {
                        self.state = State::Version;
                    }
                }
                State::Version => {
                    if byte == b'\r' {
                        self.state = State::RequestLf;
                    } else if byte == b'\n' {
                        return Err(Parse::Header(crate::error::Header::Token));
                    }
                }
                State::RequestLf => {
                    self.state = State::Name;
                }
                State::Name => {
                    if byte == b'\r' {
                        self.state = State::EndLf;
                    } else if byte == b':' {
                        self.fields += 1;
                        self.state = State::BeforeValue;
                    } else {
                        self.count(1)?;
                    }
                }
                State::BeforeValue => {
                    if byte == b'\r' {
                        self.state = State::HeaderLf;
                    } else if !matches!(byte, b' ' | b'\t') {
                        self.count(1)?;
                        self.state = State::Value;
                    }
                }
                State::Value => {
                    if byte == b'\r' {
                        self.state = State::HeaderLf;
                    } else {
                        self.count(1)?;
                    }
                }
                State::HeaderLf => {
                    self.state = State::Name;
                }
                State::EndLf => {
                    self.state = State::Done;
                }
                State::Done => unreachable!(),
            }
        }
        self.offset = written;
        if written != read {
            let tail = input.len() - read;
            input.copy_within(read.., written);
            input.truncate(written + tail);
        }
        Ok(())
    }
    fn count(&mut self, count: usize) -> Result<(), Parse> {
        self.bytes += count;
        if self.bytes >= HEADER_BYTES {
            Err(Parse::TooLarge)
        } else {
            Ok(())
        }
    }
}

// httparse needs storage for each raw line in order to validate all framing
// fields even after Node's application-visible rawHeaders has reached 1000.
// Ordinary requests keep Hyper's existing 100-entry stack fast path.
pub(super) fn header_capacity(input: &[u8]) -> usize {
    let mut lines = 0;
    for line in input.split(|byte| *byte == b'\n') {
        if matches!(line, b"\r" | b"") {
            if lines > 0 {
                break;
            }
        } else {
            lines += 1;
        }
    }
    lines.clamp(100, HEADER_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counter_is_incremental_and_does_not_count_framing_or_leading_whitespace() {
        let input = b"POST /test HTTP/1.1\r\nx:\t  abc  \r\nz:\r\n\r\nBODY";
        let mut counter = Counter::default();
        let mut buffered = BytesMut::new();
        for &byte in input {
            buffered.extend_from_slice(&[byte]);
            counter.observe(&mut buffered).unwrap();
        }
        assert!(buffered.ends_with(b"\r\n\r\nBODY"));
        assert_eq!(counter.bytes, 5 + 1 + 5 + 1);
        assert_eq!(counter.fields, 2);
        assert_eq!(header_capacity(input), 100);
    }
    #[test]
    fn partial_oversized_values_fail_before_header_terminator() {
        let mut bytes = b"GET / HTTP/1.1\r\nx:".to_vec();
        bytes.extend(std::iter::repeat_n(b'a', HEADER_BYTES - 2));
        assert!(Counter::default()
            .observe(&mut BytesMut::from(bytes.as_slice()))
            .is_err());
        bytes.pop();
        assert!(Counter::default()
            .observe(&mut BytesMut::from(bytes.as_slice()))
            .is_ok());
    }
}

/// llhttp's chunk-extension token/quoted-value grammar and Node native HTTP's
/// per-chunk name/value byte accounting. Extension text is never retained.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Chunked {
    pub(super) trailers: Counter,
    fetch: bool,
    extension_state: Extension,
    extension_bytes: usize,
}
#[derive(Debug, Clone, Copy, PartialEq)]
enum Extension {
    NameStart,
    Name,
    ValueStart,
    Value,
    Quoted,
    Escape,
    AfterQuoted,
}
impl Chunked {
    pub(super) fn new(fetch: bool) -> Self {
        Self {
            trailers: Counter::trailers(),
            fetch,
            extension_state: Extension::NameStart,
            extension_bytes: 0,
        }
    }
    pub(super) fn new_chunk(&mut self) {
        self.extension_state = Extension::NameStart;
        self.extension_bytes = 0;
    }
    pub(super) fn extension(&mut self, byte: u8) -> Result<(), std::io::Error> {
        use Extension::*;
        let token = byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte);
        let mut count = false;
        let state = match (self.extension_state, byte) {
            (NameStart | Name, _) if token => {
                count = true;
                Name
            }
            (Name, b'=') => ValueStart,
            (Name | Value | AfterQuoted | ValueStart, b';') => NameStart,
            (Name | Value | AfterQuoted | ValueStart, b'\r') => NameStart,
            (ValueStart, b'"') => {
                count = true;
                Quoted
            }
            (ValueStart | Value, _) if token => {
                count = true;
                Value
            }
            (Quoted, b'"') => {
                count = true;
                AfterQuoted
            }
            (Quoted, b'\\') => {
                count = true;
                Escape
            }
            (Quoted, b'\t' | b' ' | b'!' | b'#'..=b'[' | b']'..=b'~' | 0x80..=0xff) => {
                count = true;
                Quoted
            }
            (Escape, b'\t' | b' '..=b'~' | 0x80..=0xff) => {
                count = true;
                Quoted
            }
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "invalid chunk extension",
                ))
            }
        };
        self.extension_state = state;
        if count && !self.fetch {
            self.extension_bytes += 1;
            if self.extension_bytes > HEADER_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "chunk extensions over limit",
                ));
            }
        }
        Ok(())
    }
}
