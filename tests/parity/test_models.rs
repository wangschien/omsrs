//! Parity ports of `tests/test_models.py::test_basic_position*` (3 items).

use omsrs::models::BasicPosition;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

pub fn test_basic_position() {
    let p = BasicPosition::new("AAPL");
    assert_eq!(p.symbol, "AAPL");
}

pub fn test_basic_position_calculations() {
    let p = BasicPosition {
        symbol: "AAPL".into(),
        buy_quantity: dec!(100),
        sell_quantity: dec!(120),
        buy_value: dec!(100) * dec!(131),
        sell_value: dec!(120) * dec!(118.5),
    };
    assert_eq!(p.net_quantity(), Decimal::from(-20));
    assert_eq!(p.average_buy_value(), dec!(131));
    assert_eq!(p.average_sell_value(), dec!(118.5));
}

pub fn test_basic_position_zero_quantity() {
    let mut p = BasicPosition::new("AAPL");
    assert_eq!(p.average_buy_value(), Decimal::ZERO);
    assert_eq!(p.average_sell_value(), Decimal::ZERO);
    p.buy_quantity = dec!(10);
    assert_eq!(p.average_buy_value(), Decimal::ZERO);
    p.buy_value = dec!(1315);
    assert_eq!(p.average_buy_value(), dec!(131.5));
    assert_eq!(p.average_sell_value(), Decimal::ZERO);
}
