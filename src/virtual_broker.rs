//! `omspy.simulation.virtual.VirtualBroker` port (PORT-PLAN §8 R6).
//!
//! Multi-user virtual broker driving `VOrder` lifecycles in memory. Tests
//! inject a `MockClock` to make `VOrder::is_past_delay_at` deterministic;
//! `is_failure` reads a seeded `SmallRng` rather than `random.random()`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;
use rand::{rngs::SmallRng, Rng, SeedableRng};
use serde_json::Value;

use crate::clock::{clock_system_default, Clock};
use crate::simulation::{
    OrderResponse, OrderType, ResponseStatus, Side, Status, Ticker, VOrder, VOrderInit, VUser, OHLC,
};

/// Default per-order fill delay (matches upstream `_delay = 1_000_000` μs).
pub const DEFAULT_DELAY_US: i64 = 1_000_000;

/// Return shape of `order_place` / `order_modify` / `order_cancel`.
/// Upstream returns `Union[OrderResponse, dict]`: when the caller passes a
/// `response=` kwarg the method short-circuits and returns the passed value
/// verbatim. The `Passthrough(Value)` variant covers that.
#[derive(Debug)]
pub enum BrokerReply {
    Order(Box<OrderResponse>),
    Passthrough(Value),
}

impl BrokerReply {
    pub fn as_order(&self) -> Option<&OrderResponse> {
        match self {
            BrokerReply::Order(r) => Some(r),
            _ => None,
        }
    }

