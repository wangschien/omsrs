use omsrs::execution::{ExecutionEvidence, ExecutionScope, ExecutionSource, LedgerId};
use omsrs::host_v1::coverage::CoverageBounds;
use omsrs::host_v1::{CommitError, HostError, MemoryJournal, OmsJournal, OrderBook};
use omsrs::lifecycle::{
    AttemptId, BackfillOrderStatus, ClientOrderId, FillId, OrderCtx, OrderEvent, Side,
    SubmissionCloseReason, VenueOrderId,
};
use omsrs::observation::{
    AmendLineage, AmendPhase, ObservationRound, PageMeta, RowEvidence, UserOrderEvidence,
    UserOrderStatus,
};

fn ledger() -> LedgerId {
    LedgerId {
        scope: ExecutionScope("account-a".into()),
        market: "market-a".into(),
    }
}
fn insert(book: &mut OrderBook, client: &str, quantity: u64) -> ClientOrderId {
    let client = ClientOrderId(client.into());
    book.insert_initial(OrderCtx::new(
        client.clone(),
        "market-a",
        "group-a",
        Side::BuyYes,
        40,
        quantity,
    ))
    .unwrap();
    client
}
fn started(book: &mut OrderBook, client: &ClientOrderId, journal: &mut MemoryJournal) {
    book.commit_event(client, &OrderEvent::PrepareSubmit, journal)
        .unwrap();
    book.commit_event(
        client,
        &OrderEvent::StartSubmit {
            attempt_id: AttemptId(client.0.clone()),
        },
        journal,
    )
    .unwrap();
}
fn bind(
    book: &mut OrderBook,
    client: &str,
    filled: u64,
    journal: &mut MemoryJournal,
) -> ClientOrderId {
    let client = insert(book, client, 100);
    started(book, &client, journal);
    book.commit_event(
        &client,
        &OrderEvent::SubmitResponse {
            venue_order_id: VenueOrderId("wire-a".into()),
            fill_count: filled,
            remaining_count: 100 - filled,
            avg_price_cents: None,
            fee_cents: None,
            snapshot_boundary: None,
        },
        journal,
    )
    .unwrap();
    client
}
fn fill(id: &str, client: Option<&ClientOrderId>, wire: &str, quantity: u64) -> ExecutionEvidence {
    ExecutionEvidence {
        scope: ledger().scope,
        market: ledger().market,
        execution_id: FillId(id.into()),
        side: Side::BuyYes,
        qty: quantity,
        price_cents: 40,
        ts_ns: 1,
        venue_order_id: Some(VenueOrderId(wire.into())),
        claimed_client_order_id: client.cloned(),
        fee_cents: Some(0),
        source: ExecutionSource::WsFill,
    }
}
fn frame(client: &ClientOrderId, filled: u64) -> UserOrderEvidence {
    UserOrderEvidence {
        wire: VenueOrderId("wire-a".into()),
        market: ledger().market,
        exchange_index: 1,
        subaccount_number: 1,
        sid: 2,
        side: Side::BuyYes,
        yes_price_e4: 400_000,
        client_order_id: client.clone(),
        status: UserOrderStatus::Resting,
        filled,
        remaining: 100 - filled,
        initial: 100,
        updated_ts_ms: 10,
        connection_generation: 1,
        payload_digest: "digest-a".into(),
    }
}

#[test]
fn missing_wire_proof_and_client_are_explicitly_unknown() {
    let book = OrderBook::new(ledger());
    let wire = VenueOrderId("missing-wire".into());
    assert_eq!(book.wire_held_quantity(&wire), None);
    assert_eq!(book.wire_total_at_proof(&wire), None);
    assert!(!book.wire_known_deficit(&wire));
    assert!(matches!(
        book.member_working_capacity(&ClientOrderId("missing-client".into())),
        Err(HostError::MissingClient(_))
    ));
}

#[test]
fn venue_cumulative_deficit_uses_inventory_not_context_or_order_status() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let client = bind(&mut book, "client-a", 0, &mut journal);
    let wire = VenueOrderId("wire-a".into());
    book.commit_user_order(frame(&client, 40), &mut journal)
        .unwrap();
    assert_eq!(book.wire_held_quantity(&wire), Some(0));
    assert!(book.wire_known_deficit(&wire));
    assert_eq!(book.inventory().net(), 0);
    book.commit_routed_execution(&fill("fill-a", Some(&client), "wire-a", 40), &mut journal)
        .unwrap();
    assert_eq!(book.wire_held_quantity(&wire), Some(40));
    assert!(!book.wire_known_deficit(&wire));
}

