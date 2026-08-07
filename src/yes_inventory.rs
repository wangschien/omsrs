//! YES-market inventory authority using [`crate::BasicPosition`].
//!
//! O2: fill / net ownership lives in **omsrs**, not kbot.

use rust_decimal::Decimal;

use crate::BasicPosition;

/// YES-market inventory (contracts).
#[derive(Debug, Clone)]
pub struct YesInventory {
    pos: BasicPosition,
}

impl YesInventory {
    pub fn new(market_ticker: impl Into<String>) -> Self {
        Self {
            pos: BasicPosition::new(market_ticker.into()),
        }
    }

    pub fn on_fill_contracts(&mut self, is_buy_yes: bool, qty: Decimal, price: Decimal) {
        if qty <= Decimal::ZERO {
            return;
        }
        let notional = qty * price;
        if is_buy_yes {
            self.pos.buy_quantity += qty;
            self.pos.buy_value += notional;
        } else {
            self.pos.sell_quantity += qty;
            self.pos.sell_value += notional;
        }
    }

    pub fn net_yes_contracts(&self) -> Decimal {
        self.pos.net_quantity()
    }

    pub fn net_centi(&self) -> Result<i64, String> {
        let n = (self.net_yes_contracts() * Decimal::from(100)).round();
        n.to_string()
            .parse::<i64>()
            .map_err(|e| format!("net_centi from {n}: {e}"))
    }

    pub fn set_net_yes_contracts(&mut self, net: Decimal) {
        self.pos.buy_quantity = Decimal::ZERO;
        self.pos.sell_quantity = Decimal::ZERO;
        self.pos.buy_value = Decimal::ZERO;
        self.pos.sell_value = Decimal::ZERO;
        if net > Decimal::ZERO {
            self.pos.buy_quantity = net;
        } else if net < Decimal::ZERO {
            self.pos.sell_quantity = -net;
        }
    }
}

impl Default for YesInventory {
    fn default() -> Self {
        Self::new("YES")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn buy_then_sell_net_centi() {
        let mut inv = YesInventory::new("T");
        inv.on_fill_contracts(true, dec!(80), dec!(0.50));
        assert_eq!(inv.net_centi().unwrap(), 8_000);
        inv.on_fill_contracts(false, dec!(30), dec!(0.55));
        assert_eq!(inv.net_centi().unwrap(), 5_000);
        inv.set_net_yes_contracts(dec!(-10));
        assert_eq!(inv.net_centi().unwrap(), -1_000);
    }
}
