//! Simulation-side models (`omspy.simulation.models` port). Uses `f64` for
//! virtual market values — upstream's `random.gauss`, tick-rounding at 0.05,
//! and float-based order arithmetic aren't precision-critical and f64
//! mirrors the upstream assertions directly. `rust_decimal` continues to
//! back the real Order lifecycle (R1-R4). Plan §6 D10 + §7 call this out.

use std::fmt;

use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use rand::{rngs::SmallRng, Rng, SeedableRng};
use rand_distr::{Distribution, Normal};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use crate::models::{OrderBook, Quote};

// ── enums ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Complete = 1,
    Rejected = 2,
    Canceled = 3,
    PartialFill = 4,
    Open = 5,
    Pending = 6,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResponseStatus {
    Success,
    Failure,
}

impl ResponseStatus {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "success" => Some(Self::Success),
            "failure" => Some(Self::Failure),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Buy = 1,
    Sell = -1,
}

impl Side {
    pub fn value(self) -> i64 {
        match self {
            Side::Buy => 1,
            Side::Sell => -1,
        }
    }

    /// Upstream `accept_buy_sell_as_side` validator — accepts `"buy"`/`"b"`/
    /// `"BUY"` → `Buy`, `"sell"`/`"s"` → `Sell`, errors otherwise.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.chars().next().map(|c| c.to_ascii_lowercase()) {
            Some('b') => Ok(Side::Buy),
            Some('s') => Ok(Side::Sell),
            _ => Err(format!("{s} is not a valid side, should be buy or sell")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TickerMode {
    Random = 1,
    Manual = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    Market = 1,
    Limit = 2,
    Stop = 3,
}

impl OrderType {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_uppercase().as_str() {
            "MARKET" => Ok(OrderType::Market),
            "LIMIT" => Ok(OrderType::Limit),
            "STOP" => Ok(OrderType::Stop),
            _ => Err(format!(
                "{s} is not a valid order type, should be one of LIMIT/MARKET"
            )),
        }
    }
}

// ── OHLC / OHLCV / OHLCVI ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OHLC {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub last_price: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OHLCV {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub last_price: f64,
    pub volume: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OHLCVI {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub last_price: f64,
    pub volume: i64,
    pub open_interest: i64,
}

// ── Ticker ──────────────────────────────────────────────────────────────

/// `omspy.simulation.models.Ticker`. Upstream uses `random.gauss(0, 1) *
/// ltp * 0.01` then rounds to 0.05 tick. Rust uses `SmallRng` +
/// `Normal(0, 1)` seeded at construction (PORT-PLAN §6 D10). The real
/// seed-exact test (`test_ticker_ltp`) is replaced by a statistical
/// target (§14A); the remaining Ticker trials care about predicate
/// semantics (random != 125, manual == 125, OHLC round-trip), which
/// are tolerant of seed choice.
pub struct Ticker {
    pub name: String,
    pub token: Option<i64>,
    pub initial_price: f64,
    pub mode: TickerMode,
    pub orderbook: Option<OrderBook>,
    pub volume: Option<i64>,
    high: Mutex<f64>,
    low: Mutex<f64>,
    ltp_cell: Mutex<f64>,
    rng: Mutex<SmallRng>,
    normal: Normal<f64>,
}

impl fmt::Debug for Ticker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ticker")
            .field("name", &self.name)
            .field("initial_price", &self.initial_price)
            .field("mode", &self.mode)
            .finish()
    }
}

impl Ticker {
    pub fn new(name: impl Into<String>) -> Self {
        Self::with_initial_price(name, 100.0)
    }

    pub fn with_initial_price(name: impl Into<String>, initial_price: f64) -> Self {
        Self::with_seed(name, initial_price, 0)
    }

    pub fn with_seed(
        name: impl Into<String>,
        initial_price: f64,
        seed: u64,
    ) -> Self {
        Self {
            name: name.into(),
            token: None,
            initial_price,
            mode: TickerMode::Random,
            orderbook: None,
            volume: None,
            high: Mutex::new(initial_price),
            low: Mutex::new(initial_price),
            ltp_cell: Mutex::new(initial_price),
            rng: Mutex::new(SmallRng::seed_from_u64(seed)),
            normal: Normal::new(0.0, 1.0).expect("Normal(0,1) is valid"),
        }
    }

    pub fn with_token(mut self, token: i64) -> Self {
        self.token = Some(token);
        self
    }

    pub fn high(&self) -> f64 {
        *self.high.lock()
    }
    pub fn low(&self) -> f64 {
        *self.low.lock()
    }
    pub fn ltp_snapshot(&self) -> f64 {
        *self.ltp_cell.lock()
    }

