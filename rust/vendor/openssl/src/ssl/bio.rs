use ffi::{
    self, BIO_clear_retry_flags, BIO_new, BIO_set_retry_read, BIO_set_retry_write, BIO,
    BIO_CTRL_DGRAM_QUERY_MTU, BIO_CTRL_FLUSH,
};
use foreign_types::ForeignType;
use libc::{c_char, c_int, c_long, c_void, strlen};
use std::any::Any;
use std::io;
use std::io::prelude::*;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use crate::error::ErrorStack;
use crate::{cvt_p, util};

pub struct StreamState<S> {
    pub stream: S,
    pub error: Option<io::Error>,
    pub panic: Option<Box<dyn Any + Send>>,
    pub dtls_mtu_size: c_long,
}

foreign_type_and_impl_send_sync! {
    type CType = ffi::BIO_METHOD;
    fn drop = ffi::BIO_meth_free;

    /// Safe wrapper for `BIO_METHOD`
    pub struct BioMethod;
    /// A reference to a `BioMethod`.
    pub struct BioMethodRef;
}

pub fn new<S: Read + Write>(stream: S) -> Result<(*mut BIO, BioMethod), ErrorStack> {
    let method = BioMethod::new::<S>()?;

    let state = Box::new(StreamState {
        stream,
        error: None,
        panic: None,
        dtls_mtu_size: 0,
    });

    unsafe {
        let bio = cvt_p(BIO_new(method.as_ptr()))?;
        BIO_set_data(bio, Box::into_raw(state) as *mut _);
        BIO_set_init(bio, 1);

        Ok((bio, method))
    }
}

pub unsafe fn take_error<S>(bio: *mut BIO) -> Option<io::Error> {
    let state = state::<S>(bio);
    state.error.take()
}

pub unsafe fn take_panic<S>(bio: *mut BIO) -> Option<Box<dyn Any + Send>> {
    let state = state::<S>(bio);
    state.panic.take()
}

pub unsafe fn get_ref<'a, S: 'a>(bio: *mut BIO) -> &'a S {
    let state = &*(BIO_get_data(bio) as *const StreamState<S>);
    &state.stream
}

pub unsafe fn get_mut<'a, S: 'a>(bio: *mut BIO) -> &'a mut S {
    &mut state(bio).stream
}

pub unsafe fn set_dtls_mtu_size<S>(bio: *mut BIO, mtu_size: usize) {
    if mtu_size as u64 > c_long::MAX as u64 {
        panic!(
            "Given MTU size {} can't be represented in a positive `c_long` range",
            mtu_size
        )
    }
    state::<S>(bio).dtls_mtu_size = mtu_size as c_long;
}

unsafe fn state<'a, S: 'a>(bio: *mut BIO) -> &'a mut StreamState<S> {
    &mut *(BIO_get_data(bio) as *mut _)
}

