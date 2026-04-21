//! Parity ports of `tests/simulation/test_models.py`. R5 scope = 54 items
//! (55 collected minus `test_ticker_ltp` per §14A — replaced by the
//! statistical target).

use chrono::{Duration, TimeZone, Utc};
use omsrs::simulation::{
    generate_orderbook, GenericResponse, GenericResponseData, Instrument, OrderFill, OrderResponse,
    OrderType, Response, ResponseStatus, Side, Status, Ticker, TickerMode, VOrder, VOrderInit,
    VPosition, VQuote, VTrade, VUser, OHLC, OHLCV, OHLCVI,
};

fn vorder_kwargs() -> VOrderInit {
    VOrderInit {
        order_id: "20234567812".into(),
        symbol: "aapl".into(),
        quantity: 100.0,
        side: Some(Side::Buy),
        exchange_timestamp: Some(Utc.with_ymd_and_hms(2023, 1, 2, 7, 10, 0).unwrap()),
        now_override: Some(Utc.with_ymd_and_hms(2023, 1, 2, 7, 10, 0).unwrap()),
        ..Default::default()
    }
}

fn ohlc_args() -> OHLC {
    OHLC {
        open: 104.0,
        high: 112.0,
        low: 101.0,
        close: 108.0,
        last_price: 107.0,
    }
}

// ── simple defaults / values ────────────────────────────────────────────

pub fn test_vtrade_defaults() {
    let t = VTrade {
        trade_id: "202310001".into(),
        order_id: "20234567812".into(),
        symbol: "aapl".into(),
        quantity: 50,
        price: 120.0,
        side: Side::Buy,
        timestamp: Some(Utc.with_ymd_and_hms(2023, 1, 2, 7, 10, 0).unwrap()),
    };
    assert_eq!(t.price, 120.0);
    assert_eq!(t.side, Side::Buy);
    assert_eq!(t.value(), 6000.0);
}

pub fn test_vtrade_value() {
    let mut t = VTrade {
        trade_id: "202310001".into(),
        order_id: "20234567812".into(),
        symbol: "aapl".into(),
        quantity: 50,
        price: 120.0,
        side: Side::Buy,
        timestamp: None,
    };
    assert_eq!(t.value(), 6000.0);
    t.side = Side::Sell;
    t.price = 100.0;
    assert_eq!(t.value(), -5000.0);
}

pub fn test_vorder_defaults() {
    let vorder = VOrder::from_init(vorder_kwargs()).unwrap();
    assert_eq!(vorder.quantity, 100.0);
    assert_eq!(vorder.side, Side::Buy);
    assert!(vorder.status_message.is_none());
    assert!(vorder.timestamp.is_some());
    assert_eq!(vorder.filled_quantity, 0.0);
    assert_eq!(vorder.pending_quantity, 100.0);
    assert_eq!(vorder.canceled_quantity, 0.0);
    assert_eq!(vorder.average_price, Some(0.0));
    assert_eq!(vorder.order_type, OrderType::Market);
}

pub fn test_vorder_quantities() {
    let mut init = vorder_kwargs();
    init.pending_quantity = Some(50.0);
    let vorder = VOrder::from_init(init.clone()).unwrap();
    assert_eq!(vorder.quantity, 100.0);
    assert_eq!(vorder.filled_quantity, 50.0);
    assert_eq!(vorder.pending_quantity, 50.0);
    assert_eq!(vorder.canceled_quantity, 0.0);

    init.filled_quantity = Some(100.0);
    let vorder = VOrder::from_init(init.clone()).unwrap();
    assert_eq!(vorder.quantity, 100.0);
    assert_eq!(vorder.filled_quantity, 100.0);
    assert_eq!(vorder.pending_quantity, 0.0);
    assert_eq!(vorder.canceled_quantity, 0.0);

    init.canceled_quantity = Some(100.0);
    let vorder = VOrder::from_init(init).unwrap();
    assert_eq!(vorder.quantity, 100.0);
    assert_eq!(vorder.filled_quantity, 0.0);
    assert_eq!(vorder.pending_quantity, 0.0);
    assert_eq!(vorder.canceled_quantity, 100.0);
}

