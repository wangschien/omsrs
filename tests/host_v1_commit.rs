use omsrs::execution::{ExecutionEvidence, ExecutionScope, ExecutionSource, LedgerId};
use omsrs::host_v1::{
    CommitError, DurabilityAck, HostError, JournalError, JournalOperation, MemoryJournal,
    OmsJournal, OrderBook, ReplayStage,
};
use omsrs::lifecycle::{
    AttemptId, BackfillOrderStatus, ClientOrderId, Effect, FillId, JournalRecord, OrderCtx,
    OrderEvent, OrderState, Side, SubmissionCloseReason, VenueOrderId,
};
use omsrs::observation::{
    MemberRole, ObservationRound, PageMeta, RoundError, RowEvidence, UserOrderEvidence,
    UserOrderStatus, WorkingView,
};

#[test]
fn foreign_ledger_empty_member_observation_is_rejected_before_every_journal_operation() {
    let mut book = OrderBook::new(ledger());
    let mut journal = TraceJournal::default();
    let mut foreign = ledger();
    foreign.scope = ExecutionScope("account-b".into());
    let wire = VenueOrderId("wire-b".into());
    let registry_before = format!("{:?}", book.registry());
    let mut round = ObservationRound::begin(foreign.clone(), wire.clone());
    round.page(PageMeta {
        exhaustive: true,
        parse_failures: 0,
    });
    let row = RowEvidence {
        ledger: foreign.clone(),
        wire: wire.clone(),
        status: BackfillOrderStatus::Open,
        filled: 0,
        remaining: 100,
        client_order_id: None,
        marker: book.registry().marker(&foreign, &wire),
    };
    assert!(matches!(
        book.commit_observation(round, row, &mut journal),
        Err(CommitError::Observation(RoundError::ForeignRow))
    ));
    assert!(journal.calls.is_empty());
    assert!(journal.records().is_empty());
    assert!(journal.other_effects().is_empty());
    assert_eq!(format!("{:?}", book.registry()), registry_before);
    assert!(!book.stopped());
}

#[test]
fn terminal_observation_folds_membership_and_rebuilds_the_entire_registry_wire() {
    let (mut book, client, mut journal) = initial();
    accepted(&mut book, &client, "wire-a", &mut journal);
    let wire = VenueOrderId("wire-a".into());
    let mut round = ObservationRound::begin(ledger(), wire.clone());
    round.page(PageMeta {
        exhaustive: true,
        parse_failures: 0,
    });
    let row = RowEvidence {
        ledger: ledger(),
        wire: wire.clone(),
        status: BackfillOrderStatus::Canceled,
        filled: 0,
        remaining: 0,
        client_order_id: Some(client.clone()),
        marker: book.registry().marker(&ledger(), &wire),
    };
    book.commit_observation(round, row, &mut journal).unwrap();
    assert_eq!(book.order(&client).unwrap().state, &OrderState::Canceled);
    assert_eq!(
        book.registry().get(&ledger(), &wire).unwrap().members[0].role,
        MemberRole::Terminal
    );
    let rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    assert_eq!(
        rebuilt.registry().get(&ledger(), &wire),
        book.registry().get(&ledger(), &wire)
    );
}

#[test]
fn halt_fence_failure_is_best_effort_and_diagnostic_without_promoting_durability() {
    let (mut book, client, mut journal) = initial();
    started(&mut book, &client, &mut journal);
    book.commit_event(&client, &OrderEvent::SubmitTimeout, &mut journal)
        .unwrap();
    journal.calls.clear();
    journal.fail_fences = true;
    let event = OrderEvent::UnknownBackfillResult {
        exhaustive: true,
        matched: vec![],
    };
    let error = book
        .commit_event(&client, &event, &mut journal)
        .unwrap_err();
    let message = match error {
        CommitError::SemanticHalt { event, reason } => format!("oms halt on {event:?}: {reason:?}"),
        other => panic!("wrong source Halt result {other:?}"),
    };
    assert_eq!(
        message,
        "oms halt on UnknownBackfillResult { exhaustive: true, matched: [] }: UnknownNoMatch"
    );
    assert_eq!(journal.calls.last(), Some(&"fence"));
    assert_eq!(
        book.order(&client).unwrap().state,
        &OrderState::UnknownNoMatch
    );
    assert!(!book.stopped());
    assert_eq!(book.take_fence_errors().len(), 1);
    assert!(book.take_fence_errors().is_empty());
    let rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    assert_eq!(
        rebuilt.order(&client).unwrap().ctx,
        book.order(&client).unwrap().ctx
    );
    assert_eq!(
        rebuilt.order(&client).unwrap().state,
        book.order(&client).unwrap().state
    );
}

