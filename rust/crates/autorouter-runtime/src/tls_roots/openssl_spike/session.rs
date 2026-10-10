//! Raw HTTPS cache ownership only; fetch's GC-dependent cache remains separate.
use std::collections::VecDeque;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};

use openssl::ssl::ContextBoundSession;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct PolicyId(pub u64);

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Verification {
    Required,
    ExplicitlyDisabled,
}

#[derive(Clone, Eq, PartialEq)]
pub(super) struct Key {
    pub policy: PolicyId,
    pub origin: String,
    pub server_name: Option<String>,
    pub verification: Verification,
}

// Neither keys nor values implement Debug: sessions and endpoint identities do
// not belong in routine diagnostics. Clone shares immutable snapshot ownership.
struct Entry<T> {
    key: Key,
    value: T,
    bytes: usize,
}

pub(super) struct Cache<T> {
    capacity: usize,
    entries: VecDeque<Entry<T>>,
    bytes: usize,
    peak_bytes: usize,
}
impl<T: Clone> Cache<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: VecDeque::new(),
            bytes: 0,
            peak_bytes: 0,
        }
    }
    pub fn get(&self, key: &Key) -> Option<T> {
        self.entries
            .iter()
            .find(|entry| entry.key == *key)
            .map(|entry| entry.value.clone())
    }
    fn put(&mut self, key: Key, value: T, bytes: usize) {
        if self.capacity == 0 {
            return;
        }
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.key == key) {
            self.bytes -= entry.bytes;
            entry.value = value;
            entry.bytes = bytes;
        } else {
            if self.entries.len() == self.capacity {
                self.bytes -= self.entries.pop_front().unwrap().bytes;
            }
            self.entries.push_back(Entry { key, value, bytes });
        }
        self.bytes += bytes;
        self.peak_bytes = self.peak_bytes.max(self.bytes);
    }
    fn evict(&mut self, key: &Key) {
        if let Some(index) = self.entries.iter().position(|entry| entry.key == *key) {
            self.bytes -= self.entries.remove(index).unwrap().bytes;
        }
    }
    pub fn snapshot(&self) -> (usize, usize, usize) {
        (self.entries.len(), self.bytes, self.peak_bytes)
    }
}

pub(super) type RawCache = Arc<Mutex<Cache<ContextBoundSession>>>;
pub(super) type TicketState = Connection<ContextBoundSession>;

enum Phase<T> {
    Pending(Option<(T, usize)>),
    Accepted,
    Rejected,
}

