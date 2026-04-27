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
//! handle's own mutex internally.
//!
//! ### Lock-order discipline (R12.2 audit closeout — item 1/5)
//!
//! Each broker method acquires AT MOST ONE of {`inner`, per-handle
//! `Mutex<VOrder>`} at a time. Every path that previously held
//! `inner` while taking a handle's lock was restructured into:
//!
//! 1. brief `inner.lock()` to clone out the affected `Arc`
//!    handles (and any scalar state needed during the
//!    handle-work phase),
//! 2. drop `inner` before any `handle.lock()`,
//! 3. re-acquire `inner.lock()` afterwards only to write back
//!    post-work bookkeeping (pushes to `completed`, `retain` on
//!    `fills`).
//!
//! This eliminates the ABBA scenario where an external caller
//! holding a `handle.lock()` could deadlock with a broker method
//! holding `inner.lock()`. `place` is the one exception: it
//! locks `inner` while applying `apply_as_market` to the
//! **newly-constructed** handle that hasn't been returned to the
//! caller yet, so no external party can own a conflicting
//! `handle.lock()` at that moment.

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
            // Route through the dedupe helper for consistency with
            // the cancel + run_fill paths. The unknown-symbol reject
            // path is the only other place that promotes an order
            // straight to `completed` — keep all promotions going
            // through one chokepoint so future race scenarios can't
            // silently bypass the Arc::ptr_eq check.
            push_completed_unique(&mut inner.completed, &handle);
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
    ///
    /// Lock discipline (R12.2 audit closeout): never hold
    /// `inner` while taking the handle's lock. Resolve oid →
    /// handle under a brief `inner.lock()`, release, do the
    /// handle-mutation work, then re-acquire `inner` only to
    /// push into `completed`.
    pub async fn cancel(&self, mut args: HashMap<String, Value>) -> Option<OrderHandle> {
        let order_id = args
            .remove("order_id")
            .and_then(|v| v.as_str().map(str::to_string))?;

        // Phase 1: resolve handle under a brief inner lock.
        let handle = {
            let inner = self.inner.lock();
            inner.orders.get(&order_id)?.clone()
        };

        // Phase 2: mutate handle — inner is released, so an
        // external handle holder cannot deadlock us.
        let append_completed = {
            let mut g = handle.lock();
            if !g.is_done() {
                g.canceled_quantity = g.quantity - g.filled_quantity;
                true
            } else {
                false
            }
        };

        // Phase 3: re-acquire inner briefly for bookkeeping.
        if append_completed {
            let mut inner = self.inner.lock();
            // Idempotent push — `run_fill` may have raced with this
            // cancel and already promoted the same handle to
            // `completed`. Without this guard, both code paths push
            // the same `Arc<Mutex<VOrder>>` and downstream consumers
            // see a duplicate completion. Match by Arc identity.
            push_completed_unique(&mut inner.completed, &handle);
        }
        Some(handle)
    }

    /// Async `run_fill` — iterates pending fills, applies the
    /// OrderFill update, promotes done orders to `completed`,
    /// trims the `fills` list. Same bookkeeping as sync.
    ///
    /// Lock discipline (R12.2 audit closeout): never hold
    /// `inner` while taking a fill-order handle's lock.
    /// Restructured into phases:
    ///
    /// - Phase A (under inner): snapshot (i, handle, symbol_seed,
    ///   instrument_price) for every fill whose instrument is
    ///   registered. `symbol_seed` is reused if the handle still
    ///   holds that symbol after phase B — we re-check to avoid
    ///   a racy symbol change (in practice symbol never changes
    ///   after place, but the invariant is locally verified).
    /// - Phase B (no locks held): acquire each handle's lock in
    ///   isolation, apply `apply_fill_update`, detect done.
    /// - Phase C (under inner): write back `last_price` into
    ///   `inner.fills[i]`, push completed handles, retain
    ///   non-done fills.
    ///
    /// Symbol resolution moves to phase A because `fill.order.
    /// lock()` must happen outside the inner-lock scope. We read
    /// the symbol under a handle lock that happens OUTSIDE the
    /// inner lock — this is a separate sub-phase A2 that needs
    /// its own iteration; for simplicity we copy `symbol` inside
    /// each handle lock in phase B's prelude.
    pub async fn run_fill(&self) {
        // Phase A: collect (index, handle-arc clone, last_price)
        // under inner.lock(), using only instrument metadata
        // (string lookup) — do NOT lock any order handle here.
        // We can't read `fill.order.symbol` because that needs a
        // handle lock; instead we'll resolve symbol → instrument
        // in phase B where the handle lock is already held.
        let handles_with_index: Vec<(usize, OrderHandle)> = {
            let inner = self.inner.lock();
            inner
                .fills
                .iter()
                .enumerate()
                .map(|(i, f)| (i, f.order.clone()))
                .collect()
        };
        let instruments_snapshot: HashMap<String, f64> = {
            let inner = self.inner.lock();
            inner
                .instruments
                .iter()
                .map(|(k, v)| (k.clone(), v.last_price))
                .collect()
        };

        // Phase B: handle work with no broker lock held. For each
        // fill, read symbol + look up last_price + apply_fill_
        // update + detect done.
        let mut price_writes: Vec<(usize, f64)> = Vec::new();
        let mut done_handles: Vec<OrderHandle> = Vec::new();
        for (i, handle) in &handles_with_index {
            let symbol = { handle.lock().symbol.clone() };
            let Some(&last_price) = instruments_snapshot.get(&symbol) else {
                continue;
            };
            if last_price == 0.0 {
                continue;
            }
            apply_fill_update(handle, last_price);
            let is_done = { handle.lock().is_done() };
            price_writes.push((*i, last_price));
            if is_done {
                done_handles.push(handle.clone());
            }
        }

        // Phase C: reconcile — write back last_price, push
        // completed, retain non-done fills. Every handle.lock()
        // below is brief and happens on a handle we already
        // know's done (checked in phase B); an external holder
        // cannot be modifying a done order.
        let mut inner = self.inner.lock();
        for (i, last_price) in price_writes {
            if let Some(fill) = inner.fills.get_mut(i) {
                fill.last_price = last_price;
            }
        }
        for h in &done_handles {
            // Idempotent push — a concurrent `cancel` running
            // between Phase A's release and Phase C's acquisition
            // may have already pushed the same handle. Without the
            // dedupe, downstream consumers see double-completed
            // orders. Match by Arc identity.
            push_completed_unique(&mut inner.completed, h);
        }
        // `retain` needs a is_done check per fill; we already
        // collected done handles by identity. Use Arc::ptr_eq.
        inner.fills.retain(|f| {
            !done_handles
                .iter()
                .any(|h| Arc::ptr_eq(h, &f.order))
        });
    }
}

/// Push `handle` into `completed` only if no existing entry shares
/// the same Arc identity. Used by both `cancel` (Phase 3) and
/// `run_fill` (Phase C) so a race between them — where one path's
/// inner-lock release window overlaps the other's apply-and-promote
/// — does not produce a duplicate completed entry.
///
/// The cost is `O(n)` per push, with `n = inner.completed.len()`.
/// Acceptable because the broker is single-account paper-trading;
/// sustained completed lists are short-lived (the consumer drains
/// them per tick).
fn push_completed_unique(completed: &mut Vec<OrderHandle>, handle: &OrderHandle) {
    if !completed.iter().any(|c| Arc::ptr_eq(c, handle)) {
        completed.push(handle.clone());
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
