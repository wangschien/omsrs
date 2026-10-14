use omsrs::execution::{ExecutionEvidence, ExecutionScope, ExecutionSource, LedgerId};
use omsrs::host_v1::{MemoryJournal, OmsJournal, OrderBook};
use omsrs::lifecycle::{
    AttemptId, ClientOrderId, FillId, OrderCtx, OrderEvent, Side, VenueOrderId,
};
use omsrs::observation::Readiness;

fn ledger() -> LedgerId {
    LedgerId {
        scope: ExecutionScope("account-a".into()),
        market: "market-a".into(),
    }
}
fn seed(id: &str) -> ExecutionEvidence {
    ExecutionEvidence {
        scope: ledger().scope,
        market: ledger().market,
        execution_id: FillId(id.into()),
        side: Side::BuyYes,
        qty: 40,
        price_cents: 40,
        ts_ns: 1,
        venue_order_id: None,
        claimed_client_order_id: None,
        fee_cents: Some(0),
        source: ExecutionSource::VenueSeed,
    }
}
fn prove_empty(book: &mut OrderBook, journal: &mut MemoryJournal) {
    assert!(book.commit_seed_walk(10, 10, true, [], journal).unwrap());
}

#[test]
fn registry_ready_requires_a_fresh_cutoff_and_exhaustive_walk_for_host_readiness() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    assert_eq!(book.registry_readiness(), Ok(Readiness::Ready));
    assert!(matches!(
        book.host_readiness(false),
        Readiness::Unresolved { .. }
    ));
    assert!(!book
        .commit_seed_walk(10, 9, true, [], &mut journal)
        .unwrap());
    assert!(!book
        .commit_seed_walk(10, 10, false, [], &mut journal)
        .unwrap());
    assert!(!book.seed_proven());
    prove_empty(&mut book, &mut journal);
    assert_eq!(book.host_readiness(false), Readiness::Ready);
    assert_eq!(book.host_readiness(true), Readiness::Conflict);
    assert!(journal.records().is_empty());
}

#[test]
fn invalid_walk_resets_prior_proof_and_does_not_consume_rows_before_the_gate() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    prove_empty(&mut book, &mut journal);
    let rows = std::iter::once_with(|| panic!("unwalked seed must not consume rows"));
    assert!(!book
        .commit_seed_walk(10, 10, false, rows, &mut journal)
        .unwrap());
    assert!(!book.seed_proven());
    assert!(journal.records().is_empty());
}

#[test]
fn bad_seed_row_leaves_the_committed_prefix_but_never_proves_the_walk() {
    for invalid in 0..4 {
        let mut book = OrderBook::new(ledger());
        let mut journal = MemoryJournal::new();
        let mut bad = seed("bad");
        let bad = match invalid {
            0 => None,
            1 => {
                bad.scope = ExecutionScope("other-account".into());
                Some(bad)
            }
            2 => {
                bad.market = "other-market".into();
                Some(bad)
            }
            3 => {
                bad.source = ExecutionSource::WsFill;
                Some(bad)
            }
            _ => unreachable!(),
        };
        assert!(!book
            .commit_seed_walk(0, 1, true, [Some(seed("good")), bad], &mut journal)
            .unwrap());
        assert_eq!(book.inventory().net(), 40);
        assert_eq!(journal.records().len(), 1);
        assert!(!book.seed_proven());
        assert!(!book.stopped());
        assert!(matches!(
            book.host_readiness(false),
            Readiness::Unresolved { .. }
        ));
    }
}

#[test]
fn successful_seed_walk_deduplicates_and_reconstruction_does_not_restore_seed_proof() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let row = seed("fill-a");
    assert!(book
        .commit_seed_walk(0, 1, true, [Some(row.clone()), Some(row)], &mut journal)
        .unwrap());
    assert_eq!(book.inventory().net(), 40);
    assert_eq!(book.inventory().view().executions, 1);
    assert!(book.seed_proven());
    let rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    assert_eq!(rebuilt.inventory().view(), book.inventory().view());
    assert!(!rebuilt.seed_proven());
    assert!(matches!(
        rebuilt.host_readiness(false),
        Readiness::Unresolved { .. }
    ));
}

