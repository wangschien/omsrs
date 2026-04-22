//! R12.1 — async port of [`crate::virtual_broker::VirtualBroker`].
//!
//! Shape:
//! - Interior state lives behind a single `parking_lot::Mutex<Inner>`;
//!   every mutating method takes the lock, does pure CPU work, and
//!   drops before returning. **No await-while-locked** — the
//!   pattern matches `AsyncPaper`'s (see `src/brokers.rs` AsyncPaper
//!   body) and keeps `tokio` out of production dependencies.
//! - `clock: Arc<dyn Clock + Send + Sync>` lives outside the
//!   mutex; `now()` is read before the lock is acquired so the
//!   clock's internal locking never nests with ours.
//! - `SmallRng` stays **inside** the Inner, same as sync
//!   `VirtualBroker::rng` — locking the RNG alongside the state
//!   it feeds preserves the single-source-of-truth ordering that
//!   sync parity tests rely on.
//! - `BrokerReply` is returned directly from the three inherent
//!   methods (`place` / `modify` / `cancel`). The
//!   `impl AsyncBroker` adapter at the bottom collapses that rich
//!   surface to the trait's `Option<String>` / `()` shape; callers
//!   that want the full `BrokerReply` (parity tests, observers)
//!   use the inherent path.
//!
//! Seed parity: an `AsyncVirtualBroker` built with
//! `with_clock_and_seed(clock, s)` feeds the same `SmallRng` call
//! sequence as a sync `VirtualBroker::with_clock_and_seed(clock,
//! s)` hitting the same operations in the same order — R12.1
//! parity harness hash-compares reply sequences across both.
//!
//! Diverges from sync API:
//! - `orders_mut()` is **not** exposed. Sync returns
//!   `&mut HashMap<...>`, which is incompatible with async shared
//!   ownership. Callers mutate via `place` / `modify` / `cancel`.
//! - Accessors (`orders()`, `clients()`, `clock()`) return owned
//!   clones rather than borrowed references — an `async fn`
//!   can't tie borrows to a self lifetime that survives an await.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use rand::{rngs::SmallRng, Rng, SeedableRng};
use serde_json::Value;

use crate::async_broker::AsyncBroker;
use crate::clock::{clock_system_default, Clock};
use crate::simulation::{
    OrderResponse, OrderType, ResponseStatus, Side, Status, Ticker, VOrder, VOrderInit, VUser, OHLC,
};
use crate::virtual_broker::{BrokerReply, VirtualBrokerError, DEFAULT_DELAY_US};

/// Interior state; held under a single `parking_lot::Mutex` for
/// the full duration of each inherent call. No await happens
/// while this is locked.
struct Inner {
    name: String,
    tickers: HashMap<String, Ticker>,
    users: Vec<VUser>,
    failure_rate: f64,
    delay_us: i64,
    orders: HashMap<String, VOrder>,
    clients: HashSet<String>,
    rng: SmallRng,
}

/// Async virtual matching engine. Semantically identical to
/// `VirtualBroker` — same fill delay, same `is_failure` roll order,
/// same `BrokerReply` shape — with an `AsyncBroker` impl on top.
pub struct AsyncVirtualBroker {
    inner: Mutex<Inner>,
    clock: Arc<dyn Clock + Send + Sync>,
}

impl Default for AsyncVirtualBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncVirtualBroker {
    pub fn new() -> Self {
        Self::with_clock(clock_system_default())
    }

    pub fn with_clock(clock: Arc<dyn Clock + Send + Sync>) -> Self {
        Self::with_clock_and_seed(clock, 0)
    }

    pub fn with_clock_and_seed(clock: Arc<dyn Clock + Send + Sync>, seed: u64) -> Self {
        Self {
            inner: Mutex::new(Inner {
                name: "VBroker".into(),
                tickers: HashMap::new(),
                users: Vec::new(),
                failure_rate: 0.001,
                delay_us: DEFAULT_DELAY_US,
                orders: HashMap::new(),
                clients: HashSet::new(),
                rng: SmallRng::seed_from_u64(seed),
            }),
            clock,
        }
    }

    /// Builder setter for tickers. Takes `self` by value so the
    /// builder chain matches sync `VirtualBroker::with_tickers`.
    #[must_use]
    pub fn with_tickers(self, tickers: HashMap<String, Ticker>) -> Self {
        self.inner.lock().tickers = tickers;
        self
    }

    /// Owned clone of the broker name.
    pub fn name(&self) -> String {
        self.inner.lock().name.clone()
    }

    pub fn failure_rate(&self) -> f64 {
        self.inner.lock().failure_rate
    }