    pub fn is_random(&self) -> bool {
        matches!(self.mode, TickerMode::Random)
    }

    fn update_values(&self, last_price: f64) {
        *self.ltp_cell.lock() = last_price;
        let mut hi = self.high.lock();
        if last_price > *hi {
            *hi = last_price;
        }
        let mut lo = self.low.lock();
        if last_price < *lo {
            *lo = last_price;
        }
    }

    /// Upstream `Ticker.ltp` property — in random mode, perturbs `_ltp` by
    /// `Z * ltp * 0.01` with `Z ~ N(0,1)` then rounds to the nearest 0.05
    /// tick before updating the running high/low. Returns the new `_ltp`.
    pub fn ltp(&self) -> f64 {
        if self.is_random() {
            let z: f64 = self.normal.sample(&mut *self.rng.lock());
            let current = *self.ltp_cell.lock();
            let diff = z * current * 0.01;
            let raw = current + diff;
            let rounded = (raw * 20.0).round() / 20.0;
            self.update_values(rounded);
        }
        *self.ltp_cell.lock()
    }

    /// Manual update — sets the ltp/high/low without touching the RNG.
    pub fn update(&self, last_price: f64) -> f64 {
        self.update_values(last_price);
        *self.ltp_cell.lock()
    }

    pub fn ohlc(&self) -> OHLC {
        let ltp = *self.ltp_cell.lock();
        OHLC {
            open: self.initial_price,
            high: *self.high.lock(),
            low: *self.low.lock(),
            close: ltp,
            last_price: ltp,
        }
    }
}

// ── VQuote ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VQuote {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub last_price: f64,
    pub volume: i64,
    pub orderbook: OrderBook,
}

// ── VTrade ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VTrade {
    pub trade_id: String,
    pub order_id: String,
    pub symbol: String,
    pub quantity: i64,
    pub price: f64,
    pub side: Side,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
}

impl VTrade {
    pub fn value(&self) -> f64 {
        self.side.value() as f64 * self.quantity as f64 * self.price
    }
}

// ── VOrder ──────────────────────────────────────────────────────────────

/// `omspy.simulation.models.VOrder`. Uses `f64` for quantity / prices to
/// match upstream arithmetic (`order.value == 6000` assertions).
/// `_delay` is a `chrono::Duration` rather than an integer microsecond
/// count. Randomness for `_modify_order_by_status(PARTIAL_FILL/PENDING)`
/// comes from a seeded `SmallRng` held behind a `Mutex`.
#[derive(Debug)]
pub struct VOrder {
    pub order_id: String,
    pub symbol: String,
    pub quantity: f64,
    pub side: Side,
    pub price: Option<f64>,
    pub average_price: Option<f64>,
    pub trigger_price: Option<f64>,
    pub timestamp: Option<DateTime<Utc>>,
    pub exchange_order_id: Option<String>,
    pub exchange_timestamp: Option<DateTime<Utc>>,
    pub status_message: Option<String>,
    pub order_type: OrderType,
    pub filled_quantity: f64,
    pub pending_quantity: f64,
    pub canceled_quantity: f64,
    pub delay: Duration,
    rng: Mutex<SmallRng>,
}

#[derive(Debug, Clone, Default)]
pub struct VOrderInit {
    pub order_id: String,
    pub symbol: String,
    pub quantity: f64,
    pub side: Option<Side>,
    pub side_str: Option<String>,
    pub price: Option<f64>,
    pub average_price: Option<f64>,
    pub trigger_price: Option<f64>,
    pub timestamp: Option<DateTime<Utc>>,
    pub exchange_order_id: Option<String>,
    pub exchange_timestamp: Option<DateTime<Utc>>,
    pub status_message: Option<String>,
    pub order_type: Option<OrderType>,
    pub order_type_str: Option<String>,
    pub filled_quantity: Option<f64>,
    pub pending_quantity: Option<f64>,
    pub canceled_quantity: Option<f64>,
    pub rng_seed: Option<u64>,
    pub now_override: Option<DateTime<Utc>>,
}

