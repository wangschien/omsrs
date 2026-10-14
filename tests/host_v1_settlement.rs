use omsrs::execution::{
    ExecutionEvidence, ExecutionScope, ExecutionSource, InventoryDisposition, LedgerId,
};
use omsrs::host_v1::coverage::CoverageBounds;
use omsrs::host_v1::{
    CommitError, DurabilityAck, JournalError, JournalOperation, MemoryJournal, OmsJournal,
    OrderBook,
};
use omsrs::lifecycle::BackfillOrderStatus;
use omsrs::lifecycle::{
    AttemptId, ClientOrderId, Effect, FillId, HaltReason, JournalRecord, OrderCtx, OrderEvent,
    OrderState, Side, SubmissionCloseReason, VenueOrderId,
};
use omsrs::observation::{
    AmendLineage, AmendPhase, ObservationOutcome, ObservationRound, PageMeta, RowEvidence,
    UserOrderEvidence, UserOrderStatus, WorkingView,
};

fn ledger() -> LedgerId {
    LedgerId {
        scope: ExecutionScope("account-a".into()),
        market: "market-a".into(),
    }
}
fn wire() -> VenueOrderId {
    VenueOrderId("wire-a".into())
}
fn insert(book: &mut OrderBook, name: &str, qty: u64) -> ClientOrderId {
    let cid = ClientOrderId(name.into());
    book.insert_initial(OrderCtx::new(
        cid.clone(),
        "market-a",
        "group-a",
        Side::BuyYes,
        40,
        qty,
    ))
    .unwrap();
    cid
}
fn started(book: &mut OrderBook, cid: &ClientOrderId, journal: &mut dyn OmsJournal) {
    book.commit_event(cid, &OrderEvent::PrepareSubmit, journal)
        .unwrap();
    book.commit_event(
        cid,
        &OrderEvent::StartSubmit {
            attempt_id: AttemptId(cid.0.clone()),
        },
        journal,
    )
    .unwrap();
}
fn accept(book: &mut OrderBook, cid: &ClientOrderId, journal: &mut dyn OmsJournal) {
    let qty = book.order(cid).unwrap().ctx.qty;
    book.commit_event(
        cid,
        &OrderEvent::SubmitResponse {
            venue_order_id: wire(),
            fill_count: 0,
            remaining_count: qty,
            avg_price_cents: None,
            fee_cents: None,
            snapshot_boundary: None,
        },
        journal,
    )
    .unwrap();
}
fn bound(book: &mut OrderBook, name: &str, journal: &mut dyn OmsJournal) -> ClientOrderId {
    let cid = insert(book, name, 100);
    started(book, &cid, journal);
    accept(book, &cid, journal);
    cid
}
fn fill(id: &str, cid: Option<&ClientOrderId>, qty: u64, ts: i64) -> ExecutionEvidence {
    ExecutionEvidence {
        scope: ledger().scope,
        market: ledger().market,
        execution_id: FillId(id.into()),
        side: Side::BuyYes,
        qty,
        price_cents: 40,
        ts_ns: ts,
        venue_order_id: Some(wire()),
        claimed_client_order_id: cid.cloned(),
        fee_cents: Some(0),
        source: ExecutionSource::WsFill,
    }
}
fn frame(cid: &ClientOrderId, status: UserOrderStatus, filled: u64) -> UserOrderEvidence {
    UserOrderEvidence {
        wire: wire(),
        market: ledger().market,
        exchange_index: 1,
        subaccount_number: 1,
        sid: 2,
        side: Side::BuyYes,
        yes_price_e4: 400_000,
        client_order_id: cid.clone(),
        status,
        filled,
        remaining: if status == UserOrderStatus::Resting {
            100 - filled
        } else {
            0
        },
        initial: if status == UserOrderStatus::Executed {
            filled
        } else {
            100
        },
        updated_ts_ms: 10,
        connection_generation: 1,
        payload_digest: "digest-a".into(),
    }
}
fn declare(
    book: &mut OrderBook,
    old: &ClientOrderId,
    new: &ClientOrderId,
    journal: &mut dyn OmsJournal,
) -> AmendLineage {
    let lineage = AmendLineage {
        old_cid: old.clone(),
        new_cid: new.clone(),
        pre_generation: book.registry().marker(&ledger(), &wire()).0,
        phase: AmendPhase::HttpPending,
        declared_remaining: book.order(new).unwrap().ctx.qty,
        applied_at_utc_ns: None,
    };
    book.commit_amend_lineage(&wire(), lineage.clone(), journal)
        .unwrap();
    lineage
}
fn prove(book: &mut OrderBook, cid: &ClientOrderId, journal: &mut dyn OmsJournal) {
    let mut round = ObservationRound::begin(ledger(), wire());
    round.page(PageMeta {
        exhaustive: true,
        parse_failures: 0,
    });
    let row = RowEvidence {
        ledger: ledger(),
        wire: wire(),
        status: BackfillOrderStatus::Open,
        filled: 0,
        remaining: 100,
        client_order_id: Some(cid.clone()),
        marker: book.registry().marker(&ledger(), &wire()),
    };
    book.commit_observation(round, row, journal).unwrap();
}
fn rebuilt_same(book: &OrderBook, journal: &dyn OmsJournal, cids: &[&ClientOrderId]) {
    rebuilt_with_source_positions(book, journal, cids, false);
}
fn rebuilt_with_source_positions(
    book: &OrderBook,
    journal: &dyn OmsJournal,
    cids: &[&ClientOrderId],
    direct_lineage_before_child_bind: bool,
) {
    let rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    assert_eq!(rebuilt.inventory().view(), book.inventory().view());
    let mut expected_wire = book.registry().get(&ledger(), &wire()).cloned();
    if direct_lineage_before_child_bind {
        // S q skips the direct lineage append; S replay still enumerates it.
        // Pin BOTH exact coordinates, then compare the complete wire image.
        let member = expected_wire
            .as_mut()
            .unwrap()
            .members
            .iter_mut()
            .find(|m| m.cid.0 == "new")
            .unwrap();
        assert_eq!(member.bind_pos, 5);
        member.bind_pos = 6;
    }
    assert_eq!(
        rebuilt.registry().get(&ledger(), &wire()),
        expected_wire.as_ref()
    );
    assert_eq!(
        rebuilt.last_applied_execution_ts_ns(),
        book.last_applied_execution_ts_ns()
    );
    for cid in cids {
        let actual = book.order(cid).unwrap();
        let restored = rebuilt.order(cid).unwrap();
        assert_eq!(restored.state, actual.state);
        assert_eq!(restored.ctx, actual.ctx);
        assert_eq!(restored.cursor, actual.cursor);
        assert_eq!(
            rebuilt.post_terminal_booked_after_normal_terminal(cid),
            book.post_terminal_booked_after_normal_terminal(cid)
        );
    }
}

