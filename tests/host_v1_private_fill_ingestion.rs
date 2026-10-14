use omsrs::execution::{ExecutionEvidence, ExecutionScope, ExecutionSource, LedgerId};
use omsrs::host_v1::private_fill::{
    finish_private_fill_ingestion, PrivateFillBook, PrivateFillBookkeeping,
};
use omsrs::host_v1::{CommitError, MemoryJournal, OmsJournal, OrderBook};
use omsrs::lifecycle::{
    AttemptId, ClientOrderId, FillId, HaltReason, OrderCtx, OrderEvent, OrderState, Side,
    VenueOrderId,
};

fn ledger() -> LedgerId {
    LedgerId {
        scope: ExecutionScope("account-a".into()),
        market: "market-a".into(),
    }
}
fn bound(book: &mut OrderBook, journal: &mut MemoryJournal) -> ClientOrderId {
    let cid = ClientOrderId("client-a".into());
    book.insert_initial(OrderCtx::new(
        cid.clone(),
        "market-a",
        "group-a",
        Side::BuyYes,
        40,
        100,
    ))
    .unwrap();
    book.commit_event(&cid, &OrderEvent::PrepareSubmit, journal)
        .unwrap();
    book.commit_event(
        &cid,
        &OrderEvent::StartSubmit {
            attempt_id: AttemptId("attempt-a".into()),
        },
        journal,
    )
    .unwrap();
    book.commit_event(
        &cid,
        &OrderEvent::SubmitResponse {
            venue_order_id: VenueOrderId("wire-a".into()),
            fill_count: 0,
            remaining_count: 100,
            avg_price_cents: None,
            fee_cents: None,
            snapshot_boundary: None,
        },
        journal,
    )
    .unwrap();
    cid
}
fn fill(cid: &ClientOrderId, fee: Option<u64>) -> ExecutionEvidence {
    ExecutionEvidence {
        scope: ledger().scope,
        market: ledger().market,
        execution_id: FillId("fill-a".into()),
        side: Side::BuyYes,
        qty: 40,
        price_cents: 40,
        ts_ns: 70,
        venue_order_id: Some(VenueOrderId("wire-a".into())),
        claimed_client_order_id: Some(cid.clone()),
        fee_cents: fee,
        source: ExecutionSource::WsFill,
    }
}

// Private witness of the named caller adapter. The canonical ingestion remains
// a separate operation; this fixture makes no S journal-choreography claim.
fn finish_actual_ingestion(
    ids: &mut PrivateFillBookkeeping,
    book: &mut OrderBook,
    journal: &mut MemoryJournal,
    evidence: &ExecutionEvidence,
) -> Result<PrivateFillBook, CommitError> {
    if evidence.qty == 0 {
        return Ok(PrivateFillBook::ZeroQty);
    }
    let actual = book
        .commit_ingested_execution(evidence, journal)
        .map(|receipt| receipt.outcome);
    let state = evidence
        .claimed_client_order_id
        .as_ref()
        .and_then(|cid| book.order(cid))
        .map(|row| row.state);
    let journaled = actual.is_err() && journal.has_fill_id(&evidence.execution_id.0);
    let record_count = journal.records().len();
    let result = finish_private_fill_ingestion(
        ids,
        &evidence.execution_id.0,
        evidence.fee_cents,
        actual,
        state,
        journaled,
    );
    assert_eq!(
        journal.records().len(),
        record_count,
        "classifier made a journal write"
    );
    result
}

#[test]
fn real_tracked_ingestion_then_known_zero_upgrade_updates_ids_without_double_quantity() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let cid = bound(&mut book, &mut journal);
    let mut ids = PrivateFillBookkeeping::default();
    assert_eq!(
        finish_actual_ingestion(&mut ids, &mut book, &mut journal, &fill(&cid, None)).unwrap(),
        PrivateFillBook::Booked
    );
    assert!(ids.pending_fee_fill_ids().contains("fill-a"));
    assert_eq!(book.inventory().net(), 40);
    assert_eq!(book.order(&cid).unwrap().ctx.attributed_fill_qty, 40);
    assert_eq!(
        finish_actual_ingestion(&mut ids, &mut book, &mut journal, &fill(&cid, Some(0))).unwrap(),
        PrivateFillBook::Booked
    );
    assert!(!ids.has_pending_fee_enrichment());
    assert!(ids.fill_id_seen("fill-a"));
    assert_eq!(book.inventory().net(), 40);
    assert_eq!(
        book.order(&cid).unwrap().ctx.applied_fills[&FillId("fill-a".into())].fee,
        Some(0)
    );
}