#[test]
fn b3_seed_storage_error_leaves_proof_false_without_inventing_a_global_stop() {
    let (mut book, _, mut journal) = initial();
    assert!(book.commit_seed_walk(0, 1, true, [], &mut journal).unwrap());
    let mut seed = evidence(Some(0));
    seed.source = ExecutionSource::VenueSeed;
    journal.fail_at = Some(1);
    assert!(matches!(
        book.commit_seed_walk(0, 1, true, [Some(seed)], &mut journal),
        Err(CommitError::Journal(_))
    ));
    assert!(!book.seed_proven());
    assert!(!book.stopped());
    assert!(matches!(
        book.host_readiness(false),
        omsrs::observation::Readiness::Unresolved { .. }
    ));
    journal.fail_at = None;
    assert!(book.commit_seed_walk(0, 1, true, [], &mut journal).unwrap());
}

#[test]
fn late_execution_halt_fence_failure_still_books_the_canonical_batch() {
    use omsrs::execution::{BatchOutcome, OrderDisposition, OrderTxnRecord};
    let mut ctx = context("client-a");
    let client = ctx.client_order_id.clone();
    let wire = VenueOrderId("wire-a".into());
    ctx.venue_order_id = Some(wire.clone());
    ctx.latch_authority_complete();
    let initial = JournalRecord::OrderTxn(Box::new(OrderTxnRecord {
        ledger: ledger(),
        client_order_id: client.clone(),
        seq: 1,
        event: "terminal-fixture".into(),
        outcome: BatchOutcome::Accept,
        core_records: vec![JournalRecord::VenueBoundCid {
            client_order_id: client.clone(),
            venue_order_id: wire,
        }],
        state_after: OrderState::Canceled,
        ctx_after: ctx,
    }));
    let mut book =
        OrderBook::from_committed_records(ledger(), std::slice::from_ref(&initial)).unwrap();
    let mut journal = TraceJournal::default();
    journal.inner.records.push(initial);
    journal.fail_fences = true;
    let fill = evidence(Some(0));
    let event = OrderEvent::Fill {
        fill_id: fill.execution_id.clone(),
        qty: fill.qty,
        price_cents: fill.price_cents,
        ts_ns: fill.ts_ns,
        venue_order_id: fill.venue_order_id.clone(),
        fee_cents: fill.fee_cents,
    };
    let receipt = book
        .commit_execution_batch(&client, &event, &[fill], &mut journal)
        .unwrap();
    assert!(matches!(
        receipt.outcome[0].order,
        OrderDisposition::LateBooked
    ));
    assert_eq!(journal.calls.last(), Some(&"fence"));
    assert_eq!(book.inventory().net(), 40);
    assert!(!book.stopped());
    assert_eq!(book.take_fence_errors().len(), 1);
    let rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    assert_eq!(
        rebuilt.order(&client).unwrap().ctx,
        book.order(&client).unwrap().ctx
    );
    assert_eq!(rebuilt.inventory().view(), book.inventory().view());
}