#[test]
fn venue_clock_uses_only_new_applied_ws_rest_fills_and_survives_rebuild() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    for (id, source, ts, expected) in [
        ("seed", ExecutionSource::VenueSeed, 900, 0),
        ("create", ExecutionSource::CreateResponseBackfill, 800, 0),
        ("ws", ExecutionSource::WsFill, 50, 50),
        ("rest", ExecutionSource::RestFill, 70, 70),
        ("negative", ExecutionSource::RestFill, -1, 70),
        ("older", ExecutionSource::WsFill, 60, 70),
    ] {
        let mut evidence = fill(id, None, 5, ts);
        evidence.source = source;
        book.commit_ingested_execution(&evidence, &mut journal)
            .unwrap();
        assert_eq!(book.last_applied_execution_ts_ns(), expected);
        book.commit_ingested_execution(&evidence, &mut journal)
            .unwrap();
        assert_eq!(book.last_applied_execution_ts_ns(), expected);
    }
    assert_eq!(book.inventory().net(), 30);
    rebuilt_same(&book, &journal, &[]);
}

#[test]
fn stored_terminal_waits_for_actual_fill_then_settles_without_synthesizing_inventory() {
    for status in [UserOrderStatus::Canceled, UserOrderStatus::Executed] {
        let mut book = OrderBook::new(ledger());
        let mut journal = MemoryJournal::new();
        let cid = bound(&mut book, "a", &mut journal);
        let quantity = if status == UserOrderStatus::Executed {
            100
        } else {
            40
        };
        book.commit_user_order(frame(&cid, status, quantity), &mut journal)
            .unwrap();
        let before = journal.records().len();
        assert!(
            !book
                .commit_terminal_frame_if_complete(&wire(), &mut journal)
                .unwrap()
                .outcome
        );
        assert_eq!(journal.records().len(), before);
        assert_eq!(book.inventory().net(), 0);
        book.commit_ingested_execution(&fill("fill-a", Some(&cid), quantity, 70), &mut journal)
            .unwrap();
        assert_eq!(
            book.order(&cid).unwrap().state,
            if status == UserOrderStatus::Executed {
                &OrderState::Filled
            } else {
                &OrderState::Canceled
            }
        );
        assert_eq!(book.inventory().net(), i128::from(quantity));
        assert!(!book.post_terminal_booked_after_normal_terminal(&cid));
        rebuilt_same(&book, &journal, &[&cid]);
    }
}