pub fn test_vposition_defaults() {
    let pos = VPosition::new("aapl");
    assert_eq!(pos.buy_quantity, None);
    assert_eq!(pos.sell_quantity, None);
    assert_eq!(pos.buy_value, None);
    assert_eq!(pos.sell_value, None);
    assert_eq!(pos.average_buy_price(), 0.0);
    assert_eq!(pos.average_sell_price(), 0.0);
    assert_eq!(pos.net_quantity(), 0.0);
    assert_eq!(pos.net_value(), 0.0);
}

pub fn test_vorder_status() {
    let mut order = VOrder::from_init(vorder_kwargs()).unwrap();
    assert_eq!(order.filled_quantity, 0.0);
    assert_eq!(order.pending_quantity, 100.0);
    assert_eq!(order.status(), Status::Open);

    order.filled_quantity = 100.0;
    assert_eq!(order.status(), Status::Complete);

    order.filled_quantity = 40.0;
    order.canceled_quantity = 60.0;
    order.pending_quantity = 0.0;
    assert_eq!(order.status(), Status::PartialFill);

    order.filled_quantity = 40.0;
    order.pending_quantity = 60.0;
    order.canceled_quantity = 0.0;
    assert_eq!(order.status(), Status::Pending);
}

pub fn test_vorder_status_canceled_rejected() {
    let mut order = VOrder::from_init(vorder_kwargs()).unwrap();
    assert_eq!(order.status(), Status::Open);

    order.filled_quantity = 0.0;
    order.pending_quantity = 0.0;
    order.canceled_quantity = 100.0;
    assert_eq!(order.status(), Status::Canceled);

    order.status_message = Some("REJECTED: no margins".into());
    assert_eq!(order.status(), Status::Rejected);

    order.status_message = Some("rejected: no margins".into());
    assert_eq!(order.status(), Status::Rejected);
}

pub fn test_vorder_value() {
    let mut order = VOrder::from_init(vorder_kwargs()).unwrap();
    order.average_price = Some(120.0);
    assert_eq!(order.value(), 0.0);
    order.filled_quantity = 50.0;
    assert_eq!(order.value(), 6000.0);
    order.filled_quantity = 100.0;
    assert_eq!(order.value(), 12000.0);
    order.side = Side::Sell;
    assert_eq!(order.value(), -12000.0);
    assert_eq!(order.side, Side::Sell);
}

pub fn test_vorder_value_price() {
    let mut order = VOrder::from_init(vorder_kwargs()).unwrap();
    assert_eq!(order.value(), 0.0);
    order.filled_quantity = 50.0;
    order.price = Some(118.0);
    assert_eq!(order.value(), 5900.0);
    order.average_price = Some(120.0);
    assert_eq!(order.value(), 6000.0);
}

pub fn test_vposition_price() {
    let mut pos = VPosition {
        symbol: "aapl".into(),
        buy_quantity: Some(100.0),
        buy_value: Some(10000.0),
        sell_quantity: Some(50.0),
        sell_value: Some(5100.0),
    };
    assert_eq!(pos.average_buy_price(), 100.0);
    assert_eq!(pos.average_sell_price(), 5100.0 / 50.0);
    assert_eq!(pos.net_quantity(), 50.0);
    assert_eq!(pos.net_value(), 4900.0);

    pos.sell_quantity = Some(120.0);
    pos.sell_value = Some(12240.0);
    assert_eq!(pos.average_sell_price(), 102.0);
    assert_eq!(pos.net_value(), -2240.0);
}

pub fn test_response() {
    let known = Utc.with_ymd_and_hms(2023, 2, 1, 12, 44, 0).unwrap();
    let resp = Response::new(ResponseStatus::Success, known);
    assert_eq!(resp.status, ResponseStatus::Success);
    assert_eq!(resp.timestamp, Some(known));
}

