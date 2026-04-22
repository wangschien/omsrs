//! R12.3b — async port of [`crate::compound_order::CompoundOrder`].
//!
//! An independent type (not a wrapper) because sync `CompoundOrder`
//! stores `Option<Arc<dyn Broker>>` and we need
//! `Option<Arc<dyn AsyncBroker + Send + Sync>>`. The trait objects
//! can't be cast across trait families, so the async port carries
//! its own copy of the pure-state bookkeeping (order vec, index,
//! keys, positions/mtm/net_value aggregates).
//!
//! Pure-state methods (aggregate views, add/remove, index+keys
//! lookup, update_orders) are line-for-line copies of sync. The two
//! broker-interacting methods are async siblings:
//!
//! - `execute_all_async(kwargs)` — fans out to each child's
//!   `Order::execute_async` (R12.3a)
//! - `check_flags_async()` — same fan-out for
//!   `Order::modify_async` / `cancel_async` on expired pending
//!   orders
//!
//! Maintenance contract: any semantic change to sync
//! `CompoundOrder`'s pure-state methods must be mirrored here in
//! the same commit. The R12 plan's "pure duplication" decision
//! chose this over a generics refactor that would have broken
//! sync semver.

use std::collections::HashMap;
use std::sync::Arc;

use rust_decimal::Decimal;
use serde_json::Value;

use crate::async_broker::AsyncBroker;
use crate::clock::{clock_system_default, Clock};
use crate::compound_order::CompoundError;
use crate::order::{Order, OrderInit};
use crate::persistence::PersistenceHandle;

/// Analogue of sync `RunFn`. Strategy callbacks are kept **sync**
/// (no I/O in the closure) per R12 plan's open-question #1
/// resolution.
pub type AsyncRunFn =
    Arc<dyn Fn(&mut AsyncCompoundOrder, &HashMap<String, f64>) + Send + Sync>;

pub struct AsyncCompoundOrder {
    pub broker: Option<Arc<dyn AsyncBroker + Send + Sync>>,
    pub id: String,
    pub ltp: HashMap<String, f64>,
    pub orders: Vec<Order>,
    pub connection: Option<Arc<dyn PersistenceHandle>>,
    pub order_args: HashMap<String, Value>,
    pub run_fn: Option<AsyncRunFn>,
    index: HashMap<i64, usize>,
    keys: HashMap<String, usize>,
    clock: Arc<dyn Clock + Send + Sync>,
}

impl Default for AsyncCompoundOrder {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncCompoundOrder {
    pub fn new() -> Self {
        Self::with_clock(clock_system_default())
    }

    pub fn with_clock(clock: Arc<dyn Clock + Send + Sync>) -> Self {
        Self {
            broker: None,
            id: uuid::Uuid::new_v4().simple().to_string(),
            ltp: HashMap::new(),
            orders: Vec::new(),
            connection: None,
            order_args: HashMap::new(),
            run_fn: None,
            index: HashMap::new(),
            keys: HashMap::new(),
            clock,
        }
    }

    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    #[must_use]
    pub fn with_broker(mut self, broker: Arc<dyn AsyncBroker + Send + Sync>) -> Self {
        self.broker = Some(broker);
        self
    }

    #[must_use]
    pub fn with_connection(mut self, conn: Arc<dyn PersistenceHandle>) -> Self {
        self.connection = Some(conn);
        self
    }

    pub fn clock(&self) -> &Arc<dyn Clock + Send + Sync> {
        &self.clock
    }

    /// Cascade clock to every child order (PORT-PLAN §6 D4 — sync
    /// parity).
    pub fn set_clock(&mut self, clock: Arc<dyn Clock + Send + Sync>) {
        self.clock = clock.clone();
        for o in &mut self.orders {
            o.set_clock(clock.clone());
        }
    }

    #[must_use]
    pub fn with_orders(mut self, orders: Vec<Order>) -> Self {
        for (i, o) in orders.into_iter().enumerate() {
            self.orders.push(o);
            self.index.insert(i as i64, i);
        }
        self
    }

    // ── aggregate views (mirror sync) ──────────────────────

    pub fn count(&self) -> usize {
        self.orders.len()
    }

    pub fn len(&self) -> usize {
        self.orders.len()
    }

    pub fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }

    pub fn index_map(&self) -> &HashMap<i64, usize> {
        &self.index
    }

    pub fn keys_map(&self) -> &HashMap<String, usize> {
        &self.keys
    }

    pub fn get_next_index(&self) -> i64 {
        if self.index.is_empty() {
            0
        } else {
            self.index.keys().copied().max().unwrap() + 1
        }
    }