#[test]
fn stored_resting_frame_is_reproved_only_after_inventory_catches_up() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let cid = bound(&mut book, "a", &mut journal);
    book.commit_user_order(frame(&cid, UserOrderStatus::Resting, 40), &mut journal)
        .unwrap();
    let before = journal.records().len();
    assert!(
        !book
            .commit_resting_reproof_if_ready(&wire(), &mut journal)
            .unwrap()
            .outcome
    );
    assert_eq!(journal.records().len(), before);
    book.commit_ingested_execution(&fill("fill-a", Some(&cid), 40, 70), &mut journal)
        .unwrap();
    assert_eq!(book.working_view(&wire()), WorkingView::Known(60));
    assert_eq!(book.inventory().net(), 40);
    rebuilt_same(&book, &journal, &[&cid]);
}

#[test]
fn terminal_child_retires_old_before_lineage_clear_and_preserves_ack_order() {
    let mut book = OrderBook::new(ledger());
    let mut journal = FailTerminalJournal::default();
    let old = bound(&mut book, "old", &mut journal);
    let new = insert(&mut book, "new", 100);
    declare(&mut book, &old, &new, &mut journal);
    started(&mut book, &new, &mut journal);
    accept(&mut book, &new, &mut journal);
    book.commit_user_order(frame(&new, UserOrderStatus::Canceled, 40), &mut journal)
        .unwrap();
    let before = journal.records().len();
    let receipt = book
        .commit_ingested_execution(&fill("child-fill", Some(&new), 40, 80), &mut journal)
        .unwrap();
    assert_eq!(book.order(&old).unwrap().state, &OrderState::Canceled);
    assert_eq!(book.order(&new).unwrap().state, &OrderState::Canceled);
    assert!(book
        .registry()
        .get(&ledger(), &wire())
        .unwrap()
        .amend_lineage
        .is_none());
    assert_eq!(book.inventory().net(), 40);
    let tail = &journal.records()[before..];
    assert!(matches!(tail.first(), Some(JournalRecord::ExecutionTxn(_))));
    assert!(matches!(
        tail.last(),
        Some(JournalRecord::AmendLineage { lineage: None, .. })
    ));
    let old_terminal = tail.iter().position(|record| matches!(record, JournalRecord::OrderTxn(order) if order.client_order_id == old && order.core_records.iter().any(|record| matches!(record, JournalRecord::OrderTerminal { .. })))).unwrap();
    assert!(old_terminal < tail.len() - 1);
    let mut ends = Vec::new();
    for ack in &receipt.durability {
        match ack {
            DurabilityAck::Volatile { records_end }
            | DurabilityAck::PageCache { records_end }
            | DurabilityAck::StableStorage { records_end }
            | DurabilityAck::Uncertain { records_end, .. } => ends.push(*records_end),
        }
    }
    assert!(ends.windows(2).all(|pair| pair[0] <= pair[1]));
    assert_eq!(ends.last(), Some(&(journal.records().len() as u64)));
    rebuilt_with_source_positions(&book, &journal, &[&old, &new], true);
}

#[test]
fn resting_amend_proof_clears_lineage_and_settles_superseded_old() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let old = bound(&mut book, "old", &mut journal);
    let new = insert(&mut book, "new", 100);
    let mut lineage = declare(&mut book, &old, &new, &mut journal);
    started(&mut book, &new, &mut journal);
    accept(&mut book, &new, &mut journal);
    book.commit_event(&old, &OrderEvent::AmendObserved, &mut journal)
        .unwrap();
    lineage.phase = AmendPhase::AppliedNeedsObservation;
    book.commit_amend_lineage(&wire(), lineage, &mut journal)
        .unwrap();
    book.commit_user_order(frame(&new, UserOrderStatus::Resting, 40), &mut journal)
        .unwrap();
    book.commit_ingested_execution(&fill("child-fill", Some(&new), 40, 80), &mut journal)
        .unwrap();
    assert_eq!(book.working_view(&wire()), WorkingView::Known(60));
    assert_eq!(book.order(&old).unwrap().state, &OrderState::Canceled);
    assert!(book
        .registry()
        .get(&ledger(), &wire())
        .unwrap()
        .amend_lineage
        .is_none());
    rebuilt_same(&book, &journal, &[&old, &new]);
}