pub fn test_order_response() {
    let data = VOrder::from_init(VOrderInit {
        order_id: "order_id".into(),
        symbol: "aapl".into(),
        quantity: 10.0,
        side: Some(Side::Buy),
        price: Some(100.0),
        now_override: Some(Utc.with_ymd_and_hms(2023, 1, 1, 0, 0, 0).unwrap()),
        ..Default::default()
    })
    .unwrap();
    let resp = OrderResponse {
        status: ResponseStatus::Success,
        timestamp: None,
        error_msg: None,
        data: Some(data),
    };
    assert_eq!(resp.status, ResponseStatus::Success);
    let d = resp.data.as_ref().unwrap();
    assert_eq!(d.order_id, "order_id");
    assert_eq!(d.symbol, "aapl");
    assert_eq!(d.quantity, 10.0);
    assert_eq!(d.side, Side::Buy);
    assert_eq!(d.price, Some(100.0));
    assert_eq!(d.trigger_price, None);
    assert_eq!(d.filled_quantity, 0.0);
    assert_eq!(d.canceled_quantity, 0.0);
    assert_eq!(d.pending_quantity, 10.0);
    assert_eq!(d.status(), Status::Open);
}

// ── OHLC family ─────────────────────────────────────────────────────────

pub fn test_ohlc() {
    let ohlc = ohlc_args();
    assert_eq!(ohlc.open, 104.0);
    assert_eq!(ohlc.high, 112.0);
    assert_eq!(ohlc.low, 101.0);
    assert_eq!(ohlc.close, 108.0);
    assert_eq!(ohlc.last_price, 107.0);
}

pub fn test_ohlcv() {
    let o = OHLCV {
        open: 104.0,
        high: 112.0,
        low: 101.0,
        close: 108.0,
        last_price: 107.0,
        volume: 12600,
    };
    assert_eq!(o.open, 104.0);
    assert_eq!(o.volume, 12600);
}

pub fn test_ohlcvi() {
    let o = OHLCVI {
        open: 104.0,
        high: 112.0,
        low: 101.0,
        close: 108.0,
        last_price: 107.0,
        volume: 12600,
        open_interest: 13486720,
    };
    assert_eq!(o.volume, 12600);
    assert_eq!(o.open_interest, 13486720);
}

pub fn test_vquote() {
    let ob = generate_orderbook(100.0, 100.05, 5, 0.01, 100, 42);
    let q = VQuote {
        open: 104.0,
        high: 112.0,
        low: 101.0,
        close: 108.0,
        last_price: 107.0,
        volume: 22000,
        orderbook: ob,
    };
    assert_eq!(q.open, 104.0);
    assert_eq!(q.volume, 22000);
    assert_eq!(q.orderbook.ask.len(), 5);
    assert_eq!(q.orderbook.bid.len(), 5);
}

pub fn test_generic_response() {
    let vo = VOrder::from_init(VOrderInit {
        order_id: "order_id".into(),
        symbol: "aapl".into(),
        quantity: 10.0,
        side: Some(Side::Buy),
        price: Some(100.0),
        now_override: Some(Utc::now()),
        ..Default::default()
    })
    .unwrap();
    let resp = GenericResponse {
        status: ResponseStatus::Success,
        timestamp: None,
        error_msg: None,
        data: Some(GenericResponseData::VOrder(Box::new(vo))),
    };
    if let Some(GenericResponseData::VOrder(v)) = &resp.data {
        assert_eq!(v.price, Some(100.0));
    } else {
        panic!("expected VOrder");
    }

    let ohlc = ohlc_args();
    let resp = GenericResponse {
        status: ResponseStatus::Success,
        timestamp: None,
        error_msg: None,
        data: Some(GenericResponseData::OHLC(ohlc)),
    };
    if let Some(GenericResponseData::OHLC(o)) = &resp.data {
        assert_eq!(o.high, 112.0);
        assert_eq!(o.last_price, 107.0);
    } else {
        panic!("expected OHLC");
    }
}

// ── VUser ───────────────────────────────────────────────────────────────

