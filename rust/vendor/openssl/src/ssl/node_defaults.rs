//! Scoped public ex-data hook for the reviewed OpenSSL 3.6.3 constructor order.
//! Groups/CERT exist before ex-data new callbacks; system_default runs afterwards.
//! This is not a cross-version pre-configuration API or an arbitrary-plugin contract.
use std::cell::Cell;
use std::ffi::{c_char, c_int, c_long, c_void, CStr};
use std::marker::PhantomData;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::OnceLock;

use super::{SslContextBuilder, SslMethod};
use crate::error::ErrorStack;

// Node 22.14.0/OpenSSL 3.0.15 default-provider list. Do not inherit 3.6 ML-KEM.
const GROUPS: &CStr =
    c"X25519:P-256:X448:P-521:P-384:ffdhe2048:ffdhe3072:ffdhe4096:ffdhe6144:ffdhe8192";
static INDEX: OnceLock<Result<c_int, ErrorStack>> = OnceLock::new();

#[derive(Clone, Copy)]
struct Scope {
    groups: *const c_char,
    parent: *mut ffi::SSL_CTX,
    calls: u8,
    succeeded: bool,
}
thread_local! {
    static ACTIVE: Cell<Option<Scope>> = const { Cell::new(None) };
}

// Never transfer a thread's armed scope or outlive its borrowed Groups string.
struct Guard<'a> {
    armed: bool,
    _borrow: PhantomData<&'a CStr>,
    _thread: PhantomData<*mut ()>,
}

fn failure() -> ErrorStack {
    ErrorStack::internal_error("Node-default context construction rejected")
}

fn register() -> Result<(), ErrorStack> {
    if !crate::node_init::initialized_successfully() || crate::version::number() != 0x3060_0030 {
        return Err(failure());
    }
    INDEX
        .get_or_init(|| unsafe {
            let index = ffi::CRYPTO_get_ex_new_index(
                ffi::CRYPTO_EX_INDEX_SSL_CTX,
                0,
                ptr::null_mut(),
                Some(created),
                None,
                None,
            );
            if index < 0 {
                Err(ErrorStack::get_initialized())
            } else {
                Ok(index)
            }
        })
        .as_ref()
        .map(|_| ())
        .map_err(Clone::clone)
}

impl<'a> Guard<'a> {
    fn begin(groups: &'a CStr) -> Result<Self, ErrorStack> {
        let armed = ACTIVE.try_with(|slot| {
            if slot.get().is_some() {
                return false;
            }
            slot.set(Some(Scope {
                groups: groups.as_ptr(),
                parent: ptr::null_mut(),
                calls: 0,
                succeeded: false,
            }));
            true
        });
        if armed != Ok(true) {
            return Err(failure());
        }
        Ok(Self {
            armed: true,
            _borrow: PhantomData,
            _thread: PhantomData,
        })
    }

    // The constructor owns its pointer even on a callback/postcondition failure.
    unsafe fn finish(
        mut self,
        pointer: *mut ffi::SSL_CTX,
    ) -> Result<SslContextBuilder, ErrorStack> {
        let state = ACTIVE.try_with(|slot| slot.replace(None)).ok().flatten();
        self.armed = false;
        if pointer.is_null() {
            return Err(ErrorStack::get_initialized());
        }
        let context = SslContextBuilder::from_ptr(pointer);
        if !state.is_some_and(|s| s.calls == 1 && s.succeeded && s.parent == pointer) {
            // Drop the owned context while unarmed. Do not leak failed-setter
            // errors into another operation's queue, including after a retry.
            let _ = ErrorStack::get_initialized();
            return Err(failure());
        }
        Ok(context)
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = ACTIVE.try_with(|slot| slot.set(None));
        }
    }
}

unsafe extern "C" fn created(
    parent: *mut c_void,
    _: *mut c_void,
    _: *mut ffi::CRYPTO_EX_DATA,
    _: c_int,
    _: c_long,
    _: *mut c_void,
) {
    run_callback(parent.cast(), |context, groups| {
        // Only the reviewed initialized fields and public setters are accessed.
        ffi::SSL_CTX_set_security_level(context, 1);
        ffi::SSL_CTX_set1_groups_list(context, groups) > 0
    });
}