impl VOrder {
    pub fn from_init(init: VOrderInit) -> Result<Self, String> {
        let side = match (init.side, init.side_str) {
            (Some(s), _) => s,
            (None, Some(s)) => Side::parse(&s)?,
            (None, None) => {
                return Err("VOrder requires side (enum) or side_str".into())
            }
        };
        let order_type = match (init.order_type, init.order_type_str) {
            (Some(t), _) => t,
            (None, Some(s)) => OrderType::parse(&s)?,
            (None, None) => OrderType::Market,
        };
        let now = init.now_override.unwrap_or_else(Utc::now);
        let timestamp = Some(init.timestamp.unwrap_or(now));
        let filled = init.filled_quantity.unwrap_or(0.0);
        let pending = init.pending_quantity.unwrap_or(0.0);
        let canceled = init.canceled_quantity.unwrap_or(0.0);
        let (f, p, c) = normalise_quantities(init.quantity, filled, pending, canceled);
        let average_price = init.average_price.or(Some(0.0));
        let seed = init.rng_seed.unwrap_or(0);

        Ok(Self {
            order_id: init.order_id,
            symbol: init.symbol,
            quantity: init.quantity,
            side,
            price: init.price,
            average_price,
            trigger_price: init.trigger_price,
            timestamp,
            exchange_order_id: init.exchange_order_id,
            exchange_timestamp: init.exchange_timestamp,
            status_message: init.status_message,
            order_type,
            filled_quantity: f,
            pending_quantity: p,
            canceled_quantity: c,
            delay: Duration::microseconds(1_000_000),
            rng: Mutex::new(SmallRng::seed_from_u64(seed)),
        })
    }

    pub fn make_right_quantity(&mut self) {
        let (f, p, c) = normalise_quantities(
            self.quantity,
            self.filled_quantity,
            self.pending_quantity,
            self.canceled_quantity,
        );
        self.filled_quantity = f;
        self.pending_quantity = p;
        self.canceled_quantity = c;
    }

    /// Private in upstream; public here for the `_modify_by_status_*`
    /// parity trials that exercise it directly.
    pub fn modify_order_by_status(&mut self, status: Status) {
        match status {
            Status::Canceled | Status::Rejected => {
                self.filled_quantity = 0.0;
                self.pending_quantity = 0.0;
                self.canceled_quantity = self.quantity;
            }
            Status::Open => {
                self.filled_quantity = 0.0;
                self.canceled_quantity = 0.0;
            }
            Status::PartialFill => {
                let q = self.quantity as i64;
                let a: i64 = if q > 1 {
                    self.rng.lock().gen_range(1..q)
                } else {
                    1
                };
                let b = q - a;
                self.filled_quantity = a as f64;
                self.pending_quantity = 0.0;
                self.canceled_quantity = b as f64;
            }
            Status::Pending => {
                let q = self.quantity as i64;
                let a: i64 = if q > 1 {
                    self.rng.lock().gen_range(1..q)
                } else {
                    1
                };
                let b = q - a;
                self.filled_quantity = a as f64;
                self.pending_quantity = b as f64;
                self.canceled_quantity = 0.0;
            }
            Status::Complete => {
                self.filled_quantity = self.quantity;
                self.pending_quantity = 0.0;
                self.canceled_quantity = 0.0;
            }
        }
    }

    pub fn is_past_delay(&self) -> bool {
        self.is_past_delay_at(Utc::now())
    }

    /// Clock-injectable variant. The R5 parity trials that care about
    /// `is_past_delay` feed a frozen `now`; production calls
    /// `is_past_delay()` which reads wall time.
    pub fn is_past_delay_at(&self, now: DateTime<Utc>) -> bool {
        match self.timestamp {
            Some(ts) => now > ts + self.delay,
            None => false,
        }
    }

    pub fn status(&self) -> Status {
        if self.quantity == self.filled_quantity {
            return Status::Complete;
        }
        if self.quantity == self.canceled_quantity {
            if let Some(msg) = &self.status_message {
                if msg.to_ascii_uppercase().starts_with("REJ") {
                    return Status::Rejected;
                }
            }
            return Status::Canceled;
        }
        if self.canceled_quantity > 0.0 {
            if (self.canceled_quantity + self.filled_quantity) == self.quantity {
                return Status::PartialFill;
            }
            return Status::Pending;
        }
        if self.pending_quantity > 0.0 {
            if self.filled_quantity > 0.0 {
                return Status::Pending;
            }
            return Status::Open;
        }
        Status::Open
    }

    pub fn value(&self) -> f64 {
        let avg = match self.average_price {
            Some(x) if x != 0.0 => x,
            _ => self.price.unwrap_or(0.0),
        };
        self.side.value() as f64 * self.filled_quantity * avg
    }

    pub fn is_done(&self) -> bool {
        if self.quantity == self.filled_quantity {
            return true;
        }
        if self.quantity == self.canceled_quantity {
            return true;
        }
        self.pending_quantity <= 0.0
    }

    pub fn is_complete(&self) -> bool {
        if self.quantity == self.filled_quantity {
            return true;
        }
        matches!(self.status(), Status::Complete)
    }