pub fn test_vuser_defaults() {
    let user = VUser::new("ABCD1234");
    assert_eq!(user.userid, "ABCD1234");
    assert!(user.name.is_none());
    assert!(user.orders.is_empty());
}

pub fn test_vuser_add() {
    let mut user = VUser::new("abcd1234");
    assert_eq!(user.userid, "ABCD1234");
    let order = VOrder::from_init(vorder_kwargs()).unwrap();
    let oid = order.order_id.clone();
    user.add(order);
    assert_eq!(user.orders.len(), 1);
    assert_eq!(user.orders[0].order_id, oid);
}

// ── is_done parametrize (6 cases) ───────────────────────────────────────

fn is_done_case(filled: f64, pending: f64, canceled: f64, expected: bool) {
    let mut init = vorder_kwargs();
    init.filled_quantity = Some(filled);
    init.pending_quantity = Some(pending);
    init.canceled_quantity = Some(canceled);
    let order = VOrder::from_init(init).unwrap();
    assert_eq!(order.is_done(), expected);
}
pub fn test_vorder_is_done_case0() {
    is_done_case(0.0, 100.0, 0.0, false);
}
pub fn test_vorder_is_done_case1() {
    is_done_case(50.0, 100.0, 0.0, false);
}
pub fn test_vorder_is_done_case2() {
    is_done_case(100.0, 0.0, 0.0, true);
}
pub fn test_vorder_is_done_case3() {
    is_done_case(50.0, 50.0, 0.0, false);
}
pub fn test_vorder_is_done_case4() {
    is_done_case(50.0, 0.0, 50.0, true);
}
pub fn test_vorder_is_done_case5() {
    is_done_case(50.0, 0.0, 100.0, true);
}

// ── delay / modify_by_status ────────────────────────────────────────────

pub fn test_vorder_is_past_delay() {
    let base = Utc.with_ymd_and_hms(2023, 1, 1, 11, 20, 0).unwrap();
    let mut init = vorder_kwargs();
    init.now_override = Some(base);
    let order = VOrder::from_init(init).unwrap();
    assert!(!order.is_past_delay_at(base));
    assert!(order.is_past_delay_at(base + Duration::seconds(3)));
}

pub fn test_vorder_custom_delay() {
    let base = Utc.with_ymd_and_hms(2023, 1, 1, 11, 20, 0).unwrap();
    let mut init = vorder_kwargs();
    init.now_override = Some(base);
    let mut order = VOrder::from_init(init).unwrap();
    order.delay = Duration::microseconds(5_000_000);
    assert!(!order.is_past_delay_at(base));
    assert!(!order.is_past_delay_at(base + Duration::seconds(3)));
    // Upstream tests 5s; with strict `>`, equal == false.
    assert!(!order.is_past_delay_at(base + Duration::seconds(5)));
}

pub fn test_vorder_modify_by_status_complete() {
    let mut order = VOrder::from_init(vorder_kwargs()).unwrap();
    order.modify_order_by_status(Status::Complete);
    assert_eq!(order.quantity, order.filled_quantity);
    assert_eq!(order.pending_quantity, 0.0);
    assert_eq!(order.canceled_quantity, 0.0);
    assert_eq!(order.status(), Status::Complete);
    assert!(order.is_done());
}

pub fn test_vorder_modify_by_status_canceled() {
    let mut order = VOrder::from_init(vorder_kwargs()).unwrap();
    order.modify_order_by_status(Status::Canceled);
    assert_eq!(order.quantity, order.canceled_quantity);
    assert_eq!(order.filled_quantity, 0.0);
    assert_eq!(order.pending_quantity, 0.0);
    assert_eq!(order.status(), Status::Canceled);
    assert!(order.is_done());
}

pub fn test_vorder_modify_by_status_open() {
    let mut order = VOrder::from_init(vorder_kwargs()).unwrap();
    order.modify_order_by_status(Status::Open);
    assert_eq!(order.quantity, order.pending_quantity);
    assert_eq!(order.canceled_quantity, 0.0);
    assert_eq!(order.filled_quantity, 0.0);
    assert_eq!(order.status(), Status::Open);
    assert!(!order.is_done());
}