#[test]
fn real_no_row_ingestion_and_redelivery_mark_seen_without_pending_or_double_booking() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let mut ids = PrivateFillBookkeeping::default();
    let evidence = fill(&ClientOrderId("absent".into()), None);
    for _ in 0..2 {
        assert_eq!(
            finish_actual_ingestion(&mut ids, &mut book, &mut journal, &evidence).unwrap(),
            PrivateFillBook::NoRow
        );
        assert!(ids.fill_id_seen("fill-a"));
        assert!(!ids.has_pending_fee_enrichment());
        assert_eq!(book.inventory().net(), 40);
        assert!(journal.has_fill_id("fill-a"));
    }
}

#[test]
fn real_late_book_uses_label_and_outcome_without_claiming_normal_terminal_provenance() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let cid = bound(&mut book, &mut journal);
    book.commit_event(&cid, &OrderEvent::CancelRequested, &mut journal)
        .unwrap();
    book.commit_canceled_response(
        &cid,
        &VenueOrderId("wire-a".into()),
        false,
        false,
        None,
        &mut journal,
    )
    .unwrap();
    let mut ids = PrivateFillBookkeeping::default();
    assert_eq!(
        finish_actual_ingestion(&mut ids, &mut book, &mut journal, &fill(&cid, None)).unwrap(),
        PrivateFillBook::BookedLatePostTerminal
    );
    assert!(ids.fill_id_seen("fill-a"));
    assert!(ids.pending_fee_fill_ids().contains("fill-a"));
    assert!(matches!(
        book.order(&cid).unwrap().state,
        OrderState::Halted {
            reason: HaltReason::PostTerminalFill
        }
    ));
    assert!(book.post_terminal_booked_after_normal_terminal(&cid));
    let live = book.order(&cid).unwrap();
    let rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    let restored = rebuilt.order(&cid).unwrap();
    assert_eq!(
        (restored.state, restored.ctx, restored.cursor),
        (live.state, live.ctx, live.cursor)
    );
    assert_eq!(
        rebuilt
            .registry()
            .get(&ledger(), &VenueOrderId("wire-a".into())),
        book.registry()
            .get(&ledger(), &VenueOrderId("wire-a".into()))
    );
    assert_eq!(rebuilt.inventory().view(), book.inventory().view());
    let mut recovered_ids = PrivateFillBookkeeping::default();
    assert!(!recovered_ids.fill_id_seen("fill-a"));
    recovered_ids.seed_seen_fill_ids(["fill-a"]);
    assert!(recovered_ids.fill_id_seen("fill-a"));
    assert!(!recovered_ids.has_pending_fee_enrichment());
}

#[test]
fn real_ingestion_error_stays_an_error_and_does_not_mark_seen_or_pending() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let cid = bound(&mut book, &mut journal);
    let mut evidence = fill(&cid, None);
    evidence.market = "foreign-market".into();
    let mut ids = PrivateFillBookkeeping::default();
    let result = finish_actual_ingestion(&mut ids, &mut book, &mut journal, &evidence);
    assert!(result.is_err());
    assert!(!ids.fill_id_seen("fill-a"));
    assert!(ids.pending_fee_fill_ids().is_empty());
    assert_eq!(book.inventory().net(), 0);
    assert_eq!(book.order(&cid).unwrap().ctx.attributed_fill_qty, 0);
}

#[test]
fn caller_zero_quantity_return_precedes_ingestion_and_leaves_all_ids_untouched() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let mut evidence = fill(&ClientOrderId("absent".into()), None);
    evidence.qty = 0;
    let mut ids = PrivateFillBookkeeping::default();
    assert_eq!(
        finish_actual_ingestion(&mut ids, &mut book, &mut journal, &evidence).unwrap(),
        PrivateFillBook::ZeroQty
    );
    assert!(journal.records().is_empty());
    assert!(!ids.fill_id_seen("fill-a"));
    assert!(ids.pending_fee_fill_ids().is_empty());
    assert_eq!(book.inventory().net(), 0);
}
