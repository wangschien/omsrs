//! Persistence trait (PORT-PLAN source-notes §13).
//!
//! `PersistenceHandle` + `Option<Arc<dyn PersistenceHandle>>` on `Order` and
//! `CompoundOrder` are declared **unconditionally** so every call site on
//! the Order lifecycle can no-op when `None`. The SQLite implementation
//! lives behind `#[cfg(feature = "persistence")]` in [`sqlite`].

use std::collections::HashMap;

use serde_json::Value;

pub trait PersistenceHandle: Send + Sync + std::fmt::Debug {
    fn upsert_order(&self, row: HashMap<String, Value>) -> Result<(), PersistenceError>;
}

#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("backend error: {0}")]
    Backend(String),

    #[error("unique constraint violated: {0}")]
    Unique(String),
}

#[cfg(feature = "persistence")]
pub mod sqlite {
    //! SQLite-backed `PersistenceHandle` (PORT-PLAN §7, behind `persistence`).
    //!
    //! Schema mirrors upstream `order.create_db()` 1:1 so round-tripped
    //! payloads deserialize back into `Order`. All locking goes through
    //! `parking_lot::Mutex<Connection>`; rusqlite's `Connection` is
    //! `Send` but not `Sync`, and we only do short-lived transactions.

    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;

    use parking_lot::Mutex;
    use rusqlite::{params_from_iter, Connection, ErrorCode};
    use serde_json::Value;

    use super::{PersistenceError, PersistenceHandle};

    /// Upstream `order.create_db()` schema — column list kept in insertion
    /// order so `query("select * from orders")` yields the same key order.
    const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS orders (
        symbol text, side text, quantity integer,
        id text primary key, parent_id text, timestamp text,
        order_type text, broker_timestamp text,
        exchange_timestamp text, order_id text,
        exchange_order_id text, price real,
        trigger_price real, average_price real,
        pending_quantity integer, filled_quantity integer,
        cancelled_quantity integer, disclosed_quantity integer,
        validity text, status text,
        expires_in integer, timezone text,
        client_id text, convert_to_market_after_expiry text,
        cancel_after_expiry text, retries integer, max_modifications integer,
        exchange text, tag string, can_peg integer,
        pseudo_id string, strategy_id string, portfolio_id string,
        JSON text, error text, is_multi integer,
        last_updated_at text
    )";

    pub const ORDER_COLUMNS: &[&str] = &[
        "symbol",
        "side",
        "quantity",
        "id",
        "parent_id",
        "timestamp",
        "order_type",
        "broker_timestamp",
        "exchange_timestamp",
        "order_id",
        "exchange_order_id",
        "price",
        "trigger_price",
        "average_price",
        "pending_quantity",
        "filled_quantity",
        "cancelled_quantity",
        "disclosed_quantity",
        "validity",
        "status",
        "expires_in",
        "timezone",
        "client_id",
        "convert_to_market_after_expiry",
        "cancel_after_expiry",
        "retries",
        "max_modifications",
        "exchange",
        "tag",
        "can_peg",
        "pseudo_id",
        "strategy_id",
        "portfolio_id",
        "JSON",
        "error",
        "is_multi",
        "last_updated_at",
    ];

    #[derive(Debug, Clone)]
    pub struct SqlitePersistenceHandle {
        inner: Arc<Mutex<Connection>>,
    }

    impl SqlitePersistenceHandle {
        pub fn in_memory() -> Result<Self, PersistenceError> {
            let conn = Connection::open_in_memory().map_err(backend)?;
            conn.execute(SCHEMA, []).map_err(backend)?;
            Ok(Self {
                inner: Arc::new(Mutex::new(conn)),
            })
        }

        pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, PersistenceError> {
            let conn = Connection::open(path).map_err(backend)?;
            conn.execute(SCHEMA, []).map_err(backend)?;
            Ok(Self {
                inner: Arc::new(Mutex::new(conn)),
            })
        }

