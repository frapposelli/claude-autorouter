//! AutoRouter's first-use initialization extension for OpenSSL 3.
//!
//! All users of these entrypoints share one immutable identity and result.
//! This cannot detect earlier initialization through third-party FFI or another
//! OpenSSL API. Call before any such use, and stop startup on error. Configuration
//! can load arbitrary modules/includes; callers own the selected configuration.
use std::ffi::{CStr, CString};
use std::ptr;
use std::sync::OnceLock;

use libc::{c_char, c_int, c_ulong};

use crate::error::ErrorStack;

#[derive(Clone, Eq, PartialEq)]
enum Request {
    NoConfig,
    Config {
        filename: Option<CString>,
        shared: bool,
    },
}

struct Initialized {
    request: Request,
    result: Result<(), ErrorStack>,
}
static INITIALIZED: OnceLock<Initialized> = OnceLock::new();

// Public OpenSSL 3 crypto.h/conf.h ABI. openssl-sys does not expose these
// settings functions. Keep this FFI in the audited supplier, not the runtime.
unsafe extern "C" {
    fn OPENSSL_INIT_new() -> *mut ffi::OPENSSL_INIT_SETTINGS;
    fn OPENSSL_INIT_free(settings: *mut ffi::OPENSSL_INIT_SETTINGS);
    fn OPENSSL_INIT_set_config_filename(
        settings: *mut ffi::OPENSSL_INIT_SETTINGS,
        filename: *const c_char,
    ) -> c_int;
    fn OPENSSL_INIT_set_config_appname(
        settings: *mut ffi::OPENSSL_INIT_SETTINGS,
        appname: *const c_char,
    ) -> c_int;
    fn OPENSSL_INIT_set_config_file_flags(
        settings: *mut ffi::OPENSSL_INIT_SETTINGS,
        flags: c_ulong,
    );
    fn OPENSSL_init_crypto(options: u64, settings: *const ffi::OPENSSL_INIT_SETTINGS) -> c_int;
}

struct Settings(*mut ffi::OPENSSL_INIT_SETTINGS);
impl Drop for Settings {
    fn drop(&mut self) {
        unsafe { OPENSSL_INIT_free(self.0) }
    }
}

/// Initializes OpenSSL without implicitly loading a configuration file.
///
/// Call before any independent OpenSSL use. Repeated identical requests return
/// the original outcome. A prior configuration request is an error, even if its
/// file was empty; an already selected initialization policy cannot be undone.
pub fn init_without_config() -> Result<(), ErrorStack> {
    initialize(Request::NoConfig)
}

/// Initializes OpenSSL using Node 22's configuration selection at first use.
///
/// `filename == None` delegates default filename resolution to OpenSSL (including
/// its empty OPENSSL_CONF behavior). Shared selects the `openssl_conf` key;
/// otherwise `nodejs_conf` is selected. Only IGNORE_MISSING_FILE is enabled: no
/// fallback section or ignored module failure. Environment variables are not
/// mutated. Repeated identical requests return the cached outcome without reload;
/// a conflicting path, appname or no-config request is rejected.
///
/// This must precede every independent OpenSSL API/FFI use; this extension cannot
/// detect or reverse initialization performed outside its coordinator. On error
/// callers must stop startup. This does not qualify any configuration as safe,
/// provide a FIPS provider, seed a language runtime, or verify certificates.
/// Configuration error data is copied with lossy UTF-8 replacement; it is not a
/// byte-preserving diagnostic API. Ordinary Error::get behavior is unchanged.
pub fn init_with_config_first_use(filename: Option<&CStr>, shared: bool) -> Result<(), ErrorStack> {
    initialize(Request::Config {
        filename: filename.map(CStr::to_owned),
        shared,
    })
}

fn initialize(request: Request) -> Result<(), ErrorStack> {
    let initialized = INITIALIZED.get_or_init(|| {
        let request = request.clone();
        let result = perform(&request);
        Initialized { request, result }
    });
    if initialized.request != request {
        return Err(ErrorStack::internal_error(
            "conflicting OpenSSL initialization request",
        ));
    }
    initialized.result.clone()
}

fn perform(request: &Request) -> Result<(), ErrorStack> {
    const NO_LOAD_CONFIG: u64 = 0x0000_0080;
    const LOAD_CONFIG: u64 = 0x0000_0040;
    const IGNORE_MISSING_FILE: c_ulong = 0x10;
    match request {
        Request::NoConfig => unsafe {
            if ffi::OPENSSL_init_ssl(
                NO_LOAD_CONFIG | ffi::OPENSSL_INIT_LOAD_SSL_STRINGS | ffi::OPENSSL_INIT_NO_ATEXIT,
                ptr::null(),
            ) <= 0
            {
                Err(ErrorStack::get_initialized())
            } else {
                Ok(())
            }
        },
        Request::Config { filename, shared } => unsafe {
            let settings = Settings(OPENSSL_INIT_new());
            if settings.0.is_null() {
                return Err(ErrorStack::internal_error(
                    "OpenSSL initialization settings allocation failed",
                ));
            }
            if OPENSSL_INIT_set_config_filename(
                settings.0,
                filename.as_ref().map_or(ptr::null(), |name| name.as_ptr()),
            ) <= 0
                || OPENSSL_INIT_set_config_appname(
                    settings.0,
                    if *shared {
                        c"openssl_conf".as_ptr()
                    } else {
                        c"nodejs_conf".as_ptr()
                    },
                ) <= 0
            {
                return Err(ErrorStack::internal_error(
                    "OpenSSL initialization settings assignment failed",
                ));
            }
            OPENSSL_INIT_set_config_file_flags(settings.0, IGNORE_MISSING_FILE);
            // Like Node, queued errors determine configuration failure, not
            // this function's return code. Capture on this thread before any
            // implicit ffi::init or other OpenSSL operation can touch the queue.
            OPENSSL_init_crypto(LOAD_CONFIG, settings.0);
            drop(settings);
            let errors = ErrorStack::get_initialized();
            if !errors.errors().is_empty() {
                return Err(errors);
            }
            // The configuration once gate has now been selected. Complete the
            // ordinary rust-openssl SSL initialization without reloading it.
            ffi::init();
            Ok(())
        },
    }
}