    pub fn as_passthrough(&self) -> Option<&Value> {
        match self {
            BrokerReply::Passthrough(v) => Some(v),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VirtualBrokerError {
    #[error("failure_rate must be in [0, 1], got {0}")]
    FailureRateOutOfRange(f64),
}

pub struct VirtualBroker {
    pub name: String,
    pub tickers: HashMap<String, Ticker>,
    pub users: Vec<VUser>,
    failure_rate: f64,
    delay_us: i64,
    orders: HashMap<String, VOrder>,
    clients: HashSet<String>,
    clock: Arc<dyn Clock + Send + Sync>,
    rng: Mutex<SmallRng>,
}

impl Default for VirtualBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualBroker {
    pub fn new() -> Self {
        Self::with_clock(clock_system_default())
    }

    pub fn with_clock(clock: Arc<dyn Clock + Send + Sync>) -> Self {
        Self::with_clock_and_seed(clock, 0)
    }

    pub fn with_clock_and_seed(clock: Arc<dyn Clock + Send + Sync>, seed: u64) -> Self {
        Self {
            name: "VBroker".into(),
            tickers: HashMap::new(),
            users: Vec::new(),
            failure_rate: 0.001,
            delay_us: DEFAULT_DELAY_US,
            orders: HashMap::new(),
            clients: HashSet::new(),
            clock,
            rng: Mutex::new(SmallRng::seed_from_u64(seed)),
        }
    }

    pub fn with_tickers(mut self, tickers: HashMap<String, Ticker>) -> Self {
        self.tickers = tickers;
        self
    }

    pub fn failure_rate(&self) -> f64 {
        self.failure_rate
    }

    /// Upstream `failure_rate: float = Field(ge=0, le=1)` — pydantic
    /// raises `ValidationError` on out-of-range writes. We surface the
    /// same guard as `Result`.
    pub fn set_failure_rate(&mut self, v: f64) -> Result<(), VirtualBrokerError> {
        if !(0.0..=1.0).contains(&v) {
            return Err(VirtualBrokerError::FailureRateOutOfRange(v));
        }
        self.failure_rate = v;
        Ok(())
    }

    pub fn is_failure(&self) -> bool {
        let r: f64 = self.rng.lock().gen();
        r < self.failure_rate
    }

    pub fn orders(&self) -> &HashMap<String, VOrder> {
        &self.orders
    }

    pub fn orders_mut(&mut self) -> &mut HashMap<String, VOrder> {
        &mut self.orders
    }

    pub fn clients(&self) -> &HashSet<String> {
        &self.clients
    }

    pub fn clock(&self) -> &Arc<dyn Clock + Send + Sync> {
        &self.clock
    }

    /// Upstream `VirtualBroker.get(order_id, status=Status.COMPLETE)`.
    /// Calls `order.modify_by_status(status)` with the broker's clock and
    /// returns a mutable handle so callers can inspect the mutated state.
    pub fn get(&mut self, order_id: &str, status: Status) -> Option<&mut VOrder> {
        let now = self.clock.now();
        let order = self.orders.get_mut(order_id)?;
        order.modify_by_status(status, now);
        Some(order)
    }

    pub fn get_default(&mut self, order_id: &str) -> Option<&mut VOrder> {
        self.get(order_id, Status::Complete)
    }

    pub fn add_user(&mut self, user: VUser) -> bool {
        if self.clients.contains(&user.userid) {
            return false;
        }
        self.clients.insert(user.userid.clone());
        self.users.push(user);
        true
    }

    /// Entry point for `order_place` — maps a kwarg-style `HashMap` into
    /// the validation + construction pipeline. Keys consulted: `symbol`,
    /// `side`, `quantity`, `price`, `trigger_price`, `order_type`,
    /// `userid`, `delay`, and `response` (the short-circuit passthrough).
    pub fn order_place(&mut self, mut args: HashMap<String, Value>) -> BrokerReply {
        if let Some(v) = args.remove("response") {
            return BrokerReply::Passthrough(v);
        }
        let now = self.clock.now();
        if self.is_failure() {
            return BrokerReply::Order(Box::new(OrderResponse {
                status: ResponseStatus::Failure,
                timestamp: Some(now),
                error_msg: Some("Unexpected error".into()),
                data: None,
            }));
        }

        // Validate required VOrder fields.
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

        // Build VOrderInit from the kwargs.
        let delay_us = args
            .remove("delay")
            .and_then(|v| v.as_f64().map(|f| f as i64).or_else(|| v.as_i64()))
            .unwrap_or(self.delay_us);
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
        self.orders.insert(order_id.clone(), order);

        if let Some(uid) = userid_upper {
            if self.clients.contains(&uid) {
                // Attach the same order id to the matching user.
                let attached = self
                    .orders
                    .get(&order_id)
                    .and_then(VOrder::cloned_clone_weak);
                for u in &mut self.users {
                    if u.userid == uid {
                        if let Some(clone) = attached {
                            u.orders.push(clone);
                        }
                        break;
                    }
                }
            }
        }

        // For upstream's "same memory" assertion, we also give the caller
        // a handle by way of `OrderResponse.data`.
        let data = self
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

    pub fn order_modify(
        &mut self,
        order_id: &str,
        mut args: HashMap<String, Value>,
    ) -> BrokerReply {
        if let Some(v) = args.remove("response") {
            return BrokerReply::Passthrough(v);
        }
        let now = self.clock.now();
        if self.is_failure() {
            return BrokerReply::Order(Box::new(OrderResponse {
                status: ResponseStatus::Failure,
                timestamp: Some(now),
                error_msg: Some("Unexpected error".into()),
                data: None,
            }));
        }
        if !self.orders.contains_key(order_id) {
            return BrokerReply::Order(Box::new(OrderResponse {
                status: ResponseStatus::Failure,
                timestamp: Some(now),
                error_msg: Some(format!("Order id {order_id} not found on system")),
                data: None,
            }));
        }
        let order = self.orders.get_mut(order_id).unwrap();
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

    pub fn order_cancel(
        &mut self,
        order_id: &str,
        mut args: HashMap<String, Value>,
    ) -> BrokerReply {
        if let Some(v) = args.remove("response") {
            return BrokerReply::Passthrough(v);
        }
        let now = self.clock.now();
        if self.is_failure() {
            return BrokerReply::Order(Box::new(OrderResponse {
                status: ResponseStatus::Failure,
                timestamp: Some(now),
                error_msg: Some("Unexpected error".into()),
                data: None,
            }));
        }
        if !self.orders.contains_key(order_id) {
            return BrokerReply::Order(Box::new(OrderResponse {
                status: ResponseStatus::Failure,
                timestamp: Some(now),
                error_msg: Some(format!("Order id {order_id} not found on system")),
                data: None,
            }));
        }
        let order = self.orders.get_mut(order_id).unwrap();
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

    pub fn update_tickers(&mut self, last_price: &HashMap<String, f64>) {
        for (k, v) in last_price {
            if let Some(t) = self.tickers.get(k) {
                t.update(*v);
            }
        }
    }

    pub fn ltp(&self, symbol: &str) -> Option<HashMap<String, f64>> {
        let t = self.tickers.get(symbol)?;
        let mut m = HashMap::new();
        m.insert(symbol.to_string(), t.ltp());
        Some(m)
    }

    pub fn ltp_many(&self, symbols: &[&str]) -> HashMap<String, f64> {
        let mut out = HashMap::new();
        for s in symbols {
            if let Some(t) = self.tickers.get(*s) {
                out.insert((*s).to_string(), t.ltp());
            }
        }
        out
    }

    pub fn ohlc(&self, symbol: &str) -> Option<HashMap<String, OHLC>> {
        let t = self.tickers.get(symbol)?;
        let mut m = HashMap::new();
        m.insert(symbol.to_string(), t.ohlc());
        Some(m)
    }
}

/// Helper — extract `Side` from JSON `Value`. Upstream validator accepts
/// `1 / -1 / Side.BUY / Side.SELL` literally; JSON-side we see numbers or
/// strings.
fn value_to_side(v: &Value) -> Option<Side> {
    if let Some(n) = v.as_i64() {
        match n {
            1 => return Some(Side::Buy),
            -1 => return Some(Side::Sell),
            _ => return None,
        }
    }
    if let Some(s) = v.as_str() {
        return Side::parse(s).ok();
    }
    None
}

fn value_to_order_type(v: &Value) -> Option<OrderType> {
    v.as_str().and_then(|s| OrderType::parse(s).ok())
}

// ── VOrder weak-clone bridge ────────────────────────────────────────────

impl VOrder {
    /// "Weak clone" for response-data returns. `VOrder` isn't `Clone` (it
    /// holds a `Mutex<SmallRng>`), so we construct a fresh shell with the
    /// same public fields + a new RNG-seeded helper. The response doesn't
    /// need to mutate the RNG, so a seed=0 fresh RNG is fine.
    pub fn cloned_clone_weak(&self) -> Option<VOrder> {
        VOrder::from_init(VOrderInit {
            order_id: self.order_id.clone(),
            symbol: self.symbol.clone(),
            quantity: self.quantity,
            side: Some(self.side),
            price: self.price,
            average_price: self.average_price,
            trigger_price: self.trigger_price,
            timestamp: self.timestamp,
            exchange_order_id: self.exchange_order_id.clone(),
            exchange_timestamp: self.exchange_timestamp,
            status_message: self.status_message.clone(),
            order_type: Some(self.order_type),
            filled_quantity: Some(self.filled_quantity),
            pending_quantity: Some(self.pending_quantity),
            canceled_quantity: Some(self.canceled_quantity),
            now_override: self.timestamp,
            ..Default::default()
        })
        .ok()
    }
}