// A Rust IO error owns its OS code; it need not match C's thread-local errno.
// OpenSSL's record writer reads errno after a fatal BIO write. Publish an
// actual supplied OS code after Rust bookkeeping; keep upstream behavior for
// custom/retry errors without an OS code and for unqualified platforms.
fn set_write_errno(code: Option<c_int>) {
    let Some(code) = code else {
        return;
    };
    #[cfg(target_os = "macos")]
    unsafe {
        *libc::__error() = code;
    }
    #[cfg(target_os = "linux")]
    unsafe {
        *libc::__errno_location() = code;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = code;
}

unsafe extern "C" fn bwrite<S: Write>(bio: *mut BIO, buf: *const c_char, len: c_int) -> c_int {
    BIO_clear_retry_flags(bio);

    let state = state::<S>(bio);
    let buf = util::from_raw_parts(buf as *const _, len as usize);

    match catch_unwind(AssertUnwindSafe(|| state.stream.write(buf))) {
        Ok(Ok(len)) => len as c_int,
        Ok(Err(err)) => {
            let code = err.raw_os_error();
            if retriable_error(&err) {
                BIO_set_retry_write(bio);
            }
            state.error = Some(err);
            set_write_errno(code);
            -1
        }
        Err(err) => {
            state.panic = Some(err);
            -1
        }
    }
}

unsafe extern "C" fn bread<S: Read>(bio: *mut BIO, buf: *mut c_char, len: c_int) -> c_int {
    BIO_clear_retry_flags(bio);

    let state = state::<S>(bio);
    let buf = util::from_raw_parts_mut(buf as *mut _, len as usize);

    match catch_unwind(AssertUnwindSafe(|| state.stream.read(buf))) {
        Ok(Ok(len)) => len as c_int,
        Ok(Err(err)) => {
            if retriable_error(&err) {
                BIO_set_retry_read(bio);
            }
            state.error = Some(err);
            -1
        }
        Err(err) => {
            state.panic = Some(err);
            -1
        }
    }
}

#[allow(clippy::match_like_matches_macro)] // matches macro requires rust 1.42.0
fn retriable_error(err: &io::Error) -> bool {
    match err.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::NotConnected => true,
        _ => false,
    }
}

unsafe extern "C" fn bputs<S: Write>(bio: *mut BIO, s: *const c_char) -> c_int {
    bwrite::<S>(bio, s, strlen(s) as c_int)
}

unsafe extern "C" fn ctrl<S: Write>(
    bio: *mut BIO,
    cmd: c_int,
    _num: c_long,
    _ptr: *mut c_void,
) -> c_long {
    let state = state::<S>(bio);

    if cmd == BIO_CTRL_FLUSH {
        match catch_unwind(AssertUnwindSafe(|| state.stream.flush())) {
            Ok(Ok(())) => 1,
            Ok(Err(err)) => {
                state.error = Some(err);
                0
            }
            Err(err) => {
                state.panic = Some(err);
                0
            }
        }
    } else if cmd == BIO_CTRL_DGRAM_QUERY_MTU {
        state.dtls_mtu_size
    } else {
        0
    }
}

unsafe extern "C" fn create(bio: *mut BIO) -> c_int {
    BIO_set_init(bio, 0);
    BIO_set_data(bio, ptr::null_mut());
    BIO_set_flags(bio, 0);
    1
}

unsafe extern "C" fn destroy<S>(bio: *mut BIO) -> c_int {
    if bio.is_null() {
        return 0;
    }

    let data = BIO_get_data(bio);
    assert!(!data.is_null());
    let _ = Box::<StreamState<S>>::from_raw(data as *mut _);
    BIO_set_data(bio, ptr::null_mut());
    BIO_set_init(bio, 0);
    1
}

use crate::cvt;
use ffi::{BIO_get_data, BIO_set_data, BIO_set_flags, BIO_set_init};

