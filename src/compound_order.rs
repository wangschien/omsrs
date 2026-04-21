//! Upstream `omspy.order.CompoundOrder` port (PORT-PLAN §8 R8).
//!
//! A basket of `Order`s with shared broker/connection context, index +
//! key lookup helpers, aggregate view (positions / buy-sell quantity /
//! average price / net_value / mtm), and execute/save-all fan-out.
//!
//! Orders are stored by value (`Vec<Order>`) — `Order` is `Clone`, so
//! `add_order` / `add` move or clone depending on the construction path.
//! `index` and `keys` map logical identifiers to positions in `orders`
//! rather than duplicating references. Upstream Python uses
//! `Dict[int, Order]` where the `Order` is a shared reference; our
//! `HashMap<i64, usize>` is isomorphic because `orders` is append-only.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::DateTime;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::Value;

use crate::broker::Broker;
use crate::clock::{clock_system_default, Clock};
use crate::order::{Order, OrderInit};
use crate::persistence::PersistenceHandle;

/// `run_fn` is the Rust analogue of upstream's CompoundOrder subclassing
/// pattern from `test_order_strategy_run` (`CompoundOrderRun.run(self, data)`).
/// Strategies call each child's `run_fn` when present; compound orders
/// without it are the `CompoundOrderNoRun` analogue (run is skipped).
pub type RunFn = Arc<
    dyn Fn(&mut CompoundOrder, &std::collections::HashMap<String, f64>) + Send + Sync,
>;

pub struct CompoundOrder {
    pub broker: Option<Arc<dyn Broker>>,
    pub id: String,
    pub ltp: HashMap<String, f64>,
    pub orders: Vec<Order>,
    pub connection: Option<Arc<dyn PersistenceHandle>>,
    pub order_args: HashMap<String, Value>,
    pub run_fn: Option<RunFn>,
    index: HashMap<i64, usize>,
    keys: HashMap<String, usize>,
    clock: Arc<dyn Clock + Send + Sync>,
}

#[derive(Debug, thiserror::Error)]
pub enum CompoundError {
    #[error("Order already assigned to this index: {0}")]
    IndexAlreadyUsed(i64),
    #[error("Order already assigned to this key: {0}")]
    KeyAlreadyUsed(String),
    #[error("invalid order: {0}")]
    InvalidOrder(String),
}

impl Default for CompoundOrder {
    fn default() -> Self {
        Self::new()
    }
}

impl CompoundOrder {
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

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    pub fn with_broker(mut self, broker: Arc<dyn Broker>) -> Self {
        self.broker = Some(broker);
        self
    }

    pub fn with_connection(mut self, conn: Arc<dyn PersistenceHandle>) -> Self {
        self.connection = Some(conn);
        self
    }

    pub fn clock(&self) -> &Arc<dyn Clock + Send + Sync> {
        &self.clock
    }

    /// Overwrite the compound's clock AND cascade to every child order
    /// (PORT-PLAN §6 D4 — "immediately cascades to every already-contained
    /// child order within the same call"). Used by `OrderStrategy::add`.
    pub fn set_clock(&mut self, clock: Arc<dyn Clock + Send + Sync>) {
        self.clock = clock.clone();
        for o in &mut self.orders {
            o.set_clock(clock.clone());
        }
    }

    pub fn with_orders(mut self, orders: Vec<Order>) -> Self {
        for (i, o) in orders.into_iter().enumerate() {
            self.orders.push(o);
            self.index.insert(i as i64, i);
        }
        self
    }

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

    /// Upstream `CompoundOrder.get(key)` — key-first, fall back to int
    /// index (accepting `int` or numeric-string).
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
            let sign: i64 = if order.side.eq_ignore_ascii_case("sell") { -1 } else { 1 };
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
            *quantity.entry(order.symbol.clone()).or_insert(Decimal::ZERO) += qty;
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
        // Start with -net_value (cost basis inverted).
        for (sym, v) in self.net_value() {
            *c.entry(sym).or_insert(Decimal::ZERO) -= v;
        }
        // Add current market value (position * ltp).
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

    /// Upstream `add_order(**kwargs)` — constructs a new `Order` with
    /// `parent_id = self.id`, propagating our `connection` if the caller
    /// didn't provide one. Returns the assigned order id.
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

    /// Upstream `add(order, index=None, key=None)` — takes an existing
    /// `Order`, assigns `parent_id` + (if missing) our connection, then
    /// inserts it at the given or next index.
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
        // Upstream coerces `index = int(index)` — accepts float, int, or
        // numeric string. We accept `Option<f64>` and truncate toward zero.
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

    /// Upstream `update_orders(data)` — iterates `pending_orders`; any
    /// pending `order_id` present in `data` gets
    /// `order.update(data[order_id])`. Returns {order_id → bool}.
    pub fn update_orders(
        &mut self,
        data: &HashMap<String, HashMap<String, Value>>,
    ) -> HashMap<String, bool> {
        let mut out = HashMap::new();
        // Collect pending order positions first (lifetime-wise simpler
        // than holding borrows across mutation).
        let pending_positions: Vec<usize> = self
            .orders
            .iter()
            .enumerate()
            .filter(|(_, o)| o.is_pending())
            .map(|(i, _)| i)
            .collect();
        for pos in pending_positions {
            let Some(oid) = self.orders[pos].order_id.clone() else { continue };
            if let Some(update) = data.get(&oid) {
                let ok = self.orders[pos].update(update);
                out.insert(oid, ok);
            }
        }
        out
    }

    /// Upstream `execute_all(**kwargs)` — for every order, call
    /// `order.execute(broker, ...)` with `order_args` + caller kwargs
    /// merged in (caller's kwargs take precedence).
    pub fn execute_all(&mut self, kwargs: HashMap<String, Value>) {
        let Some(broker) = self.broker.clone() else {
            return;
        };
        for order in &mut self.orders {
            let mut merged = self.order_args.clone();
            for (k, v) in &kwargs {
                merged.insert(k.clone(), v.clone());
            }
            order.execute(broker.as_ref(), None, merged);
        }
    }

    /// Upstream `check_flags` — for every pending-and-expired order,
    /// either convert it to MARKET (if `convert_to_market_after_expiry`)
    /// or cancel it (if `cancel_after_expiry`).
    pub fn check_flags(&mut self) {
        let Some(broker) = self.broker.clone() else {
            return;
        };
        for order in &mut self.orders {
            if order.is_pending() && order.has_expired() {
                if order.convert_to_market_after_expiry {
                    order.order_type = "MARKET".into();
                    order.price = None;
                    order.trigger_price = Decimal::ZERO;
                    order.modify(broker.as_ref(), None, HashMap::new());
                } else if order.cancel_after_expiry {
                    order.cancel(broker.as_ref(), None);
                }
            }
        }
    }

    /// Upstream `save()` — per-order `save_to_db`, counted.
    pub fn save(&self) -> usize {
        self.orders.iter().filter(|o| o.save_to_db()).count()
    }
}

// Ensure `DateTime` import stays even when unused via other helpers.
#[allow(dead_code)]
fn _touch(_: DateTime<chrono::Utc>) {}

#[allow(dead_code)]
fn _touch_to_primitive(d: Decimal) -> Option<f64> {
    d.to_f64()
}