fn run_callback(
    parent: *mut ffi::SSL_CTX,
    configure: impl FnOnce(*mut ffi::SSL_CTX, *const c_char) -> bool,
) {
    let scope = ACTIVE
        .try_with(|slot| {
            let mut scope = slot.get()?;
            // Consume before calling OpenSSL: a recursive or duplicate callback may
            // invalidate this construction but cannot apply these defaults again.
            scope.calls = scope.calls.saturating_add(1);
            if scope.calls != 1 || parent.is_null() {
                scope.succeeded = false;
                slot.set(Some(scope));
                return None;
            }
            scope.parent = parent;
            slot.set(Some(scope));
            Some(scope)
        })
        .ok()
        .flatten();
    let Some(scope) = scope else { return };
    // No borrow/lock crosses C, and Rust unwinding must not cross this callback.
    // panic=abort, foreign unwinding and arbitrary other callbacks are separate.
    let succeeded = matches!(
        catch_unwind(AssertUnwindSafe(|| configure(parent, scope.groups))),
        Ok(true)
    );
    let _ = ACTIVE.try_with(|slot| {
        if let Some(mut current) = slot.get() {
            if current.calls == 1 && current.parent == parent {
                current.succeeded = succeeded;
                slot.set(Some(current));
            }
        }
    });
}

fn construct(
    groups: &CStr,
    constructor: impl FnOnce() -> *mut ffi::SSL_CTX,
) -> Result<SslContextBuilder, ErrorStack> {
    register()?;
    let guard = Guard::begin(groups)?;
    let context = constructor();
    unsafe { guard.finish(context) }
}

