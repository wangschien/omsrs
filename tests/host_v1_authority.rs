use omsrs::execution::{
    ExecutionEvidence, ExecutionOutcome, ExecutionScope, ExecutionSource, LedgerId,
};
use omsrs::host_v1::order_book::AuthorityReadToken;
use omsrs::host_v1::{
    CommitError, DurabilityAck, JournalError, JournalOperation, MemoryJournal, OmsJournal,
    OrderBook,
};
use omsrs::lifecycle::{
    AttemptId, BackfillOrderStatus, ClientOrderId, Effect, FillId, JournalRecord, OrderCtx,
    OrderEvent, OrderState, Side, VenueOrderId,
};
use omsrs::observation::{
    AmendLineage, AmendPhase, ObservationOutcome, OrderStateProofError, PageMeta, RoundError,
    RowEvidence, UserOrderEvidence, UserOrderStatus, WireMarker, WorkingView,
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
fn bound(book: &mut OrderBook, name: &str, journal: &mut dyn OmsJournal) -> ClientOrderId {
    let cid = ClientOrderId(name.into());
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
            attempt_id: AttemptId(name.into()),
        },
        journal,
    )
    .unwrap();
    book.commit_event(
        &cid,
        &OrderEvent::SubmitResponse {
            venue_order_id: wire(),
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
fn fill(cid: &ClientOrderId) -> ExecutionEvidence {
    ExecutionEvidence {
        scope: ledger().scope,
        market: ledger().market,
        execution_id: FillId("fill-a".into()),
        side: Side::BuyYes,
        qty: 40,
        price_cents: 40,
        ts_ns: 80,
        venue_order_id: Some(wire()),
        claimed_client_order_id: Some(cid.clone()),
        fee_cents: Some(0),
        source: ExecutionSource::WsFill,
    }
}
fn frame(cid: &ClientOrderId) -> UserOrderEvidence {
    UserOrderEvidence {
        wire: wire(),
        market: ledger().market,
        exchange_index: 1,
        subaccount_number: 1,
        sid: 2,
        side: Side::BuyYes,
        yes_price_e4: 400_000,
        client_order_id: cid.clone(),
        status: UserOrderStatus::Resting,
        filled: 0,
        remaining: 100,
        initial: 100,
        updated_ts_ms: 10,
        connection_generation: 1,
        payload_digest: "digest".into(),
    }
}
fn lineage(
    book: &mut OrderBook,
    old: &ClientOrderId,
    new: &ClientOrderId,
    journal: &mut dyn OmsJournal,
) -> AmendLineage {
    let l = AmendLineage {
        old_cid: old.clone(),
        new_cid: new.clone(),
        pre_generation: book.registry().marker(&ledger(), &wire()).0,
        phase: AmendPhase::HttpPending,
        declared_remaining: 100,
        applied_at_utc_ns: None,
    };
    book.commit_amend_lineage(&wire(), l.clone(), journal)
        .unwrap();
    l
}
fn applied(
    book: &mut OrderBook,
    journal: &mut dyn OmsJournal,
) -> (ClientOrderId, Vec<ExecutionOutcome>) {
    let old = bound(book, "z-old", journal);
    let new = ClientOrderId("a-new".into());
    let mut l = lineage(book, &old, &new, journal);
    let new = bound(book, "a-new", journal);
    book.commit_event(&old, &OrderEvent::AmendObserved, journal)
        .unwrap();
    l.phase = AmendPhase::RestDue;
    book.commit_amend_lineage(&wire(), l, journal).unwrap();
    let out = book
        .commit_routed_execution(&fill(&new), journal)
        .unwrap()
        .outcome;
    book.commit_wire_authority_hold(&wire(), journal).unwrap();
    (new, out)
}
fn row(cid: &ClientOrderId, token: &AuthorityReadToken) -> RowEvidence {
    RowEvidence {
        ledger: ledger(),
        wire: wire(),
        status: BackfillOrderStatus::Open,
        filled: 40,
        remaining: 60,
        client_order_id: Some(cid.clone()),
        marker: WireMarker(token.generation),
    }
}
fn complete() -> PageMeta {
    PageMeta {
        exhaustive: true,
        parse_failures: 0,
    }
}
fn terminal(book: &mut OrderBook, cid: &ClientOrderId, journal: &mut dyn OmsJournal) {
    book.commit_event(
        cid,
        &OrderEvent::UserOrderTerminalClaim {
            venue_order_id: wire(),
            filled: 0,
            executed: false,
        },
        journal,
    )
    .unwrap();
    assert_eq!(book.order(cid).unwrap().state, &OrderState::Canceled);
}
#[derive(Debug, Default)]
struct TraceJournal {
    inner: MemoryJournal,
    calls: Vec<&'static str>,
    fail_fence: bool,
}
impl OmsJournal for TraceJournal {
    fn apply_effects(&mut self, effects: &[Effect]) -> Result<DurabilityAck, JournalError> {
        self.calls.push("append_or_effects");
        self.inner.apply_effects(effects)?;
        Ok(DurabilityAck::Volatile {
            records_end: self.inner.records().len() as u64,
        })
    }
    fn commit_now(&mut self) -> Result<DurabilityAck, JournalError> {
        self.calls.push("fence");
        if self.fail_fence {
            return Err(JournalError::Io {
                operation: JournalOperation::ExplicitFence,
                source: std::io::Error::other("test fence fault"),
            });
        }
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
fn token_tracks_every_source_snapshot_component_and_missing_wire_is_absent() {
    let mut book = OrderBook::new(ledger());
    let mut journal = TraceJournal::default();
    assert_eq!(book.authority_read_token(&wire()), None);
    let cid = bound(&mut book, "a", &mut journal);
    let token = book.authority_read_token(&wire()).unwrap();
    assert_eq!(token.current_cid, Some(cid.clone()));
    assert!(book.authority_token_still_current(&token));
    let mut altered = token.clone();
    altered.generation += 1;
    assert!(!book.authority_token_still_current(&altered));
    altered = token.clone();
    altered.inventory_generation += 1;
    assert!(!book.authority_token_still_current(&altered));
    altered = token.clone();
    altered.authority_hold_revision += 1;
    assert!(!book.authority_token_still_current(&altered));
    altered = token.clone();
    altered.current_cid = Some(ClientOrderId("wrong".into()));
    assert!(!book.authority_token_still_current(&altered));
    altered = token.clone();
    altered.wire = VenueOrderId("other".into());
    assert!(!book.authority_token_still_current(&altered));
    assert_eq!(book.inventory().net(), 0);
}

#[test]
fn newer_hold_has_a_new_revision_and_rebuild_preserves_token_and_hold() {
    let mut book = OrderBook::new(ledger());
    let mut journal = TraceJournal::default();
    bound(&mut book, "a", &mut journal);
    let before = book.authority_read_token(&wire()).unwrap();
    journal.calls.clear();
    let receipt = book
        .commit_wire_authority_hold(&wire(), &mut journal)
        .unwrap();
    assert_eq!(journal.calls, ["append_or_effects", "fence"]);
    assert_eq!(receipt.durability.len(), 2);
    let first = book.authority_read_token(&wire()).unwrap();
    assert!(!book.authority_token_still_current(&before));
    assert!(first.authority_hold_revision > 0);
    book.commit_wire_authority_hold(&wire(), &mut journal)
        .unwrap();
    let second = book.authority_read_token(&wire()).unwrap();
    assert!(second.authority_hold_revision > first.authority_hold_revision);
    assert!(!book.authority_token_still_current(&first));
    let rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    assert_eq!(rebuilt.authority_read_token(&wire()), Some(second));
    assert_eq!(
        rebuilt.registry().get(&ledger(), &wire()),
        book.registry().get(&ledger(), &wire())
    );
    assert_eq!(book.working_view(&wire()), WorkingView::Unresolved);
}

#[test]
fn actual_own_action_inventory_and_cid_changes_invalidate_old_tokens() {
    let mut book = OrderBook::new(ledger());
    let mut journal = TraceJournal::default();
    let cid = bound(&mut book, "a", &mut journal);
    let before = book.authority_read_token(&wire()).unwrap();
    book.commit_event(
        &cid,
        &OrderEvent::DecreaseObserved {
            remaining: 60,
            ts_ms: 1,
        },
        &mut journal,
    )
    .unwrap();
    assert!(!book.authority_token_still_current(&before));
    let before = book.authority_read_token(&wire()).unwrap();
    let mut outside = fill(&cid);
    outside.execution_id = FillId("outside".into());
    outside.venue_order_id = Some(VenueOrderId("other".into()));
    outside.claimed_client_order_id = None;
    book.commit_routed_execution(&outside, &mut journal)
        .unwrap();
    assert!(!book.authority_token_still_current(&before));
    let before = book.authority_read_token(&wire()).unwrap();
    bound(&mut book, "new", &mut journal);
    assert!(!book.authority_token_still_current(&before));
}

#[test]
fn held_terminal_fallback_uses_last_venue_bind_not_cid_sort_and_excludes_http_pending() {
    for pending in [false, true] {
        let mut book = OrderBook::new(ledger());
        let mut journal = TraceJournal::default();
        let old = bound(&mut book, "z-old", &mut journal);
        let new = ClientOrderId("a-new".into());
        if pending {
            lineage(&mut book, &old, &new, &mut journal);
        }
        let new = bound(&mut book, "a-new", &mut journal);
        terminal(&mut book, &old, &mut journal);
        terminal(&mut book, &new, &mut journal);
        assert_eq!(book.authority_terminal_identity(&wire()), None);
        book.commit_wire_authority_hold(&wire(), &mut journal)
            .unwrap();
        assert_eq!(
            book.authority_terminal_identity(&wire()),
            if pending { None } else { Some(&new) }
        );
        assert_eq!(
            book.authority_read_token(&wire()).unwrap().current_cid,
            if pending { None } else { Some(new) }
        );
    }
}

#[test]
fn held_proved_terminal_identity_does_not_regain_resting_authority() {
    let mut book = OrderBook::new(ledger());
    let mut journal = TraceJournal::default();
    let cid = bound(&mut book, "a", &mut journal);
    book.commit_user_order(frame(&cid), &mut journal).unwrap();
    book.commit_resting_proof(&wire(), &mut journal).unwrap();
    book.commit_wire_authority_hold(&wire(), &mut journal)
        .unwrap();
    terminal(&mut book, &cid, &mut journal);
    assert_eq!(
        book.authority_read_token(&wire()).unwrap().current_cid,
        Some(cid)
    );
    assert_eq!(book.authority_terminal_identity(&wire()), None);
    assert!(
        book.registry()
            .get(&ledger(), &wire())
            .unwrap()
            .authority_hold
    );
    assert_eq!(book.working_view(&wire()), WorkingView::Unresolved);
    assert!(book.commit_resting_proof(&wire(), &mut journal).is_err());
}

#[test]
fn stale_token_foreign_row_wrong_wire_or_cid_rejects_before_every_journal_op() {
    for case in 0..4 {
        let mut book = OrderBook::new(ledger());
        let mut journal = TraceJournal::default();
        let (cid, out) = applied(&mut book, &mut journal);
        let mut token = book.authority_read_token(&wire()).unwrap();
        let mut response = row(&cid, &token);
        match case {
            0 => {
                book.commit_wire_authority_hold(&wire(), &mut journal)
                    .unwrap();
            }
            1 => response.ledger.scope = ExecutionScope("foreign".into()),
            2 => response.wire = VenueOrderId("foreign-wire".into()),
            3 => response.client_order_id = Some(ClientOrderId("wrong-cid".into())),
            _ => unreachable!(),
        }
        let before = journal.records().len();
        let registry = format!("{:?}", book.registry());
        journal.calls.clear();
        let result =
            book.commit_rest_order_page(response, 100, &token, &out, complete(), &mut journal);
        match case {
            1 => assert!(matches!(
                result,
                Err(CommitError::Observation(RoundError::ForeignRow))
            )),
            3 => assert!(matches!(
                result,
                Err(CommitError::OrderStateProof(
                    OrderStateProofError::WrongCurrent
                ))
            )),
            0 | 2 => assert!(matches!(
                result,
                Err(CommitError::OrderStateProof(
                    OrderStateProofError::GenerationChanged
                ))
            )),
            _ => unreachable!(),
        }
        assert!(journal.calls.is_empty());
        assert_eq!(journal.records().len(), before);
        assert_eq!(format!("{:?}", book.registry()), registry);
        assert!(!book.stopped());
        // Keep a separate fully current value available to callers after retry.
        token = book.authority_read_token(&wire()).unwrap();
        assert!(book.authority_token_still_current(&token));
    }
}

#[test]
fn complete_identity_bound_rest_page_commits_separate_proof_without_ws_frame() {
    let mut book = OrderBook::new(ledger());
    let mut journal = TraceJournal::default();
    let (cid, out) = applied(&mut book, &mut journal);
    let token = book.authority_read_token(&wire()).unwrap();
    let before = journal.records().len();
    journal.calls.clear();
    let receipt = book
        .commit_rest_order_page(
            row(&cid, &token),
            100,
            &token,
            &out,
            complete(),
            &mut journal,
        )
        .unwrap();
    assert_eq!(
        receipt.outcome,
        ObservationOutcome::Consistent {
            remaining_fresh: false
        }
    );
    let state = book.registry().get(&ledger(), &wire()).unwrap();
    assert!(!state.authority_hold);
    assert!(state.amend_lineage.is_none());
    assert_eq!(state.proved_current_cid, Some(cid.clone()));
    assert!(state.latest_user_order.is_none());
    assert_eq!(book.working_view(&wire()), WorkingView::Known(60));
    assert_eq!(book.inventory().net(), 40);
    assert_eq!(book.last_applied_execution_ts_ns(), 80);
    assert!(matches!(
        journal.records().last(),
        Some(JournalRecord::RestOrderProofTxn(_))
    ));
    assert!(journal.records()[before..]
        .iter()
        .all(|record| !matches!(record, JournalRecord::UserOrderReceived { .. })));
    assert_eq!(journal.calls.last(), Some(&"fence"));
    let rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    assert_eq!(rebuilt.registry().get(&ledger(), &wire()), Some(state));
    assert_eq!(
        rebuilt.authority_read_token(&wire()),
        book.authority_read_token(&wire())
    );
}

#[test]
fn incomplete_or_parse_failed_page_does_not_create_rest_proof_or_clear_hold() {
    for page in [
        PageMeta {
            exhaustive: false,
            parse_failures: 0,
        },
        PageMeta {
            exhaustive: true,
            parse_failures: 1,
        },
    ] {
        let mut book = OrderBook::new(ledger());
        let mut journal = TraceJournal::default();
        let (cid, out) = applied(&mut book, &mut journal);
        let token = book.authority_read_token(&wire()).unwrap();
        let before = journal.records().len();
        book.commit_rest_order_page(row(&cid, &token), 100, &token, &out, page, &mut journal)
            .unwrap();
        assert!(!journal.records()[before..]
            .iter()
            .any(|record| matches!(record, JournalRecord::RestOrderProofTxn(_))));
        assert!(
            book.registry()
                .get(&ledger(), &wire())
                .unwrap()
                .authority_hold
        );
        assert!(book
            .registry()
            .get(&ledger(), &wire())
            .unwrap()
            .amend_lineage
            .is_some());
        assert_eq!(book.working_view(&wire()), WorkingView::Unresolved);
    }
}

#[test]
fn wrong_initial_keeps_committed_observation_but_never_prepares_a_rest_proof() {
    let mut book = OrderBook::new(ledger());
    let mut journal = TraceJournal::default();
    let (cid, out) = applied(&mut book, &mut journal);
    let token = book.authority_read_token(&wire()).unwrap();
    let before = journal.records().len();
    assert!(matches!(
        book.commit_rest_order_page(
            row(&cid, &token),
            99,
            &token,
            &out,
            complete(),
            &mut journal
        ),
        Err(CommitError::OrderStateProof(
            OrderStateProofError::QuantityMismatch
        ))
    ));
    assert!(journal.records().len() > before);
    assert!(journal.records()[before..]
        .iter()
        .any(|record| matches!(record, JournalRecord::ObservationTxn(_))));
    assert!(!journal.records()[before..]
        .iter()
        .any(|record| matches!(record, JournalRecord::RestOrderProofTxn(_))));
    assert!(
        book.registry()
            .get(&ledger(), &wire())
            .unwrap()
            .authority_hold
    );
    assert!(!book.stopped());
    assert_eq!(book.inventory().net(), 40);
}

#[test]
fn rest_proof_fence_failure_stops_before_applying_proof_and_preserves_observation_prefix() {
    let mut book = OrderBook::new(ledger());
    let mut journal = TraceJournal::default();
    let (cid, out) = applied(&mut book, &mut journal);
    let token = book.authority_read_token(&wire()).unwrap();
    journal.fail_fence = true;
    assert!(matches!(
        book.commit_rest_order_page(
            row(&cid, &token),
            100,
            &token,
            &out,
            complete(),
            &mut journal
        ),
        Err(CommitError::Journal(_))
    ));
    assert!(book.stopped());
    assert!(
        book.registry()
            .get(&ledger(), &wire())
            .unwrap()
            .authority_hold
    );
    assert_eq!(book.working_view(&wire()), WorkingView::Unresolved);
    assert_eq!(book.inventory().net(), 40);
    let before = journal.calls.len();
    assert!(matches!(
        book.commit_wire_authority_hold(&wire(), &mut journal),
        Err(CommitError::Stopped)
    ));
    assert_eq!(journal.calls.len(), before);
}

#[test]
fn hold_fence_failure_never_folds_a_hold_or_continues_on_the_owner() {
    let mut book = OrderBook::new(ledger());
    let mut journal = TraceJournal::default();
    bound(&mut book, "a", &mut journal);
    let before = book.authority_read_token(&wire()).unwrap();
    journal.fail_fence = true;
    assert!(matches!(
        book.commit_wire_authority_hold(&wire(), &mut journal),
        Err(CommitError::Journal(_))
    ));
    assert!(book.stopped());
    assert_eq!(book.authority_read_token(&wire()), Some(before));
    assert!(
        !book
            .registry()
            .get(&ledger(), &wire())
            .unwrap()
            .authority_hold
    );
}

#[test]
fn other_wire_page_outcome_never_supplies_missing_held_execution_coverage() {
    let mut book = OrderBook::new(ledger());
    let mut journal = TraceJournal::default();
    let (cid, mut out) = applied(&mut book, &mut journal);
    let token = book.authority_read_token(&wire()).unwrap();
    out[0].evidence.venue_order_id = Some(VenueOrderId("other-wire".into()));
    let result = book
        .commit_rest_order_page(
            row(&cid, &token),
            100,
            &token,
            &out,
            complete(),
            &mut journal,
        )
        .unwrap();
    assert_ne!(
        result.outcome,
        ObservationOutcome::Consistent {
            remaining_fresh: false
        }
    );
    assert!(
        book.registry()
            .get(&ledger(), &wire())
            .unwrap()
            .authority_hold
    );
    assert!(!journal
        .records()
        .iter()
        .any(|record| matches!(record, JournalRecord::RestOrderProofTxn(_))));
}

#[test]
fn foreign_ledger_page_outcome_rejects_before_every_journal_operation() {
    for foreign_scope in [true, false] {
        let mut book = OrderBook::new(ledger());
        let mut journal = TraceJournal::default();
        let (cid, mut out) = applied(&mut book, &mut journal);
        let token = book.authority_read_token(&wire()).unwrap();
        if foreign_scope {
            out[0].evidence.scope = ExecutionScope("foreign".into());
        } else {
            out[0].evidence.market = "foreign-market".into();
        }
        let before = journal.records().len();
        journal.calls.clear();
        assert!(matches!(
            book.commit_rest_order_page(
                row(&cid, &token),
                100,
                &token,
                &out,
                complete(),
                &mut journal
            ),
            Err(CommitError::Observation(RoundError::ForeignRow))
        ));
        assert!(journal.calls.is_empty());
        assert_eq!(journal.records().len(), before);
        assert!(
            book.registry()
                .get(&ledger(), &wire())
                .unwrap()
                .authority_hold
        );
        assert!(book.authority_token_still_current(&token));
        assert!(!book.stopped());
    }
}