fn ledger() -> LedgerId {
    LedgerId {
        scope: ExecutionScope("account-a".into()),
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
fn initial() -> (OrderBook, ClientOrderId, TraceJournal) {
    let mut book = OrderBook::new(ledger());
    let ctx = context("client-a");
    let client = ctx.client_order_id.clone();
    book.insert_initial(ctx).unwrap();
    (book, client, TraceJournal::default())
}

#[derive(Debug, Default)]
struct TraceJournal {
    inner: MemoryJournal,
    calls: Vec<&'static str>,
    fail_at: Option<usize>,
    fail_fences: bool,
}
impl TraceJournal {
    fn step(&mut self, call: &'static str) -> Result<(), JournalError> {
        self.calls.push(call);
        if self.fail_at == Some(self.calls.len()) || (self.fail_fences && call == "fence") {
            Err(JournalError::Io {
                operation: if call == "fence" {
                    JournalOperation::ExplicitFence
                } else {
                    JournalOperation::AppendWrite
                },
                source: std::io::Error::new(std::io::ErrorKind::Other, "injected"),
            })
        } else {
            Ok(())
        }
    }
}
impl OmsJournal for TraceJournal {
    fn apply_effects(&mut self, effects: &[Effect]) -> Result<DurabilityAck, JournalError> {
        self.step(if matches!(effects.first(), Some(Effect::AppendFsync(_))) {
            "append"
        } else {
            "effects"
        })?;
        self.inner.apply_effects(effects)
    }
    fn commit_now(&mut self) -> Result<DurabilityAck, JournalError> {
        self.step("fence")?;
        self.inner.commit_now()
    }
    fn fsync_errors(&self) -> u64 {
        self.inner.fsync_errors()
    }
    fn truncated_bytes(&self) -> u64 {
        self.inner.truncated_bytes()
    }
    fn records(&self) -> &[JournalRecord] {
        self.inner.records()
    }
    fn other_effects(&self) -> &[Effect] {
        self.inner.other_effects()
    }
}

fn started(book: &mut OrderBook, client: &ClientOrderId, journal: &mut TraceJournal) {
    book.commit_event(client, &OrderEvent::PrepareSubmit, journal)
        .unwrap();
    book.commit_event(
        client,
        &OrderEvent::StartSubmit {
            attempt_id: AttemptId("attempt-a".into()),
        },
        journal,
    )
    .unwrap();
}
fn accepted(book: &mut OrderBook, client: &ClientOrderId, wire: &str, journal: &mut TraceJournal) {
    started(book, client, journal);
    book.commit_event(
        client,
        &OrderEvent::SubmitResponse {
            venue_order_id: VenueOrderId(wire.into()),
            fill_count: 0,
            remaining_count: 100,
            avg_price_cents: None,
            fee_cents: None,
            snapshot_boundary: None,
        },
        journal,
    )
    .unwrap();
}
fn evidence(fee: Option<u64>) -> ExecutionEvidence {
    ExecutionEvidence {
        scope: ledger().scope,
        market: ledger().market,
        execution_id: FillId("fill-a".into()),
        side: Side::BuyYes,
        qty: 40,
        price_cents: 40,
        ts_ns: 1,
        venue_order_id: Some(VenueOrderId("wire-a".into())),
        claimed_client_order_id: None,
        fee_cents: fee,
        source: ExecutionSource::WsFill,
    }
}

#[test]
fn event_commit_preserves_effect_order_and_rebuilds_the_same_canonical_row() {
    let (mut book, client, mut journal) = initial();
    started(&mut book, &client, &mut journal);
    journal.calls.clear();
    let expected = book
        .prepare_event(&client, &OrderEvent::SubmitTimeout)
        .unwrap();
    let receipt = book
        .commit_event(&client, &OrderEvent::SubmitTimeout, &mut journal)
        .unwrap();
    assert_eq!(journal.calls, ["append", "effects"]);
    assert_eq!(receipt.effects, expected.effects);
    assert_eq!(journal.other_effects(), receipt.effects.as_slice());
    assert_eq!(
        receipt.durability,
        vec![DurabilityAck::Volatile { records_end: 0 }; 2]
    );
    assert_eq!(
        book.order(&client).unwrap().state,
        &OrderState::SubmitUnknown
    );
    let rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    assert_eq!(
        rebuilt.order(&client).unwrap().ctx,
        book.order(&client).unwrap().ctx
    );
    assert_eq!(
        rebuilt.order(&client).unwrap().state,
        book.order(&client).unwrap().state
    );
    assert_eq!(
        rebuilt.order(&client).unwrap().cursor,
        book.order(&client).unwrap().cursor
    );
}

#[test]
fn b3_ordinary_append_failure_preserves_state_and_permits_source_retry() {
    let (mut book, client, mut journal) = initial();
    journal.fail_at = Some(1);
    assert!(matches!(
        book.commit_event(&client, &OrderEvent::PrepareSubmit, &mut journal),
        Err(CommitError::Journal(_))
    ));
    assert_eq!(book.order(&client).unwrap().state, &OrderState::New);
    assert_eq!(book.order(&client).unwrap().cursor.seq, 0);
    assert!(journal.records().is_empty());
    assert!(!book.stopped());
    journal.fail_at = None;
    book.commit_event(&client, &OrderEvent::PrepareSubmit, &mut journal)
        .unwrap();
    assert_eq!(journal.calls.len(), 3);
}

#[test]
fn b3_ordinary_effect_error_leaves_source_owner_unstopped_with_committed_prefix() {
    let (mut book, client, mut journal) = initial();
    started(&mut book, &client, &mut journal);
    let before = book.order(&client).unwrap().state.clone();
    journal.calls.clear();
    journal.fail_at = Some(2);
    assert!(matches!(
        book.commit_event(&client, &OrderEvent::SubmitTimeout, &mut journal),
        Err(CommitError::Journal(_))
    ));
    assert_eq!(journal.calls, ["append", "effects"]);
    assert_eq!(book.order(&client).unwrap().state, &before);
    assert!(!book.stopped());
    assert_eq!(journal.records().len(), 3);
    assert!(journal.other_effects().is_empty());
    let rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    assert_eq!(
        rebuilt.order(&client).unwrap().state,
        &OrderState::SubmitUnknown
    );
}

#[test]
fn closure_fence_failure_never_applies_closure_or_retries_on_the_frozen_owner() {
    let (mut book, client, mut journal) = initial();
    started(&mut book, &client, &mut journal);
    let before = book.order(&client).unwrap().state.clone();
    journal.calls.clear();
    journal.fail_at = Some(3);
    let event = OrderEvent::CloseSubmission {
        reason: SubmissionCloseReason::LocalNotSent,
    };
    assert!(matches!(
        book.commit_event(&client, &event, &mut journal),
        Err(CommitError::Journal(_))
    ));
    assert_eq!(journal.calls, ["append", "effects", "fence"]);
    assert_eq!(book.order(&client).unwrap().state, &before);
    assert!(book.stopped());
    assert!(matches!(
        book.commit_event(&client, &event, &mut journal),
        Err(CommitError::Stopped)
    ));
}

#[test]
fn rejected_or_missing_owner_event_writes_nothing_and_does_not_freeze_the_owner() {
    let (mut book, client, mut journal) = initial();
    assert!(matches!(
        book.commit_event(&client, &OrderEvent::SubmitTimeout, &mut journal),
        Err(CommitError::Rejected(_))
    ));
    assert!(matches!(
        book.commit_event(
            &ClientOrderId("absent".into()),
            &OrderEvent::PrepareSubmit,
            &mut journal
        ),
        Err(CommitError::Host(_))
    ));
    assert!(journal.calls.is_empty());
    assert!(!book.stopped());
    book.commit_event(&client, &OrderEvent::PrepareSubmit, &mut journal)
        .unwrap();
}

#[test]
fn committed_execution_duplicate_and_fee_upgrade_do_not_rebook_inventory() {
    let (mut book, _, mut journal) = initial();
    book.commit_execution(None, &evidence(None), &mut journal)
        .unwrap();
    book.commit_execution(None, &evidence(None), &mut journal)
        .unwrap();
    book.commit_execution(None, &evidence(Some(3)), &mut journal)
        .unwrap();
    assert_eq!(book.inventory().net(), 40);
    assert_eq!(book.inventory().view().executions, 1);
    assert_eq!(
        book.inventory()
            .get(&FillId("fill-a".into()))
            .unwrap()
            .fee_cents,
        Some(3)
    );
    let rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    assert_eq!(rebuilt.inventory().view(), book.inventory().view());
    assert_eq!(
        rebuilt
            .registry()
            .get(&ledger(), &VenueOrderId("wire-a".into())),
        book.registry()
            .get(&ledger(), &VenueOrderId("wire-a".into()))
    );
}

#[test]
fn b1_duplicate_no_row_execution_calls_empty_effects_and_preserves_committed_image() {
    let (mut book, _, mut journal) = initial();
    let fill = evidence(None);
    book.commit_execution(None, &fill, &mut journal).unwrap();
    let prepared = book.prepare_execution(None, &fill).unwrap();
    assert!(prepared.journal.is_none());
    assert!(prepared.effects.is_empty());
    let records = journal.records().to_vec();
    let inventory = book.inventory().view();
    let registry = format!("{:?}", book.registry());
    journal.calls.clear();
    let receipt = book.commit_execution(None, &fill, &mut journal).unwrap();
    assert_eq!(journal.calls, ["effects"]);
    assert_eq!(receipt.durability.len(), 1);
    assert_eq!(journal.records(), records);
    assert_eq!(book.inventory().view(), inventory);
    assert_eq!(format!("{:?}", book.registry()), registry);
}

#[test]
fn b1_duplicate_identified_execution_exposes_an_empty_effects_error() {
    let (mut book, client, mut journal) = initial();
    accepted(&mut book, &client, "wire-a", &mut journal);
    let fill = evidence(None);
    book.commit_execution(Some(&client), &fill, &mut journal)
        .unwrap();
    let prepared = book.prepare_execution(Some(&client), &fill).unwrap();
    assert!(prepared.journal.is_none());
    assert!(prepared.effects.is_empty());
    let before = book.order(&client).unwrap();
    let image = (before.state.clone(), before.ctx.clone(), before.cursor);
    let records = journal.records().to_vec();
    journal.calls.clear();
    journal.fail_at = Some(1);
    assert!(matches!(
        book.commit_execution(Some(&client), &fill, &mut journal),
        Err(CommitError::Journal(_))
    ));
    assert_eq!(journal.calls, ["effects"]);
    let after = book.order(&client).unwrap();
    assert_eq!(
        (after.state.clone(), after.ctx.clone(), after.cursor),
        image
    );
    assert_eq!(journal.records(), records);
    // Per-path stop-latch parity is B3, outside this B1 patch.
}

#[test]
fn b1_record_bearing_close_has_append_empty_effects_fence_and_recordless_retry_has_none() {
    let (mut book, client, mut journal) = initial();
    started(&mut book, &client, &mut journal);
    let event = OrderEvent::CloseSubmission {
        reason: SubmissionCloseReason::LocalNotSent,
    };
    let prepared = book.prepare_event(&client, &event).unwrap();
    assert!(prepared.journal.is_some());
    assert!(prepared.effects.is_empty());
    journal.calls.clear();
    book.commit_event(&client, &event, &mut journal).unwrap();
    assert_eq!(journal.calls, ["append", "effects", "fence"]);
    let prepared = book.prepare_event(&client, &event).unwrap();
    assert!(prepared.journal.is_none());
    journal.calls.clear();
    book.commit_event(&client, &event, &mut journal).unwrap();
    assert!(journal.calls.is_empty());
}

#[test]
fn b3_execution_append_error_preserves_inventory_and_permits_source_retry() {
    let (mut book, _, mut journal) = initial();
    journal.fail_at = Some(1);
    assert!(matches!(
        book.commit_execution(None, &evidence(None), &mut journal),
        Err(CommitError::Journal(_))
    ));
    assert_eq!(book.inventory().net(), 0);
    assert_eq!(book.inventory().view().executions, 0);
    assert!(!book.stopped());
    journal.fail_at = None;
    book.commit_execution(None, &evidence(None), &mut journal)
        .unwrap();
    assert_eq!(book.inventory().net(), 40);
}

#[test]
fn identified_batch_uses_canonical_validation_and_rebuilds_order_and_inventory() {
    let (mut book, client, mut journal) = initial();
    accepted(&mut book, &client, "wire-a", &mut journal);
    let fill = evidence(Some(3));
    let event = OrderEvent::Fill {
        fill_id: fill.execution_id.clone(),
        qty: fill.qty,
        price_cents: fill.price_cents,
        ts_ns: fill.ts_ns,
        venue_order_id: fill.venue_order_id.clone(),
        fee_cents: fill.fee_cents,
    };
    let mut wrong = fill.clone();
    wrong.qty += 1;
    let before = journal.records().len();
    assert!(matches!(
        book.commit_execution_batch(&client, &event, &[wrong], &mut journal),
        Err(CommitError::Host(_))
    ));
    assert_eq!(journal.records().len(), before);
    book.commit_execution_batch(&client, &event, &[fill], &mut journal)
        .unwrap();
    let rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    assert_eq!(book.inventory().net(), 40);
    assert_eq!(rebuilt.inventory().view(), book.inventory().view());
    assert_eq!(
        rebuilt.order(&client).unwrap().ctx,
        book.order(&client).unwrap().ctx
    );
    assert_eq!(
        rebuilt.order(&client).unwrap().state,
        book.order(&client).unwrap().state
    );
    let wire = VenueOrderId("wire-a".into());
    assert_eq!(
        rebuilt.registry().get(&ledger(), &wire),
        book.registry().get(&ledger(), &wire),
        "live and rebuilt membership must use the same journal record position"
    );
}

#[test]
fn observation_commits_exact_wire_members_and_leaves_other_wire_rows_unchanged() {
    let (mut book, client, mut journal) = initial();
    accepted(&mut book, &client, "wire-a", &mut journal);
    let other = context("client-b");
    let other_id = other.client_order_id.clone();
    book.insert_initial(other).unwrap();
    accepted(&mut book, &other_id, "wire-b", &mut journal);
    let other_before = book.order(&other_id).unwrap().ctx.clone();
    let wire = VenueOrderId("wire-a".into());
    let mut round = ObservationRound::begin(ledger(), wire.clone());
    round.page(PageMeta {
        exhaustive: true,
        parse_failures: 0,
    });
    let row = RowEvidence {
        ledger: ledger(),
        wire: wire.clone(),
        status: BackfillOrderStatus::Open,
        filled: 0,
        remaining: 100,
        client_order_id: Some(client.clone()),
        marker: book.registry().marker(&ledger(), &wire),
    };
    book.commit_observation(round, row, &mut journal).unwrap();
    assert_eq!(book.order(&other_id).unwrap().ctx, &other_before);
    assert!(!matches!(book.working_view(&wire), WorkingView::Unresolved));
    let rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    assert_eq!(rebuilt.working_view(&wire), book.working_view(&wire));
    assert_eq!(
        rebuilt.order(&client).unwrap().ctx,
        book.order(&client).unwrap().ctx
    );
}

#[test]
fn frame_duplicate_stale_and_resting_proof_preserve_distinct_canonical_boundaries() {
    let (mut book, client, mut journal) = initial();
    accepted(&mut book, &client, "wire-a", &mut journal);
    let wire = VenueOrderId("wire-a".into());
    let frame = UserOrderEvidence {
        wire: wire.clone(),
        market: ledger().market,
        exchange_index: 1,
        subaccount_number: 1,
        sid: 2,
        side: Side::BuyYes,
        yes_price_e4: 400_000,
        client_order_id: client.clone(),
        status: UserOrderStatus::Resting,
        filled: 0,
        remaining: 100,
        initial: 100,
        updated_ts_ms: 10,
        connection_generation: 1,
        payload_digest: "digest-a".into(),
    };
    journal.calls.clear();
    book.commit_user_order(frame.clone(), &mut journal)
        .unwrap()
        .unwrap();
    assert_eq!(journal.calls, ["append", "fence"]);
    assert_eq!(book.working_view(&wire), WorkingView::Unresolved);
    let before = journal.records().len();
    assert!(book
        .commit_user_order(frame.clone(), &mut journal)
        .unwrap()
        .is_none());
    let mut stale = frame;
    stale.updated_ts_ms -= 1;
    assert!(matches!(
        book.commit_user_order(stale, &mut journal),
        Err(CommitError::OrderStateProof(_))
    ));
    assert_eq!(journal.records().len(), before);
    book.commit_resting_proof(&wire, &mut journal).unwrap();
    assert!(!matches!(book.working_view(&wire), WorkingView::Unresolved));
    assert_eq!(book.inventory().net(), 0);
    let rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    assert_eq!(rebuilt.working_view(&wire), book.working_view(&wire));
}

#[test]
fn b2_direct_registry_appends_do_not_advance_transaction_fold_coordinates() {
    let (mut book, client, mut journal) = initial();
    accepted(&mut book, &client, "wire-a", &mut journal);
    assert_eq!(journal.records().len(), 3);
    let wire = VenueOrderId("wire-a".into());
    let frame = UserOrderEvidence {
        wire: wire.clone(),
        market: ledger().market,
        exchange_index: 1,
        subaccount_number: 1,
        sid: 2,
        side: Side::BuyYes,
        yes_price_e4: 400_000,
        client_order_id: client.clone(),
        status: UserOrderStatus::Resting,
        filled: 0,
        remaining: 100,
        initial: 100,
        updated_ts_ms: 10,
        connection_generation: 1,
        payload_digest: "digest-a".into(),
    };
    book.commit_user_order(frame, &mut journal)
        .unwrap()
        .unwrap();
    assert_eq!(
        book.registry()
            .get(&ledger(), &wire)
            .unwrap()
            .latest_user_order_revision,
        4
    );
    book.commit_wire_authority_hold(&wire, &mut journal)
        .unwrap();
    assert_eq!(
        book.registry()
            .get(&ledger(), &wire)
            .unwrap()
            .authority_hold_revision,
        5
    );
    let second = context("client-b");
    let second_id = second.client_order_id.clone();
    book.insert_initial(second).unwrap();
    accepted(&mut book, &second_id, "wire-b", &mut journal);
    let second_wire = VenueOrderId("wire-b".into());
    let live = book.registry().get(&ledger(), &second_wire).unwrap();
    assert_eq!(
        live.members[0].bind_pos, 5,
        "S q counts only successful transaction folds"
    );
    let mut rebuilt = OrderBook::from_committed_records(ledger(), journal.records()).unwrap();
    assert_eq!(
        rebuilt
            .registry()
            .get(&ledger(), &second_wire)
            .unwrap()
            .members[0]
            .bind_pos,
        7,
        "S replay enumerates the complete journal, distinct from live q after direct appends"
    );
    let replay_prefix = journal.records().len() as u64;
    let third = context("client-c");
    let third_id = third.client_order_id.clone();
    rebuilt.insert_initial(third).unwrap();
    accepted(&mut rebuilt, &third_id, "wire-c", &mut journal);
    assert_eq!(
        rebuilt
            .registry()
            .get(&ledger(), &VenueOrderId("wire-c".into()))
            .unwrap()
            .members[0]
            .bind_pos,
        replay_prefix + 2,
        "S sets q to journal length on replay before further transactions"
    );
}

#[test]
fn b3_direct_hold_helper_remains_callable_after_a_closure_stop() {
    let (mut book, client, mut journal) = initial();
    started(&mut book, &client, &mut journal);
    journal.fail_at = Some(journal.calls.len() + 1);
    assert!(matches!(
        book.commit_event(
            &client,
            &OrderEvent::CloseSubmission {
                reason: SubmissionCloseReason::LocalNotSent,
            },
            &mut journal
        ),
        Err(CommitError::Journal(_))
    ));
    assert!(book.stopped());
    journal.fail_at = None;
    journal.calls.clear();
    let wire = VenueOrderId("wire-for-diagnostic-hold".into());
    book.commit_wire_authority_hold(&wire, &mut journal)
        .unwrap();
    assert_eq!(journal.calls, ["append", "fence"]);
    assert!(
        book.registry()
            .get(&ledger(), &wire)
            .unwrap()
            .authority_hold
    );
    assert!(
        book.stopped(),
        "successful helper must not clear the prior source stop"
    );
}

#[test]
fn b3_observation_finish_error_latches_before_any_journal_call() {
    let (mut book, client, mut journal) = initial();
    accepted(&mut book, &client, "wire-a", &mut journal);
    journal.calls.clear();
    let round = ObservationRound::begin(ledger(), VenueOrderId("wrong-round-wire".into()));
    let row = RowEvidence {
        ledger: ledger(),
        wire: VenueOrderId("wire-a".into()),
        status: BackfillOrderStatus::Open,
        filled: 0,
        remaining: 100,
        client_order_id: Some(client),
        marker: book
            .registry()
            .marker(&ledger(), &VenueOrderId("wire-a".into())),
    };
    assert!(matches!(
        book.commit_observation(round, row, &mut journal),
        Err(CommitError::Observation(RoundError::ForeignRow))
    ));
    assert!(journal.calls.is_empty());
    assert!(book.stopped());
}

#[test]
fn b3_failing_fence_drain_pins_source_memory_and_file_anyhow_log_text() {
    use std::error::Error;
    fn memory_anyhow_alternate(error: &dyn Error) -> String {
        let mut text = error.to_string();
        let mut next = error.source();
        while let Some(source) = next {
            text.push_str(": ");
            text.push_str(&source.to_string());
            next = source.source();
        }
        text
    }
    let (mut book, client, mut journal) = initial();
    started(&mut book, &client, &mut journal);
    book.commit_event(&client, &OrderEvent::SubmitTimeout, &mut journal)
        .unwrap();
    journal.fail_fences = true;
    let event = OrderEvent::UnknownBackfillResult {
        exhaustive: true,
        matched: vec![],
    };
    let error = book
        .commit_event(&client, &event, &mut journal)
        .unwrap_err();
    let halt_reason = match error {
        CommitError::SemanticHalt { reason, .. } => Some(reason),
        other => panic!("unexpected semantic result {other:?}"),
    };
    let queued = book.take_fence_errors();
    assert_eq!(queued.len(), 1);
    let memory = memory_anyhow_alternate(&queued[0]);
    assert_eq!(memory, "journal ExplicitFence: injected: injected");
    let file = match &queued[0] {
        JournalError::Io {
            operation: JournalOperation::ExplicitFence,
            source,
        } => format!("fsync journal (commit_now): {source}"),
        other => panic!("unexpected fence error {other:?}"),
    };
    assert_eq!(file, "fsync journal (commit_now): injected");
    assert_eq!(format!("★ halt 证据落盘失败({halt_reason:?}):{memory} —— 重启后 journal 可能缺尾"),
        "★ halt 证据落盘失败(Some(UnknownNoMatch)):journal ExplicitFence: injected: injected —— 重启后 journal 可能缺尾");
    assert_eq!(format!("★ halt 证据落盘失败({halt_reason:?}):{file} —— 重启后 journal 可能缺尾"),
        "★ halt 证据落盘失败(Some(UnknownNoMatch)):fsync journal (commit_now): injected —— 重启后 journal 可能缺尾");
    assert!(book.take_fence_errors().is_empty());
    assert!(!book.stopped());
}

#[test]
fn b4_in_place_rebuild_retains_prior_stop_while_fresh_constructor_stays_fresh() {
    let (mut book, client, mut journal) = initial();
    started(&mut book, &client, &mut journal);
    let prefix = journal.records().to_vec();
    journal.fail_at = Some(journal.calls.len() + 1);
    let close = OrderEvent::CloseSubmission {
        reason: SubmissionCloseReason::LocalNotSent,
    };
    assert!(matches!(
        book.commit_event(&client, &close, &mut journal),
        Err(CommitError::Journal(_))
    ));
    assert!(book.stopped());
    book.rebuild_in_place(&prefix).unwrap();
    assert!(book.stopped());
    assert!(matches!(
        book.commit_event(&client, &close, &mut journal),
        Err(CommitError::Stopped)
    ));
    let mut fresh = OrderBook::from_committed_records(ledger(), &prefix).unwrap();
    assert!(!fresh.stopped());
    journal.fail_at = None;
    fresh.commit_event(&client, &close, &mut journal).unwrap();
}

#[test]
fn b4_empty_no_ledger_and_wrong_scope_preserve_the_entire_existing_owner() {
    let (mut book, client, mut journal) = initial();
    started(&mut book, &client, &mut journal);
    assert!(book.commit_seed_walk(0, 1, true, [], &mut journal).unwrap());
    let image = format!("{book:?}");
    book.rebuild_in_place(&[]).unwrap();
    assert_eq!(format!("{book:?}"), image);
    let no_ledger = [omsrs::lifecycle::fill_journal_record(
        client.clone(),
        FillId("legacy".into()),
        1,
        40,
        1,
        None,
        None,
    )];
    book.rebuild_in_place(&no_ledger).unwrap();
    assert_eq!(format!("{book:?}"), image);
    let mut foreign = journal.records().to_vec();
    if let JournalRecord::OrderTxn(order) = &mut foreign[0] {
        order.ledger.scope = ExecutionScope("other-account".into());
    }
    assert!(matches!(
        book.rebuild_in_place(&foreign),
        Err(HostError::JournalScopeMismatch { .. })
    ));
    assert_eq!(format!("{book:?}"), image);
}

#[test]
fn b4_registry_failure_keeps_source_partial_inventory_mutation_and_old_order_map() {
    let (mut book, client, mut journal) = initial();
    accepted(&mut book, &client, "wire-a", &mut journal);
    book.commit_execution(Some(&client), &evidence(None), &mut journal)
        .unwrap();
    assert_eq!(book.inventory().net(), 40);
    let registry_before = format!("{:?}", book.registry());
    let mut prefix = journal.records()[..2].to_vec();
    let mut divergent = prefix[0].clone();
    if let JournalRecord::OrderTxn(order) = &mut divergent {
        order.event.push_str("-divergent");
    }
    prefix.push(divergent);
    assert!(matches!(
        book.rebuild_in_place(&prefix),
        Err(HostError::Replay {
            stage: ReplayStage::Registry,
            ..
        })
    ));
    assert!(book.stopped());
    assert_eq!(
        book.inventory().net(),
        0,
        "inventory assignment precedes failed registry rebuild"
    );
    assert_eq!(format!("{:?}", book.registry()), registry_before);
    assert_eq!(
        book.order(&client).unwrap().ctx.attributed_fill_qty,
        40,
        "order map assignment follows all fallible per-CID rebuilds"
    );
}

#[test]
fn b4_success_rebuilds_owned_images_resets_g2_and_seeds_future_fold_position() {
    let (mut book, client, mut journal) = initial();
    accepted(&mut book, &client, "wire-a", &mut journal);
    book.commit_execution(Some(&client), &evidence(Some(0)), &mut journal)
        .unwrap();
    let mut recovered = OrderBook::new(ledger());
    assert!(recovered
        .commit_seed_walk(0, 1, true, [], &mut journal)
        .unwrap());
    recovered.rebuild_in_place(journal.records()).unwrap();
    assert!(!recovered.seed_proven());
    assert!(!recovered.stopped());
    assert_eq!(recovered.inventory().view(), book.inventory().view());
    assert_eq!(
        recovered
            .registry()
            .get(&ledger(), &VenueOrderId("wire-a".into())),
        book.registry()
            .get(&ledger(), &VenueOrderId("wire-a".into()))
    );
    let actual = recovered.order(&client).unwrap();
    let live = book.order(&client).unwrap();
    assert_eq!(
        (actual.state, actual.ctx, actual.cursor),
        (live.state, live.ctx, live.cursor)
    );
    let prefix_len = journal.records().len() as u64;
    let next = context("client-next");
    let next_id = next.client_order_id.clone();
    recovered.insert_initial(next).unwrap();
    accepted(&mut recovered, &next_id, "wire-next", &mut journal);
    assert_eq!(
        recovered
            .registry()
            .get(&ledger(), &VenueOrderId("wire-next".into()))
            .unwrap()
            .members[0]
            .bind_pos,
        prefix_len + 2
    );
}

#[test]
fn b4_inventory_failure_assigns_source_ledger_before_preserving_previous_inventory() {
    let (mut book, client, mut journal) = initial();
    accepted(&mut book, &client, "wire-a", &mut journal);
    book.commit_execution(Some(&client), &evidence(None), &mut journal)
        .unwrap();
    let before_inventory = book.inventory().view();
    let before_registry = format!("{:?}", book.registry());
    let mut records = journal.records().to_vec();
    let execution = records
        .iter()
        .find(|r| matches!(r, JournalRecord::ExecutionTxn(_)))
        .unwrap()
        .clone();
    records.push(execution);
    for record in &mut records {
        match record {
            JournalRecord::OrderTxn(order) => order.ledger.market = "recovered-market".into(),
            JournalRecord::ExecutionTxn(execution) => {
                execution.ledger.market = "recovered-market".into()
            }
            _ => {}
        }
    }
    assert!(matches!(
        book.rebuild_in_place(&records),
        Err(HostError::Replay {
            stage: ReplayStage::Inventory,
            ..
        })
    ));
    assert_eq!(book.ledger().market, "recovered-market");
    assert_eq!(book.inventory().view(), before_inventory);
    assert_eq!(format!("{:?}", book.registry()), before_registry);
    assert!(book.stopped());
}

#[test]
fn b4_order_failure_keeps_new_inventory_registry_and_previous_order_map() {
    let (mut book, client, mut journal) = initial();
    accepted(&mut book, &client, "wire-a", &mut journal);
    book.commit_execution(Some(&client), &evidence(None), &mut journal)
        .unwrap();
    let seq_gap = [journal.records()[1].clone()];
    assert!(matches!(
        book.rebuild_in_place(&seq_gap),
        Err(HostError::Replay {
            stage: ReplayStage::Order(_),
            ..
        })
    ));
    assert_eq!(book.inventory().net(), 0);
    assert!(book
        .registry()
        .get(&ledger(), &VenueOrderId("wire-a".into()))
        .is_none());
    assert_eq!(book.order(&client).unwrap().ctx.attributed_fill_qty, 40);
    assert!(book.stopped());
}
