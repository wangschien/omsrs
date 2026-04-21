//! MVP data models ported from `omspy.models`.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

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
