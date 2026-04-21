//! Broker trait — the abstract interface `Order::execute/modify/cancel` call
//! into (PORT-PLAN §2, §13). `Broker` objects know nothing about `Order`
//! semantics; they receive flat kwarg maps and return broker-assigned ids.
//!
//! Upstream uses Python's `**kwargs` everywhere, so the trait's args are
//! `HashMap<String, serde_json::Value>` rather than typed structs. That keeps
//! the parity tests (which assert arbitrary kwarg contents) readable.
//!
//! R4 adds the default methods from `omspy.base.Broker`:
//! - `close_all_positions` (mirrors `order.py` `base.Broker.close_all_positions`),
//! - `cancel_all_orders`,
//! - `get_positions_from_orders`,
//! - `rename` (static helper for override-key rewriting),
//! - `orders` / `positions` / `trades` read-accessors returning the broker's
//!   current view. Implementations override them with their real sources;
//!   the defaults return `Vec::new()` so brokers that don't use the
//!   positions/orders flow (e.g. a pure execute-only adapter) can leave them
//!   alone. Trait objects stay object-safe: no generic methods.

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::models::BasicPosition;
use crate::utils::{create_basic_positions_from_orders_dict, dict_filter, OrderRecord};

pub trait Broker: Send + Sync {
    fn order_place(&self, args: HashMap<String, Value>) -> Option<String>;
    fn order_modify(&self, args: HashMap<String, Value>);
    fn order_cancel(&self, args: HashMap<String, Value>);

    /// Attribute-name lists the broker wants copied onto `order_place` /
    /// `order_modify` / `order_cancel` calls. Default-empty mirrors
    /// upstream: `hasattr(broker, name)` is `False` ⇒ nothing copied.
    fn attribs_to_copy_execute(&self) -> Option<Vec<String>> {
        None
    }
    fn attribs_to_copy_modify(&self) -> Option<Vec<String>> {
        None
    }
    fn attribs_to_copy_cancel(&self) -> Option<Vec<String>> {
        None
    }

    /// Broker's current view of resting orders. Default empty so brokers
    /// that never expose an orders view (e.g. pure execute-only adapters)
    /// don't have to implement it. `cancel_all_orders` + the upstream
    /// `get_positions_from_orders` flow consult this.
    fn orders(&self) -> Vec<HashMap<String, Value>> {
        Vec::new()
    }

    fn positions(&self) -> Vec<HashMap<String, Value>> {
        Vec::new()
    }

    fn trades(&self) -> Vec<HashMap<String, Value>> {
        Vec::new()
    }

    /// Mirrors `base.Broker.close_all_positions`. Iterates `positions` (or
    /// `self.positions()` if `None`), places a MARKET order opposite-side
    /// for each non-zero net quantity. `keys_to_copy` pulls named fields
    /// from each position row (skipping any overlap with the static
    /// symbol/side/quantity/order_type set); `keys_to_add` layers
    /// constant kwargs; `symbol_transformer` is applied to each symbol
    /// before being placed.
    fn close_all_positions(
        &self,
        positions: Option<Vec<HashMap<String, Value>>>,
        keys_to_copy: Option<&[&str]>,
        keys_to_add: Option<&HashMap<String, Value>>,
        symbol_transformer: Option<&dyn Fn(&str) -> String>,
    ) {
        const STATIC_KEYS: [&str; 4] = ["quantity", "side", "symbol", "order_type"];
        let rows = positions.unwrap_or_else(|| self.positions());
        for position in &rows {
            let Some(quantity) = coerce_quantity(position.get("quantity")) else {
                continue;
            };
            if quantity == 0 {
                continue;
            }
            let symbol_raw = match position.get("symbol").and_then(Value::as_str) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let symbol = match symbol_transformer {
                Some(f) => f(&symbol_raw),
                None => symbol_raw,
            };
            let side = if quantity > 0 { "sell" } else { "buy" };

            let mut order_args: HashMap<String, Value> = HashMap::new();
            order_args.insert("quantity".into(), json!(quantity.abs()));
            order_args.insert("side".into(), json!(side));
            order_args.insert("symbol".into(), json!(symbol));
            order_args.insert("order_type".into(), json!("MARKET"));

            if let Some(keys) = keys_to_copy {
                for key in keys {
                    if STATIC_KEYS.contains(key) {
                        continue;
                    }
                    if let Some(v) = position.get(*key) {
                        if !v.is_null() {
                            order_args.insert((*key).to_string(), v.clone());
                        }
                    }
                }
            }

            let mut final_args: HashMap<String, Value> = HashMap::new();
            if let Some(extras) = keys_to_add {
                for (k, v) in extras {
                    final_args.insert(k.clone(), v.clone());
                }
            }
            for (k, v) in order_args {
                final_args.insert(k, v);
            }
            self.order_place(final_args);
        }
    }

