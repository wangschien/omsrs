//! Shared test fixtures for the R1 parity suite.

use std::collections::HashMap;
use std::str::FromStr;

use omsrs::utils::OrderRecord;
use rust_decimal::Decimal;
use serde_json::{json, Value};

const REAL_ORDERS_CSV: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/data/real_orders.csv"
));

/// Upstream `tests/test_utils.py::load_orders` — filters out CANCELED /
/// REJECTED rows, returns the remaining 27.
///
/// Within-symbol row order is **not** preserved across Python's default
/// `pandas.sort_values` (quicksort, not stable) and isn't required by any
/// aggregate-style parity test. The one ordering-sensitive test
/// (`test_create_basic_positions_from_orders_dict_qty_non_match`) builds its
/// orders explicitly rather than indexing into this fixture.
pub fn load_orders() -> Vec<OrderRecord> {
    let mut lines = REAL_ORDERS_CSV.lines();
    let header = lines.next().expect("csv header");
    let columns: Vec<&str> = header.split(',').collect();

    let idx = |name: &str| columns.iter().position(|c| *c == name).expect(name);
    let i_symbol = idx("symbol");
    let i_side = idx("side");
    let i_quantity = idx("quantity");
    let i_price = idx("price");
    let i_trigger = idx("trigger_price");
    let i_average = idx("average_price");
    let i_status = idx("status");

    let parse_dec = |s: &str| -> Decimal {
        if s.is_empty() {
            Decimal::ZERO
        } else {
            Decimal::from_str(s).unwrap_or_else(|e| panic!("parse {s:?}: {e}"))
        }
    };

    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(',').collect();
        let status = fields[i_status];
        if matches!(status, "CANCELED" | "REJECTED") {
            continue;
        }
        out.push(OrderRecord {
            symbol: Some(fields[i_symbol].to_string()),
            side: Some(fields[i_side].to_string()),
            quantity: parse_dec(fields[i_quantity]),
            price: parse_dec(fields[i_price]),
            trigger_price: parse_dec(fields[i_trigger]),
            average_price: parse_dec(fields[i_average]),
        });
    }
    out
}

/// Upstream `tests/test_utils.py::dict_for_filter`.
///
/// Produces 24 dicts of the form `{x, y, z}` where:
/// - x cycles through `["A", "B", "C"]`, each repeated 8 times total
/// - y cycles through `[100, 200, 300, 400]`, each repeated 6 times total
/// - z cycles through `[1, 2, 3, 4, 5, 6]`, each repeated 4 times total
///
/// Python uses `itertools.chain.from_iterable(itertools.repeat(it, n))` which
/// flattens `n` copies of the iterable; e.g. repeat 8× of `[A,B,C]` gives
/// `[A,B,C,A,B,C,...,A,B,C]` of length 24, not `[A,A,...,B,B,...,C,C,...]`.
/// We replicate that semantics exactly.
pub fn dict_for_filter() -> Vec<HashMap<String, Value>> {
    let xs = ["A", "B", "C"];
    let ys = [100_i64, 200, 300, 400];
    let zs = [1_i64, 2, 3, 4, 5, 6];

    let x_iter: Vec<&str> = (0..8).flat_map(|_| xs.iter().copied()).collect();
    let y_iter: Vec<i64> = (0..6).flat_map(|_| ys.iter().copied()).collect();
    let z_iter: Vec<i64> = (0..4).flat_map(|_| zs.iter().copied()).collect();

    x_iter
        .into_iter()
        .zip(y_iter)
        .zip(z_iter)
        .map(|((x, y), z)| mk_dict(json!({"x": x, "y": y, "z": z})))
        .collect()
}

pub fn mk_dict(v: Value) -> HashMap<String, Value> {
    match v {
        Value::Object(m) => m.into_iter().collect(),
        other => panic!("expected object, got {other:?}"),
    }
}