#[test]
fn unknown_child_full_fill_resolves_using_booked_ids_without_another_fill() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let cid = insert(&mut book, "new", 100);
    started(&mut book, &cid, &mut journal);
    book.commit_event(&cid, &OrderEvent::SubmitTimeout, &mut journal)
        .unwrap();
    book.commit_ingested_execution(&fill("child-fill", Some(&cid), 100, 80), &mut journal)
        .unwrap();
    assert_eq!(book.order(&cid).unwrap().state, &OrderState::Filled);
    assert_eq!(book.inventory().net(), 100);
    assert_eq!(book.inventory().view().executions, 1);
    assert!(!book.post_terminal_booked_after_normal_terminal(&cid));
    rebuilt_same(&book, &journal, &[&cid]);
}

#[test]
fn old_full_fill_closes_unknown_unfilled_child_and_late_child_stays_inventory_only() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let old = bound(&mut book, "old", &mut journal);
    let new = insert(&mut book, "new", 100);
    let mut lineage = declare(&mut book, &old, &new, &mut journal);
    started(&mut book, &new, &mut journal);
    book.commit_event(&new, &OrderEvent::SubmitTimeout, &mut journal)
        .unwrap();
    lineage.phase = AmendPhase::Unknown;
    book.commit_amend_lineage(&wire(), lineage, &mut journal)
        .unwrap();
    book.commit_ingested_execution(&fill("old-fill", Some(&old), 100, 80), &mut journal)
        .unwrap();
    assert_eq!(
        book.order(&new).unwrap().state,
        &OrderState::SubmissionClosed {
            reason: SubmissionCloseReason::ExhaustiveAbsent
        }
    );
    let before = book.order(&new).unwrap().ctx.clone();
    let cursor = book.order(&new).unwrap().cursor;
    book.commit_ingested_execution(&fill("late-child", Some(&new), 5, 90), &mut journal)
        .unwrap();
    assert_eq!(book.inventory().net(), 105);
    assert_eq!(book.order(&new).unwrap().ctx, &before);
    assert_eq!(book.order(&new).unwrap().cursor, cursor);
    assert!(!book.post_terminal_booked_after_normal_terminal(&new));
    rebuilt_same(&book, &journal, &[&old, &new]);
}

#[test]
fn child_partial_fill_retires_superseded_old_without_clearing_unproved_lineage() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let old = bound(&mut book, "old", &mut journal);
    let new = insert(&mut book, "new", 100);
    declare(&mut book, &old, &new, &mut journal);
    started(&mut book, &new, &mut journal);
    accept(&mut book, &new, &mut journal);
    book.commit_ingested_execution(&fill("child-fill", Some(&new), 20, 80), &mut journal)
        .unwrap();
    assert_eq!(book.order(&old).unwrap().state, &OrderState::Canceled);
    assert!(book
        .registry()
        .get(&ledger(), &wire())
        .unwrap()
        .amend_lineage
        .is_some());
    assert_eq!(book.inventory().net(), 20);
    rebuilt_with_source_positions(&book, &journal, &[&old, &new], true);
}

