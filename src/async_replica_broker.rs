//! R12.2 — async port of [`crate::replica_broker::ReplicaBroker`].
//!
//! This is a **direct port of the standalone matching engine**, not
//! a primary/replica wrapper. Sync `ReplicaBroker` keeps `orders` /
//! `pending` / `completed` / `fills` / `user_orders` / `instruments`
//! as public state; the async port preserves that surface via owned
//! snapshot accessors (sync's `pub` fields can't carry borrows
//! across an await).
//!
//! Shared-identity contract: `OrderHandle = Arc<Mutex<VOrder>>` is
//! reused unchanged from the sync module. Every collection holds the
//! same `Arc`, so `Arc::ptr_eq` across `orders()` / `pending()` /
//! `completed()` / `fills()` / `user_orders()` still compares
//! identity — a core sync parity test depends on this.
//!
//! Shape + locking pattern mirror `AsyncVirtualBroker`:
//! - single `parking_lot::Mutex<Inner>` for the bookkeeping
//!   collections
//! - no `.await` while the inner lock is held
//! - every inherent method returns the rich sync shape
//!   (`OrderHandle`); the `impl AsyncBroker` adapter collapses it
//!   to `Option<String>` / `()` for trait-object use
//!
//! `apply_as_market` / `apply_fill_update` are re-exported as
//! `pub(crate)` from `replica_broker` to avoid duplicating the
//! OrderFill semantics. Both take `&OrderHandle` and acquire the
//! handle's own mutex internally; we hold `inner.lock()` across
//! those calls only during `place` / `run_fill`, where no external
//! caller has yet received the handle (so there's no contention
//! path that could deadlock).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::Value;

use crate::async_broker::AsyncBroker;
use crate::replica_broker::{
    apply_as_market, apply_fill_update, value_to_order_type, value_to_side, OrderHandle,
    ReplicaFill,
};
use crate::simulation::{Instrument, VOrder, VOrderInit};

struct Inner {
    name: String,
    instruments: HashMap<String, Instrument>,
    orders: HashMap<String, OrderHandle>,
    users: HashSet<String>,
    pending: Vec<OrderHandle>,
    completed: Vec<OrderHandle>,
    fills: Vec<ReplicaFill>,
    user_orders: HashMap<String, Vec<OrderHandle>>,
}

/// Async matching engine. Semantically identical to sync
/// `ReplicaBroker` (same OrderFill rules, same shared-identity
/// semantics) with `async fn` surface + `AsyncBroker` adapter.
pub struct AsyncReplicaBroker {
    inner: Mutex<Inner>,
}

impl Default for AsyncReplicaBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncReplicaBroker {
    pub fn new() -> Self {
        let mut users = HashSet::new();
        users.insert("default".into());
        Self {
            inner: Mutex::new(Inner {
                name: "replica".into(),
                instruments: HashMap::new(),
                orders: HashMap::new(),
                users,
                pending: Vec::new(),
                completed: Vec::new(),
                fills: Vec::new(),
                user_orders: HashMap::new(),
            }),
        }
    }

    pub fn name(&self) -> String {
        self.inner.lock().name.clone()
    }

    pub fn update(&self, instruments: Vec<Instrument>) {
        let mut inner = self.inner.lock();
        for inst in instruments {
            let name = inst.name.clone();
            inner.instruments.insert(name, inst);
        }
    }

    // ── owned snapshots ──────────────────────────────────────
    //
    // Every accessor returns an owned clone of the underlying
    // HashMap / Vec. For handle-holding collections that means
    // cloning the `Arc`s — shared identity is preserved, external
    // callers can still `Arc::ptr_eq` + `.lock()` the inner VOrder.

    pub fn instruments(&self) -> HashMap<String, Instrument> {
        self.inner.lock().instruments.clone()
    }

    pub fn orders(&self) -> HashMap<String, OrderHandle> {
        self.inner.lock().orders.clone()
    }

    pub fn users(&self) -> HashSet<String> {
        self.inner.lock().users.clone()
    }

    pub fn pending(&self) -> Vec<OrderHandle> {
        self.inner.lock().pending.clone()
    }

    pub fn completed(&self) -> Vec<OrderHandle> {
        self.inner.lock().completed.clone()
    }

    pub fn fills(&self) -> Vec<ReplicaFill> {
        self.inner.lock().fills.clone()
    }

    pub fn user_orders(&self, user: &str) -> Option<Vec<OrderHandle>> {
        self.inner.lock().user_orders.get(user).cloned()
    }

    /// Async `order_place` — returns the `OrderHandle` for shared
    /// identity across collections. Behaves like sync
    /// `ReplicaBroker::order_place`.
    pub async fn place(&self, mut args: HashMap<String, Value>) -> OrderHandle {
        let user = args
            .remove("user")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "default".into());
        let order_id = uuid::Uuid::new_v4().simple().to_string();

        let init = VOrderInit {
            order_id: order_id.clone(),
            symbol: args
                .get("symbol")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            quantity: args.get("quantity").and_then(Value::as_f64).unwrap_or(0.0),
            side: args.get("side").and_then(value_to_side),
            price: args.get("price").and_then(Value::as_f64),
            trigger_price: args.get("trigger_price").and_then(Value::as_f64),
            order_type: args.get("order_type").and_then(value_to_order_type),
            ..Default::default()
        };

        // Build the VOrder outside the lock — from_init is CPU-only.
        let order = VOrder::from_init(init).expect(
            "AsyncReplicaBroker rejects invalid side via upstream ValueError path — \
             upstream doesn't guard either",
        );
        let handle: OrderHandle = Arc::new(Mutex::new(order));

