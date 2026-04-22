//! R12.3b — async mirror of `tests/parity/test_order_strategy.rs`
//! focused on: add cascade, run callback, aggregate views,
//! update_ltp / update_orders fan-out.
//!
//! Sync parity at `tests/parity/test_order_strategy.rs` covers 7
//! items; async surface is identical for all except the broker
//! trait object type. No async-specific method on strategy (run
//! callback stays sync per R12 plan open Q #1).

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use omsrs::async_compound_order::AsyncCompoundOrder;
use omsrs::async_order_strategy::AsyncOrderStrategy;
use omsrs::clock::{Clock, MockClock};
use omsrs::order::{Order, OrderInit};

fn mock_clock(t: chrono::DateTime<Utc>) -> Arc<dyn Clock + Send + Sync> {
    Arc::new(MockClock::new(t))
}

fn strategy_clock() -> Arc<dyn Clock + Send + Sync> {
    mock_clock(Utc.with_ymd_and_hms(2023, 1, 1, 9, 15, 0).unwrap())
}

fn old_clock() -> Arc<dyn Clock + Send + Sync> {
    mock_clock(Utc.with_ymd_and_hms(2022, 12, 31, 15, 30, 0).unwrap())
}

// ── R12.3b.async_strategy.1 — defaults
#[tokio::test]
async fn defaults_match_sync() {
    let s = AsyncOrderStrategy::new();
    assert!(s.broker.is_none());
    assert!(s.orders.is_empty());
    assert!(s.ltp.is_empty());
}

// ── R12.3b.async_strategy.2 — add cascades strategy clock to
// compound and to every already-contained child order. Mirrors
// the sync test at
// `tests/parity/test_order_strategy.rs:add_cascades_strategy_
// clock_to_prepopulated_compound_orders`.
#[tokio::test]
async fn add_cascades_strategy_clock() {
    let strat_clk = strategy_clock();
    let old_clk = old_clock();

    let mut compound = AsyncCompoundOrder::with_clock(old_clk.clone());
    compound
        .add(
            Order::from_init_with_clock(
                OrderInit {
                    symbol: "aapl".into(),
                    side: "buy".into(),
                    quantity: 1,
                    ..Default::default()
                },
                old_clk.clone(),
            ),
            None,
            None,
        )
        .unwrap();
    compound
        .add(
            Order::from_init_with_clock(
                OrderInit {
                    symbol: "msft".into(),
                    side: "sell".into(),
                    quantity: 2,
                    ..Default::default()
                },
                old_clk.clone(),
            ),
            None,
            None,
        )
        .unwrap();

    // Before add: child clocks point at old_clk.
    assert!(Arc::ptr_eq(compound.clock(), &old_clk));
    assert!(compound
        .orders
        .iter()
        .all(|o| Arc::ptr_eq(o.clock(), &old_clk)));

    let mut strategy = AsyncOrderStrategy::with_clock(strat_clk.clone());
    strategy.add(compound);

    let added = &strategy.orders[0];
    assert!(Arc::ptr_eq(strategy.clock(), &strat_clk));
    assert!(Arc::ptr_eq(added.clock(), &strat_clk));
    assert!(added
        .orders
        .iter()
        .all(|o| Arc::ptr_eq(o.clock(), &strat_clk)));
}

// ── R12.3b.async_strategy.3 — update_ltp propagates to every
// compound
#[tokio::test]
async fn update_ltp_propagates_to_each_compound() {
    let mut strat = AsyncOrderStrategy::with_clock(strategy_clock());
    strat.add(AsyncCompoundOrder::with_clock(old_clock()));
    strat.add(AsyncCompoundOrder::with_clock(old_clock()));

    let mut prices = HashMap::new();
    prices.insert("aapl".into(), 150.0);
    prices.insert("msft".into(), 400.0);
    strat.update_ltp(&prices);

    assert_eq!(strat.ltp.get("aapl").copied(), Some(150.0));
    for co in &strat.orders {
        assert_eq!(co.ltp.get("aapl").copied(), Some(150.0));
        assert_eq!(co.ltp.get("msft").copied(), Some(400.0));
    }
}

// ── R12.3b.async_strategy.4 — positions + mtm aggregate across
// compounds
#[tokio::test]
async fn positions_and_mtm_aggregate_across_compounds() {
    let mut strat = AsyncOrderStrategy::with_clock(strategy_clock());

    let mut co1 = AsyncCompoundOrder::with_clock(old_clock());
    co1.add_order(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            filled_quantity: Some(10),
            average_price: Some(rust_decimal_macros::dec!(100)),
            status: Some("COMPLETE".into()),
            ..Default::default()
        },
        None,
        None,
    )
    .unwrap();
    let mut co2 = AsyncCompoundOrder::with_clock(old_clock());
    co2.add_order(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 5,
            filled_quantity: Some(5),
            average_price: Some(rust_decimal_macros::dec!(100)),
            status: Some("COMPLETE".into()),
            ..Default::default()
        },
        None,
        None,
    )
    .unwrap();

    strat.add(co1);
    strat.add(co2);

    let pos = strat.positions();
    assert_eq!(pos.get("aapl").copied(), Some(15));

    // Feed ltp → mtm = position * (ltp - avg_price)
    let mut prices = HashMap::new();
    prices.insert("aapl".into(), 110.0);
    strat.update_ltp(&prices);

    let mtm = strat.mtm();
    // 15 shares, entry 100, ltp 110 → mtm = 15 * 10 = 150
    let v = mtm.get("aapl").copied().unwrap_or_default();
    assert_eq!(v, rust_decimal_macros::dec!(150));
    assert_eq!(strat.total_mtm(), rust_decimal_macros::dec!(150));
}

// ── R12.3b.async_strategy.5 — run() fires each compound's
// sync run_fn callback with the provided ltp map
#[tokio::test]
async fn run_fires_each_compound_run_fn() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let mut strat = AsyncOrderStrategy::with_clock(strategy_clock());
    let call_count = Arc::new(AtomicUsize::new(0));

    for _ in 0..3 {
        let mut co = AsyncCompoundOrder::with_clock(old_clock());
        let cc = call_count.clone();
        co.run_fn = Some(Arc::new(move |_co, _ltp| {
            cc.fetch_add(1, Ordering::Relaxed);
        }));
        strat.add(co);
    }

    strat.run(&HashMap::new());
    assert_eq!(call_count.load(Ordering::Relaxed), 3);
    // run() is synchronous; running again re-invokes.
    strat.run(&HashMap::new());
    assert_eq!(call_count.load(Ordering::Relaxed), 6);
}
