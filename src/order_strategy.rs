//! `omspy.order.OrderStrategy` port (PORT-PLAN §8 R9).
//!
//! A thin wrapper around `Vec<CompoundOrder>` with aggregate views
//! (`positions`, `mtm`, `total_mtm`) and fan-out methods (`update_ltp`,
//! `update_orders`, `run`, `save`). Carries an injected
//! `Arc<dyn Clock + Send + Sync>` (PORT-PLAN §6 D4); `add(compound)`
//! overwrites the child's clock AND immediately cascades that clock to
//! every already-contained child order.
//!
//! Each child `CompoundOrder` can opt into strategy-level `run` by
//! setting its `run_fn` (Rust analogue of upstream's
//! `CompoundOrderRun.run(self, data)` subclassing).

use std::collections::HashMap;
use std::sync::Arc;

use rust_decimal::Decimal;

use crate::broker::Broker;
use crate::clock::{clock_system_default, Clock};
use crate::compound_order::CompoundOrder;

pub struct OrderStrategy {
    pub broker: Option<Arc<dyn Broker>>,
    pub id: String,
    pub ltp: HashMap<String, f64>,
    pub orders: Vec<CompoundOrder>,
    clock: Arc<dyn Clock + Send + Sync>,
}

impl Default for OrderStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl OrderStrategy {
    pub fn new() -> Self {
        Self::with_clock(clock_system_default())
    }

    pub fn with_clock(clock: Arc<dyn Clock + Send + Sync>) -> Self {
        Self {
            broker: None,
            id: uuid::Uuid::new_v4().simple().to_string(),
            ltp: HashMap::new(),
            orders: Vec::new(),
            clock,
        }
    }

    pub fn with_broker(mut self, broker: Arc<dyn Broker>) -> Self {
        self.broker = Some(broker);
        self
    }

    /// Set `orders` in bulk — cascade the strategy clock to each compound
    /// immediately (matches `add` semantics but batched).
    pub fn with_orders(mut self, mut orders: Vec<CompoundOrder>) -> Self {
        for co in &mut orders {
            co.set_clock(self.clock.clone());
        }
        self.orders = orders;
        self
    }

    pub fn clock(&self) -> &Arc<dyn Clock + Send + Sync> {
        &self.clock
    }

    pub fn positions(&self) -> HashMap<String, i64> {
        let mut out: HashMap<String, i64> = HashMap::new();
        for co in &self.orders {
            for (sym, qty) in co.positions() {
                *out.entry(sym).or_insert(0) += qty;
            }
        }
        out
    }

    /// Upstream `update_ltp` stores the dict on `self.ltp` AND propagates
    /// to every compound order. Returns the updated strategy ltp.
    pub fn update_ltp(&mut self, last_price: &HashMap<String, f64>) -> HashMap<String, f64> {
        for (sym, v) in last_price {
            self.ltp.insert(sym.clone(), *v);
        }
        for co in &mut self.orders {
            co.update_ltp(last_price);
        }
        self.ltp.clone()
    }

    pub fn update_orders(
        &mut self,
        data: &HashMap<String, HashMap<String, serde_json::Value>>,
    ) {
        for co in &mut self.orders {
            co.update_orders(data);
        }
    }

    pub fn mtm(&self) -> HashMap<String, Decimal> {
        let mut out: HashMap<String, Decimal> = HashMap::new();
        for co in &self.orders {
            for (sym, v) in co.mtm() {
                *out.entry(sym).or_insert(Decimal::ZERO) += v;
            }
        }
        out
    }

    pub fn total_mtm(&self) -> Decimal {
        self.mtm().values().copied().sum()
    }

    /// Upstream iterates orders and calls `compound_order.run(ltp=ltp)` if
    /// `callable(getattr(compound_order, "run"))`. Our Rust analogue:
    /// call `compound.run_fn(compound, &ltp)` if `run_fn.is_some()`.
    pub fn run(&mut self, ltp: &HashMap<String, f64>) {
        for co in &mut self.orders {
            let Some(f) = co.run_fn.clone() else { continue };
            f(co, ltp);
        }
    }

    /// Upstream `add(compound)` — our Rust version additionally cascades
    /// the strategy clock to the compound AND to every already-contained
    /// child order (PORT-PLAN §6 D4).
    pub fn add(&mut self, mut compound: CompoundOrder) {
        compound.set_clock(self.clock.clone());
        if self.broker.is_some() && compound.broker.is_none() {
            compound.broker = self.broker.clone();
        }
        self.orders.push(compound);
    }

    /// Upstream `save()` — per-compound `save()` call.
    pub fn save(&self) -> usize {
        self.orders.iter().map(|co| co.save()).sum()
    }
}
