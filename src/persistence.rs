//! Persistence trait (PORT-PLAN source-notes §13).
//!
//! `PersistenceHandle` + `Option<Arc<dyn PersistenceHandle>>` on `Order` and
//! `CompoundOrder` are declared **unconditionally** so every call site on
//! the Order lifecycle can no-op when `None`. The SQLite implementation
//! lives behind `#[cfg(feature = "persistence")]` in `sqlite.rs`.

use std::collections::HashMap;

use serde_json::Value;

pub trait PersistenceHandle: Send + Sync + std::fmt::Debug {
    fn upsert_order(&self, row: HashMap<String, Value>) -> Result<(), PersistenceError>;
}

#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("backend error: {0}")]
    Backend(String),
}

#[cfg(feature = "persistence")]
pub mod sqlite {
    //! SQLite-backed `PersistenceHandle`. Lands in R3.b.
}