    /// Mirrors `base.Broker.cancel_all_orders`. Iterates `self.orders()`,
    /// cancels each order whose `status` is not in
    /// `(COMPLETE, CANCELED, REJECTED)`.
    fn cancel_all_orders(
        &self,
        keys_to_copy: Option<&[&str]>,
        keys_to_add: Option<&HashMap<String, Value>>,
    ) {
        const TERMINAL: [&str; 3] = ["COMPLETE", "CANCELED", "REJECTED"];
        for order in self.orders() {
            let status = order
                .get("status")
                .and_then(Value::as_str)
                .map(|s| s.to_uppercase())
                .unwrap_or_else(|| "PENDING".to_string());
            let Some(oid) = order.get("order_id").cloned() else {
                continue;
            };
            if oid.is_null() {
                continue;
            }
            if TERMINAL.contains(&status.as_str()) {
                continue;
            }
            let mut final_args: HashMap<String, Value> = HashMap::new();
            if let Some(keys) = keys_to_copy {
                for key in keys {
                    if let Some(v) = order.get(*key) {
                        final_args.insert((*key).to_string(), v.clone());
                    }
                }
            }
            if let Some(extras) = keys_to_add {
                for (k, v) in extras {
                    final_args.insert(k.clone(), v.clone());
                }
            }
            final_args.insert("order_id".into(), oid);
            self.order_cancel(final_args);
        }
    }

    /// Mirrors `base.Broker.get_positions_from_orders`. Filters out
    /// CANCELED / REJECTED orders, applies `filters` via `dict_filter`,
    /// then aggregates via `create_basic_positions_from_orders_dict`.
    fn get_positions_from_orders(
        &self,
        filters: &HashMap<String, Value>,
    ) -> HashMap<String, BasicPosition> {
        let all = self.orders();
        let non_terminal: Vec<HashMap<String, Value>> = all
            .into_iter()
            .filter(|o| {
                let status = o.get("status").and_then(Value::as_str).unwrap_or("");
                !matches!(status, "CANCELED" | "REJECTED")
            })
            .collect();
        let filtered = dict_filter(&non_terminal, filters);
        let records: Vec<OrderRecord> = filtered.iter().map(order_record_from_dict).collect();
        create_basic_positions_from_orders_dict(&records)
    }
}

/// Upstream `Broker.rename` — renames dict keys using a mapping. Returns a
/// new dict; keys not in the mapping pass through unchanged.
pub fn rename(
    dct: &HashMap<String, Value>,
    keys: &HashMap<String, String>,
) -> HashMap<String, Value> {
    let mut new = HashMap::with_capacity(dct.len());
    for (k, v) in dct {
        if let Some(replacement) = keys.get(k) {
            new.insert(replacement.clone(), v.clone());
        } else {
            new.insert(k.clone(), v.clone());
        }
    }
    new
}

/// Upstream `int(position.get("quantity"))`. Accepts numeric, numeric-string,
/// and returns `None` on non-coercible values (matches upstream's
/// `try … except` → logged + skipped).
fn coerce_quantity(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Null => None,
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn order_record_from_dict(d: &HashMap<String, Value>) -> OrderRecord {
    use rust_decimal::Decimal;
    let s = |k: &str| d.get(k).and_then(Value::as_str).map(str::to_string);
    let dec = |k: &str| -> rust_decimal::Decimal {
        d.get(k)
            .and_then(|v| match v {
                Value::Number(n) => n.as_f64().and_then(|f| Decimal::try_from(f).ok()),
                Value::String(x) => x.parse().ok(),
                _ => None,
            })
            .unwrap_or(Decimal::ZERO)
    };
    OrderRecord {
        symbol: s("symbol"),
        side: s("side"),
        quantity: dec("quantity"),
        price: dec("price"),
        trigger_price: dec("trigger_price"),
        average_price: dec("average_price"),
    }
}
