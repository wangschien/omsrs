//! Async sibling of [`crate::Broker`]. v0.2 additive: neither replaces
//! nor breaks the sync trait.
//!
//! Motivation (from pbot R3.3b planning, 2026-04-21): every real
//! prediction-market SDK pbot integrates with is async (Polymarket's
//! `rs-clob-client`, Kalshi's `kalshi-rs`). The v0.1.0 sync `Broker`
//! stays the right contract for `Paper`, `VirtualBroker`, `ReplicaBroker`,
//! `CompoundOrder`, and `OrderStrategy` — all of which model omspy
//! semantics that are intrinsically sync. But wrapping an async venue
//! client behind the sync trait forces a `block_on` bridge at every
//! broker implementation. `AsyncBroker` lives alongside `Broker` so
//! venue adapters can implement the async path directly; sync
//! consumers are untouched.
//!
//! The default methods (`close_all_positions`, `cancel_all_orders`,
//! `get_positions_from_orders`) are line-for-line async mirrors of
//! their sync counterparts in [`crate::broker`]. Keeping them in
//! lock-step means pbot's R2.3-style 10-item Paper parity harness
//! ports to AsyncPaper with only the `#[tokio::test]` macro change.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

/// Shorthand for the symbol-transformer closure type used by
/// [`AsyncBroker::close_all_positions`].
///
/// Wrapped in `Arc` rather than borrowed because `async_trait` boxes
/// the method body into a future with a generated `'async_trait`
/// lifetime. A `&dyn Fn(&str) -> String + Send + Sync` **can** be
/// passed through that future in principle, but routing its
/// anonymous caller-scoped lifetime all the way into the trait
/// signature and its implementations is cumbersome compared to
/// paying one `Arc::clone` at the call site.
pub type AsyncSymbolTransformer = Arc<dyn Fn(&str) -> String + Send + Sync>;

use crate::models::BasicPosition;
use crate::utils::{create_basic_positions_from_orders_dict, dict_filter, OrderRecord};

/// Async market-maker / order broker surface. Semantically identical
/// to [`crate::Broker`]; methods are `async fn` so implementations can
/// call async venue SDKs without a sync-over-async bridge.
///
/// Implementations must be `Send + Sync + 'static` to support
/// `Arc<dyn AsyncBroker>` usage in multi-threaded tokio runtimes.
///
/// # Implementor note
///
/// Like all `#[async_trait]`-annotated traits, implementors **must**
/// place `#[async_trait]` on their `impl` block as well:
///
/// ```ignore
/// use async_trait::async_trait;
/// use omsrs::AsyncBroker;
///
/// struct MyBroker;
///
/// #[async_trait]
/// impl AsyncBroker for MyBroker {
///     async fn order_place(&self, _args: std::collections::HashMap<String, serde_json::Value>) -> Option<String> {
///         None
///     }
///     async fn order_modify(&self, _args: std::collections::HashMap<String, serde_json::Value>) {}
///     async fn order_cancel(&self, _args: std::collections::HashMap<String, serde_json::Value>) {}
/// }
/// ```
#[async_trait]
pub trait AsyncBroker: Send + Sync {
    async fn order_place(&self, args: HashMap<String, Value>) -> Option<String>;
    async fn order_modify(&self, args: HashMap<String, Value>);
    async fn order_cancel(&self, args: HashMap<String, Value>);

    async fn attribs_to_copy_execute(&self) -> Option<Vec<String>> {
        None
    }
    async fn attribs_to_copy_modify(&self) -> Option<Vec<String>> {
        None
    }
    async fn attribs_to_copy_cancel(&self) -> Option<Vec<String>> {
        None
    }

    /// Broker's current view of resting orders. Default empty to match
    /// sync [`crate::Broker::orders`].
    async fn orders(&self) -> Vec<HashMap<String, Value>> {
        Vec::new()
    }

    async fn positions(&self) -> Vec<HashMap<String, Value>> {
        Vec::new()
    }

    async fn trades(&self) -> Vec<HashMap<String, Value>> {
        Vec::new()
    }

    /// Async mirror of [`crate::Broker::close_all_positions`]. Same
    /// rules: filter zero-quantity rows, produce opposite-side MARKET
    /// orders, apply `keys_to_copy` (skipping the static set) +
    /// `keys_to_add`, then run `symbol_transformer` on each symbol
    /// before placement.
    async fn close_all_positions(
        &self,
        positions: Option<Vec<HashMap<String, Value>>>,
        keys_to_copy: Option<&[&str]>,
        keys_to_add: Option<&HashMap<String, Value>>,
        symbol_transformer: Option<AsyncSymbolTransformer>,
    ) {
        const STATIC_KEYS: [&str; 4] = ["quantity", "side", "symbol", "order_type"];
        let rows = match positions {
            Some(p) => p,
            None => self.positions().await,
        };
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
            let symbol = match symbol_transformer.as_ref() {
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
            self.order_place(final_args).await;
        }
    }

    /// Async mirror of [`crate::Broker::cancel_all_orders`].
    async fn cancel_all_orders(
        &self,
        keys_to_copy: Option<&[&str]>,
        keys_to_add: Option<&HashMap<String, Value>>,
    ) {
        const TERMINAL: [&str; 3] = ["COMPLETE", "CANCELED", "REJECTED"];
        let orders = self.orders().await;
        for order in orders {
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
            self.order_cancel(final_args).await;
        }
    }

    /// Async mirror of [`crate::Broker::get_positions_from_orders`].
    async fn get_positions_from_orders(
        &self,
        filters: &HashMap<String, Value>,
    ) -> HashMap<String, BasicPosition> {
        let all = self.orders().await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// R11.1.smoke — `AsyncBroker` is dyn-compatible (object-safe): we
    /// can construct an `Arc<dyn AsyncBroker>`. This is the same
    /// regression guard that `R2.3.10` gives for the sync `Broker`.
    #[test]
    fn async_broker_trait_is_dyn_compatible() {
        // Minimal impl just to construct the trait object.
        struct Noop;
        #[async_trait]
        impl AsyncBroker for Noop {
            async fn order_place(&self, _: HashMap<String, Value>) -> Option<String> {
                None
            }
            async fn order_modify(&self, _: HashMap<String, Value>) {}
            async fn order_cancel(&self, _: HashMap<String, Value>) {}
        }
        let _: Arc<dyn AsyncBroker> = Arc::new(Noop);
    }

    /// R11.1.smoke — defaults mirror sync `Broker` defaults at least
    /// structurally (empty Vec / None).
    #[tokio::test]
    async fn async_broker_defaults_return_empty() {
        struct Noop;
        #[async_trait]
        impl AsyncBroker for Noop {
            async fn order_place(&self, _: HashMap<String, Value>) -> Option<String> {
                None
            }
            async fn order_modify(&self, _: HashMap<String, Value>) {}
            async fn order_cancel(&self, _: HashMap<String, Value>) {}
        }
        let b = Noop;
        assert!(b.orders().await.is_empty());
        assert!(b.positions().await.is_empty());
        assert!(b.trades().await.is_empty());
        assert!(b.attribs_to_copy_execute().await.is_none());
        assert!(b.attribs_to_copy_modify().await.is_none());
        assert!(b.attribs_to_copy_cancel().await.is_none());
    }
}