pub fn test_vorder_modify_by_status_pending() {
    let mut order = VOrder::from_init(vorder_kwargs()).unwrap();
    order.modify_order_by_status(Status::Pending);
    assert!(order.filled_quantity > 0.0);
    assert!(order.pending_quantity > 0.0);
    assert_eq!(order.canceled_quantity, 0.0);
    assert_eq!(
        order.filled_quantity + order.canceled_quantity + order.pending_quantity,
        order.quantity
    );
    assert_eq!(order.status(), Status::Pending);
    assert!(!order.is_done());
}

pub fn test_vorder_modify_by_status() {
    let base = Utc.with_ymd_and_hms(2023, 1, 1, 11, 20, 0).unwrap();
    let mut init = vorder_kwargs();
    init.now_override = Some(base);
    let mut order = VOrder::from_init(init).unwrap();
    order.modify_by_status(Status::Complete, base);
    assert!(!order.is_done());
    assert_eq!(order.filled_quantity, 0.0);
    // Upstream: +1s is_past_delay is False (1e6 microseconds == 1s, strict >)
    order.modify_by_status(Status::Complete, base + Duration::seconds(1));
    assert_eq!(order.status(), Status::Open);
    // +2s is past delay
    order.modify_by_status(Status::Complete, base + Duration::seconds(2));
    assert_eq!(order.status(), Status::Complete);
    assert!(order.is_done());
    assert_eq!(order.filled_quantity, 100.0);
}

pub fn test_vorder_modify_by_status_do_not_modify_done() {
    let base = Utc.with_ymd_and_hms(2023, 1, 1, 11, 20, 0).unwrap();
    let mut init = vorder_kwargs();
    init.now_override = Some(base);
    let mut order = VOrder::from_init(init).unwrap();
    order.modify_by_status(Status::Complete, base + Duration::seconds(2));
    assert_eq!(order.status(), Status::Complete);
    order.modify_by_status(Status::Canceled, base + Duration::seconds(5));
    assert_eq!(order.status(), Status::Complete);
}

/// Upstream has this same name twice in the test file (`_partial_fill`);
/// pytest collects only the second definition. The Rust port mirrors that
/// "second definition wins" semantic by naming this trial `*_full_flow`
/// to avoid shadowing the `_unit` trial above.
pub fn test_vorder_modify_by_status_partial_fill() {
    let base = Utc.with_ymd_and_hms(2023, 1, 1, 11, 20, 0).unwrap();
    let mut init = vorder_kwargs();
    init.now_override = Some(base);
    let mut order = VOrder::from_init(init).unwrap();
    order.modify_by_status(Status::PartialFill, base + Duration::seconds(2));
    assert!(order.filled_quantity < order.quantity);
    assert!(order.canceled_quantity > 0.0);
    assert_eq!(order.pending_quantity, 0.0);
    assert!(order.is_done());
}

// ── Ticker ──────────────────────────────────────────────────────────────

fn basic_ticker() -> Ticker {
    Ticker::with_initial_price("aapl", 125.0).with_token(1234)
}

pub fn test_ticker_defaults() {
    let t = Ticker::new("abcd");
    assert_eq!(t.name, "abcd");
    assert_eq!(t.token, None);
    assert_eq!(t.initial_price, 100.0);
    assert_eq!(t.mode, TickerMode::Random);
    assert_eq!(t.high(), 100.0);
    assert_eq!(t.low(), 100.0);
    assert_eq!(t.ltp_snapshot(), 100.0);
}

pub fn test_ticker_is_random() {
    let mut t = Ticker::new("abcd");
    assert!(t.is_random());
    t.mode = TickerMode::Manual;
    assert!(!t.is_random());
}

pub fn test_ticker_ohlc() {
    let t = basic_ticker();
    let ohlc = t.ohlc();
    assert_eq!(ohlc.open, 125.0);
    assert_eq!(ohlc.high, 125.0);
    assert_eq!(ohlc.low, 125.0);
    assert_eq!(ohlc.close, 125.0);
    for _ in 0..15 {
        t.ltp();
    }
    let ohlc = t.ohlc();
    // After 15 random draws, high/low diverge from 125; close == latest ltp.
    assert!(ohlc.high >= 125.0 || ohlc.low <= 125.0);
    assert_eq!(ohlc.close, t.ltp_snapshot());
}

