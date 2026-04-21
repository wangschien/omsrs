//! Broker implementations. R4 adds `Paper` (dummy in-memory broker used as a
//! scaffolding target for tests). Real venue brokers (zerodha, finvasia, …)
//! stay out of the MVP per PORT-PLAN §2.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;

use crate::broker::Broker;

/// Upstream `omspy.brokers.paper.Paper` — echoes kwargs through the
/// `order_place / order_modify / order_cancel` trio and exposes an
/// in-memory `orders` / `trades` / `positions` snapshot.
#[derive(Debug, Default)]
pub struct Paper {
    orders: Mutex<Option<Vec<HashMap<String, Value>>>>,
    trades: Mutex<Option<Vec<HashMap<String, Value>>>>,
    positions: Mutex<Option<Vec<HashMap<String, Value>>>>,
    place_calls: Mutex<Vec<HashMap<String, Value>>>,
    modify_calls: Mutex<Vec<HashMap<String, Value>>>,
    cancel_calls: Mutex<Vec<HashMap<String, Value>>>,
}

impl Paper {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_orders(mut self, orders: Vec<HashMap<String, Value>>) -> Self {
        *self.orders.get_mut().unwrap() = Some(orders);
        self
    }

    pub fn with_trades(mut self, trades: Vec<HashMap<String, Value>>) -> Self {
        *self.trades.get_mut().unwrap() = Some(trades);
        self
    }

    pub fn with_positions(mut self, positions: Vec<HashMap<String, Value>>) -> Self {
        *self.positions.get_mut().unwrap() = Some(positions);
        self
    }

    pub fn place_calls(&self) -> Vec<HashMap<String, Value>> {
        self.place_calls.lock().unwrap().clone()
    }

    pub fn modify_calls(&self) -> Vec<HashMap<String, Value>> {
        self.modify_calls.lock().unwrap().clone()
    }

    pub fn cancel_calls(&self) -> Vec<HashMap<String, Value>> {
        self.cancel_calls.lock().unwrap().clone()
    }

    pub fn place_call_count(&self) -> usize {
        self.place_calls.lock().unwrap().len()
    }

    pub fn modify_call_count(&self) -> usize {
        self.modify_calls.lock().unwrap().len()
    }

    pub fn cancel_call_count(&self) -> usize {
        self.cancel_calls.lock().unwrap().len()
    }
}

impl Broker for Paper {
    fn order_place(&self, args: HashMap<String, Value>) -> Option<String> {
        let mut guard = self.place_calls.lock().unwrap();
        guard.push(args);
        Some(format!("PAPER-{}", guard.len()))
    }

    fn order_modify(&self, args: HashMap<String, Value>) {
        self.modify_calls.lock().unwrap().push(args);
    }

    fn order_cancel(&self, args: HashMap<String, Value>) {
        self.cancel_calls.lock().unwrap().push(args);
    }

    fn orders(&self) -> Vec<HashMap<String, Value>> {
        self.orders
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| vec![HashMap::new()])
    }

    fn trades(&self) -> Vec<HashMap<String, Value>> {
        self.trades
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| vec![HashMap::new()])
    }

    fn positions(&self) -> Vec<HashMap<String, Value>> {
        self.positions
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| vec![HashMap::new()])
    }
}

// ── R11.2: AsyncPaper ──────────────────────────────────────────────
//
// Async sibling of `Paper`. Semantically identical: methods record
// every call and echo `PAPER-{n}` oids. Uses the same `Mutex` under
// the hood (locks are held only for the short push/clone window, no
// `.await` is called while holding a lock — no parking_lot contract
// ambiguity).
//
// Intended for use with the v0.2 `AsyncBroker` trait. The R11.2
// parity harness asserts AsyncPaper passes the same 10-item suite as
// sync `Paper`.

use async_trait::async_trait;

use crate::async_broker::AsyncBroker;

/// Async counterpart to [`Paper`] that implements
/// [`crate::AsyncBroker`]. Call bodies are sync (lock + push); the
/// methods are `async fn` so this type can feed sync-consumer
/// adapters and async-consumer adapters through the same structure.
#[derive(Debug, Default)]
pub struct AsyncPaper {
    orders: Mutex<Option<Vec<HashMap<String, Value>>>>,
    trades: Mutex<Option<Vec<HashMap<String, Value>>>>,
    positions: Mutex<Option<Vec<HashMap<String, Value>>>>,
    place_calls: Mutex<Vec<HashMap<String, Value>>>,
    modify_calls: Mutex<Vec<HashMap<String, Value>>>,
    cancel_calls: Mutex<Vec<HashMap<String, Value>>>,
}

impl AsyncPaper {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_orders(mut self, orders: Vec<HashMap<String, Value>>) -> Self {
        *self.orders.get_mut().unwrap() = Some(orders);
        self
    }

    pub fn with_trades(mut self, trades: Vec<HashMap<String, Value>>) -> Self {
        *self.trades.get_mut().unwrap() = Some(trades);
        self
    }

    pub fn with_positions(mut self, positions: Vec<HashMap<String, Value>>) -> Self {
        *self.positions.get_mut().unwrap() = Some(positions);
        self
    }

    pub fn place_calls(&self) -> Vec<HashMap<String, Value>> {
        self.place_calls.lock().unwrap().clone()
    }

    pub fn modify_calls(&self) -> Vec<HashMap<String, Value>> {
        self.modify_calls.lock().unwrap().clone()
    }

    pub fn cancel_calls(&self) -> Vec<HashMap<String, Value>> {
        self.cancel_calls.lock().unwrap().clone()
    }

    pub fn place_call_count(&self) -> usize {
        self.place_calls.lock().unwrap().len()
    }

    pub fn modify_call_count(&self) -> usize {
        self.modify_calls.lock().unwrap().len()
    }

    pub fn cancel_call_count(&self) -> usize {
        self.cancel_calls.lock().unwrap().len()
    }
}

#[async_trait]
impl AsyncBroker for AsyncPaper {
    async fn order_place(&self, args: HashMap<String, Value>) -> Option<String> {
        let mut guard = self.place_calls.lock().unwrap();
        guard.push(args);
        Some(format!("PAPER-{}", guard.len()))
    }

    async fn order_modify(&self, args: HashMap<String, Value>) {
        self.modify_calls.lock().unwrap().push(args);
    }

    async fn order_cancel(&self, args: HashMap<String, Value>) {
        self.cancel_calls.lock().unwrap().push(args);
    }

    async fn orders(&self) -> Vec<HashMap<String, Value>> {
        self.orders
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| vec![HashMap::new()])
    }

    async fn trades(&self) -> Vec<HashMap<String, Value>> {
        self.trades
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| vec![HashMap::new()])
    }

    async fn positions(&self) -> Vec<HashMap<String, Value>> {
        self.positions
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| vec![HashMap::new()])
    }
}
