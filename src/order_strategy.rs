//! `omspy.order.OrderStrategy` port (PORT-PLAN §8 R9).
//!
//! A thin wrapper around `Vec<CompoundOrder>` with aggregate views
//! (`positions`, `mtm`, `total_mtm`) and fan-out methods (`update_ltp`,
//! `update_orders`, `run`, `save`). Each child `CompoundOrder` can opt
//! into strategy-level `run` by setting its `run_fn` (Rust analogue of
//! upstream's `CompoundOrderRun.run(self, data)` subclassing).

use std::collections::HashMap;
use std::sync::Arc;

use rust_decimal::Decimal;

use crate::broker::Broker;
use crate::compound_order::CompoundOrder;

pub struct OrderStrategy {
    pub broker: Option<Arc<dyn Broker>>,
    pub id: String,
    pub ltp: HashMap<String, f64>,
    pub orders: Vec<CompoundOrder>,
}

impl Default for OrderStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl OrderStrategy {
    pub fn new() -> Self {
        Self {
            broker: None,
            id: uuid::Uuid::new_v4().simple().to_string(),
            ltp: HashMap::new(),
            orders: Vec::new(),
        }
    }

    pub fn with_broker(mut self, broker: Arc<dyn Broker>) -> Self {
        self.broker = Some(broker);
        self
    }

    pub fn with_orders(mut self, orders: Vec<CompoundOrder>) -> Self {
        self.orders = orders;
        self
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

    pub fn add(&mut self, mut compound: CompoundOrder) {
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