    /// Mirror of sync `set_failure_rate`. Returns the same
    /// pydantic-parity guard error on out-of-range values.
    pub fn set_failure_rate(&self, v: f64) -> Result<(), VirtualBrokerError> {
        if !(0.0..=1.0).contains(&v) {
            return Err(VirtualBrokerError::FailureRateOutOfRange(v));
        }
        self.inner.lock().failure_rate = v;
        Ok(())
    }

    /// Roll `rng` once + compare to `failure_rate`. Advances RNG
    /// state deterministically — sync parity tests count on the
    /// same call sequence producing the same boolean sequence.
    pub fn is_failure(&self) -> bool {
        let mut inner = self.inner.lock();
        let fr = inner.failure_rate;
        let r: f64 = inner.rng.gen();
        r < fr
    }

    /// Owned snapshot of the order map. `VOrder` isn't `Clone`
    /// (it holds its own `Mutex<SmallRng>`); we use the existing
    /// `cloned_clone_weak` bridge that `order_place` already uses
    /// for response-data snapshots.
    pub fn orders(&self) -> HashMap<String, VOrder> {
        let inner = self.inner.lock();
        inner
            .orders
            .iter()
            .filter_map(|(k, v)| v.cloned_clone_weak().map(|clone| (k.clone(), clone)))
            .collect()
    }

    /// Owned snapshot of client userids.
    pub fn clients(&self) -> HashSet<String> {
        self.inner.lock().clients.clone()
    }

    /// Count of registered users. Sync exposes `VirtualBroker.users`
    /// as a `pub Vec<VUser>`; async can't hand out the vec (each
    /// `VUser.orders: Vec<VOrder>` has `VOrder` with interior RNG,
    /// so cloning the whole tree is expensive and rarely what the
    /// caller wants). For parity-test needs, `users_count` +
    /// `user_order_count` are enough.
    pub fn users_count(&self) -> usize {
        self.inner.lock().users.len()
    }

    /// Orders attached to a specific user. Returns `None` if the
    /// user isn't registered. Userid comparison is case-sensitive
    /// against the stored value (sync uppercases at attach time —
    /// R12.1 test helpers pass already-uppercased ids so this is
    /// consistent).
    pub fn user_order_count(&self, userid: &str) -> Option<usize> {
        let inner = self.inner.lock();
        inner
            .users
            .iter()
            .find(|u| u.userid == userid)
            .map(|u| u.orders.len())
    }

    /// `Arc` clone of the clock. Cheap.
    pub fn clock(&self) -> Arc<dyn Clock + Send + Sync> {
        self.clock.clone()
    }

    /// Mirror of sync `get(order_id, status)`. Mutates the target
    /// order's status via `modify_by_status` and returns a weak
    /// clone of the mutated order (sync returns `&mut VOrder`; we
    /// return owned because an async fn can't hand out borrows).
    pub fn get(&self, order_id: &str, status: Status) -> Option<VOrder> {
        let now = self.clock.now();
        let mut inner = self.inner.lock();
        let order = inner.orders.get_mut(order_id)?;
        order.modify_by_status(status, now);
        order.cloned_clone_weak()
    }

    pub fn get_default(&self, order_id: &str) -> Option<VOrder> {
        self.get(order_id, Status::Complete)
    }

    /// Mirror of sync `add_user`. `true` if new, `false` if
    /// userid already registered.
    pub fn add_user(&self, user: VUser) -> bool {
        let mut inner = self.inner.lock();
        if inner.clients.contains(&user.userid) {
            return false;
        }
        inner.clients.insert(user.userid.clone());
        inner.users.push(user);
        true
    }

