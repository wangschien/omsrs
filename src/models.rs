//! MVP data models ported from `omspy.models`.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::clock::{clock_system_default, Clock};

/// Upstream `models.QuantityMatch`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuantityMatch {
    #[serde(default)]
    pub buy: i64,
    #[serde(default)]
    pub sell: i64,
}

impl QuantityMatch {
    pub fn is_equal(&self) -> bool {
        self.buy == self.sell
    }

    pub fn not_matched(&self) -> i64 {
        self.buy - self.sell
    }
}

/// Upstream `models.BasicPosition`.
///
/// `buy_value`/`sell_value` are `Decimal` so arithmetic is byte-deterministic
/// (PORT-PLAN §7, R.10). `buy_quantity`/`sell_quantity` are also `Decimal` to
/// keep the arithmetic domain uniform across the port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BasicPosition {
    pub symbol: String,
    #[serde(default)]
    pub buy_quantity: Decimal,
    #[serde(default)]
    pub sell_quantity: Decimal,
    #[serde(default)]
    pub buy_value: Decimal,
    #[serde(default)]
    pub sell_value: Decimal,
}

impl BasicPosition {
    pub fn new(symbol: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            buy_quantity: Decimal::ZERO,
            sell_quantity: Decimal::ZERO,
            buy_value: Decimal::ZERO,
            sell_value: Decimal::ZERO,
        }
    }

    pub fn net_quantity(&self) -> Decimal {
        self.buy_quantity - self.sell_quantity
    }

    pub fn average_buy_value(&self) -> Decimal {
        if self.buy_value > Decimal::ZERO {
            self.buy_value / self.buy_quantity
        } else {
            Decimal::ZERO
        }
    }

    pub fn average_sell_value(&self) -> Decimal {
        if self.sell_quantity > Decimal::ZERO {
            self.sell_value / self.sell_quantity
        } else {
            Decimal::ZERO
        }
    }
}

/// Upstream `models.Quote`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    pub price: Decimal,
    pub quantity: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orders_count: Option<i64>,
}

impl Quote {
    pub fn new(price: Decimal, quantity: i64) -> Self {
        Self {
            price,
            quantity,
            orders_count: None,
        }
    }

    pub fn with_orders_count(price: Decimal, quantity: i64, orders_count: i64) -> Self {
        Self {
            price,
            quantity,
            orders_count: Some(orders_count),
        }
    }

    pub fn value(&self) -> Decimal {
        self.price * Decimal::from(self.quantity)
    }
}

/// Upstream `models.OrderBook`. All aggregators short-circuit to 0 when
/// either side is empty — mirrors `is_bid_ask` gating in the Python source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderBook {
    pub bid: Vec<Quote>,
    pub ask: Vec<Quote>,
}

impl OrderBook {
    pub fn new(bid: Vec<Quote>, ask: Vec<Quote>) -> Self {
        Self { bid, ask }
    }

    pub fn is_bid_ask(&self) -> bool {
        !self.bid.is_empty() && !self.ask.is_empty()
    }

    pub fn spread(&self) -> Decimal {
        if self.is_bid_ask() {
            self.ask[0].price - self.bid[0].price
        } else {
            Decimal::ZERO
        }
    }

    pub fn total_bid_quantity(&self) -> i64 {
        if self.is_bid_ask() {
            self.bid.iter().map(|q| q.quantity).sum()
        } else {
            0
        }
    }

    pub fn total_ask_quantity(&self) -> i64 {
        if self.is_bid_ask() {
            self.ask.iter().map(|q| q.quantity).sum()
        } else {
            0
        }
    }
}

/// Upstream `models.OrderLock` — a hard dependency of `Order`.
///
/// Clock access goes through the injected `Arc<dyn Clock + Send + Sync>`
/// field so tests drive it deterministically via `MockClock` (PORT-PLAN §6
/// D4). The `timezone` field is carried for forward compatibility but does
/// not affect arithmetic — all instants are stored as `DateTime<Utc>` and
/// compared by instant, matching pendulum's underlying semantics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderLock {
    pub max_order_creation_lock_time: f64,
    pub max_order_modification_lock_time: f64,
    pub max_order_cancellation_lock_time: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,

    creation_lock_till: DateTime<Utc>,
    modification_lock_till: DateTime<Utc>,
    cancellation_lock_till: DateTime<Utc>,

    #[serde(skip, default = "clock_system_default")]
    clock: Arc<dyn Clock + Send + Sync>,
}

impl OrderLock {
    pub fn new() -> Self {
        Self::with_clock(clock_system_default())
    }

    pub fn with_clock(clock: Arc<dyn Clock + Send + Sync>) -> Self {
        let now = clock.now();
        Self {
            max_order_creation_lock_time: 60.0,
            max_order_modification_lock_time: 60.0,
            max_order_cancellation_lock_time: 60.0,
            timezone: None,
            creation_lock_till: now,
            modification_lock_till: now,
            cancellation_lock_till: now,
            clock,
        }
    }

    pub fn with_timezone(mut self, tz: impl Into<String>) -> Self {
        self.timezone = Some(tz.into());
        self
    }

    pub fn creation_lock_till(&self) -> DateTime<Utc> {
        self.creation_lock_till
    }

    pub fn modification_lock_till(&self) -> DateTime<Utc> {
        self.modification_lock_till
    }

    pub fn cancellation_lock_till(&self) -> DateTime<Utc> {
        self.cancellation_lock_till
    }

    /// Upstream truncates the `seconds` arg via `int(seconds)` before adding.
    /// We match that: cap at max, then truncate toward zero.
    fn secs_delta(seconds: f64, cap: f64) -> Duration {
        let capped = seconds.min(cap);
        Duration::seconds(capped.trunc() as i64)
    }

    pub fn create(&mut self, seconds: f64) -> DateTime<Utc> {
        let delta = Self::secs_delta(seconds, self.max_order_creation_lock_time);
        self.creation_lock_till = self.clock.now() + delta;
        self.creation_lock_till
    }

    pub fn modify(&mut self, seconds: f64) -> DateTime<Utc> {
        let delta = Self::secs_delta(seconds, self.max_order_modification_lock_time);
        self.modification_lock_till = self.clock.now() + delta;
        self.modification_lock_till
    }

    pub fn cancel(&mut self, seconds: f64) -> DateTime<Utc> {
        let delta = Self::secs_delta(seconds, self.max_order_cancellation_lock_time);
        self.cancellation_lock_till = self.clock.now() + delta;
        self.cancellation_lock_till
    }

    pub fn can_create(&self) -> bool {
        self.clock.now() > self.creation_lock_till
    }

    pub fn can_modify(&self) -> bool {
        self.clock.now() > self.modification_lock_till
    }

    pub fn can_cancel(&self) -> bool {
        self.clock.now() > self.cancellation_lock_till
    }
}

impl Default for OrderLock {
    fn default() -> Self {
        Self::new()
    }
}
