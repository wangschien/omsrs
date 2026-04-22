//! R12.3b — async port of [`crate::order_strategy::OrderStrategy`].
//!
//! Thin wrapper around `Vec<AsyncCompoundOrder>` mirroring sync
//! `OrderStrategy`. Aggregate views + update_ltp / update_orders
//! are identical to sync; the async-specific surface covers
//! `AsyncCompoundOrder` children.
//!
//! `run_fn` callback stays **sync** (R12 plan open Q #1 — the
//! run-path closures don't do I/O, so async overhead isn't
//! justified). Child `run_fn` signature is `Fn(&mut
//! AsyncCompoundOrder, &HashMap<String, f64>)`, same shape as
//! sync but with the async compound type.

use std::collections::HashMap;
use std::sync::Arc;

use rust_decimal::Decimal;

use crate::async_broker::AsyncBroker;
use crate::async_compound_order::AsyncCompoundOrder;
use crate::clock::{clock_system_default, Clock};

pub struct AsyncOrderStrategy {
    pub broker: Option<Arc<dyn AsyncBroker + Send + Sync>>,
    pub id: String,
    pub ltp: HashMap<String, f64>,
    pub orders: Vec<AsyncCompoundOrder>,
    clock: Arc<dyn Clock + Send + Sync>,
}

impl Default for AsyncOrderStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncOrderStrategy {
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

    #[must_use]
    pub fn with_broker(mut self, broker: Arc<dyn AsyncBroker + Send + Sync>) -> Self {
        self.broker = Some(broker);
        self
    }

    /// Cascade strategy clock to each compound in bulk.
    #[must_use]
    pub fn with_orders(mut self, mut orders: Vec<AsyncCompoundOrder>) -> Self {
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

    pub fn update_ltp(&mut self, last_price: &HashMap<String, f64>) -> HashMap<String, f64> {
        for (sym, v) in last_price {
            self.ltp.insert(sym.clone(), *v);
        }
        for co in &mut self.orders {
            co.update_ltp(last_price);
        }
        self.ltp.clone()
    }

    pub fn update_orders(&mut self, data: &HashMap<String, HashMap<String, serde_json::Value>>) {
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

    /// Sync `run_fn` callback, same shape as sync
    /// `OrderStrategy::run`. Closure takes `&mut
    /// AsyncCompoundOrder` — if the closure wants to call
    /// `execute_all_async`, it has to bridge through its own
    /// runtime (rarely needed — real strategies manipulate state
    /// and return; the async execution happens from the owning
    /// event loop afterwards).
    pub fn run(&mut self, ltp: &HashMap<String, f64>) {
        for co in &mut self.orders {
            let Some(f) = co.run_fn.clone() else { continue };
            f(co, ltp);
        }
    }

    /// Mirror sync `add(compound)` — cascade strategy clock + inherit
    /// broker if the compound has none.
    pub fn add(&mut self, mut compound: AsyncCompoundOrder) {
        compound.set_clock(self.clock.clone());
        if self.broker.is_some() && compound.broker.is_none() {
            compound.broker = self.broker.clone();
        }
        self.orders.push(compound);
    }

    /// Mirror sync `save()` — per-compound `save()`, counted.
    /// Stays sync (persistence caveat per R12 plan).
    pub fn save(&self) -> usize {
        self.orders.iter().map(|co| co.save()).sum()
    }
}
