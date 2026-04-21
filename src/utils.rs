//! MVP utility functions ported from `omspy.utils`.
//!
//! Deferred / dropped per PORT-PLAN §2 + source-notes §12:
//! `tick` (no MVP caller), `stop_loss_step_decimal` (unused), `load_broker`
//! (imports Indian brokers — out of scope).

use std::collections::HashMap;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::BasicPosition;

/// Upstream `utils.UQty` named tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UQty {
    pub q: i64,
    pub f: i64,
    pub p: i64,
    pub c: i64,
}

impl UQty {
    pub fn new(q: i64, f: i64, p: i64, c: i64) -> Self {
        Self { q, f, p, c }
    }
}

/// Order-record shape consumed by `create_basic_positions_from_orders_dict`.
/// Upstream `utils.create_basic_positions_from_orders_dict` takes `List[Dict]`;
/// in Rust we type it explicitly with `#[serde(default)]` on every field so
/// CSV / JSON fixtures with missing columns deserialize cleanly.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct OrderRecord {
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub side: Option<String>,
    #[serde(default)]
    pub quantity: Decimal,
    #[serde(default)]
    pub price: Decimal,
    #[serde(default)]
    pub trigger_price: Decimal,
    #[serde(default)]
    pub average_price: Decimal,
}

/// Mirrors `utils.create_basic_positions_from_orders_dict`.
pub fn create_basic_positions_from_orders_dict(
    orders: &[OrderRecord],
) -> HashMap<String, BasicPosition> {
    let mut out: HashMap<String, BasicPosition> = HashMap::new();
    for order in orders {
        let Some(symbol) = order.symbol.as_ref() else {
            continue;
        };
        let price = max3(order.average_price, order.price, order.trigger_price);
        let quantity = order.quantity.abs();
        let Some(side) = order.side.as_ref() else {
            continue;
        };
        let side = side.to_ascii_lowercase();
        let pos = out
            .entry(symbol.clone())
            .or_insert_with(|| BasicPosition::new(symbol.clone()));
        if side == "buy" {
            pos.buy_quantity += quantity;
            pos.buy_value += price * quantity;
        } else if side == "sell" {
            pos.sell_quantity += quantity;
            pos.sell_value += price * quantity;
        }
    }
    out
}

fn max3(a: Decimal, b: Decimal, c: Decimal) -> Decimal {
    a.max(b).max(c)
}

/// Mirrors `utils.dict_filter`. AND-semantics across all `filters` entries.
///
/// Upstream prints "Nothing in the list" on an empty input and returns `[]`.
/// That print is preserved so stdout parity is maintained for any downstream
/// caller that scrapes it (none in MVP, but behavior is cheap to keep).
pub fn dict_filter(
    lst: &[HashMap<String, Value>],
    filters: &HashMap<String, Value>,
) -> Vec<HashMap<String, Value>> {
    if lst.is_empty() {
        println!("Nothing in the list");
        return Vec::new();
    }
    lst.iter()
        .filter(|d| filters.iter().all(|(k, v)| d.get(k) == Some(v)))
        .cloned()
        .collect()
}

/// Mirrors `utils.update_quantity`.
///
/// Conservation invariant: `q = f + p + c` on return for any non-negative
/// input. Cancel > Fill > Pending priority per upstream docstring.
pub fn update_quantity(q: i64, mut f: i64, mut p: i64, mut c: i64) -> UQty {
    if c > 0 {
        c = c.min(q);
        f = q - c;
        p = q - c - f;
    } else if f > 0 {
        f = f.min(q);
        p = q - f;
    } else if p > 0 {
        p = p.min(q);
        f = q - p;
    } else {
        p = q - p;
    }
    UQty::new(q, f, p, c)
}