#[test]
fn remaining_proof_total_is_checked_and_own_action_invalidates_it() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let client = bind(&mut book, "client-a", 0, &mut journal);
    let wire = VenueOrderId("wire-a".into());
    let committed = book
        .commit_routed_execution(&fill("fill-a", Some(&client), "wire-a", 40), &mut journal)
        .unwrap();
    let mut round = ObservationRound::begin(ledger(), wire.clone());
    for outcome in &committed.outcome {
        round.record(outcome).unwrap();
    }
    round.page(PageMeta {
        exhaustive: true,
        parse_failures: 0,
    });
    let row = RowEvidence {
        ledger: ledger(),
        wire: wire.clone(),
        status: BackfillOrderStatus::Open,
        filled: 40,
        remaining: 60,
        client_order_id: Some(client.clone()),
        marker: book.registry().marker(&ledger(), &wire),
    };
    book.commit_observation(round, row, &mut journal).unwrap();
    assert_eq!(book.wire_total_at_proof(&wire), Some(100));
    assert_eq!(book.member_working_capacity(&client).unwrap(), (60, false));
    book.commit_event(
        &client,
        &OrderEvent::DecreaseObserved {
            remaining: 20,
            ts_ms: 11,
        },
        &mut journal,
    )
    .unwrap();
    assert_eq!(book.wire_total_at_proof(&wire), None);
    assert_eq!(book.member_working_capacity(&client).unwrap(), (60, false));
    let rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    assert_eq!(rebuilt.wire_total_at_proof(&wire), None);
    assert_eq!(
        rebuilt.member_working_capacity(&client).unwrap(),
        (60, false)
    );
}

#[test]
fn member_capacity_subtracts_other_attribution_and_refuses_other_member_debt() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let old = bind(&mut book, "old", 0, &mut journal);
    book.commit_routed_execution(&fill("old-fill", Some(&old), "wire-a", 40), &mut journal)
        .unwrap();
    let new = bind(&mut book, "new", 20, &mut journal);
    let wire = VenueOrderId("wire-a".into());
    assert_eq!(book.member_working_capacity(&old).unwrap(), (60, true));
    assert_eq!(book.member_working_capacity(&new).unwrap(), (60, false));
    assert_eq!(book.backfill_member_quantity(&wire, &old, 40), None);
    assert_eq!(book.backfill_member_quantity(&wire, &new, 40), Some(0));
    assert_eq!(book.backfill_member_quantity(&wire, &new, 39), None);
    book.commit_routed_execution(&fill("new-fill", Some(&new), "wire-a", 20), &mut journal)
        .unwrap();
    assert_eq!(book.member_working_capacity(&old).unwrap(), (40, false));
    assert_eq!(book.member_working_capacity(&new).unwrap(), (40, false));
    assert_eq!(book.backfill_member_quantity(&wire, &old, 60), Some(40));
    assert_eq!(book.backfill_member_quantity(&wire, &new, 60), Some(20));
}

#[test]
fn unbound_amend_child_borrows_coverage_wire_without_acceptance_and_rejects_second_pending() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let old = bind(&mut book, "old", 0, &mut journal);
    let new = insert(&mut book, "new", 120);
    let wire = VenueOrderId("wire-a".into());
    let lineage = AmendLineage {
        old_cid: old.clone(),
        new_cid: new.clone(),
        pre_generation: 0,
        phase: AmendPhase::HttpPending,
        declared_remaining: 120,
        applied_at_utc_ns: None,
    };
    book.commit_amend_lineage(&wire, lineage.clone(), &mut journal)
        .unwrap();
    assert_eq!(book.coverage_wire_for_client(&new), Some(&wire));
    assert_eq!(book.order(&new).unwrap().ctx.venue_order_id, None);
    assert!(!book.coverage_client_superseded(&old));
    let facts = book.base_coverage_members(-100);
    let bounds = CoverageBounds::from_members(facts).unwrap();
    assert_eq!(bounds.lower(), 100);
    assert_eq!(bounds.upper_rows(), Some(220));
    assert_eq!(bounds.upper_wires(), Some(120));
    let before = journal.records().len();
    assert!(matches!(
        book.commit_amend_lineage(&wire, lineage, &mut journal),
        Err(CommitError::OrderStateProof(_))
    ));
    assert_eq!(journal.records().len(), before);
    assert!(!book.stopped());
}

#[test]
fn bound_amend_child_excludes_old_only_from_lower_and_rebuilds_same_facts() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let old = bind(&mut book, "old", 0, &mut journal);
    let new = insert(&mut book, "new", 100);
    let wire = VenueOrderId("wire-a".into());
    book.commit_amend_lineage(
        &wire,
        AmendLineage {
            old_cid: old.clone(),
            new_cid: new.clone(),
            pre_generation: 0,
            phase: AmendPhase::HttpPending,
            declared_remaining: 100,
            applied_at_utc_ns: None,
        },
        &mut journal,
    )
    .unwrap();
    started(&mut book, &new, &mut journal);
    book.commit_event(
        &new,
        &OrderEvent::SubmitResponse {
            venue_order_id: wire,
            fill_count: 0,
            remaining_count: 100,
            avg_price_cents: None,
            fee_cents: None,
            snapshot_boundary: None,
        },
        &mut journal,
    )
    .unwrap();
    assert!(book.coverage_client_superseded(&old));
    let facts = book.base_coverage_members(-100);
    let bounds = CoverageBounds::from_members(facts).unwrap();
    assert_eq!(bounds.lower(), 100);
    assert_eq!(bounds.upper_rows(), Some(200));
    assert_eq!(bounds.upper_wires(), Some(100));
    let rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    assert!(rebuilt.coverage_client_superseded(&old));
    let rebuilt_facts = rebuilt.base_coverage_members(-100);
    let other = CoverageBounds::from_members(rebuilt_facts).unwrap();
    assert_eq!(
        (other.lower(), other.upper_rows(), other.upper_wires()),
        (bounds.lower(), bounds.upper_rows(), bounds.upper_wires())
    );
}