    /// Upstream default is `status=Status.COMPLETE`.
    pub fn modify_by_status(&mut self, status: Status, now: DateTime<Utc>) -> bool {
        if self.is_done() {
            return false;
        }
        if self.is_past_delay_at(now) {
            self.modify_order_by_status(status);
            return true;
        }
        false
    }
}

/// Upstream `utils.update_quantity` applied to `f64`. Identical conservation
/// semantics to the `i64` version in `src/utils.rs`.
fn normalise_quantities(q: f64, f: f64, p: f64, c: f64) -> (f64, f64, f64) {
    let (mut f, mut p, mut c) = (f, p, c);
    if c > 0.0 {
        c = c.min(q);
        f = q - c;
        p = q - c - f;
    } else if f > 0.0 {
        f = f.min(q);
        p = q - f;
    } else if p > 0.0 {
        p = p.min(q);
        f = q - p;
    } else {
        p = q - p;
    }
    (f, p, c)
}

// ── VPosition ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VPosition {
    pub symbol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buy_quantity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sell_quantity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buy_value: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sell_value: Option<f64>,
}

impl VPosition {
    pub fn new(symbol: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            ..Default::default()
        }
    }

    pub fn average_buy_price(&self) -> f64 {
        match (self.buy_quantity, self.buy_value) {
            (Some(q), Some(v)) if q != 0.0 && v != 0.0 => v / q,
            _ => 0.0,
        }
    }

    pub fn average_sell_price(&self) -> f64 {
        match (self.sell_quantity, self.sell_value) {
            (Some(q), Some(v)) if q != 0.0 && v != 0.0 => v / q,
            _ => 0.0,
        }
    }

    pub fn net_quantity(&self) -> f64 {
        self.buy_quantity.unwrap_or(0.0) - self.sell_quantity.unwrap_or(0.0)
    }

    pub fn net_value(&self) -> f64 {
        self.buy_value.unwrap_or(0.0) - self.sell_value.unwrap_or(0.0)
    }
}

// ── VUser ───────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct VUser {
    pub userid: String,
    pub name: Option<String>,
    pub orders: Vec<VOrder>,
}

impl VUser {
    pub fn new(userid: impl AsRef<str>) -> Self {
        Self {
            userid: userid.as_ref().to_ascii_uppercase(),
            name: None,
            orders: Vec::new(),
        }
    }

    pub fn add(&mut self, order: VOrder) {
        self.orders.push(order);
    }
}

// ── Responses ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub status: ResponseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
}

impl Response {
    pub fn new(status: ResponseStatus, now: DateTime<Utc>) -> Self {
        Self {
            status,
            timestamp: Some(now),
        }
    }
}

#[derive(Debug)]
pub struct OrderResponse {
    pub status: ResponseStatus,
    pub timestamp: Option<DateTime<Utc>>,
    pub error_msg: Option<String>,
    pub data: Option<VOrder>,
}

/// `GenericResponse` is `OrderResponse` with `data: Any` — Rust mirrors
/// with a pragmatic `GenericResponseData` enum covering VOrder / OHLC
/// (the two upstream test cases for it).
#[derive(Debug)]
pub enum GenericResponseData {
    VOrder(Box<VOrder>),
    OHLC(OHLC),
    Other(Value),
}