        let mut inner = self.inner.lock();
        inner.orders.insert(order_id.clone(), handle.clone());
        inner
            .user_orders
            .entry(user.clone())
            .or_default()
            .push(handle.clone());
        inner.users.insert(user);

        let symbol = handle.lock().symbol.clone();
        if !inner.instruments.contains_key(&symbol) {
            let mut g = handle.lock();
            g.status_message = Some(format!("REJECTED: Symbol {symbol} not found on the system"));
            g.canceled_quantity = g.quantity;
            g.pending_quantity = 0.0;
            drop(g);
            inner.completed.push(handle.clone());
            return handle;
        }

        let last_price = inner.instruments[&symbol].last_price;
        // Apply OrderFill's `as_market` branch in-place on the
        // shared order handle.
        apply_as_market(&handle, last_price);
        inner.pending.push(handle.clone());
        inner.fills.push(ReplicaFill {
            order: handle.clone(),
            last_price,
        });
        handle
    }

    /// Async `order_modify`. `order_id` lives in `args["order_id"]`
    /// per R12.1 kwarg convention (sync takes it as a separate
    /// param — async normalizes for the `AsyncBroker` trait shape).
    pub async fn modify(&self, mut args: HashMap<String, Value>) -> Option<OrderHandle> {
        let order_id = args
            .remove("order_id")
            .and_then(|v| v.as_str().map(str::to_string))?;

        let inner = self.inner.lock();
        let handle = inner.orders.get(&order_id)?.clone();
        drop(inner);

        {
            let mut g = handle.lock();
            for (k, v) in &args {
                match k.as_str() {
                    "price" => {
                        if let Some(n) = v.as_f64() {
                            g.price = Some(n);
                        }
                    }
                    "trigger_price" => {
                        if let Some(n) = v.as_f64() {
                            g.trigger_price = Some(n);
                        }
                    }
                    "quantity" => {
                        if let Some(n) = v.as_f64() {
                            g.quantity = n;
                        }
                    }
                    "order_type" => {
                        if let Some(ot) = value_to_order_type(v) {
                            g.order_type = ot;
                        }
                    }
                    _ => {}
                }
            }
        }
        Some(handle)
    }

    /// Async `order_cancel`. `order_id` via args kwarg (same
    /// convention as `modify`).
    pub async fn cancel(&self, mut args: HashMap<String, Value>) -> Option<OrderHandle> {
        let order_id = args
            .remove("order_id")
            .and_then(|v| v.as_str().map(str::to_string))?;

        let mut inner = self.inner.lock();
        let handle = inner.orders.get(&order_id)?.clone();

        let mut append_completed = false;
        {
            let mut g = handle.lock();
            if !g.is_done() {
                g.canceled_quantity = g.quantity - g.filled_quantity;
                append_completed = true;
            }
        }
        if append_completed {
            inner.completed.push(handle.clone());
        }
        Some(handle)
    }

    /// Async `run_fill` — iterates pending fills, applies the
    /// OrderFill update, promotes done orders to `completed`,
    /// trims the `fills` list. Same bookkeeping as sync.
    pub async fn run_fill(&self) {
        let mut inner = self.inner.lock();
        let mut done_ids: Vec<String> = Vec::new();

        // Compute (fill_index, last_price) pairs up-front without
        // retaining a borrow on `inner.fills` — the borrow
        // checker needs either fills or instruments borrowed at
        // a time, not both. Index into `inner.fills` each time
        // so `fill.order` is grabbed as `Arc::clone`.
        let price_updates: Vec<(usize, f64)> = {
            let mut out = Vec::new();
            for (i, fill) in inner.fills.iter().enumerate() {
                let symbol = fill.order.lock().symbol.clone();
                let Some(inst) = inner.instruments.get(&symbol) else {
                    continue;
                };
                let last_price = inst.last_price;
                if last_price == 0.0 {
                    continue;
                }
                out.push((i, last_price));
            }
            out
        };

        for (i, last_price) in price_updates {
            inner.fills[i].last_price = last_price;
            let handle = inner.fills[i].order.clone();
            apply_fill_update(&handle, last_price);
            if handle.lock().is_done() {
                done_ids.push(handle.lock().order_id.clone());
            }
        }

        // Promote done orders to `completed` — clone handles out
        // of `orders` before pushing to avoid overlapping borrow.
        let mut to_complete: Vec<OrderHandle> = Vec::new();
        for id in &done_ids {
            if let Some(h) = inner.orders.get(id) {
                to_complete.push(h.clone());
            }
        }
        for h in to_complete {
            inner.completed.push(h);
        }
        inner.fills.retain(|f| !f.order.lock().is_done());
    }
}

// ── AsyncBroker lossy adapter ────────────────────────────────
//
// Same pattern as `AsyncVirtualBroker`: rich `OrderHandle` on the
// inherent methods; `Option<String>` / `()` on the trait path for
// dyn-dispatch consumers (pbot-style `Arc<dyn AsyncBroker>`).

#[async_trait]
impl AsyncBroker for AsyncReplicaBroker {
    async fn order_place(&self, args: HashMap<String, Value>) -> Option<String> {
        let handle = self.place(args).await;
        let oid = handle.lock().order_id.clone();
        Some(oid)
    }

    async fn order_modify(&self, args: HashMap<String, Value>) {
        let _ = self.modify(args).await;
    }

    async fn order_cancel(&self, args: HashMap<String, Value>) {
        let _ = self.cancel(args).await;
    }
}