pub fn test_ticker_ticker_mode() {
    let mut t = basic_ticker();
    t.mode = TickerMode::Manual;
    for _ in 0..3 {
        let _ = t.ltp();
    }
    // In manual mode, ltp never moves from initial_price.
    assert_eq!(t.ltp(), 125.0);
    t.mode = TickerMode::Random;
    // SmallRng::seed_from_u64(0) + Normal(0,1) draws a non-zero Z at the
    // first call, so rounded ltp != 125. (§14B candidate if this ever
    // flakes — probabilistic ≥95/100 acceptance per plan §6 D10 — but with
    // the fixed seed it's deterministic here.)
    assert_ne!(t.ltp(), 125.0);
}

pub fn test_ticker_update() {
    let t = basic_ticker();
    for ltp in [128.0_f64, 123.0, 124.0, 126.0] {
        t.update(ltp);
    }
    let ohlc = t.ohlc();
    assert_eq!(ohlc.open, 125.0);
    assert_eq!(ohlc.high, 128.0);
    assert_eq!(ohlc.low, 123.0);
    assert_eq!(ohlc.close, 126.0);
    assert_eq!(ohlc.last_price, 126.0);
}

// ── side parsing ────────────────────────────────────────────────────────

pub fn test_vorder_side() {
    for s in ["buy", "BUY", "b"] {
        let order = VOrder::from_init(VOrderInit {
            order_id: "123456789".into(),
            symbol: "aapl".into(),
            quantity: 100.0,
            side_str: Some(s.into()),
            now_override: Some(Utc::now()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(order.side, Side::Buy);
    }
    for s in ["s", "sell"] {
        let order = VOrder::from_init(VOrderInit {
            order_id: "123456789".into(),
            symbol: "aapl".into(),
            quantity: 100.0,
            side_str: Some(s.into()),
            now_override: Some(Utc::now()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(order.side, Side::Sell);
    }
}

pub fn test_vorder_side_error() {
    let err = VOrder::from_init(VOrderInit {
        order_id: "123456789".into(),
        symbol: "aapl".into(),
        quantity: 100.0,
        side_str: Some("unknown".into()),
        now_override: Some(Utc::now()),
        ..Default::default()
    });
    assert!(err.is_err(), "expected validation error for unknown side");
}

pub fn test_instrument_defaults() {
    let inst = Instrument {
        name: "nifty".into(),
        token: None,
        last_price: 12340.0,
        open: 12188.0,
        high: 12400.0,
        low: 12100.0,
        close: 12340.0,
        volume: None,
        open_interest: None,
        strike: None,
        expiry: None,
        orderbook: None,
        last_update_time: None,
    };
    assert!(inst.token.is_none());
    assert!(inst.volume.is_none());
    assert!(inst.orderbook.is_none());
    assert!(inst.last_update_time.is_none());
}

// ── OrderFill ───────────────────────────────────────────────────────────

fn order_fill_ltp() -> OrderFill {
    let order = VOrder::from_init(VOrderInit {
        order_id: "order_id".into(),
        symbol: "aapl".into(),
        quantity: 100.0,
        side: Some(Side::Buy),
        price: Some(127.0),
        now_override: Some(Utc::now()),
        ..Default::default()
    })
    .unwrap();
    OrderFill::new(order, 128.0)
}

pub fn test_order_fill_ltp() {
    let mut fill = order_fill_ltp();
    fill.update();
    assert_eq!(fill.order.filled_quantity, 100.0);
    assert!(fill.done());
    assert_eq!(fill.order.average_price, Some(128.0));
    assert_eq!(fill.order.status(), Status::Complete);

    // After complete, update() is a no-op.
    fill.last_price = 130.0;
    fill.update();
    assert_eq!(fill.order.average_price, Some(128.0));
    assert_eq!(fill.order.filled_quantity, 100.0);
}

pub fn test_order_fill_different_ltp() {
    let mut fill = order_fill_ltp();
    fill.order.quantity = 120.0;
    fill.update_with_price(Some(129.0));
    assert_eq!(fill.order.filled_quantity, 120.0);
    assert!(fill.done());
    assert_eq!(fill.order.average_price, Some(129.0));
    assert_eq!(fill.order.status(), Status::Complete);
}

pub fn test_order_fill_ltp_buy() {
    let mut fill = order_fill_ltp();
    fill.order.order_type = OrderType::Limit;
    fill.update();
    assert_eq!(fill.order.filled_quantity, 0.0);
    fill.last_price = 128.0;
    fill.update();
    assert_eq!(fill.order.filled_quantity, 0.0);
    fill.last_price = 126.95;
    fill.update();
    assert_eq!(fill.order.filled_quantity, 100.0);
    assert_eq!(fill.order.average_price, Some(127.0));
    assert_eq!(fill.order.price, Some(127.0));
}

pub fn test_order_fill_ltp_sell() {
    let mut fill = order_fill_ltp();
    fill.order.order_type = OrderType::Limit;
    fill.order.side = Side::Sell;
    fill.order.price = Some(128.0);
    fill.update();
    assert_eq!(fill.order.filled_quantity, 0.0);
    fill.last_price = 127.5;
    fill.update();
    assert_eq!(fill.order.filled_quantity, 0.0);
    fill.last_price = 128.05;
    fill.update();
    assert_eq!(fill.order.filled_quantity, 100.0);
    assert_eq!(fill.order.average_price, Some(128.0));
    assert_eq!(fill.order.price, Some(128.0));
}

pub fn test_order_fill_modified_price() {
    let mut fill = order_fill_ltp();
    fill.order.order_type = OrderType::Limit;
    fill.last_price = 128.0;
    fill.update();
    for l in [128.05, 128.1, 128.25, 128.3, 128.0, 128.25] {
        fill.last_price = l;
        fill.update();
        assert!(!fill.done());
    }
    fill.order.price = Some(128.3);
    fill.update();
    assert!(fill.done());
    assert_eq!(fill.order.price, Some(128.3));
    assert_eq!(fill.order.average_price, Some(128.3));
}

pub fn test_order_fill_as_market_buy() {
    let order = VOrder::from_init(VOrderInit {
        order_id: "order_id".into(),
        symbol: "aapl".into(),
        quantity: 100.0,
        side: Some(Side::Buy),
        price: Some(130.0),
        order_type: Some(OrderType::Limit),
        now_override: Some(Utc::now()),
        ..Default::default()
    })
    .unwrap();
    let mut fill = OrderFill::new(order, 128.0);
    assert!(fill.done());
    assert_eq!(fill.order.filled_quantity, 100.0);
    assert_eq!(fill.order.pending_quantity, 0.0);
    assert_eq!(fill.order.average_price, Some(128.0));
    assert_eq!(fill.order.price, Some(130.0));
    fill.update();
    assert_eq!(fill.order.average_price, Some(128.0));
}

pub fn test_order_fill_as_market_sell() {
    let order = VOrder::from_init(VOrderInit {
        order_id: "order_id".into(),
        symbol: "aapl".into(),
        quantity: 100.0,
        side: Some(Side::Sell),
        price: Some(130.0),
        order_type: Some(OrderType::Limit),
        now_override: Some(Utc::now()),
        ..Default::default()
    })
    .unwrap();
    let mut fill = OrderFill::new(order, 134.0);
    assert!(fill.done());
    assert_eq!(fill.order.filled_quantity, 100.0);
    assert_eq!(fill.order.average_price, Some(134.0));
    assert_eq!(fill.order.price, Some(130.0));
    fill.update();
    assert_eq!(fill.order.average_price, Some(134.0));
}

pub fn test_order_fill_ltp_all_quantity() {
    let mut fill = order_fill_ltp();
    fill.update();
    assert_eq!(fill.order.filled_quantity, 100.0);
    assert_eq!(fill.order.pending_quantity, 0.0);
    assert_eq!(fill.order.canceled_quantity, 0.0);
    assert!(fill.done());
    assert_eq!(fill.order.average_price, Some(128.0));
    assert_eq!(fill.order.status(), Status::Complete);
}

pub fn test_order_fill_stop_no_trigger_price() {
    let order = VOrder::from_init(VOrderInit {
        order_id: "1234".into(),
        symbol: "aapl".into(),
        quantity: 100.0,
        side: Some(Side::Buy),
        price: Some(130.0),
        order_type: Some(OrderType::Stop),
        now_override: Some(Utc::now()),
        ..Default::default()
    })
    .unwrap();
    let mut fill = OrderFill::new(order, 128.0);
    assert!(!fill.done());
    assert_eq!(fill.order.filled_quantity, 0.0);
    fill.update_with_price(Some(130.2));
    assert!(fill.done());
    assert_eq!(fill.order.filled_quantity, 100.0);
    assert_eq!(fill.order.pending_quantity, 0.0);
    assert_eq!(fill.order.canceled_quantity, 0.0);
    assert_eq!(fill.order.average_price, Some(130.2));
    assert_eq!(fill.order.price, Some(130.0));
}

pub fn test_order_fill_stop_buy() {
    let order = VOrder::from_init(VOrderInit {
        order_id: "1234".into(),
        symbol: "aapl".into(),
        quantity: 100.0,
        side: Some(Side::Buy),
        trigger_price: Some(130.0),
        price: Some(130.0),
        order_type: Some(OrderType::Stop),
        now_override: Some(Utc::now()),
        ..Default::default()
    })
    .unwrap();
    let mut fill = OrderFill::new(order, 128.0);
    assert!(!fill.done());
    for ltp in 122..128 {
        fill.update_with_price(Some(ltp as f64));
        assert!(!fill.done());
    }
    fill.update_with_price(Some(130.2));
    assert!(fill.done());
    assert_eq!(fill.order.filled_quantity, 100.0);
    assert_eq!(fill.order.average_price, Some(130.2));
    assert_eq!(fill.order.price, Some(130.0));
}

pub fn test_order_fill_stop_sell() {
    let order = VOrder::from_init(VOrderInit {
        order_id: "1234".into(),
        symbol: "aapl".into(),
        quantity: 100.0,
        side: Some(Side::Sell),
        trigger_price: Some(130.0),
        price: Some(130.0),
        order_type: Some(OrderType::Stop),
        now_override: Some(Utc::now()),
        ..Default::default()
    })
    .unwrap();
    let mut fill = OrderFill::new(order, 132.0);
    for ltp in 132..140 {
        fill.update_with_price(Some(ltp as f64));
        assert!(!fill.done());
    }
    fill.update_with_price(Some(129.0));
    assert!(fill.done());
    assert_eq!(fill.order.filled_quantity, 100.0);
    assert_eq!(fill.order.average_price, Some(129.0));
    assert_eq!(fill.order.price, Some(130.0));
}

pub fn test_order_fill_stop_as_market() {
    let order = VOrder::from_init(VOrderInit {
        order_id: "1234".into(),
        symbol: "aapl".into(),
        quantity: 100.0,
        side: Some(Side::Buy),
        trigger_price: Some(130.0),
        order_type: Some(OrderType::Stop),
        now_override: Some(Utc::now()),
        ..Default::default()
    })
    .unwrap();
    let fill = OrderFill::new(order, 132.0);
    assert!(fill.done());

    let order = VOrder::from_init(VOrderInit {
        order_id: "1234".into(),
        symbol: "aapl".into(),
        quantity: 100.0,
        side: Some(Side::Sell),
        trigger_price: Some(130.0),
        order_type: Some(OrderType::Stop),
        now_override: Some(Utc::now()),
        ..Default::default()
    })
    .unwrap();
    let fill = OrderFill::new(order, 128.0);
    assert!(fill.done());
}
