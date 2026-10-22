//! Observation only. A selected connection is not a request ownership lease.
//!
//! In particular, this module intentionally has no abort, poison, session-evict
//! or generation-claim operation. A delayed capture observer cannot prove the
//! request remains assigned when a pooled connection has already been reused.
use std::sync::{Arc, Weak};

use hyper::http::Extensions;
use hyper_util::client::legacy::connect::CaptureConnection;

pub(super) struct ConnectionLifetime;

// No endpoint, TLS/session material, pointer formatting, or strong IO ownership.
#[derive(Clone)]
pub(super) struct ConnectionIdentity(Weak<ConnectionLifetime>);

impl ConnectionIdentity {
    pub(super) fn new(lifetime: &Arc<ConnectionLifetime>) -> Self {
        Self(Arc::downgrade(lifetime))
    }

    pub(super) fn captured(capture: &CaptureConnection) -> Option<Self> {
        let metadata = capture.connection_metadata();
        let mut extensions = Extensions::new();
        metadata.as_ref()?.get_extras(&mut extensions);
        extensions.remove::<Self>()
    }

    pub(super) fn same_connection(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.0, &other.0)
    }

    pub(super) fn is_alive(&self) -> bool {
        self.0.strong_count() != 0
    }
}

#[test]
fn metadata_is_weak_and_distinguishes_connections_without_reusing_identity() {
    let first = Arc::new(ConnectionLifetime);
    let second = Arc::new(ConnectionLifetime);
    let identity = ConnectionIdentity::new(&first);
    let copy = identity.clone();
    assert!(identity.same_connection(&copy));
    assert!(!identity.same_connection(&ConnectionIdentity::new(&second)));
    assert_eq!(Arc::strong_count(&first), 1);
    drop(first);
    assert!(!identity.is_alive());
    assert!(!copy.is_alive());
    // Keeping the Weak retains the allocation identity, preventing address
    // reuse from turning a later connection into the old connection's identity.
    assert!(identity.same_connection(&copy));
}