pub(super) fn new_client() -> Result<SslContextBuilder, ErrorStack> {
    construct(GROUPS, || unsafe {
        ffi::SSL_CTX_new(SslMethod::tls_client().as_ptr())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use foreign_types::ForeignType;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    struct DropCount(Arc<AtomicUsize>);
    impl Drop for DropCount {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn ready() {
        crate::init_without_config().unwrap();
        register().unwrap();
    }
    fn clear() {
        assert!(ACTIVE.with(|slot| slot.get().is_none()));
    }
    fn raw() -> *mut ffi::SSL_CTX {
        unsafe { ffi::SSL_CTX_new(SslMethod::tls_client().as_ptr()) }
    }

    // Execute alone in a fresh process. Ordinary test returns are not evidence
    // of either guard: the owner supplies only its synthetic configuration file.
    #[test]
    fn first_use_gate_child() {
        let Ok(mode) = std::env::var("AUTOROUTER_SYNTHETIC_NODE_DEFAULTS_INIT") else {
            return;
        };
        assert!(!crate::node_init::initialized_successfully());
        assert!(new_client().is_err());
        assert!(INDEX.get().is_none());
        match mode.as_str() {
            "success" => {
                crate::init_without_config().unwrap();
                assert_eq!(new_client().unwrap().build().security_level(), 1);
            }
            "failure" => {
                let filename = std::ffi::CString::new(
                    std::env::var("AUTOROUTER_SYNTHETIC_NODE_DEFAULTS_FILE").unwrap(),
                )
                .unwrap();
                assert!(crate::init_with_config_first_use(Some(&filename), false).is_err());
                assert!(new_client().is_err());
                assert!(INDEX.get().is_none());
            }
            _ => panic!("unknown owned first-use gate mode"),
        }
        clear();
        eprintln!("Node-default first-use gate verified");
    }

    #[test]
    fn two_live_contexts_bind_distinct_parents_and_leave_unarmed_defaults() {
        ready();
        let first = new_client().unwrap();
        let second = new_client().unwrap();
        assert_ne!(first.as_ptr(), second.as_ptr());
        for context in [first, second] {
            assert_eq!(context.build().security_level(), 1);
        }
        assert_eq!(
            SslContextBuilder::new(SslMethod::tls_client())
                .unwrap()
                .build()
                .security_level(),
            2
        );
        clear();
    }

    #[test]
    fn invalid_groups_reject_real_context_and_allow_clean_retry() {
        ready();
        let drops = Arc::new(AtomicUsize::new(0));
        let index = super::super::SslContext::new_ex_index::<DropCount>().unwrap();
        assert!(construct(c"not-a-real-group", || unsafe {
            let pointer = raw();
            assert!(!pointer.is_null());
            let mut context = SslContextBuilder::from_ptr(pointer);
            context.set_ex_data(index, DropCount(Arc::clone(&drops)));
            let pointer = context.as_ptr();
            std::mem::forget(context);
            pointer
        })
        .is_err());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        clear();
        assert!(ErrorStack::get_initialized().errors().is_empty());
        assert_eq!(new_client().unwrap().build().security_level(), 1);
    }

    #[test]
    fn real_null_method_and_owned_post_callback_failure_clear_scope() {
        ready();
        assert!(construct(GROUPS, || unsafe { ffi::SSL_CTX_new(ptr::null()) }).is_err());
        clear();
        // Labelled post-callback failure seam; not a claim to induce real OOM.
        assert!(construct(GROUPS, || unsafe {
            let context = raw();
            assert!(!context.is_null());
            ffi::SSL_CTX_free(context);
            ptr::null_mut()
        })
        .is_err());
        clear();
        assert_eq!(new_client().unwrap().build().security_level(), 1);
    }

    #[test]
    fn nested_guard_rejects_before_a_second_constructor_and_preserves_outer() {
        ready();
        let outer = construct(GROUPS, || {
            assert!(construct(GROUPS, || panic!("nested constructor must not run")).is_err());
            raw()
        })
        .unwrap();
        assert_eq!(outer.build().security_level(), 1);
        clear();
    }

    #[test]
    fn synchronized_threads_keep_invalid_valid_and_unarmed_scopes_independent() {
        ready();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let context = construct(c"not-a-real-group", || {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                raw()
            });
            assert!(context.is_err());
            clear();
            assert_eq!(new_client().unwrap().build().security_level(), 1);
        });
        // A failing main assertion still releases and joins the owned worker.
        struct Worker(
            Option<mpsc::Sender<()>>,
            Option<std::thread::JoinHandle<()>>,
        );
        impl Drop for Worker {
            fn drop(&mut self) {
                if let Some(release) = self.0.take() {
                    let _ = release.send(());
                }
                if let Some(handle) = self.1.take() {
                    let _ = handle.join();
                }
            }
        }
        let mut owner = Worker(Some(release_tx), Some(worker));
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(new_client().unwrap().build().security_level(), 1);
        assert_eq!(
            SslContextBuilder::new(SslMethod::tls_client())
                .unwrap()
                .build()
                .security_level(),
            2
        );
        owner.0.take().unwrap().send(()).unwrap();
        owner.1.take().unwrap().join().unwrap();
        clear();
    }

    #[test]
    fn wrong_missing_and_duplicate_callback_records_reject_owned_contexts() {
        ready();
        let other = SslContextBuilder::new(SslMethod::tls_client())
            .unwrap()
            .build();
        let drops = Arc::new(AtomicUsize::new(0));
        let index = super::super::SslContext::new_ex_index::<DropCount>().unwrap();
        for mode in 0..3 {
            assert!(construct(GROUPS, || {
                let context = raw();
                assert!(!context.is_null());
                let mut owned = unsafe { SslContextBuilder::from_ptr(context) };
                owned.set_ex_data(index, DropCount(Arc::clone(&drops)));
                std::mem::forget(owned);
                if mode == 2 {
                    let called = Cell::new(false);
                    run_callback(context, |_, _| {
                        called.set(true);
                        true
                    });
                    assert!(!called.get(), "duplicate callback must not configure");
                } else {
                    // Record-only seams using two live owned context pointers.
                    ACTIVE.with(|slot| {
                        let mut state = slot.get().unwrap();
                        if mode == 0 {
                            state.parent = other.as_ptr();
                        } else {
                            state.calls = 0;
                        }
                        slot.set(Some(state));
                    });
                }
                context
            })
            .is_err());
            assert_eq!(drops.load(Ordering::SeqCst), mode + 1);
            clear();
        }
        assert_eq!(other.security_level(), 2);
        assert_eq!(new_client().unwrap().build().security_level(), 1);
    }

    #[test]
    fn callback_panic_is_contained_and_constructor_unwind_drops_scope() {
        ready();
        let context = SslContextBuilder::new(SslMethod::tls_client()).unwrap();
        let guard = Guard::begin(GROUPS).unwrap();
        run_callback(context.as_ptr(), |_, _| panic!("synthetic callback panic"));
        assert!(!ACTIVE.with(|slot| slot.get().unwrap().succeeded));
        drop(guard);
        clear();
        assert!(catch_unwind(|| {
            let _ = construct(GROUPS, || panic!("synthetic constructor unwind"));
        })
        .is_err());
        clear();
        assert_eq!(new_client().unwrap().build().security_level(), 1);
    }
}
