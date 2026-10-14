use omsrs::execution::{
    ExecutionEvidence, ExecutionOutcome, ExecutionScope, ExecutionSource, InventoryDisposition,
    OrderDisposition,
};
use omsrs::host_v1::private_fill::{
    finish_private_fill_ingestion, PrivateFillBook, PrivateFillBookkeeping,
};
use omsrs::lifecycle::{FillId, HaltReason, OrderState, Side};

fn outcome(order: OrderDisposition) -> ExecutionOutcome {
    ExecutionOutcome {
        evidence: ExecutionEvidence {
            scope: ExecutionScope("account-a".into()),
            market: "market-a".into(),
            execution_id: FillId("fill-a".into()),
            side: Side::BuyYes,
            qty: 40,
            price_cents: 40,
            ts_ns: 70,
            venue_order_id: None,
            claimed_client_order_id: None,
            fee_cents: None,
            source: ExecutionSource::WsFill,
        },
        order,
        inventory: InventoryDisposition::Unchanged,
        entry_after: None,
        conflict: None,
    }
}
fn success(
    ids: &mut PrivateFillBookkeeping,
    fee: Option<u64>,
    outcomes: Vec<ExecutionOutcome>,
) -> PrivateFillBook {
    finish_private_fill_ingestion::<()>(ids, "fill-a", fee, Ok(outcomes), None, false).unwrap()
}
fn pending(ids: &mut PrivateFillBookkeeping) {
    assert_eq!(
        success(ids, None, vec![outcome(OrderDisposition::Attributed)]),
        PrivateFillBook::Booked
    );
}

#[test]
fn recovery_seed_marks_only_seen_ids_without_inventing_pending_fees() {
    let mut ids = PrivateFillBookkeeping::default();
    assert!(!ids.fill_id_seen("a"));
    assert!(!ids.has_pending_fee_enrichment());
    ids.seed_seen_fill_ids(["b", "a", "a", ""]);
    for id in ["a", "b", ""] {
        assert!(ids.fill_id_seen(id));
    }
    assert!(!ids.fill_id_seen("c"));
    assert!(ids.pending_fee_fill_ids().is_empty());
}

#[test]
fn normal_known_zero_fee_removes_pending_without_forgetting_seen_identity() {
    let mut ids = PrivateFillBookkeeping::default();
    pending(&mut ids);
    assert!(ids.pending_fee_fill_ids().contains("fill-a"));
    assert!(ids.fill_id_seen("fill-a"));
    assert_eq!(
        success(&mut ids, Some(0), vec![outcome(OrderDisposition::Enriched)]),
        PrivateFillBook::Booked
    );
    assert!(ids.pending_fee_fill_ids().is_empty());
    assert!(ids.fill_id_seen("fill-a"));
}

#[test]
fn no_row_marks_seen_without_creating_or_clearing_pending_even_with_known_fee() {
    let mut ids = PrivateFillBookkeeping::default();
    assert_eq!(
        success(&mut ids, None, vec![outcome(OrderDisposition::NoRow)]),
        PrivateFillBook::NoRow
    );
    assert!(ids.fill_id_seen("fill-a"));
    assert!(ids.pending_fee_fill_ids().is_empty());
    pending(&mut ids);
    assert_eq!(
        success(&mut ids, Some(5), vec![outcome(OrderDisposition::NoRow)]),
        PrivateFillBook::NoRow
    );
    assert!(ids.pending_fee_fill_ids().contains("fill-a"));
}

#[test]
fn late_outcome_precedes_no_row_and_keeps_existing_pending_on_known_fee() {
    let mut ids = PrivateFillBookkeeping::default();
    pending(&mut ids);
    for outcomes in [
        vec![
            outcome(OrderDisposition::NoRow),
            outcome(OrderDisposition::LateBooked),
        ],
        vec![
            outcome(OrderDisposition::LateBooked),
            outcome(OrderDisposition::NoRow),
        ],
    ] {
        assert_eq!(
            success(&mut ids, Some(5), outcomes),
            PrivateFillBook::BookedLatePostTerminal
        );
        assert!(ids.pending_fee_fill_ids().contains("fill-a"));
    }
    let mut fresh = PrivateFillBookkeeping::default();
    assert_eq!(
        success(
            &mut fresh,
            None,
            vec![outcome(OrderDisposition::LateBooked)]
        ),
        PrivateFillBook::BookedLatePostTerminal
    );
    assert!(fresh.fill_id_seen("fill-a"));
    assert!(fresh.pending_fee_fill_ids().contains("fill-a"));
}

#[test]
fn empty_success_retains_source_booked_fallback_without_new_disposition_rules() {
    let mut ids = PrivateFillBookkeeping::default();
    assert_eq!(success(&mut ids, None, vec![]), PrivateFillBook::Booked);
    assert!(ids.pending_fee_fill_ids().contains("fill-a"));
    assert_eq!(success(&mut ids, Some(7), vec![]), PrivateFillBook::Booked);
    assert!(ids.pending_fee_fill_ids().is_empty());
}

#[test]
fn error_requires_both_post_terminal_label_and_journaled_fill_and_keeps_error_identity() {
    struct ErrorToken;
    let token = ErrorToken;
    let post_terminal = OrderState::Halted {
        reason: HaltReason::PostTerminalFill,
    };
    let other_halt = OrderState::Halted {
        reason: HaltReason::UnknownNoMatch,
    };
    for (state, journaled) in [
        (None, true),
        (Some(&other_halt), true),
        (Some(&post_terminal), false),
    ] {
        let mut ids = PrivateFillBookkeeping::default();
        ids.seed_seen_fill_ids(["prior"]);
        let result =
            finish_private_fill_ingestion(&mut ids, "failed", None, Err(&token), state, journaled);
        match result {
            Err(error) => assert!(std::ptr::eq(error, &token)),
            Ok(_) => panic!("unbooked or unrelated error was hidden"),
        }
        assert!(!ids.fill_id_seen("failed"));
        assert!(ids.fill_id_seen("prior"));
        assert!(ids.pending_fee_fill_ids().is_empty());
    }
}

#[test]
fn committed_post_terminal_error_is_booked_and_known_fee_keeps_previous_pending() {
    let state = OrderState::Halted {
        reason: HaltReason::PostTerminalFill,
    };
    let mut ids = PrivateFillBookkeeping::default();
    for fee in [None, Some(5)] {
        let result = finish_private_fill_ingestion(
            &mut ids,
            "fill-a",
            fee,
            Err("original error"),
            Some(&state),
            true,
        );
        assert_eq!(result, Ok(PrivateFillBook::BookedLatePostTerminal));
        assert!(ids.fill_id_seen("fill-a"));
        assert!(ids.pending_fee_fill_ids().contains("fill-a"));
    }
}

#[test]
fn success_uses_actual_outcomes_without_consulting_error_only_facts() {
    let post_terminal = OrderState::Halted {
        reason: HaltReason::PostTerminalFill,
    };
    let mut ids = PrivateFillBookkeeping::default();
    let result = finish_private_fill_ingestion::<()>(
        &mut ids,
        "fill-a",
        Some(0),
        Ok(vec![outcome(OrderDisposition::Attributed)]),
        Some(&post_terminal),
        true,
    );
    assert_eq!(result, Ok(PrivateFillBook::Booked));
    assert!(ids.fill_id_seen("fill-a"));
    assert!(ids.pending_fee_fill_ids().is_empty());
}
