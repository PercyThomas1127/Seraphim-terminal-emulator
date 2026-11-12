//! # seraphim-events
//!
//! The Seraphim **Event Bus** (spec §11): a publish/subscribe hub connecting
//! subsystems without direct dependencies.
//!
//! Design constraints taken from the spec:
//!
//! * **Publishing never blocks.** The terminal I/O path publishes events; a
//!   slow Lua script must not stall it (spec §13 "Scripts cannot block
//!   rendering"). Each subscription has a bounded queue; when it is full the
//!   event is dropped *for that subscriber only* and counted.
//! * **Filtering at the source.** Subscribers declare a [`Pattern`]
//!   (`"package.install.*"`, `"*"`, or an exact name) so they never see
//!   events they did not ask for.
//! * **Trusted provenance.** `source` is set by the publisher handle, not by
//!   event payloads, so extensions cannot spoof e.g. `session.privileged`.
//!
//! Priorities, cancellation and async handlers (spec §11.3) belong to the
//! scripting dispatcher built on top of this bus in Phase III.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, RwLock, Weak};
use std::time::{Duration, SystemTime};

pub use serde_json::{json, Value};

/// A single event (spec §11.1).
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// Dotted name, e.g. `command.completed`.
    pub name: String,
    pub timestamp: SystemTime,
    /// Publishing subsystem, e.g. `terminal`, `package`. Set by [`Publisher`].
    pub source: String,
    /// Session the event concerns, if any.
    pub session: Option<u64>,
    pub data: Value,
}

/// A subscription filter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// Everything.
    All,
    /// `prefix.*` — any event whose name starts with `prefix.`.
    Prefix(String),
    Exact(String),
    /// Any of these (one subscription for a set of patterns, so an event
    /// matching several of them arrives once).
    Any(Vec<Pattern>),
}

impl Pattern {
    pub fn parse(s: &str) -> Pattern {
        if s == "*" || s.is_empty() {
            Pattern::All
        } else if let Some(p) = s.strip_suffix(".*") {
            Pattern::Prefix(format!("{p}."))
        } else {
            Pattern::Exact(s.to_owned())
        }
    }

    pub fn matches(&self, name: &str) -> bool {
        match self {
            Pattern::All => true,
            Pattern::Prefix(p) => name.starts_with(p.as_str()),
            Pattern::Exact(e) => e == name,
            Pattern::Any(ps) => ps.iter().any(|p| p.matches(name)),
        }
    }
}

impl From<&str> for Pattern {
    fn from(s: &str) -> Self {
        Pattern::parse(s)
    }
}

struct Sub {
    id: u64,
    pattern: Pattern,
    tx: SyncSender<Arc<Event>>,
    dropped: Arc<AtomicU64>,
}

#[derive(Default)]
struct Inner {
    subs: RwLock<Vec<Sub>>,
    next_id: AtomicU64,
    published: AtomicU64,
}

/// Cloneable handle to the bus.
#[derive(Clone, Default)]
pub struct EventBus {
    inner: Arc<Inner>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe with a bounded queue of `capacity` events.
    pub fn subscribe(&self, pattern: impl Into<Pattern>, capacity: usize) -> Subscription {
        let (tx, rx) = mpsc::sync_channel(capacity.max(1));
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let dropped = Arc::new(AtomicU64::new(0));
        self.inner.subs.write().unwrap().push(Sub {
            id,
            pattern: pattern.into(),
            tx,
            dropped: dropped.clone(),
        });
        Subscription { id, rx, dropped, bus: Arc::downgrade(&self.inner) }
    }

    /// A publishing handle stamped with a fixed, trusted `source`.
    pub fn publisher(&self, source: impl Into<String>) -> Publisher {
        Publisher { bus: self.clone(), source: source.into() }
    }

    /// Deliver to all matching subscribers without blocking. Returns the
    /// number of subscribers that accepted the event.
    fn publish(&self, event: Event) -> usize {
        self.inner.published.fetch_add(1, Ordering::Relaxed);
        let event = Arc::new(event);
        let mut delivered = 0;
        let mut dead = Vec::new();
        {
            let subs = self.inner.subs.read().unwrap();
            for sub in subs.iter().filter(|s| s.pattern.matches(&event.name)) {
                match sub.tx.try_send(event.clone()) {
                    Ok(()) => delivered += 1,
                    Err(TrySendError::Full(_)) => {
                        sub.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(TrySendError::Disconnected(_)) => dead.push(sub.id),
                }
            }
        }
        if !dead.is_empty() {
            // Receiver gone without running Drop (e.g. leaked); prune.
            self.inner.subs.write().unwrap().retain(|s| !dead.contains(&s.id));
        }
        delivered
    }

    /// Whether anyone listens for `name`: lets publishers skip building
    /// costly payloads (output text) nobody asked for.
    pub fn wants(&self, name: &str) -> bool {
        self.inner.subs.read().unwrap().iter().any(|s| s.pattern.matches(name))
    }

    pub fn subscriber_count(&self) -> usize {
        self.inner.subs.read().unwrap().len()
    }

    pub fn published_count(&self) -> u64 {
        self.inner.published.load(Ordering::Relaxed)
    }
}

/// Publishes events with a fixed `source`.
#[derive(Clone)]
pub struct Publisher {
    bus: EventBus,
    source: String,
}

impl Publisher {
    /// A publisher on the same bus for another (trusted) subsystem.
    pub fn with_source(&self, source: &str) -> Publisher {
        Publisher { bus: self.bus.clone(), source: source.to_owned() }
    }