impl BioMethod {
    fn new<S: Read + Write>() -> Result<BioMethod, ErrorStack> {
        #[cfg(any(boringssl, awslc))]
        use ffi::{
            BIO_meth_set_create, BIO_meth_set_ctrl, BIO_meth_set_destroy, BIO_meth_set_puts,
            BIO_meth_set_read, BIO_meth_set_write,
        };
        #[cfg(not(any(boringssl, awslc)))]
        use ffi::{
            BIO_meth_set_create__fixed_rust as BIO_meth_set_create,
            BIO_meth_set_ctrl__fixed_rust as BIO_meth_set_ctrl,
            BIO_meth_set_destroy__fixed_rust as BIO_meth_set_destroy,
            BIO_meth_set_puts__fixed_rust as BIO_meth_set_puts,
            BIO_meth_set_read__fixed_rust as BIO_meth_set_read,
            BIO_meth_set_write__fixed_rust as BIO_meth_set_write,
        };

        unsafe {
            let ptr = cvt_p(ffi::BIO_meth_new(ffi::BIO_TYPE_NONE, c"rust".as_ptr()))?;
            let method = BioMethod::from_ptr(ptr);
            cvt(BIO_meth_set_write(method.as_ptr(), Some(bwrite::<S>)))?;
            cvt(BIO_meth_set_read(method.as_ptr(), Some(bread::<S>)))?;
            cvt(BIO_meth_set_puts(method.as_ptr(), Some(bputs::<S>)))?;
            cvt(BIO_meth_set_ctrl(method.as_ptr(), Some(ctrl::<S>)))?;
            cvt(BIO_meth_set_create(method.as_ptr(), Some(create)))?;
            cvt(BIO_meth_set_destroy(method.as_ptr(), Some(destroy::<S>)))?;
            Ok(method)
        }
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod write_errno_tests {
    use super::*;
    use std::sync::Arc;

    // Public OpenSSL ABI (bio.h); not exported by the pinned openssl-sys.
    extern "C" {
        #[link_name = "BIO_test_flags"]
        fn c_bio_test_flags(bio: *const BIO, flags: c_int) -> c_int;
    }

    fn errno_slot() -> *mut c_int {
        #[cfg(target_os = "macos")]
        unsafe {
            libc::__error()
        }
        #[cfg(target_os = "linux")]
        unsafe {
            libc::__errno_location()
        }
    }

    enum Failure {
        Os(c_int),
        Custom(io::Error),
    }
    struct Planned {
        failure: Option<Failure>,
        writes: usize,
        observed_len: usize,
        observed_first: Option<u8>,
    }
    impl Planned {
        fn new(failure: Failure) -> Self {
            Self {
                failure: Some(failure),
                writes: 0,
                observed_len: 0,
                observed_first: None,
            }
        }
    }
    impl Read for Planned {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }
    impl Write for Planned {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            self.observed_len = buf.len();
            self.observed_first = buf.first().copied();
            // No errno writes, allocations, foreign calls or error formatting.
            match self.failure.take().expect("one declared write") {
                Failure::Os(code) => Err(io::Error::from_raw_os_error(code)),
                Failure::Custom(error) => Err(error),
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct Fixture {
        pointer: *mut BIO,
        _method: BioMethod,
    }
    impl Fixture {
        fn new(failure: Failure) -> Self {
            #[cfg(ossl300)]
            crate::init_without_config().unwrap();
            #[cfg(not(ossl300))]
            crate::init();
            let (pointer, method) = new(Planned::new(failure)).unwrap();
            Self {
                pointer,
                _method: method,
            }
        }
        fn write(&self, sentinel: c_int) -> (c_int, c_int) {
            let slot = errno_slot();
            assert!(!slot.is_null());
            // Prove that bwrite clears stale READ flags before classifying IO.
            unsafe {
                ffi::BIO_set_retry_read(self.pointer);
                *slot = sentinel;
                let returned = ffi::BIO_write(self.pointer, b"w".as_ptr().cast(), 1);
                // Capture before assertions, flag reads, error extraction or Drop.
                let observed_errno = *slot;
                (returned, observed_errno)
            }
        }
        fn retry_flags(&self) -> c_int {
            unsafe {
                c_bio_test_flags(
                    self.pointer,
                    ffi::BIO_FLAGS_RWS | ffi::BIO_FLAGS_SHOULD_RETRY,
                )
            }
        }
        fn owned_error(&self) -> io::Error {
            let stream = unsafe { get_ref::<Planned>(self.pointer) };
            assert_eq!(stream.writes, 1);
            assert_eq!(stream.observed_len, 1);
            assert_eq!(stream.observed_first, Some(b'w'));
            assert!(stream.failure.is_none());
            assert!(unsafe { take_panic::<Planned>(self.pointer) }.is_none());
            let error = unsafe { take_error::<Planned>(self.pointer) }
                .expect("actual BIO callback retained its owned IO error");
            assert!(unsafe { take_error::<Planned>(self.pointer) }.is_none());
            error
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // The BIO method field remains alive through this destructor body.
            unsafe { ffi::BIO_free_all(self.pointer) }
        }
    }

    fn direct_os_control(code: c_int, kind: io::ErrorKind, sentinel: c_int) {
        let mut planned = Planned::new(Failure::Os(code));
        let slot = errno_slot();
        assert!(!slot.is_null());
        unsafe { *slot = sentinel };
        let returned = planned.write(b"w");
        let observed_errno = unsafe { *slot };
        let owned = returned.unwrap_err();
        assert_eq!(
            observed_errno, sentinel,
            "Rust IO did not publish the OS code"
        );
        assert_eq!(owned.kind(), kind);
        assert_eq!(owned.raw_os_error(), Some(code));
        assert_eq!(planned.writes, 1);
    }

    #[test]
    fn os_error_is_preserved_across_actual_bio_write() {
        for (code, kind) in [
            (libc::EPIPE, io::ErrorKind::BrokenPipe),
            (libc::ECONNRESET, io::ErrorKind::ConnectionReset),
        ] {
            let sentinel = libc::ENOENT;
            assert_ne!(code, sentinel);
            direct_os_control(code, kind, sentinel);
            let fixture = Fixture::new(Failure::Os(code));
            let (returned, observed_errno) = fixture.write(sentinel);
            assert_eq!(returned, -1);
            assert_eq!(
                observed_errno, code,
                "the actual C BIO callback publishes its OS error"
            );
            assert_eq!(fixture.retry_flags(), 0);
            let owned = fixture.owned_error();
            assert_eq!(owned.kind(), kind);
            assert_eq!(owned.raw_os_error(), Some(code));
        }
    }

    #[test]
    fn raw_retry_error_keeps_bio_retry_and_owned_errno() {
        let code = libc::EAGAIN;
        let sentinel = libc::ENOENT;
        assert_ne!(code, sentinel);
        direct_os_control(code, io::ErrorKind::WouldBlock, sentinel);
        let fixture = Fixture::new(Failure::Os(code));
        let (returned, observed_errno) = fixture.write(sentinel);
        assert_eq!(returned, -1);
        assert_eq!(observed_errno, code);
        assert_eq!(
            fixture.retry_flags(),
            ffi::BIO_FLAGS_WRITE | ffi::BIO_FLAGS_SHOULD_RETRY
        );
        let owned = fixture.owned_error();
        assert_eq!(owned.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(owned.raw_os_error(), Some(code));
    }

    #[derive(Debug)]
    struct Payload(Arc<()>);
    impl std::fmt::Display for Payload {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("synthetic owned BIO error")
        }
    }
    impl std::error::Error for Payload {}

    #[test]
    fn custom_error_keeps_original_payload_without_an_os_code() {
        let identity = Arc::new(());
        let failure = io::Error::new(io::ErrorKind::Other, Payload(identity.clone()));
        assert_eq!(failure.raw_os_error(), None);
        let fixture = Fixture::new(Failure::Custom(failure));
        let sentinel = libc::ENOENT;
        let (returned, observed_errno) = fixture.write(sentinel);
        assert_eq!(returned, -1);
        assert_eq!(
            observed_errno, sentinel,
            "rawNone does not invent an OS error"
        );
        assert_eq!(fixture.retry_flags(), 0);
        let owned = fixture.owned_error();
        assert_eq!(owned.kind(), io::ErrorKind::Other);
        assert_eq!(owned.raw_os_error(), None);
        let payload = owned
            .get_ref()
            .unwrap()
            .downcast_ref::<Payload>()
            .expect("retain the original payload type");
        assert!(Arc::ptr_eq(&payload.0, &identity));
        assert_eq!(Arc::strong_count(&identity), 2);
        drop(owned);
        assert_eq!(Arc::strong_count(&identity), 1);
    }
}
