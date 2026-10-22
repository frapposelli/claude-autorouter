//! Contain optional observer failures without printing private panic payloads.
//!
//! The executable initializes the delegating hook before starting workers.
//! Embedding hosts must explicitly do the same after installing their own hook,
//! before starting concurrent work, and must not replace it afterward. Rust's
//! hook is process-global; swapping it around individual callbacks is unsafe.
use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Once;

thread_local! {
    static OBSERVER_SCOPE: Cell<bool> = const { Cell::new(false) };
}
static INITIALIZE: Once = Once::new();

/// An ordinary observer panic; the payload is deliberately unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallbackPanicked;

/// Explicit host startup operation; unrelated panics retain the previous hook.
///
/// Call before creating worker threads. `take_hook`/`set_hook` cannot atomically
/// replace a hook on stable Rust. A later host replacement revokes this guard.
pub fn initialize_observer_privacy() {
    INITIALIZE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !OBSERVER_SCOPE.with(Cell::get) {
                previous(info);
            }
        }));
    });
}

struct Scope(bool);
impl Scope {
    fn enter() -> Self {
        Self(OBSERVER_SCOPE.with(|value| value.replace(true)))
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        OBSERVER_SCOPE.with(|value| value.set(self.0));
    }
}

/// Catch an ordinary unwinding callback panic, with no payload in the result.
///
/// Scope is synchronous and thread-local. Futures must enter it for each poll,
/// never hold it across an await. Abort, a replaced/aborting hook, or a panic
/// payload with a panicking destructor are outside this containment contract.
pub fn contain_callback<T>(callback: impl FnOnce() -> T) -> Result<T, CallbackPanicked> {
    let _scope = Scope::enter();
    catch_unwind(AssertUnwindSafe(callback)).map_err(|_| CallbackPanicked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nesting_and_unwinding_restore_the_previous_scope() {
        assert!(!OBSERVER_SCOPE.with(Cell::get));
        assert_eq!(
            contain_callback(|| {
                assert!(OBSERVER_SCOPE.with(Cell::get));
                assert!(contain_callback(|| panic!("synthetic observer failure")).is_err());
                assert!(OBSERVER_SCOPE.with(Cell::get));
                42
            }),
            Ok(42)
        );
        assert!(!OBSERVER_SCOPE.with(Cell::get));
        assert!(contain_callback(|| panic!("synthetic observer failure")).is_err());
        assert!(!OBSERVER_SCOPE.with(Cell::get));
    }
}