#[test]
fn late_after_normal_terminal_is_owner_bound_and_reconstructed_from_record_order() {
    for intervening_halt in [false, true] {
        let mut book = OrderBook::new(ledger());
        let mut journal = MemoryJournal::new();
        let cid = bound(&mut book, "a", &mut journal);
        book.commit_event(&cid, &OrderEvent::CancelRequested, &mut journal)
            .unwrap();
        book.commit_canceled_response(&cid, &wire(), false, false, None, &mut journal)
            .unwrap();
        assert_eq!(book.order(&cid).unwrap().state, &OrderState::Canceled);
        if intervening_halt {
            book.commit_ingested_execution(&fill("overfill", Some(&cid), 101, 80), &mut journal)
                .unwrap();
            assert!(matches!(
                book.order(&cid).unwrap().state,
                OrderState::Halted {
                    reason: HaltReason::OverFill { .. }
                }
            ));
            assert_eq!(book.order(&cid).unwrap().ctx.attributed_fill_qty, 0);
            assert_eq!(book.inventory().net(), 101);
            assert!(!book.post_terminal_booked_after_normal_terminal(&cid));
        } else {
            book.commit_ingested_execution(&fill("late-fill", Some(&cid), 40, 80), &mut journal)
                .unwrap();
            assert!(book.post_terminal_booked_after_normal_terminal(&cid));
            assert_eq!(book.inventory().net(), 40);
            rebuilt_same(&book, &journal, &[&cid]);
        }
        let receipt = book
            .commit_ingested_execution(&fill("later-fill", Some(&cid), 5, 90), &mut journal)
            .unwrap();
        assert!(matches!(
            receipt.outcome[0].order,
            omsrs::execution::OrderDisposition::LateBooked
        ));
        assert_eq!(
            book.order(&cid).unwrap().state,
            &OrderState::Halted {
                reason: HaltReason::PostTerminalFill
            }
        );
        assert_eq!(
            book.order(&cid).unwrap().ctx.attributed_fill_qty,
            if intervening_halt { 5 } else { 45 }
        );
        assert_eq!(
            book.inventory().net(),
            if intervening_halt { 106 } else { 45 }
        );
        assert_eq!(book.last_applied_execution_ts_ns(), 90);
        // Immediate pre-fill OverFill cannot create the bit. Once established
        // by a normal terminal, a subsequent non-normal halt cannot erase it.
        assert_eq!(
            book.post_terminal_booked_after_normal_terminal(&cid),
            !intervening_halt
        );
        rebuilt_same(&book, &journal, &[&cid]);
    }
}

#[test]
fn frozen_unknown_no_match_late_book_is_not_a_normal_terminal_provenance() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let cid = insert(&mut book, "a", 100);
    started(&mut book, &cid, &mut journal);
    book.commit_event(&cid, &OrderEvent::SubmitTimeout, &mut journal)
        .unwrap();
    assert!(matches!(
        book.commit_event(
            &cid,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: Vec::new()
            },
            &mut journal,
        ),
        Err(CommitError::SemanticHalt { .. })
    ));
    book.commit_ingested_execution(&fill("late-fill", Some(&cid), 40, 80), &mut journal)
        .unwrap();
    assert_eq!(
        book.order(&cid).unwrap().state,
        &OrderState::Halted {
            reason: HaltReason::PostTerminalFill
        }
    );
    assert!(!book.post_terminal_booked_after_normal_terminal(&cid));
    rebuilt_same(&book, &journal, &[&cid]);
}

#[test]
fn positive_delete_keeps_venue_residual_and_discharge_uses_real_inventory() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let cid = bound(&mut book, "a", &mut journal);
    prove(&mut book, &cid, &mut journal);
    assert_eq!(book.wire_total_at_proof(&wire()), Some(100));
    book.commit_event(&cid, &OrderEvent::CancelRequested, &mut journal)
        .unwrap();
    assert_eq!(book.wire_total_at_proof(&wire()), Some(100));
    book.commit_canceled_response(&cid, &wire(), true, false, Some(60), &mut journal)
        .unwrap();
    assert!(matches!(
        book.order(&cid).unwrap().state,
        OrderState::ReconcilePending { .. }
    ));
    assert_eq!(book.order(&cid).unwrap().ctx.fill_obligation, 40);
    assert_eq!(book.inventory().net(), 0);
    assert_eq!(book.positive_cancel_residual(&cid), Some(40));
    let bounds = CoverageBounds::from_members(book.base_coverage_members(-100)).unwrap();
    assert_eq!(bounds.lower(), 0);
    assert_eq!(bounds.upper_rows(), Some(40));
    assert_eq!(bounds.upper_wires(), Some(40));
    book.commit_ingested_execution(&fill("real-fill", Some(&cid), 20, 80), &mut journal)
        .unwrap();
    assert_eq!(book.positive_cancel_residual(&cid), Some(20));
    assert_eq!(book.inventory().net(), 20);
    let rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    assert_eq!(rebuilt.positive_cancel_residual(&cid), None);
    assert_eq!(rebuilt.inventory().net(), 20);
    assert_eq!(rebuilt.order(&cid).unwrap().ctx.fill_obligation, 40);
}