#[test]
fn conflict_has_precedence_over_seed_and_unknown_submission_blockers() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let row = seed("fill-a");
    book.commit_execution(None, &row, &mut journal).unwrap();
    let mut conflict = row;
    conflict.qty += 1;
    book.commit_execution(None, &conflict, &mut journal)
        .unwrap();
    assert!(!book.seed_proven());
    assert_eq!(book.host_readiness(false), Readiness::Conflict);
    prove_empty(&mut book, &mut journal);
    assert_eq!(book.host_readiness(false), Readiness::Conflict);
}

#[test]
fn unknown_submission_remains_unready_despite_seed_and_registry_proof() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let client = ClientOrderId("client-a".into());
    book.insert_initial(OrderCtx::new(
        client.clone(),
        "market-a",
        "group-a",
        Side::BuyYes,
        40,
        100,
    ))
    .unwrap();
    book.commit_event(&client, &OrderEvent::PrepareSubmit, &mut journal)
        .unwrap();
    book.commit_event(
        &client,
        &OrderEvent::StartSubmit {
            attempt_id: AttemptId("attempt-a".into()),
        },
        &mut journal,
    )
    .unwrap();
    book.commit_event(&client, &OrderEvent::SubmitTimeout, &mut journal)
        .unwrap();
    prove_empty(&mut book, &mut journal);
    assert_eq!(book.registry_readiness(), Ok(Readiness::Ready));
    assert!(matches!(
        book.host_readiness(false),
        Readiness::Unresolved { .. }
    ));
    assert_eq!(book.host_readiness(true), Readiness::Conflict);
}

#[test]
fn canonical_fill_obligation_is_a_high_water_debt_and_execution_discharges_it_once() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let client = ClientOrderId("client-a".into());
    let wire = VenueOrderId("wire-a".into());
    book.insert_initial(OrderCtx::new(
        client.clone(),
        "market-a",
        "group-a",
        Side::BuyYes,
        40,
        100,
    ))
    .unwrap();
    book.commit_event(&client, &OrderEvent::PrepareSubmit, &mut journal)
        .unwrap();
    book.commit_event(
        &client,
        &OrderEvent::StartSubmit {
            attempt_id: AttemptId("attempt-a".into()),
        },
        &mut journal,
    )
    .unwrap();
    book.commit_event(
        &client,
        &OrderEvent::SubmitResponse {
            venue_order_id: wire.clone(),
            fill_count: 40,
            remaining_count: 60,
            avg_price_cents: None,
            fee_cents: None,
            snapshot_boundary: None,
        },
        &mut journal,
    )
    .unwrap();
    assert_eq!(book.outstanding_fill_debt(), 40);
    assert_eq!(book.outstanding_fill_obligation_rows(), 1);
    let mut fill = seed("fill-a");
    fill.source = ExecutionSource::WsFill;
    fill.venue_order_id = Some(wire);
    fill.claimed_client_order_id = Some(client.clone());
    book.commit_execution(Some(&client), &fill, &mut journal)
        .unwrap();
    book.commit_execution(Some(&client), &fill, &mut journal)
        .unwrap();
    assert_eq!(book.inventory().net(), 40);
    assert_eq!(book.order(&client).unwrap().ctx.fill_obligation, 40);
    assert_eq!(book.outstanding_fill_debt(), 0);
    assert_eq!(book.outstanding_fill_obligation_rows(), 0);
    let rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    assert_eq!(
        rebuilt.outstanding_fill_debt(),
        book.outstanding_fill_debt()
    );
    assert_eq!(
        rebuilt.order(&client).unwrap().ctx,
        book.order(&client).unwrap().ctx
    );
}