    pub fn publish(&self, name: impl Into<String>, session: Option<u64>, data: Value) -> usize {
        self.bus.publish(Event {
            name: name.into(),
            timestamp: SystemTime::now(),
            source: self.source.clone(),
            session,
            data,
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// See [`EventBus::wants`].
    pub fn wants(&self, name: &str) -> bool {
        self.bus.wants(name)
    }
}

/// Receiving end of a subscription. Unsubscribes on drop.
pub struct Subscription {
    id: u64,
    rx: Receiver<Arc<Event>>,
    dropped: Arc<AtomicU64>,
    bus: Weak<Inner>,
}

impl Subscription {
    pub fn try_recv(&self) -> Option<Arc<Event>> {
        self.rx.try_recv().ok()
    }

    pub fn recv(&self) -> Option<Arc<Event>> {
        self.rx.recv().ok()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<Arc<Event>, RecvTimeoutError> {
        self.rx.recv_timeout(timeout)
    }

    /// Drain whatever is queued right now.
    pub fn drain(&self) -> Vec<Arc<Event>> {
        self.rx.try_iter().collect()
    }

    /// A handle that changes this subscription's pattern from any thread.
    pub fn pattern_handle(&self) -> PatternHandle {
        PatternHandle { id: self.id, bus: self.bus.clone() }
    }

    /// Events discarded because this subscriber's queue was full.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Changes a subscription's pattern (see [`Subscription::pattern_handle`]).
#[derive(Clone)]
pub struct PatternHandle {
    id: u64,
    bus: Weak<Inner>,
}

impl PatternHandle {
    pub fn set(&self, pattern: Pattern) {
        if let Some(inner) = self.bus.upgrade() {
            if let Some(s) = inner.subs.write().unwrap().iter_mut().find(|s| s.id == self.id) {
                s.pattern = pattern;
            }
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(inner) = self.bus.upgrade() {
            inner.subs.write().unwrap().retain(|s| s.id != self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        let p = Pattern::parse("package.install.*");
        assert!(p.matches("package.install.started"));
        assert!(p.matches("package.install.progress.detail"));
        assert!(!p.matches("package.installer"));
        assert!(!p.matches("package.remove.started"));
        assert!(Pattern::parse("*").matches("anything"));
        assert!(Pattern::parse("command.completed").matches("command.completed"));
        assert!(!Pattern::parse("command.completed").matches("command.completed.x"));
    }

    #[test]
    fn filtered_delivery_and_source_stamping() {
        let bus = EventBus::new();
        let all = bus.subscribe("*", 16);
        let pkgs = bus.subscribe("package.*", 16);
        let p = bus.publisher("package");
        assert_eq!(p.publish("package.install.started", None, json!({"package": "firefox"})), 2);
        bus.publisher("terminal").publish("terminal.bell", Some(1), Value::Null);

        let g = pkgs.drain();
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].source, "package");
        assert_eq!(g[0].data["package"], "firefox");
        assert_eq!(all.drain().len(), 2);
    }

    #[test]
    fn slow_subscriber_never_blocks_publisher() {
        let bus = EventBus::new();
        let slow = bus.subscribe("*", 2);
        let fast = bus.subscribe("*", 1000);
        let p = bus.publisher("test");
        let start = std::time::Instant::now();
        for i in 0..500 {
            p.publish("tick", None, json!(i));
        }
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(slow.drain().len(), 2);
        assert_eq!(slow.dropped(), 498);
        assert_eq!(fast.drain().len(), 500);
        assert_eq!(fast.dropped(), 0);
    }

    #[test]
    fn dropping_subscription_unsubscribes() {
        let bus = EventBus::new();
        let s = bus.subscribe("*", 1);
        assert_eq!(bus.subscriber_count(), 1);
        drop(s);
        assert_eq!(bus.subscriber_count(), 0);
        assert_eq!(bus.publisher("x").publish("e", None, Value::Null), 0);
    }

    #[test]
    fn any_patterns_and_retargeting() {
        let bus = EventBus::new();
        let s = bus.subscribe(Pattern::Any(vec!["a.*".into(), "a.b".into()]), 8);
        let p = bus.publisher("t");
        p.publish("a.b", None, Value::Null);
        assert_eq!(s.drain().len(), 1, "matches two patterns, delivered once");
        assert!(bus.wants("a.c") && !bus.wants("b"));
        s.pattern_handle().set(Pattern::parse("b"));
        p.publish("a.b", None, Value::Null);
        p.publish("b", None, Value::Null);
        assert_eq!(s.drain().len(), 1);
        assert!(!bus.wants("a.c"));
    }

    #[test]
    fn cross_thread() {
        let bus = EventBus::new();
        let sub = bus.subscribe("work.*", 64);
        let p = bus.publisher("worker");
        let h = std::thread::spawn(move || {
            for i in 0..10 {
                p.publish("work.item", None, json!(i));
            }
        });
        h.join().unwrap();
        let got: Vec<i64> = sub.drain().iter().map(|e| e.data.as_i64().unwrap()).collect();
        assert_eq!(got, (0..10).collect::<Vec<_>>());
    }
}