#[test]
fn positive_delete_without_proof_refuses_zero_synthesis_until_venue_terminal() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let cid = bound(&mut book, "a", &mut journal);
    book.commit_event(&cid, &OrderEvent::CancelRequested, &mut journal)
        .unwrap();
    book.commit_canceled_response(&cid, &wire(), true, false, Some(60), &mut journal)
        .unwrap();
    assert!(book.cancel_without_proof_member(&wire()));
    assert_eq!(book.deficit_member_wires(), vec![&wire()]);
    assert_eq!(book.positive_cancel_residual(&cid), None);
    assert!(matches!(
        book.order(&cid).unwrap().state,
        OrderState::ReconcilePending { .. }
    ));
    assert_eq!(book.inventory().net(), 0);
    book.commit_user_order(frame(&cid, UserOrderStatus::Canceled, 0), &mut journal)
        .unwrap();
    book.commit_terminal_frame_if_complete(&wire(), &mut journal)
        .unwrap();
    assert!(!book.cancel_without_proof_member(&wire()));
    assert_eq!(book.order(&cid).unwrap().state, &OrderState::Canceled);
}

#[test]
fn nonpositive_notfound_and_impossible_reduction_never_create_positive_provenance() {
    for (positive, not_found, reduced) in [
        (false, false, Some(60)),
        (true, true, Some(60)),
        (true, false, None),
        (true, false, Some(101)),
    ] {
        let mut book = OrderBook::new(ledger());
        let mut journal = MemoryJournal::new();
        let cid = bound(&mut book, "a", &mut journal);
        prove(&mut book, &cid, &mut journal);
        book.commit_event(&cid, &OrderEvent::CancelRequested, &mut journal)
            .unwrap();
        book.commit_canceled_response(&cid, &wire(), positive, not_found, reduced, &mut journal)
            .unwrap();
        assert_eq!(book.positive_cancel_residual(&cid), None);
        assert!(!book.cancel_without_proof_member(&wire()));
        assert_eq!(book.inventory().net(), 0);
    }
}

#[test]
fn known_deficit_refuses_canceled_empty_before_any_journal_op() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let cid = bound(&mut book, "a", &mut journal);
    book.commit_user_order(frame(&cid, UserOrderStatus::Resting, 40), &mut journal)
        .unwrap();
    let before = journal.records().len();
    let ctx = book.order(&cid).unwrap().ctx.clone();
    let state = book.order(&cid).unwrap().state.clone();
    let receipt = book
        .commit_canceled_empty_if_no_deficit(&wire(), &mut journal)
        .unwrap();
    assert_eq!(receipt.outcome, ObservationOutcome::NotCovered);
    assert!(receipt.durability.is_empty());
    assert!(receipt.effects.is_empty());
    assert_eq!(journal.records().len(), before);
    assert_eq!(book.order(&cid).unwrap().ctx, &ctx);
    assert_eq!(book.order(&cid).unwrap().state, &state);
    assert!(!book.stopped());
}

#[test]
fn fee_enrichment_in_composed_ingestion_never_rebooks_quantity_or_advances_clock() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let cid = bound(&mut book, "a", &mut journal);
    let mut evidence = fill("fill-a", Some(&cid), 40, 80);
    evidence.fee_cents = None;
    book.commit_ingested_execution(&evidence, &mut journal)
        .unwrap();
    assert_eq!(
        book.inventory()
            .get(&evidence.execution_id)
            .unwrap()
            .fee_cents,
        None
    );
    evidence.fee_cents = Some(3);
    let upgraded = book
        .commit_ingested_execution(&evidence, &mut journal)
        .unwrap();
    assert!(matches!(
        upgraded.outcome[0].inventory,
        InventoryDisposition::Upgraded
    ));
    assert_eq!(book.inventory().net(), 40);
    assert_eq!(
        book.inventory()
            .get(&evidence.execution_id)
            .unwrap()
            .fee_cents,
        Some(3)
    );
    assert_eq!(book.order(&cid).unwrap().ctx.attributed_fill_qty, 40);
    assert_eq!(book.last_applied_execution_ts_ns(), 80);
    rebuilt_same(&book, &journal, &[&cid]);
}

