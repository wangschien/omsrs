use omsrs::execution::{ExecutionEvidence, ExecutionScope, ExecutionSource, LedgerId};
use omsrs::host_v1::{
    CommitError, DurabilityAck, JournalError, JournalOperation, MemoryJournal, OmsJournal,
    OrderBook,
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
    let receipt = book
        .commit_event(
            &client,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![],
            },
            &mut journal,
        )
        .unwrap();
    assert_eq!(journal.calls.last(), Some(&"fence"));
    assert_eq!(
        book.order(&client).unwrap().state,
        &OrderState::UnknownNoMatch
    );
    assert!(!book.stopped());
    assert!(receipt
        .durability
        .iter()
        .all(|ack| matches!(ack, DurabilityAck::Volatile { .. })));
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
fn seed_storage_failure_leaves_proof_false_and_storage_stop_unready() {
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
    assert!(book.stopped());
    assert!(matches!(
        book.host_readiness(false),
        omsrs::observation::Readiness::Unresolved { .. }
    ));
    assert!(matches!(
        book.commit_seed_walk(0, 1, true, [], &mut journal),
        Err(CommitError::Stopped)
    ));
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
                operation: JournalOperation::AppendWrite,
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
fn append_failure_leaves_state_uncommitted_and_freezes_subsequent_commits() {
    let (mut book, client, mut journal) = initial();
    journal.fail_at = Some(1);
    assert!(matches!(
        book.commit_event(&client, &OrderEvent::PrepareSubmit, &mut journal),
        Err(CommitError::Journal(_))
    ));
    assert_eq!(book.order(&client).unwrap().state, &OrderState::New);
    assert_eq!(book.order(&client).unwrap().cursor.seq, 0);
    assert!(journal.records().is_empty());
    assert!(book.stopped());
    journal.fail_at = None;
    assert!(matches!(
        book.commit_event(&client, &OrderEvent::PrepareSubmit, &mut journal),
        Err(CommitError::Stopped)
    ));
    assert_eq!(journal.calls.len(), 1);
}

#[test]
fn effects_failure_after_append_keeps_the_owner_frozen_and_requires_reconstruction() {
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
    assert!(book.stopped());
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
fn execution_append_failure_preserves_inventory_and_blocks_later_execution() {
    let (mut book, _, mut journal) = initial();
    journal.fail_at = Some(1);
    assert!(matches!(
        book.commit_execution(None, &evidence(None), &mut journal),
        Err(CommitError::Journal(_))
    ));
    assert_eq!(book.inventory().net(), 0);
    assert_eq!(book.inventory().view().executions, 0);
    assert!(matches!(
        book.commit_execution(None, &evidence(None), &mut journal),
        Err(CommitError::Stopped)
    ));
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
