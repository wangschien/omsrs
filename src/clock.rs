//! Clock abstraction (PORT-PLAN §6 D4).
//!
//! Models pendulum's "travel_to / freeze" testability as a trait object
//! injected at construction time. Every type that pre-v5 omspy read
//! `pendulum.now()` from — `Order`, `CompoundOrder`, `OrderStrategy`,
//! `VirtualBroker`, `OrderLock` — holds an `Arc<dyn Clock + Send + Sync>`
//! with `clock_system_default()` as its serde-skip default.

use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;

pub trait Clock: Send + Sync + fmt::Debug {
    fn now(&self) -> DateTime<Utc>;
}

/// Production clock — thin wrapper over `chrono::Utc::now`.
#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Test clock — a `Mutex<DateTime<Utc>>` that can be frozen and advanced.
/// Cloning a `MockClock` gives a new handle pointing at the same instant so
/// multiple owners can observe / mutate the same frozen timeline.
#[derive(Debug, Clone)]
pub struct MockClock {
    inner: Arc<Mutex<DateTime<Utc>>>,
}

impl MockClock {
    pub fn new(t: DateTime<Utc>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(t)),
        }
    }

    pub fn set(&self, t: DateTime<Utc>) {
        *self.inner.lock() = t;
    }

    pub fn advance(&self, d: Duration) {
        let mut guard = self.inner.lock();
        *guard += d;
    }
}

impl Clock for MockClock {
    fn now(&self) -> DateTime<Utc> {
        *self.inner.lock()
    }
}

/// Serde-compatible default for `Arc<dyn Clock + Send + Sync>` fields. Used
/// via `#[serde(skip, default = "clock_system_default")]` on every type that
/// carries a clock.
pub fn clock_system_default() -> Arc<dyn Clock + Send + Sync> {
    Arc::new(SystemClock)
}