    /// Async `order_place`, returning the full `BrokerReply`.
    /// Semantically identical to sync `VirtualBroker::order_place`.
    ///
    /// The method is marked `async` (no internal awaits today) so
    /// the signature is compatible with the `AsyncBroker` adapter
    /// below and future changes can add I/O without a breaking
    /// signature change.
    pub async fn place(&self, mut args: HashMap<String, Value>) -> BrokerReply {
        if let Some(v) = args.remove("response") {
            return BrokerReply::Passthrough(v);
        }
        let now = self.clock.now();

        // Validate required fields before taking the lock — no
        // shared state needed and this produces the same
        // "Field required" error text as sync.
        let mut errors: Vec<&'static str> = Vec::new();
        if !args.contains_key("symbol") {
            errors.push("symbol");
        }
        if !args.contains_key("side") && !args.contains_key("side_str") {
            errors.push("side");
        }
        if !args.contains_key("quantity") {
            errors.push("quantity");
        }

        let mut inner = self.inner.lock();

        // Failure roll mirrors sync exactly: even on validation
        // failure, sync performs the is_failure roll FIRST
        // (`virtual_broker.rs:168`), so the RNG state advances
        // whether or not args are well-formed. Preserve that order.
        let fr = inner.failure_rate;
        let r: f64 = inner.rng.gen();
        if r < fr {
            return BrokerReply::Order(Box::new(OrderResponse {
                status: ResponseStatus::Failure,
                timestamp: Some(now),
                error_msg: Some("Unexpected error".into()),
                data: None,
            }));
        }

        if !errors.is_empty() {
            let n = errors.len();
            let fld = errors[0];
            let msg = format!("Found {n} validation errors; in field {fld} Field required");
            return BrokerReply::Order(Box::new(OrderResponse {
                status: ResponseStatus::Failure,
                timestamp: Some(now),
                error_msg: Some(msg),
                data: None,
            }));
        }

        let delay_us = args
            .remove("delay")
            .and_then(|v| v.as_f64().map(|f| f as i64).or_else(|| v.as_i64()))
            .unwrap_or(inner.delay_us);
        let userid_upper = args
            .remove("userid")
            .and_then(|v| v.as_str().map(|s| s.to_ascii_uppercase()));

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
            side_str: args
                .get("side_str")
                .and_then(Value::as_str)
                .map(str::to_string),
            price: args.get("price").and_then(Value::as_f64),
            trigger_price: args.get("trigger_price").and_then(Value::as_f64),
            average_price: args.get("average_price").and_then(Value::as_f64),
            order_type: args.get("order_type").and_then(value_to_order_type),
            order_type_str: args
                .get("order_type")
                .and_then(Value::as_str)
                .map(str::to_string),
            now_override: Some(now),
            ..Default::default()
        };

        let mut order = match VOrder::from_init(init) {
            Ok(o) => o,
            Err(msg) => {
                return BrokerReply::Order(Box::new(OrderResponse {
                    status: ResponseStatus::Failure,
                    timestamp: Some(now),
                    error_msg: Some(format!("Found 1 validation errors; in field side {msg}")),
                    data: None,
                }));
            }
        };
        order.delay = chrono::Duration::microseconds(delay_us);
        inner.orders.insert(order_id.clone(), order);

        if let Some(uid) = userid_upper {
            if inner.clients.contains(&uid) {
                let attached = inner
                    .orders
                    .get(&order_id)
                    .and_then(VOrder::cloned_clone_weak);
                for u in &mut inner.users {
                    if u.userid == uid {
                        if let Some(clone) = attached {
                            u.orders.push(clone);
                        }
                        break;
                    }
                }
            }
        }