#[derive(Debug, Default)]
struct FailTerminalJournal {
    inner: MemoryJournal,
    fail_terminal: bool,
}
impl OmsJournal for FailTerminalJournal {
    fn apply_effects(&mut self, effects: &[Effect]) -> Result<DurabilityAck, JournalError> {
        let terminal_append = effects.iter().any(|effect| match effect {
            Effect::AppendFsync(JournalRecord::OrderTxn(order)) => order
                .core_records
                .iter()
                .any(|record| matches!(record, JournalRecord::OrderTerminal { .. })),
            _ => false,
        });
        if self.fail_terminal && terminal_append {
            return Err(JournalError::Io {
                operation: JournalOperation::AppendWrite,
                source: std::io::Error::other("terminal fault"),
            });
        }
        self.inner.apply_effects(effects)?;
        Ok(DurabilityAck::Volatile {
            records_end: self.inner.records().len() as u64,
        })
    }
    fn commit_now(&mut self) -> Result<DurabilityAck, JournalError> {
        self.inner.commit_now()?;
        Ok(DurabilityAck::Volatile {
            records_end: self.inner.records().len() as u64,
        })
    }
    fn fsync_errors(&self) -> u64 {
        0
    }
    fn truncated_bytes(&self) -> u64 {
        0
    }
    fn records(&self) -> &[JournalRecord] {
        self.inner.records()
    }
    fn other_effects(&self) -> &[Effect] {
        self.inner.other_effects()
    }
}
#[test]
fn b3_terminal_tail_error_preserves_committed_fill_prefix_without_global_stop() {
    let mut book = OrderBook::new(ledger());
    let mut journal = FailTerminalJournal::default();
    let cid = bound(&mut book, "a", &mut journal);
    book.commit_user_order(frame(&cid, UserOrderStatus::Canceled, 40), &mut journal)
        .unwrap();
    journal.fail_terminal = true;
    assert!(matches!(
        book.commit_ingested_execution(&fill("fill-a", Some(&cid), 40, 80), &mut journal),
        Err(CommitError::Journal(_))
    ));
    assert!(!book.stopped());
    assert_eq!(book.inventory().net(), 40);
    assert_eq!(book.order(&cid).unwrap().ctx.attributed_fill_qty, 40);
    assert!(!matches!(
        book.order(&cid).unwrap().state,
        OrderState::Canceled
    ));
    let rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    assert_eq!(rebuilt.inventory().net(), 40);
    assert_eq!(rebuilt.order(&cid).unwrap().ctx.attributed_fill_qty, 40);
}

#[test]
fn positive_current_cancel_retires_old_but_notfound_never_does() {
    for not_found in [false, true] {
        let mut book = OrderBook::new(ledger());
        let mut journal = MemoryJournal::new();
        let old = bound(&mut book, "old", &mut journal);
        let new = insert(&mut book, "new", 100);
        declare(&mut book, &old, &new, &mut journal);
        started(&mut book, &new, &mut journal);
        accept(&mut book, &new, &mut journal);
        book.commit_event(&new, &OrderEvent::CancelRequested, &mut journal)
            .unwrap();
        book.commit_canceled_response(&new, &wire(), true, not_found, Some(100), &mut journal)
            .unwrap();
        assert_eq!(
            matches!(book.order(&old).unwrap().state, OrderState::Canceled),
            !not_found
        );
        assert_eq!(book.cancel_without_proof_member(&wire()), !not_found);
        assert_eq!(book.inventory().net(), 0);
        rebuilt_with_source_positions(&book, &journal, &[&old, &new], true);
    }
}

#[test]
fn lineage_clear_returns_actual_previous_identity_and_absent_clear_appends_nothing() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let old = bound(&mut book, "old", &mut journal);
    let new = insert(&mut book, "new", 100);
    let lineage = declare(&mut book, &old, &new, &mut journal);
    let receipt = book
        .commit_clear_amend_lineage(&wire(), &mut journal)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.outcome, lineage);
    assert_eq!(receipt.durability.len(), 2);
    let before = journal.records().len();
    assert!(book
        .commit_clear_amend_lineage(&wire(), &mut journal)
        .unwrap()
        .is_none());
    assert_eq!(journal.records().len(), before);
    assert_eq!(book.inventory().net(), 0);
}

#[test]
fn private_seed_walk_uses_the_same_unknown_child_settlement_tail() {
    let mut book = OrderBook::new(ledger());
    let mut journal = MemoryJournal::new();
    let cid = insert(&mut book, "new", 100);
    started(&mut book, &cid, &mut journal);
    book.commit_event(&cid, &OrderEvent::SubmitTimeout, &mut journal)
        .unwrap();
    let mut seed = fill("seed-a", Some(&cid), 100, 80);
    seed.source = ExecutionSource::VenueSeed;
    assert!(book
        .commit_seed_walk(0, 80, true, [Some(seed)], &mut journal)
        .unwrap());
    assert_eq!(book.order(&cid).unwrap().state, &OrderState::Filled);
    assert_eq!(book.inventory().net(), 100);
    assert_eq!(book.last_applied_execution_ts_ns(), 0);
    assert!(book.seed_proven());
    rebuilt_same(&book, &journal, &[&cid]);
}