#[test]
fn coverage_facts_keep_reducing_direction_and_prior_cancel_lower_zero() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let client = bind(&mut book, "client-a", 0, &mut journal);
    assert_eq!(
        CoverageBounds::from_members(book.base_coverage_members(100))
            .unwrap()
            .lower(),
        0
    );
    assert_eq!(
        CoverageBounds::from_members(book.base_coverage_members(0))
            .unwrap()
            .upper_rows(),
        Some(0)
    );
    book.commit_event(&client, &OrderEvent::CancelRequested, &mut journal)
        .unwrap();
    let bounds = CoverageBounds::from_members(book.base_coverage_members(-100)).unwrap();
    assert_eq!(bounds.lower(), 0);
    assert_eq!(bounds.upper_rows(), Some(100));
}

#[test]
fn identified_seed_routes_to_the_known_owner_and_discharges_debt_once() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let client = bind(&mut book, "client-a", 40, &mut journal);
    let mut seed = fill("seed-a", Some(&client), "wire-a", 40);
    seed.source = ExecutionSource::VenueSeed;
    assert!(book
        .commit_seed_walk(0, 1, true, [Some(seed.clone()), Some(seed)], &mut journal)
        .unwrap());
    assert_eq!(book.inventory().net(), 40);
    assert_eq!(book.outstanding_fill_debt(), 0);
    assert_eq!(book.order(&client).unwrap().ctx.attributed_fill_qty, 40);
    assert_eq!(
        book.inventory()
            .get(&FillId("seed-a".into()))
            .unwrap()
            .owner,
        Some(client)
    );
}

#[test]
fn missing_or_wrong_wire_claim_never_selects_a_different_known_wire_owner() {
    for claim in ["missing", "client-a"] {
        let mut book = OrderBook::new(ledger());
        let mut journal = MemoryJournal::new();
        let client = bind(&mut book, "client-a", 0, &mut journal);
        let claim = ClientOrderId(claim.into());
        let wire = if claim == client {
            "other-wire"
        } else {
            "wire-a"
        };
        book.commit_routed_execution(&fill("fill-a", Some(&claim), wire, 40), &mut journal)
            .unwrap();
        assert_eq!(
            book.inventory()
                .get(&FillId("fill-a".into()))
                .unwrap()
                .owner,
            None
        );
        assert_eq!(book.order(&client).unwrap().ctx.attributed_fill_qty, 0);
    }
}

#[test]
fn unclaimed_execution_uses_unique_route_and_preserves_existing_owner_after_new_binding() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let old = bind(&mut book, "old", 0, &mut journal);
    book.commit_routed_execution(&fill("fill-a", None, "wire-a", 20), &mut journal)
        .unwrap();
    let new = bind(&mut book, "new", 0, &mut journal);
    book.commit_routed_execution(&fill("fill-a", None, "wire-a", 20), &mut journal)
        .unwrap();
    assert_eq!(
        book.inventory()
            .get(&FillId("fill-a".into()))
            .unwrap()
            .owner,
        Some(old)
    );
    assert_eq!(book.order(&new).unwrap().ctx.attributed_fill_qty, 0);
    book.commit_routed_execution(&fill("ambiguous", None, "wire-a", 10), &mut journal)
        .unwrap();
    assert_eq!(
        book.inventory()
            .get(&FillId("ambiguous".into()))
            .unwrap()
            .owner,
        None
    );
}

#[test]
fn closed_submission_routed_fill_is_inventory_only_and_does_not_revive_the_order() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let client = insert(&mut book, "client-a", 100);
    started(&mut book, &client, &mut journal);
    book.commit_event(
        &client,
        &OrderEvent::CloseSubmission {
            reason: SubmissionCloseReason::LocalNotSent,
        },
        &mut journal,
    )
    .unwrap();
    let before = book.order(&client).unwrap().ctx.clone();
    book.commit_routed_execution(&fill("late", Some(&client), "wire-a", 40), &mut journal)
        .unwrap();
    assert_eq!(book.order(&client).unwrap().ctx, &before);
    assert!(book.order(&client).unwrap().state.is_submission_closed());
    assert_eq!(
        book.inventory().get(&FillId("late".into())).unwrap().owner,
        None
    );
    assert_eq!(book.inventory().net(), 40);
}