    pub fn get_by_index(&self, idx: i64) -> Option<&Order> {
        let pos = *self.index.get(&idx)?;
        self.orders.get(pos)
    }

    pub fn get_by_key(&self, key: &str) -> Option<&Order> {
        let pos = *self.keys.get(key)?;
        self.orders.get(pos)
    }

    pub fn get(&self, key: &str) -> Option<&Order> {
        if let Some(o) = self.get_by_key(key) {
            return Some(o);
        }
        if let Ok(n) = key.parse::<i64>() {
            return self.get_by_index(n);
        }
        None
    }

    pub fn positions(&self) -> HashMap<String, i64> {
        let mut out: HashMap<String, i64> = HashMap::new();
        for order in &self.orders {
            let qty = order.filled_quantity;
            let sign: i64 = if order.side.eq_ignore_ascii_case("sell") {
                -1
            } else {
                1
            };
            *out.entry(order.symbol.clone()).or_insert(0) += qty * sign;
        }
        out
    }

    pub fn buy_quantity(&self) -> HashMap<String, i64> {
        self.total_quantity().0
    }

    pub fn sell_quantity(&self) -> HashMap<String, i64> {
        self.total_quantity().1
    }

    fn total_quantity(&self) -> (HashMap<String, i64>, HashMap<String, i64>) {
        let (mut buy, mut sell) = (HashMap::new(), HashMap::new());
        for order in &self.orders {
            let q = order.filled_quantity.abs();
            if q == 0 {
                continue;
            }
            let side = order.side.to_ascii_lowercase();
            let target = if side == "buy" {
                &mut buy
            } else if side == "sell" {
                &mut sell
            } else {
                continue;
            };
            *target.entry(order.symbol.clone()).or_insert(0) += q;
        }
        (buy, sell)
    }

    fn average_price(&self, side_filter: &str) -> HashMap<String, Decimal> {
        let mut value: HashMap<String, Decimal> = HashMap::new();
        let mut quantity: HashMap<String, Decimal> = HashMap::new();
        for order in &self.orders {
            let order_side = order.side.to_ascii_lowercase();
            if order_side != side_filter || order.filled_quantity == 0 {
                continue;
            }
            let qty = Decimal::from(order.filled_quantity);
            let price = order.average_price;
            let v = price * qty;
            *value.entry(order.symbol.clone()).or_insert(Decimal::ZERO) += v;
            *quantity
                .entry(order.symbol.clone())
                .or_insert(Decimal::ZERO) += qty;
        }
        let mut out = HashMap::new();
        for (sym, v) in &value {
            if let Some(q) = quantity.get(sym) {
                if *q > Decimal::ZERO {
                    out.insert(sym.clone(), v / q);
                }
            }
        }
        out
    }

    pub fn average_buy_price(&self) -> HashMap<String, Decimal> {
        self.average_price("buy")
    }

    pub fn average_sell_price(&self) -> HashMap<String, Decimal> {
        self.average_price("sell")
    }

    pub fn update_ltp(&mut self, last_price: &HashMap<String, f64>) -> HashMap<String, f64> {
        for (k, v) in last_price {
            self.ltp.insert(k.clone(), *v);
        }
        self.ltp.clone()
    }

    pub fn net_value(&self) -> HashMap<String, Decimal> {
        let mut c: HashMap<String, Decimal> = HashMap::new();
        for order in &self.orders {
            if order.filled_quantity > 0 {
                let sign: Decimal = if order.side.eq_ignore_ascii_case("sell") {
                    Decimal::NEGATIVE_ONE
                } else {
                    Decimal::ONE
                };
                let v = Decimal::from(order.filled_quantity) * order.average_price * sign;
                *c.entry(order.symbol.clone()).or_insert(Decimal::ZERO) += v;
            }
        }
        c
    }

    pub fn mtm(&self) -> HashMap<String, Decimal> {
        let mut c: HashMap<String, Decimal> = HashMap::new();
        for (sym, v) in self.net_value() {
            *c.entry(sym).or_insert(Decimal::ZERO) -= v;
        }
        for (sym, qty) in self.positions() {
            if let Some(ltp) = self.ltp.get(&sym) {
                if let Ok(ltp_dec) = Decimal::try_from(*ltp) {
                    *c.entry(sym).or_insert(Decimal::ZERO) += Decimal::from(qty) * ltp_dec;
                }
            }
        }
        c
    }

    pub fn total_mtm(&self) -> Decimal {
        self.mtm().values().copied().sum()
    }

    pub fn completed_orders(&self) -> Vec<&Order> {
        self.orders.iter().filter(|o| o.is_complete()).collect()
    }

