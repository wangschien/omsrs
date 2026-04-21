//! Parity ports of `tests/test_utils.py` (17 portable items — `tick`,
//! `stop_loss_step_decimal`, and `load_broker_*` excluded per PORT-PLAN §14A).

use std::collections::HashMap;

use omsrs::utils::{
    create_basic_positions_from_orders_dict, dict_filter, update_quantity, OrderRecord, UQty,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{json, Value};

use crate::fixtures::{dict_for_filter, load_orders, mk_dict};

const BASIC_POSITION_SYMBOLS: [&str; 11] = [
    "BHARATFORG",
    "CANBK",
    "IRCTC",
    "LICHSGFIN",
    "MANAPPURAM",
    "MINDTREE",
    "NIFTY2221017450PE",
    "NIFTY22FEB17400CE",
    "PAGEIND",
    "PETRONET",
    "SRF",
];

// ── test_create_basic_positions_from_orders_dict_* ──────────────────────

pub fn test_create_basic_positions_from_orders_dict_keys() {
    let orders = load_orders();
    assert_eq!(orders.len(), 27, "fixture row count");
    let positions = create_basic_positions_from_orders_dict(&orders);
    for s in BASIC_POSITION_SYMBOLS {
        assert!(positions.contains_key(s), "missing symbol {s}");
    }
}

pub fn test_create_basic_positions_from_orders_dict_quantity() {
    let orders = load_orders();
    let positions = create_basic_positions_from_orders_dict(&orders);
    let expected = [160, 429, 136, 286, 733, 28, 50, 50, 2, 540, 46];
    for (s, q) in BASIC_POSITION_SYMBOLS.iter().zip(expected) {
        let pos = positions
            .get(*s)
            .unwrap_or_else(|| panic!("no pos for {s}"));
        let q = Decimal::from(q);
        assert_eq!(pos.buy_quantity, q, "buy_qty {s}");
        assert_eq!(pos.sell_quantity, q, "sell_qty {s}");
    }
}

pub fn test_create_basic_positions_from_orders_dict_value() {
    let orders = load_orders();
    let positions = create_basic_positions_from_orders_dict(&orders);

    let buy_value = [
        dec!(119792),
        dec!(111540),
        dec!(115600),
        dec!(112154.9),
        dec!(116107.2),
        dec!(111885.2),
        dec!(4715),
        dec!(12375),
        dec!(84918),
        dec!(117759.05),
        dec!(118803.75),
    ];
    let sell_value = [
        dec!(117064.05),
        dec!(112097.7),
        dec!(116817.2),
        dec!(111840.3),
        dec!(117353.3),
        dec!(110038.6),
        dec!(4122.5),
        dec!(13650),
        dec!(82797.9),
        dec!(117315),
        dec!(117293.1),
    ];

    for (s, bv) in BASIC_POSITION_SYMBOLS.iter().zip(buy_value) {
        let pos = positions.get(*s).unwrap();
        assert_eq!(pos.buy_value.round_dp(2), bv.round_dp(2), "buy_value {s}");
    }
    for (s, sv) in BASIC_POSITION_SYMBOLS.iter().zip(sell_value) {
        let pos = positions.get(*s).unwrap();
        assert_eq!(pos.sell_value.round_dp(2), sv.round_dp(2), "sell_value {s}");
    }
}

/// Upstream replicates `orders = load_orders[:3]; del orders[1]`, which after
/// pandas' (non-stable) quicksort leaves a BHARATFORG BUY 160 @ 748.7 and a
/// BHARATFORG SELL 153 @ 731.6. We build those two orders explicitly to
/// avoid depending on Python's sort semantics — the test is about position
/// arithmetic, not sort parity.
pub fn test_create_basic_positions_from_orders_dict_qty_non_match() {
    let mut orders = vec![
        OrderRecord {
            symbol: Some("BHARATFORG".into()),
            side: Some("BUY".into()),
            quantity: dec!(160),
            price: dec!(0),
            trigger_price: dec!(0),
            average_price: dec!(748.7),
        },
        OrderRecord {
            symbol: Some("BHARATFORG".into()),
            side: Some("SELL".into()),
            quantity: dec!(153),
            price: dec!(731.6),
            trigger_price: dec!(0),
            average_price: dec!(731.6),
        },
    ];
    let positions = create_basic_positions_from_orders_dict(&orders);
    let pos = positions.get("BHARATFORG").unwrap();
    assert_eq!(pos.sell_quantity, dec!(153));
    assert_eq!(pos.sell_value, dec!(111934.8));
    assert_eq!(pos.average_sell_value(), dec!(731.6));

    // Modified copy of orders[0] (BUY) — replicates upstream `deepcopy + mutate`.
    let mut o = orders[0].clone();
    o.quantity = dec!(130);
    o.price = dec!(0);
    o.trigger_price = dec!(728);
    o.average_price = dec!(0);
    orders.push(o);

    let positions = create_basic_positions_from_orders_dict(&orders);
    let pos = positions.get("BHARATFORG").unwrap();
    assert_eq!(pos.buy_quantity, dec!(290));
    assert_eq!(pos.buy_value, dec!(214432));
    assert_eq!(pos.average_buy_value().round_dp(2), dec!(739.42));
}

// ── test_dict_filter ────────────────────────────────────────────────────

pub fn test_empty_dict() {
    let filters: HashMap<String, Value> = HashMap::new();
    let out = dict_filter(&[], &filters);
    assert!(out.is_empty());
}

pub fn test_identity_dict() {
    let dct: Vec<HashMap<String, Value>> = [15, 20, 10]
        .iter()
        .map(|v| mk_dict(json!({"a": *v})))
        .collect();
    let out = dict_filter(&dct, &HashMap::new());
    assert_eq!(out, dct);
}

pub fn test_simple_dict() {
    let dct: Vec<HashMap<String, Value>> = [15, 20, 10]
        .iter()
        .map(|v| mk_dict(json!({"a": *v})))
        .collect();
    let filters = mk_dict(json!({"a": 10}));
    let out = dict_filter(&dct, &filters);
    assert_eq!(out, vec![mk_dict(json!({"a": 10}))]);
}

pub fn test_no_matching_dict() {
    let dct = dict_for_filter();
    let f1 = mk_dict(json!({"y": 1500}));
    assert!(dict_filter(&dct, &f1).is_empty());
    let f2 = mk_dict(json!({"m": 10}));
    assert!(dict_filter(&dct, &f2).is_empty());
}

pub fn test_filter_one() {
    let dct = dict_for_filter();
    let x = ["A"; 8];
    let y = [100, 400, 300, 200, 100, 400, 300, 200];
    let z = [1, 4, 1, 4, 1, 4, 1, 4];
    let expected: Vec<HashMap<String, Value>> = x
        .iter()
        .zip(y.iter())
        .zip(z.iter())
        .map(|((a, b), c)| mk_dict(json!({"x": a, "y": b, "z": c})))
        .collect();
    let filters = mk_dict(json!({"x": "A"}));
    let out = dict_filter(&dct, &filters);
    assert_eq!(out, expected);
}

pub fn test_filter_two() {
    let dct = dict_for_filter();
    let x = ["B"; 4];
    let y = [100, 300, 100, 300];
    let z = [5_i64; 4];
    let expected: Vec<HashMap<String, Value>> = x
        .iter()
        .zip(y.iter())
        .zip(z.iter())
        .map(|((a, b), c)| mk_dict(json!({"x": a, "y": b, "z": c})))
        .collect();
    let filters = mk_dict(json!({"z": 5}));
    let out = dict_filter(&dct, &filters);
    assert_eq!(out, expected);
}

pub fn test_multi_filter() {
    let dct = dict_for_filter();

    let expected1 = vec![mk_dict(json!({"x": "A", "y": 100, "z": 1})); 2];
    let f1 = mk_dict(json!({"x": "A", "y": 100}));
    assert_eq!(dict_filter(&dct, &f1), expected1);

    let expected2 = vec![mk_dict(json!({"x": "B", "y": 300, "z": 5})); 2];
    let f2 = mk_dict(json!({"x": "B", "y": 300, "z": 5}));
    assert_eq!(dict_filter(&dct, &f2), expected2);
    let f3 = mk_dict(json!({"x": "B", "y": 300}));
    assert_eq!(dict_filter(&dct, &f3), expected2);
}

// ── test_update_quantity (parametrized, 6 cases) ────────────────────────

fn check_uqty(q: i64, f: i64, p: i64, c: i64, expected: UQty) {
    let out = update_quantity(q, f, p, c);
    assert_eq!(out, expected, "update_quantity({q},{f},{p},{c})");
}

pub fn test_update_quantity_case0() {
    check_uqty(100, 0, 0, 0, UQty::new(100, 0, 100, 0));
}

pub fn test_update_quantity_case1() {
    check_uqty(100, 50, 0, 0, UQty::new(100, 50, 50, 0));
}

pub fn test_update_quantity_case2() {
    check_uqty(100, 100, 50, 0, UQty::new(100, 100, 0, 0));
}

pub fn test_update_quantity_case3() {
    check_uqty(100, 50, 50, 100, UQty::new(100, 0, 0, 100));
}

pub fn test_update_quantity_case4() {
    check_uqty(100, 50, 50, 50, UQty::new(100, 50, 0, 50));
}

pub fn test_update_quantity_case5() {
    check_uqty(100, 0, 0, 50, UQty::new(100, 50, 0, 50));
}