pub(super) struct Connection<T> {
    cache: Weak<Mutex<Cache<T>>>,
    key: Key,
    phase: Mutex<Phase<T>>,
    error_closed: AtomicBool,
}
impl<T: Clone> Connection<T> {
    pub fn new(cache: &Arc<Mutex<Cache<T>>>, key: Key) -> Self {
        Self {
            cache: Arc::downgrade(cache),
            key,
            phase: Mutex::new(Phase::Pending(None)),
            error_closed: AtomicBool::new(false),
        }
    }
    pub fn lookup(&self) -> Option<T> {
        self.cache.upgrade()?.lock().unwrap().get(&self.key)
    }
    pub fn ticket(&self, ticket: T, bytes: usize) {
        let mut phase = self.phase.lock().unwrap();
        match &mut *phase {
            Phase::Pending(last) => *last = Some((ticket, bytes)),
            Phase::Accepted => {
                if let Some(cache) = self.cache.upgrade() {
                    cache.lock().unwrap().put(self.key.clone(), ticket, bytes);
                }
            }
            Phase::Rejected => {}
        }
    }
    // Called only after the connector's acceptance checks. No TLS operation or
    // await occurs while these locks are held, including from ticket callbacks.
    pub fn accept(&self) {
        let mut phase = self.phase.lock().unwrap();
        if let Phase::Pending(last) = &mut *phase {
            if let Some((ticket, bytes)) = last.take()
                && let Some(cache) = self.cache.upgrade()
            {
                cache.lock().unwrap().put(self.key.clone(), ticket, bytes);
            }
            *phase = Phase::Accepted;
        }
    }
    pub fn reject(&self) {
        *self.phase.lock().unwrap() = Phase::Rejected;
    }
    // Raw Node HTTPS closes with hadError => evict by key, even when a newer
    // connection has replaced the entry. This is deliberately not generation-CAS.
    pub fn close_error(&self) {
        if self.error_closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut phase = self.phase.lock().unwrap();
        *phase = Phase::Rejected;
        if let Some(cache) = self.cache.upgrade() {
            cache.lock().unwrap().evict(&self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(origin: usize) -> Key {
        Key {
            policy: PolicyId(1),
            origin: format!("127.0.0.1:{origin}"),
            server_name: None,
            verification: Verification::Required,
        }
    }
    #[test]
    fn raw_fifo_replacement_preserves_order_and_accounts_bytes() {
        let mut cache = Cache::new(100);
        for n in 0..100 {
            cache.put(key(n), n, 2);
        }
        cache.put(key(0), 200, 5);
        assert_eq!(cache.snapshot(), (100, 203, 203));
        assert_eq!(cache.get(&key(0)), Some(200));
        cache.put(key(100), 100, 3);
        assert_eq!(cache.get(&key(0)), None);
        assert_eq!(cache.get(&key(1)), Some(1));
        assert_eq!(cache.snapshot(), (100, 201, 203));
        cache.evict(&key(1));
        assert_eq!(cache.snapshot(), (99, 199, 203));
        let mut disabled = Cache::new(0);
        disabled.put(key(0), 1, 3);
        assert_eq!(disabled.snapshot(), (0, 0, 0));
    }
    #[test]
    fn pending_latest_ticket_acceptance_rejection_and_late_error_are_distinct() {
        let cache = Arc::new(Mutex::new(Cache::new(100)));
        let old = Connection::new(&cache, key(0));
        old.ticket(1, 1);
        old.ticket(2, 2);
        assert_eq!(old.lookup(), None);
        old.accept();
        assert_eq!(old.lookup(), Some(2));
        old.ticket(3, 3);
        assert_eq!(old.lookup(), Some(3));
        let newer = Connection::new(&cache, key(0));
        newer.ticket(4, 4);
        newer.accept();
        old.close_error();
        assert_eq!(newer.lookup(), None);
        old.ticket(5, 5);
        old.accept();
        assert_eq!(newer.lookup(), None);
        let rejected = Connection::new(&cache, key(1));
        rejected.ticket(6, 6);
        rejected.reject();
        rejected.accept();
        rejected.ticket(7, 7);
        assert_eq!(rejected.lookup(), None);
        assert_eq!(cache.lock().unwrap().snapshot(), (0, 0, 4));
        newer.ticket(8, 8);
        old.close_error();
        assert_eq!(
            newer.lookup(),
            Some(8),
            "one error-close cannot evict twice"
        );
    }
    #[test]
    fn ordinary_connection_drop_releases_pending_ticket_without_evicting_accepted_cache() {
        use std::sync::atomic::AtomicUsize;
        struct Marker(Arc<AtomicUsize>);
        impl Drop for Marker {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicUsize::new(0));
        let cache = Arc::new(Mutex::new(Cache::new(100)));
        let accepted = Connection::new(&cache, key(0));
        accepted.ticket(Arc::new(Marker(dropped.clone())), 4);
        accepted.accept();
        let pending = Connection::new(&cache, key(0));
        pending.ticket(Arc::new(Marker(dropped.clone())), 8);
        drop(pending);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        drop(accepted);
        assert_eq!(cache.lock().unwrap().snapshot(), (1, 4, 4));
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        drop(cache);
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn typed_keys_isolate_origin_sni_policy_and_verification_and_state_is_weak() {
        let cache = Arc::new(Mutex::new(Cache::new(100)));
        let original = key(0);
        let connection = Connection::new(&cache, original.clone());
        connection.ticket(1, 1);
        connection.accept();
        for changed in [
            Key {
                origin: "127.0.0.1:1".into(),
                ..original.clone()
            },
            Key {
                server_name: Some("localhost".into()),
                ..original.clone()
            },
            Key {
                policy: PolicyId(2),
                ..original.clone()
            },
            Key {
                verification: Verification::ExplicitlyDisabled,
                ..original.clone()
            },
        ] {
            assert_eq!(Connection::new(&cache, changed).lookup(), None);
        }
        assert_eq!(Arc::strong_count(&cache), 1);
        drop(cache);
        assert_eq!(connection.lookup(), None);
        connection.ticket(2, 2);
        connection.close_error();
    }
}