    pub fn pending_orders(&self) -> Vec<&Order> {
        self.orders.iter().filter(|o| o.is_pending()).collect()
    }

    // ── add + add_order (mirror sync) ──────────────────────

    pub fn add_order(
        &mut self,
        mut init: OrderInit,
        index: Option<i64>,
        key: Option<String>,
    ) -> Result<String, CompoundError> {
        init.parent_id = Some(self.id.clone());
        if init.connection.is_none() {
            init.connection = self.connection.clone();
        }
        let idx = index.unwrap_or_else(|| self.get_next_index());
        if self.index.contains_key(&idx) {
            return Err(CompoundError::IndexAlreadyUsed(idx));
        }
        if let Some(k) = &key {
            if self.keys.contains_key(k) {
                return Err(CompoundError::KeyAlreadyUsed(k.clone()));
            }
        }
        let order = Order::from_init_with_clock(init, self.clock.clone());
        let id = order.id.clone().unwrap();
        let position = self.orders.len();
        self.orders.push(order);
        self.index.insert(idx, position);
        if let Some(k) = key {
            self.keys.insert(k, position);
        }
        let _ = self.orders[position].save_to_db();
        Ok(id)
    }

    pub fn add(
        &mut self,
        mut order: Order,
        index: Option<f64>,
        key: Option<String>,
    ) -> Result<String, CompoundError> {
        order.parent_id = Some(self.id.clone());
        if order.connection.is_none() {
            order.connection = self.connection.clone();
        }
        if order.id.is_none() {
            order.id = Some(uuid::Uuid::new_v4().simple().to_string());
        }
        let idx = index
            .map(|f| f.trunc() as i64)
            .unwrap_or_else(|| self.get_next_index());
        if self.index.contains_key(&idx) {
            return Err(CompoundError::IndexAlreadyUsed(idx));
        }
        if let Some(k) = &key {
            if self.keys.contains_key(k) {
                return Err(CompoundError::KeyAlreadyUsed(k.clone()));
            }
        }
        let id = order.id.clone().unwrap();
        let position = self.orders.len();
        self.orders.push(order);
        self.index.insert(idx, position);
        if let Some(k) = key {
            self.keys.insert(k, position);
        }
        let _ = self.orders[position].save_to_db();
        Ok(id)
    }

    pub fn update_orders(
        &mut self,
        data: &HashMap<String, HashMap<String, Value>>,
    ) -> HashMap<String, bool> {
        let mut out = HashMap::new();
        let pending_positions: Vec<usize> = self
            .orders
            .iter()
            .enumerate()
            .filter(|(_, o)| o.is_pending())
            .map(|(i, _)| i)
            .collect();
        for pos in pending_positions {
            let Some(oid) = self.orders[pos].order_id.clone() else {
                continue;
            };
            if let Some(update) = data.get(&oid) {
                let ok = self.orders[pos].update(update);
                out.insert(oid, ok);
            }
        }
        out
    }

    // ── broker-interacting methods (async) ─────────────────

    /// Async sibling of sync [`crate::compound_order::CompoundOrder
    /// ::execute_all`]. Iterates every child order and calls
    /// [`Order::execute_async`]. Merges `self.order_args` +
    /// caller's `kwargs` (caller wins).
    pub async fn execute_all_async(&mut self, kwargs: HashMap<String, Value>) {
        let Some(broker) = self.broker.clone() else {
            return;
        };
        for order in &mut self.orders {
            let mut merged = self.order_args.clone();
            for (k, v) in &kwargs {
                merged.insert(k.clone(), v.clone());
            }
            order.execute_async(broker.as_ref(), None, merged).await;
        }
    }

    /// Async sibling of sync [`crate::compound_order::CompoundOrder
    /// ::check_flags`]. For each pending-and-expired order, either
    /// converts to MARKET (`convert_to_market_after_expiry`) and
    /// modifies, or cancels (`cancel_after_expiry`).
    pub async fn check_flags_async(&mut self) {
        let Some(broker) = self.broker.clone() else {
            return;
        };
        for order in &mut self.orders {
            if order.is_pending() && order.has_expired() {
                if order.convert_to_market_after_expiry {
                    order.order_type = "MARKET".into();
                    order.price = None;
                    order.trigger_price = Decimal::ZERO;
                    order.modify_async(broker.as_ref(), None, HashMap::new()).await;
                } else if order.cancel_after_expiry {
                    order.cancel_async(broker.as_ref(), None).await;
                }
            }
        }
    }

    /// Upstream `save()` — per-order `save_to_db`, counted.
    /// Stays **sync** per R12 persistence caveat.
    pub fn save(&self) -> usize {
        self.orders.iter().filter(|o| o.save_to_db()).count()
    }
}