#[derive(Debug)]
pub struct GenericResponse {
    pub status: ResponseStatus,
    pub timestamp: Option<DateTime<Utc>>,
    pub error_msg: Option<String>,
    pub data: Option<GenericResponseData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResponse {
    pub status: ResponseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
    pub user_id: String,
    #[serde(default = "default_auth_message")]
    pub message: String,
}

fn default_auth_message() -> String {
    "Authentication successful".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LTPResponse {
    pub status: ResponseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_msg: Option<String>,
    pub data: HashMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OHLCVResponse {
    pub status: ResponseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
    pub data: HashMap<String, OHLCV>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteResponse {
    pub status: ResponseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
    pub data: HashMap<String, VQuote>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookResponse {
    pub status: ResponseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
    pub data: HashMap<String, OrderBook>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionResponse {
    pub status: ResponseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
    pub data: Vec<VPosition>,
}

// ── Instrument ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instrument {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<i64>,
    pub last_price: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_interest: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strike: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orderbook: Option<OrderBook>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_update_time: Option<DateTime<Utc>>,
}

// ── OrderFill ───────────────────────────────────────────────────────────

/// `omspy.simulation.models.OrderFill`. Constructing an `OrderFill`
/// immediately runs the `_as_market` branch (LIMIT-behaving-as-market +
/// STOP-immediate-trigger); `update()` then walks the remaining fill
/// states until `order.is_done()`.
#[derive(Debug)]
pub struct OrderFill {
    pub order: VOrder,
    pub last_price: f64,
}

impl OrderFill {
    pub fn new(order: VOrder, last_price: f64) -> Self {
        let mut fill = Self { order, last_price };
        fill.as_market();
        fill
    }

    pub fn done(&self) -> bool {
        self.order.is_done()
    }

    fn as_market(&mut self) {
        let side = self.order.side;
        let order_type = self.order.order_type;
        let ltp = self.last_price;
        match order_type {
            OrderType::Limit => {
                let price = match self.order.price {
                    Some(p) => p,
                    None => return,
                };
                let triggered = match side {
                    Side::Buy => ltp < price,
                    Side::Sell => ltp > price,
                };
                if triggered {
                    self.order.filled_quantity = self.order.quantity;
                    self.order.average_price = Some(self.last_price);
                }
                self.order.make_right_quantity();
            }
            OrderType::Stop => {
                let price = self.order.trigger_price.or(self.order.price);
                let Some(price) = price else {
                    return;
                };
                let triggered = match side {
                    Side::Buy => ltp > price,
                    Side::Sell => ltp < price,
                };
                if triggered {
                    self.order.filled_quantity = self.order.quantity;
                    self.order.average_price = Some(self.last_price);
                }
                self.order.make_right_quantity();
            }
            OrderType::Market => {}
        }
    }

    pub fn update(&mut self) {
        self.update_with_price(None);
    }

    pub fn update_with_price(&mut self, last_price: Option<f64>) {
        if self.order.is_done() {
            return;
        }
        let ltp = last_price.unwrap_or(self.last_price);
        let side = self.order.side;
        let order_type = self.order.order_type;
        match order_type {
            OrderType::Market => {
                self.order.price = Some(ltp);
                self.order.average_price = Some(ltp);
                self.order.filled_quantity = self.order.quantity;
                self.order.make_right_quantity();
            }
            OrderType::Limit => {
                let price = match self.order.price {
                    Some(p) => p,
                    None => return,
                };
                let triggered = match side {
                    Side::Buy => ltp < price,
                    Side::Sell => ltp > price,
                };
                if triggered {
                    self.order.average_price = Some(price);
                    self.order.filled_quantity = self.order.quantity;
                    self.order.make_right_quantity();
                }
            }
            OrderType::Stop => {
                if self.order.trigger_price.is_none() {
                    self.order.trigger_price = self.order.price;
                }
                let Some(trigger) = self.order.trigger_price else {
                    return;
                };
                let triggered = match side {
                    Side::Buy => ltp > trigger,
                    Side::Sell => ltp < trigger,
                };
                if triggered {
                    self.order.average_price = Some(ltp);
                    self.order.filled_quantity = self.order.quantity;
                    self.order.make_right_quantity();
                }
            }
        }
    }
}

// ── generate_orderbook helper ───────────────────────────────────────────

/// `omspy.simulation.virtual.generate_orderbook` — seed-able variant. Upstream
/// uses `random.randrange(5, 15)` + `random.randrange(q/2, q*3/2)` to fill in
/// quantity/order-count; we use `SmallRng::seed_from_u64(seed)` so tests can
/// reproduce the same orderbook across runs.
pub fn generate_orderbook(
    bid: f64,
    ask: f64,
    depth: usize,
    tick: f64,
    quantity: i64,
    seed: u64,
) -> OrderBook {
    let (bid, ask) = if bid > ask { (ask, bid) } else { (bid, ask) };
    let mut rng = SmallRng::seed_from_u64(seed);
    let q1 = (quantity as f64 * 0.5) as i64;
    let q2 = (quantity as f64 * 1.5) as i64;
    let mut bids = Vec::with_capacity(depth);
    let mut asks = Vec::with_capacity(depth);
    for i in 0..depth {
        let bid_qty: i64 = rng.gen_range(q1..q2);
        let ask_qty: i64 = rng.gen_range(q1..q2);
        let bid_count = rng.gen_range(5..15).min(bid_qty);
        let ask_count = rng.gen_range(5..15).min(ask_qty);
        let bid_price = Decimal::try_from(bid - (i as f64) * tick).unwrap();
        let ask_price = Decimal::try_from(ask + (i as f64) * tick).unwrap();
        bids.push(Quote::with_orders_count(bid_price, bid_qty, bid_count));
        asks.push(Quote::with_orders_count(ask_price, ask_qty, ask_count));
    }
    OrderBook::new(bids, asks)
}