        /// Insert an arbitrary dict into `orders`. Mirrors sqlite-utils'
        /// `con["orders"].insert(dict)` — the caller supplies whichever
        /// columns they want; unmentioned columns default to NULL.
        ///
        /// Fails with `PersistenceError::Unique` on primary-key conflict;
        /// other rusqlite errors bubble up as `Backend`.
        pub fn insert_raw(
            &self,
            row: HashMap<String, Value>,
        ) -> Result<(), PersistenceError> {
            if row.is_empty() {
                return Err(PersistenceError::Backend("empty row".into()));
            }
            let cols: Vec<&str> = row.keys().map(String::as_str).collect();
            let placeholders = vec!["?"; cols.len()].join(", ");
            let sql = format!(
                "INSERT INTO orders ({}) VALUES ({})",
                cols.iter()
                    .map(|c| format!("\"{c}\""))
                    .collect::<Vec<_>>()
                    .join(", "),
                placeholders
            );
            let conn = self.inner.lock();
            let values: Vec<rusqlite::types::Value> =
                cols.iter().map(|k| json_to_sql(&row[*k])).collect();
            conn.execute(&sql, params_from_iter(values.iter()))
                .map_err(classify)?;
            Ok(())
        }

        /// UPSERT by primary key `id`. Used by `Order::save_to_db`.
        pub fn upsert(&self, row: HashMap<String, Value>) -> Result<(), PersistenceError> {
            let cols: Vec<&str> = row.keys().map(String::as_str).collect();
            let placeholders = vec!["?"; cols.len()].join(", ");
            let sql = format!(
                "INSERT OR REPLACE INTO orders ({}) VALUES ({})",
                cols.iter()
                    .map(|c| format!("\"{c}\""))
                    .collect::<Vec<_>>()
                    .join(", "),
                placeholders
            );
            let conn = self.inner.lock();
            let values: Vec<rusqlite::types::Value> =
                cols.iter().map(|k| json_to_sql(&row[*k])).collect();
            conn.execute(&sql, params_from_iter(values.iter()))
                .map_err(classify)?;
            Ok(())
        }

        /// `SELECT * FROM orders` — returns each row as a column → JSON
        /// `Value` map. Column order matches `ORDER_COLUMNS`.
        pub fn query_all(&self) -> Result<Vec<HashMap<String, Value>>, PersistenceError> {
            let conn = self.inner.lock();
            let mut stmt = conn
                .prepare("SELECT * FROM orders")
                .map_err(backend)?;
            let col_names: Vec<String> = stmt
                .column_names()
                .iter()
                .map(|s| s.to_string())
                .collect();
            let rows = stmt
                .query_map([], |row| {
                    let mut m = HashMap::new();
                    for (i, name) in col_names.iter().enumerate() {
                        let v: rusqlite::types::Value = row.get(i)?;
                        m.insert(name.clone(), sql_to_json(&v));
                    }
                    Ok(m)
                })
                .map_err(backend)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(backend)
        }

        pub fn count(&self) -> Result<usize, PersistenceError> {
            let conn = self.inner.lock();
            let n: i64 = conn
                .query_row("SELECT COUNT(*) FROM orders", [], |row| row.get(0))
                .map_err(backend)?;
            Ok(n as usize)
        }
    }

    impl PersistenceHandle for SqlitePersistenceHandle {
        fn upsert_order(&self, row: HashMap<String, Value>) -> Result<(), PersistenceError> {
            self.upsert(row)
        }
    }

    fn json_to_sql(v: &Value) -> rusqlite::types::Value {
        use rusqlite::types::Value as S;
        match v {
            Value::Null => S::Null,
            Value::Bool(b) => S::Integer(if *b { 1 } else { 0 }),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    S::Integer(i)
                } else if let Some(f) = n.as_f64() {
                    S::Real(f)
                } else {
                    S::Text(n.to_string())
                }
            }
            Value::String(s) => S::Text(s.clone()),
            // HashMap or Array — serialise to a JSON string (matches upstream
            // `row["JSON"] == json.dumps({...})`).
            Value::Array(_) | Value::Object(_) => S::Text(v.to_string()),
        }
    }

    fn sql_to_json(v: &rusqlite::types::Value) -> Value {
        use rusqlite::types::Value as S;
        match v {
            S::Null => Value::Null,
            S::Integer(i) => serde_json::json!(i),
            S::Real(f) => serde_json::json!(f),
            S::Text(s) => Value::String(s.clone()),
            S::Blob(_) => Value::Null,
        }
    }

    fn backend(e: rusqlite::Error) -> PersistenceError {
        PersistenceError::Backend(e.to_string())
    }

    fn classify(e: rusqlite::Error) -> PersistenceError {
        if let rusqlite::Error::SqliteFailure(sqlite_err, msg) = &e {
            if sqlite_err.code == ErrorCode::ConstraintViolation {
                return PersistenceError::Unique(
                    msg.clone().unwrap_or_else(|| e.to_string()),
                );
            }
        }
        PersistenceError::Backend(e.to_string())
    }
}

#[cfg(feature = "persistence")]
pub use sqlite::SqlitePersistenceHandle;