// Exact sealed S helper for the supervisor-authorized one-field contrast.
fn source_replay_late_flags(records: &[JournalRecord]) -> std::collections::BTreeSet<String> {
    fn terminal_in(o: &omsrs::execution::OrderTxnRecord) -> bool {
        o.core_records.iter().any(|r| {
            matches!(
                r,
                JournalRecord::OrderTerminal {
                    authority_complete: true,
                    ..
                }
            )
        })
    }
    let mut terminal = std::collections::BTreeSet::<String>::new();
    let mut out = std::collections::BTreeSet::<String>::new();
    for record in records {
        match record {
            JournalRecord::OrderTxn(o) => {
                if terminal_in(o) {
                    terminal.insert(o.client_order_id.0.clone());
                }
            }
            // Observation-driven terminal (exhaustive Canceled/Filled page): the member itself carries
            // its own post-state; the standalone OrderTerminal that follows has no owner.
            JournalRecord::ObservationTxn(t) => {
                for m in &t.members_after {
                    if matches!(
                        m.state_after,
                        OrderState::Filled | OrderState::Canceled | OrderState::Terminal
                    ) {
                        terminal.insert(m.client_order_id.0.clone());
                    }
                }
            }
            JournalRecord::ExecutionTxn(e) => {
                // #38 H1: keyed on the row the execution was COMMITTED to (order.client_order_id) + a genuinely
                // LateBooked disposition, never on the claimed CID (an unclaimed unique-wire fill has none; a
                // claimed CID on a wrong wire is NoRow and has no order part).
                if let Some(owner) = e.order.as_ref().map(|o| &o.client_order_id) {
                    if terminal.contains(&owner.0)
                        && e.executions.iter().any(|o| {
                            matches!(o.order, omsrs::execution::OrderDisposition::LateBooked)
                        })
                    {
                        out.insert(owner.0.clone());
                    }
                }
                if let Some(o) = e.order.as_ref() {
                    if terminal_in(o) {
                        terminal.insert(o.client_order_id.0.clone());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[test]
fn b4_supervisor_one_field_schedule_matches_native_live_and_both_replay_entries() {
    for intervening_halt in [false, true] {
        let mut book = OrderBook::new(ledger());
        let mut journal = MemoryJournal::new();
        let cid = bound(&mut book, "b4-provenance", &mut journal);
        book.commit_event(&cid, &OrderEvent::CancelRequested, &mut journal)
            .unwrap();
        book.commit_canceled_response(&cid, &wire(), false, false, None, &mut journal)
            .unwrap();
        if intervening_halt {
            book.commit_ingested_execution(&fill("overfill", Some(&cid), 101, 80), &mut journal)
                .unwrap();
        }
        book.commit_ingested_execution(&fill("late", Some(&cid), 5, 90), &mut journal)
            .unwrap();
        let expected = !intervening_halt;
        let fresh = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
        let mut inplace = OrderBook::new(ledger());
        inplace.rebuild_in_place(journal.records()).unwrap();
        assert_eq!(
            book.post_terminal_booked_after_normal_terminal(&cid),
            expected
        );
        assert_eq!(
            fresh.post_terminal_booked_after_normal_terminal(&cid),
            expected
        );
        assert_eq!(
            inplace.post_terminal_booked_after_normal_terminal(&cid),
            expected
        );
        assert!(
            source_replay_late_flags(journal.records()).contains(&cid.0),
            "S replay is true for the contrast and the known intervening-halt defect"
        );
        assert_eq!(fresh.inventory().view(), inplace.inventory().view());
        assert_eq!(
            fresh.registry().get(&ledger(), &wire()),
            inplace.registry().get(&ledger(), &wire())
        );
        let f = fresh.order(&cid).unwrap();
        let i = inplace.order(&cid).unwrap();
        assert_eq!((f.state, f.ctx, f.cursor), (i.state, i.ctx, i.cursor));
        assert_eq!(
            fresh.last_applied_execution_ts_ns(),
            inplace.last_applied_execution_ts_ns()
        );
    }
}
