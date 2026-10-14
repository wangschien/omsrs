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
    ObservationRound, PageMeta, RowEvidence, UserOrderEvidence, UserOrderStatus, WorkingView,
};

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
}
impl TraceJournal {
    fn step(&mut self, call: &'static str) -> Result<(), JournalError> {
        self.calls.push(call);
        if self.fail_at == Some(self.calls.len()) {
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
    journal.fail_at = Some(2);
    let event = OrderEvent::CloseSubmission {
        reason: SubmissionCloseReason::LocalNotSent,
    };
    assert!(matches!(
        book.commit_event(&client, &event, &mut journal),
        Err(CommitError::Journal(_))
    ));
    assert_eq!(journal.calls, ["append", "fence"]);
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
