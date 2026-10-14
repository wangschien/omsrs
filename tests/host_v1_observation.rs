use omsrs::execution::{
    BatchOutcome, ExecutionScope, LedgerId, OrderCursor, OrderTxnRecord, RebuildError,
};
use omsrs::host_v1::{HostError, OrderBook};
use omsrs::lifecycle::{
    ClientOrderId, JournalRecord, OrderCtx, OrderEvent, OrderState, Side, VenueOrderId,
};
use omsrs::observation::{Readiness, WorkingView};

fn ledger() -> LedgerId {
    LedgerId {
        scope: ExecutionScope("scope-a".into()),
        market: "market-a".into(),
    }
}
fn context(client: &str) -> OrderCtx {
    OrderCtx::new(
        ClientOrderId(client.into()),
        "market-a",
        "group-a",
        Side::BuyYes,
        40,
        100,
    )
}
fn bound(client: &str, wire: &str) -> JournalRecord {
    let cid = ClientOrderId(client.into());
    let wire = VenueOrderId(wire.into());
    let mut ctx = context(client);
    ctx.venue_order_id = Some(wire.clone());
    JournalRecord::OrderTxn(Box::new(OrderTxnRecord {
        ledger: ledger(),
        client_order_id: cid.clone(),
        seq: 1,
        event: "SubmitResponse".into(),
        outcome: BatchOutcome::Accept,
        core_records: vec![JournalRecord::VenueBoundCid {
            client_order_id: cid,
            venue_order_id: wire.clone(),
        }],
        state_after: OrderState::Accepted {
            venue_order_id: wire,
        },
        ctx_after: ctx,
    }))
}

#[test]
fn preparation_preserves_committed_state_and_effects() {
    let mut book = OrderBook::new(ledger());
    let ctx = context("client-a");
    let cid = ctx.client_order_id.clone();
    book.insert_initial(ctx.clone()).unwrap();
    let transaction = book
        .prepare_event(&cid, &OrderEvent::PrepareSubmit)
        .unwrap();
    assert!(transaction.journal.is_some());
    assert!(transaction.rejected.is_none());
    let row = book.order(&cid).unwrap();
    assert_eq!(row.state, &OrderState::New);
    assert_eq!(row.ctx, &ctx);
    assert_eq!(row.cursor, OrderCursor { seq: 0 });
    assert_eq!(book.inventory().net(), 0);
    assert!(book.wire_for_client(&cid).is_none());
}

#[test]
fn rebuild_retains_canonical_membership_and_unproved_working_view() {
    let record = bound("client-a", "wire-a");
    let book = OrderBook::from_committed_records(ledger(), &[record]).unwrap();
    let cid = ClientOrderId("client-a".into());
    assert_eq!(
        book.wire_for_client(&cid),
        Some(&VenueOrderId("wire-a".into()))
    );
    assert_eq!(book.order(&cid).unwrap().cursor.seq, 1);
    assert_eq!(
        book.working_view(&VenueOrderId("wire-a".into())),
        WorkingView::Unresolved
    );
    assert_eq!(book.registry_readiness(), Ok(Readiness::Ready));
    assert!(!book.order(&cid).unwrap().ctx.authority_is_fresh());
    assert_eq!(book.view().orders, 1);
}

#[test]
fn rebuild_rejects_sequence_gaps_and_divergent_order_snapshots() {
    let first = bound("client-a", "wire-a");
    let mut gap = first.clone();
    if let JournalRecord::OrderTxn(order) = &mut gap {
        order.seq = 2;
    }
    assert!(matches!(
        OrderBook::from_committed_records(ledger(), &[gap]),
        Err(HostError::Rebuild(RebuildError::SeqGap {
            expected: 1,
            found: 2,
            ..
        }))
    ));
    let mut divergent = first.clone();
    if let JournalRecord::OrderTxn(order) = &mut divergent {
        order.ctx_after.qty = 101;
    }
    assert!(matches!(
        OrderBook::from_committed_records(ledger(), &[first, divergent]),
        Err(HostError::Rebuild(RebuildError::Divergent { .. }))
    ));
}

#[test]
fn initial_registration_rejects_foreign_market_and_duplicate_without_overwrite() {
    let mut book = OrderBook::new(ledger());
    let ctx = context("client-a");
    let cid = ctx.client_order_id.clone();
    book.insert_initial(ctx.clone()).unwrap();
    let mut replacement = ctx.clone();
    replacement.qty = 1;
    assert_eq!(
        book.insert_initial(replacement),
        Err(HostError::DuplicateClient(cid.clone()))
    );
    assert_eq!(book.order(&cid).unwrap().ctx, &ctx);
    let mut foreign = context("client-b");
    foreign.market = "market-b".into();
    assert!(matches!(
        book.insert_initial(foreign),
        Err(HostError::ForeignMarket { .. })
    ));
    assert_eq!(book.view().orders, 1);
}