        let data = inner
            .orders
            .get(&order_id)
            .and_then(|o| o.cloned_clone_weak());
        BrokerReply::Order(Box::new(OrderResponse {
            status: ResponseStatus::Success,
            timestamp: Some(now),
            error_msg: None,
            data,
        }))
    }

    /// Async `order_modify`. Reads `order_id` from args (sync
    /// takes it as a separate parameter; the `AsyncBroker` trait
    /// signature doesn't carry an explicit order_id so we use the
    /// kwarg form for both inherent and trait paths).
    pub async fn modify(&self, mut args: HashMap<String, Value>) -> BrokerReply {
        if let Some(v) = args.remove("response") {
            return BrokerReply::Passthrough(v);
        }
        let now = self.clock.now();

        let Some(order_id) = args
            .get("order_id")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            return BrokerReply::Order(Box::new(OrderResponse {
                status: ResponseStatus::Failure,
                timestamp: Some(now),
                error_msg: Some("Found 1 validation errors; in field order_id Field required".into()),
                data: None,
            }));
        };

        let mut inner = self.inner.lock();
        let fr = inner.failure_rate;
        let r: f64 = inner.rng.gen();
        if r < fr {
            return BrokerReply::Order(Box::new(OrderResponse {
                status: ResponseStatus::Failure,
                timestamp: Some(now),
                error_msg: Some("Unexpected error".into()),
                data: None,
            }));
        }
        if !inner.orders.contains_key(&order_id) {
            return BrokerReply::Order(Box::new(OrderResponse {
                status: ResponseStatus::Failure,
                timestamp: Some(now),
                error_msg: Some(format!("Order id {order_id} not found on system")),
                data: None,
            }));
        }
        let order = inner.orders.get_mut(&order_id).unwrap();
        if let Some(v) = args.get("price").and_then(Value::as_f64) {
            order.price = Some(v);
        }
        if let Some(v) = args.get("trigger_price").and_then(Value::as_f64) {
            order.trigger_price = Some(v);
        }
        if let Some(v) = args.get("quantity").and_then(Value::as_f64) {
            order.quantity = v;
        }
        let data = order.cloned_clone_weak();
        BrokerReply::Order(Box::new(OrderResponse {
            status: ResponseStatus::Success,
            timestamp: Some(now),
            error_msg: None,
            data,
        }))
    }

    /// Async `order_cancel`. Same order_id-via-kwarg convention
    /// as `modify`.
    pub async fn cancel(&self, mut args: HashMap<String, Value>) -> BrokerReply {
        if let Some(v) = args.remove("response") {
            return BrokerReply::Passthrough(v);
        }
        let now = self.clock.now();

        let Some(order_id) = args
            .get("order_id")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            return BrokerReply::Order(Box::new(OrderResponse {
                status: ResponseStatus::Failure,
                timestamp: Some(now),
                error_msg: Some("Found 1 validation errors; in field order_id Field required".into()),
                data: None,
            }));
        };

        let mut inner = self.inner.lock();
        let fr = inner.failure_rate;
        let r: f64 = inner.rng.gen();
        if r < fr {
            return BrokerReply::Order(Box::new(OrderResponse {
                status: ResponseStatus::Failure,
                timestamp: Some(now),
                error_msg: Some("Unexpected error".into()),
                data: None,
            }));
        }
        if !inner.orders.contains_key(&order_id) {
            return BrokerReply::Order(Box::new(OrderResponse {
                status: ResponseStatus::Failure,
                timestamp: Some(now),
                error_msg: Some(format!("Order id {order_id} not found on system")),
                data: None,
            }));
        }
        let order = inner.orders.get_mut(&order_id).unwrap();
        if order.status() == Status::Complete {
            return BrokerReply::Order(Box::new(OrderResponse {
                status: ResponseStatus::Failure,
                timestamp: Some(now),
                error_msg: Some(format!("Order {order_id} already completed")),
                data: None,
            }));
        }
        order.canceled_quantity = order.quantity - order.filled_quantity;
        order.pending_quantity = 0.0;
        let data = order.cloned_clone_weak();
        BrokerReply::Order(Box::new(OrderResponse {
            status: ResponseStatus::Success,
            timestamp: Some(now),
            error_msg: None,
            data,
        }))
    }

    pub fn update_tickers(&self, last_price: &HashMap<String, f64>) {
        let inner = self.inner.lock();
        for (k, v) in last_price {
            if let Some(t) = inner.tickers.get(k) {
                t.update(*v);
            }
        }
    }

    pub fn ltp(&self, symbol: &str) -> Option<HashMap<String, f64>> {
        let inner = self.inner.lock();
        let t = inner.tickers.get(symbol)?;
        let mut m = HashMap::new();
        m.insert(symbol.to_string(), t.ltp());
        Some(m)
    }

    pub fn ltp_many(&self, symbols: &[&str]) -> HashMap<String, f64> {
        let inner = self.inner.lock();
        let mut out = HashMap::new();
        for s in symbols {
            if let Some(t) = inner.tickers.get(*s) {
                out.insert((*s).to_string(), t.ltp());
            }
        }
        out
    }

    pub fn ohlc(&self, symbol: &str) -> Option<HashMap<String, OHLC>> {
        let inner = self.inner.lock();
        let t = inner.tickers.get(symbol)?;
        let mut m = HashMap::new();
        m.insert(symbol.to_string(), t.ohlc());
        Some(m)
    }
}

// ── AsyncBroker lossy adapter ─────────────────────────────────
//
// The trait collapses `BrokerReply` to `Option<String>` / `()` —
// callers that want the rich reply use the inherent
// `place`/`modify`/`cancel` methods above. Pbot's event loop only
// needs the `Option<String>` so the lossy path is what production
// actually uses; parity tests drive the inherent path.

#[async_trait]
impl AsyncBroker for AsyncVirtualBroker {
    async fn order_place(&self, args: HashMap<String, Value>) -> Option<String> {
        match self.place(args).await {
            BrokerReply::Order(resp) if resp.status == ResponseStatus::Success => {
                resp.data.as_ref().map(|o| o.order_id.clone())
            }
            _ => None,
        }
    }

    async fn order_modify(&self, args: HashMap<String, Value>) {
        let _ = self.modify(args).await;
    }

    async fn order_cancel(&self, args: HashMap<String, Value>) {
        let _ = self.cancel(args).await;
    }
}

// ── helpers duplicated from sync virtual_broker ──────────────
//
// These are `pub(crate)` in the sync module (actually private);
// re-implementing here avoids widening the sync module's public
// surface. The two helpers are tiny and the duplication is
// explicit.

fn value_to_side(v: &Value) -> Option<Side> {
    if let Some(n) = v.as_i64() {
        return match n {
            1 => Some(Side::Buy),
            -1 => Some(Side::Sell),
            _ => None,
        };
    }
    if let Some(s) = v.as_str() {
        return Side::parse(s).ok();
    }
    None
}

fn value_to_order_type(v: &Value) -> Option<OrderType> {
    v.as_str().and_then(|s| OrderType::parse(s).ok())
}
