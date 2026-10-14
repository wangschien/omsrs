use std::collections::{BTreeMap, BTreeSet};

use crate::execution::{
    execution_debt, prepare_execution, prepare_execution_batch, prepare_order_event, BatchOutcome,
    ExecutionEvidence, ExecutionOutcome, ExecutionSource, ExecutionTransaction, InventoryHealth,
    LedgerId, MarketInventory, OrderCursor, OrderRef, OrderTarget, OrderTransaction,
};
use crate::lifecycle::{
    ClientOrderId, Effect, JournalRecord, OrderCtx, OrderEvent, OrderState, VenueOrderId,
};
use crate::observation::{
    rebuild_order_with_observations, AmendLineage, MemberRole, ObservationOutcome,
    ObservationRound, OrderStateTxnRecord, Readiness, ReadinessError, RoundError, Route,
    RowEvidence, UserOrderEvidence, WireRegistry, WorkingView,
};

use super::coverage::CoverageMember;
use super::identity::{LocalOrdinal, OrdinalView};
use super::persistence::OmsJournal;
use super::recovery::RecoveryCadence;
use super::risk::{AdmissionReasons, AuthorityHold, BookGuard, StatusGuard};
use super::types::{
    CommitError, CommitReceipt, DurabilityAck, HostError, JournalError, OrderHostView, OrderView,
};

#[derive(Debug)]
struct OrderRow {
    state: OrderState,
    ctx: OrderCtx,
    cursor: OrderCursor,
}

/// Owns the existing lifecycle contexts, inventory and wire registry once.
/// Preparation uses the canonical transaction functions and does not mutate
/// committed state or authorize an external write.
#[derive(Debug)]
pub struct OrderBook {
    ledger: LedgerId,
    orders: BTreeMap<ClientOrderId, OrderRow>,
    inventory: MarketInventory,
    registry: WireRegistry,
    recovery: RecoveryCadence,
    risk: AdmissionReasons,
    authority_hold: AuthorityHold,
    book_guard: BookGuard,
    status_guard: StatusGuard,
    stopped: bool,
    seed_proven: bool,
    fence_errors: Vec<JournalError>,
}

impl OrderBook {
    pub fn new(ledger: LedgerId) -> Self {
        Self {
            inventory: MarketInventory::new(ledger.clone()),
            ledger,
            orders: BTreeMap::new(),
            registry: WireRegistry::new(),
            recovery: RecoveryCadence::default(),
            risk: AdmissionReasons::default(),
            authority_hold: AuthorityHold::default(),
            book_guard: BookGuard::default(),
            status_guard: StatusGuard::default(),
            stopped: false,
            seed_proven: false,
            fence_errors: Vec::new(),
        }
    }

    /// Restore canonical state from already committed records. This pure
    /// reconstruction does not validate a storage device or assert durability.
    pub fn from_committed_records(
        ledger: LedgerId,
        records: &[JournalRecord],
    ) -> Result<Self, HostError> {
        let inventory =
            MarketInventory::rebuild(ledger.clone(), records).map_err(HostError::Rebuild)?;
        let registry = WireRegistry::rebuild(records, &inventory).map_err(HostError::Rebuild)?;
        let mut clients = BTreeSet::new();
        for record in records {
            match record {
                JournalRecord::OrderTxn(order) if order.ledger == ledger => {
                    clients.insert(order.client_order_id.clone());
                }
                JournalRecord::ExecutionTxn(execution) if execution.ledger == ledger => {
                    if let Some(order) = &execution.order {
                        clients.insert(order.client_order_id.clone());
                    }
                }
                JournalRecord::ObservationTxn(observation) if observation.ledger == ledger => {
                    clients.extend(
                        observation
                            .members_after
                            .iter()
                            .map(|m| m.client_order_id.clone()),
                    );
                }
                _ => {}
            }
        }
        let mut orders = BTreeMap::new();
        for client in clients {
            if let Some((state, ctx, cursor)) =
                rebuild_order_with_observations(&ledger, &client, records)
                    .map_err(HostError::Rebuild)?
            {
                orders.insert(client, OrderRow { state, ctx, cursor });
            }
        }
        Ok(Self {
            ledger,
            orders,
            inventory,
            registry,
            recovery: RecoveryCadence::default(),
            risk: AdmissionReasons::default(),
            authority_hold: AuthorityHold::default(),
            book_guard: BookGuard::default(),
            status_guard: StatusGuard::default(),
            stopped: false,
            seed_proven: false,
            fence_errors: Vec::new(),
        })
    }

    pub fn ledger(&self) -> &LedgerId {
        &self.ledger
    }
    pub fn inventory(&self) -> &MarketInventory {
        &self.inventory
    }
    pub fn registry(&self) -> &WireRegistry {
        &self.registry
    }

    pub fn allocate_ordinal(&mut self) -> LocalOrdinal {
        self.registry.allocate_ordinal()
    }

    pub fn bind_ordinal(&mut self, local: LocalOrdinal, wire: VenueOrderId, client: Option<&str>) {
        self.registry.bind_ordinal(local, wire, client);
    }

    pub fn wire_for_ordinal(&self, local: LocalOrdinal) -> Option<&VenueOrderId> {
        self.registry.wire_for_ordinal(local)
    }

    pub fn ordinal_for_wire(&self, wire: &str) -> Option<LocalOrdinal> {
        self.registry.ordinal_for_wire(wire)
    }

    pub fn ordinal_for_client(&self, client: &str) -> Option<LocalOrdinal> {
        self.registry.ordinal_for_client(client)
    }

    pub fn client_for_ordinal(&self, local: LocalOrdinal) -> Option<&ClientOrderId> {
        self.registry.client_for_ordinal(local)
    }

    pub fn forget_ordinal(&mut self, local: LocalOrdinal) {
        self.registry.forget_ordinal(local);
    }

    pub fn ordinal_view(&self) -> OrdinalView<'_> {
        self.registry.ordinal_view()
    }

    pub fn forget_wire_if_known(&mut self, wire: &str) {
        let wire = wire.trim();
        if wire.is_empty() {
            return;
        }
        if let Some(local) = self.registry.ordinal_for_wire(wire) {
            self.registry.forget_ordinal(local);
        }
    }

    /// Register an initial lifecycle context without fabricating a journal
    /// cursor, accepted wire, inventory entry or authority observation.
    pub fn insert_initial(&mut self, ctx: OrderCtx) -> Result<(), HostError> {
        if ctx.market != self.ledger.market {
            return Err(HostError::ForeignMarket {
                expected: self.ledger.market.clone(),
                found: ctx.market.clone(),
            });
        }
        if self.orders.contains_key(&ctx.client_order_id) {
            return Err(HostError::DuplicateClient(ctx.client_order_id.clone()));
        }
        self.orders.insert(
            ctx.client_order_id.clone(),
            OrderRow {
                state: OrderState::New,
                ctx,
                cursor: OrderCursor { seq: 0 },
            },
        );
        Ok(())
    }

    pub fn order(&self, client: &ClientOrderId) -> Option<OrderView<'_>> {
        self.orders.get(client).map(|row| OrderView {
            state: &row.state,
            ctx: &row.ctx,
            cursor: row.cursor,
        })
    }

    fn order_ref(&self, client: &ClientOrderId) -> Option<OrderRef<'_>> {
        self.orders.get(client).map(|row| OrderRef {
            ledger: &self.ledger,
            state: &row.state,
            ctx: &row.ctx,
            cursor: row.cursor,
        })
    }

    pub fn prepare_event(
        &self,
        client: &ClientOrderId,
        event: &OrderEvent,
    ) -> Result<OrderTransaction, HostError> {
        let order = self
            .order_ref(client)
            .ok_or_else(|| HostError::MissingClient(client.clone()))?;
        prepare_order_event(order, event).map_err(HostError::Prepare)
    }

    /// An explicit owner selects the order transaction. Claimed client IDs in
    /// execution evidence never silently choose a lifecycle owner here.
    pub fn prepare_execution(
        &self,
        owner: Option<&ClientOrderId>,
        evidence: &ExecutionEvidence,
    ) -> Result<ExecutionTransaction, HostError> {
        let order = owner
            .map(|client| {
                self.order_ref(client)
                    .ok_or_else(|| HostError::MissingClient(client.clone()))
            })
            .transpose()?;
        prepare_execution(order, &self.inventory, evidence).map_err(HostError::Prepare)
    }

    /// Canonical registry readiness only. A Ready result does not establish
    /// host recovery completion, stop state, working quantity or send authority.
    pub fn registry_readiness(&self) -> Result<Readiness, ReadinessError> {
        self.registry.readiness(
            &self.ledger,
            |client| self.order_ref(client),
            &self.inventory,
        )
    }

    pub fn working_view(&self, wire: &VenueOrderId) -> WorkingView {
        self.registry.working_view(&self.ledger, wire)
    }

    /// A missing wire/held execution or overflowing total is unknown, not zero.
    pub fn wire_held_quantity(&self, wire: &VenueOrderId) -> Option<u64> {
        let state = self.registry.get(&self.ledger, wire)?;
        state
            .held
            .iter()
            .try_fold(0u64, |sum, id| sum.checked_add(self.inventory.get(id)?.qty))
    }

    /// Total at the last canonical remaining proof. An own action invalidates
    /// that proof; missing covered executions never become a fabricated total.
    pub fn wire_total_at_proof(&self, wire: &VenueOrderId) -> Option<u64> {
        let proof = self
            .registry
            .get(&self.ledger, wire)?
            .remaining_observation
            .as_ref()?;
        proof.covered.iter().try_fold(proof.remaining, |sum, id| {
            sum.checked_add(self.inventory.get(id)?.qty)
        })
    }

    /// Compare venue cumulative fills with canonical inventory held by this
    /// wire. Unknown held inventory counts as zero for this deficit predicate
    /// only, preserving the conservative requirement for further recovery.
    pub fn wire_known_deficit(&self, wire: &VenueOrderId) -> bool {
        let Some(state) = self.registry.get(&self.ledger, wire) else {
            return false;
        };
        let venue_filled = state
            .latest_user_order
            .as_ref()
            .map_or(0, |frame| frame.filled);
        venue_filled > self.wire_held_quantity(wire).unwrap_or(0)
    }

    fn other_member_attribution(
        &self,
        wire: &VenueOrderId,
        client: &ClientOrderId,
    ) -> Option<(u64, bool)> {
        let Some(state) = self.registry.get(&self.ledger, wire) else {
            return Some((0, false));
        };
        let mut attributed = 0u64;
        let mut debt = false;
        for member in &state.members {
            if &member.cid == client {
                continue;
            }
            let row = self.orders.get(&member.cid)?;
            attributed = attributed.checked_add(row.ctx.attributed_fill_qty)?;
            debt |= row.ctx.fill_obligation > row.ctx.attributed_fill_qty;
        }
        Some((attributed, debt))
    }

    /// Translate a cumulative wire obligation into this member's obligation.
    /// Other-member debt, missing rows or negative differences remain unknown.
    pub fn backfill_member_quantity(
        &self,
        wire: &VenueOrderId,
        client: &ClientOrderId,
        cumulative: u64,
    ) -> Option<u64> {
        let (other_attributed, other_debt) = self.other_member_attribution(wire, client)?;
        if other_debt {
            return None;
        }
        cumulative.checked_sub(other_attributed)
    }

    /// Canonical working proof, otherwise checked member capacity and an
    /// explicit unresolvable flag. Capacity is not exact venue remaining and
    /// does not grant create/decrease authority. A missing client is an error.
    pub fn member_working_capacity(
        &self,
        client: &ClientOrderId,
    ) -> Result<(u64, bool), HostError> {
        let row = self
            .orders
            .get(client)
            .ok_or_else(|| HostError::MissingClient(client.clone()))?;
        if let WorkingView::Known(quantity) =
            self.registry.member_working_view(&self.ledger, client)
        {
            return Ok((quantity, false));
        }
        let wire = row
            .ctx
            .venue_order_id
            .as_ref()
            .or_else(|| self.registry.wire_for_client(&self.ledger, client));
        let (other_attributed, other_debt) = match wire {
            Some(wire) => match self.other_member_attribution(wire, client) {
                Some(facts) => facts,
                None => return Ok((0, true)),
            },
            None if self.registry.has_client(&self.ledger, client) => return Ok((0, true)),
            None => (0, false),
        };
        Ok(
            match row
                .ctx
                .qty
                .checked_sub(other_attributed)
                .and_then(|capacity| capacity.checked_sub(row.ctx.attributed_fill_qty))
            {
                Some(quantity) => (quantity, other_debt),
                None => (0, true),
            },
        )
    }

    /// Borrow canonical wire association, including an unbound amend child.
    /// This coverage identity is not evidence that the child was accepted.
    pub fn coverage_wire_for_client(&self, client: &ClientOrderId) -> Option<&VenueOrderId> {
        let row = self.orders.get(client)?;
        row.ctx
            .venue_order_id
            .as_ref()
            .or_else(|| self.registry.wire_for_client(&self.ledger, client))
    }

    pub fn coverage_client_superseded(&self, client: &ClientOrderId) -> bool {
        self.orders.values().any(|row| {
            let Some(wire) = row.ctx.venue_order_id.as_ref() else {
                return false;
            };
            let Some(lineage) = self
                .registry
                .get(&self.ledger, wire)
                .and_then(|state| state.amend_lineage.as_ref())
            else {
                return false;
            };
            &lineage.old_cid == client
                && self
                    .orders
                    .get(&lineage.new_cid)
                    .is_some_and(|new| new.ctx.venue_order_id.as_ref() == Some(wire))
        })
    }

    /// Base coverage facts from the one canonical owner. Positive-cancel
    /// provenance is deliberately a separate fact until its owner API lands;
    /// callers must not use this base view to discard such a known residual.
    /// Role/price/strategy policy is not selected here; reducing direction uses
    /// the supplied signed net in the same quantity unit as the canonical rows.
    pub fn base_coverage_members(&self, net: i128) -> Vec<CoverageMember<'_>> {
        self.orders
            .iter()
            .map(|(client, row)| {
                let reducing = match row.ctx.side {
                    crate::lifecycle::Side::BuyYes => net < 0,
                    crate::lifecycle::Side::SellYes => net > 0,
                };
                let (live, cancelling) = match row.state {
                    OrderState::New
                    | OrderState::SubmitPrepared
                    | OrderState::SubmitStarted { .. }
                    | OrderState::SubmitUnknown
                    | OrderState::Accepted { .. }
                    | OrderState::Partial { .. }
                    | OrderState::ImmediateFillUnattributed { .. } => (true, false),
                    OrderState::CancelPending { .. } | OrderState::ReconcilePending { .. } => {
                        (true, true)
                    }
                    OrderState::Filled
                    | OrderState::Canceled
                    | OrderState::Terminal
                    | OrderState::ImmediateFillUnresolved
                    | OrderState::UnknownNoMatch
                    | OrderState::Halted { .. }
                    | OrderState::SubmissionClosed { .. } => (false, false),
                };
                // This iterator comes directly from the owned map, so lookup cannot
                // be missing. Any future structural failure explicitly marks the
                // quantity unresolvable, which refuses the UPPER coverage proof.
                let (quantity, unresolvable) =
                    self.member_working_capacity(client).unwrap_or((0, true));
                CoverageMember {
                    client,
                    wire: self.coverage_wire_for_client(client),
                    quantity: (live && reducing).then_some(quantity),
                    unresolvable,
                    cancelling,
                    superseded: self.coverage_client_superseded(client),
                }
            })
            .collect()
    }

    pub fn view(&self) -> OrderHostView {
        OrderHostView {
            inventory: self.inventory.view(),
            registry_readiness: self.registry_readiness(),
            orders: self.orders.len(),
        }
    }

    pub fn recovery(&self) -> &RecoveryCadence {
        &self.recovery
    }
    pub fn recovery_mut(&mut self) -> &mut RecoveryCadence {
        &mut self.recovery
    }
    pub fn risk(&self) -> &AdmissionReasons {
        &self.risk
    }
    pub fn risk_mut(&mut self) -> &mut AdmissionReasons {
        &mut self.risk
    }
    pub fn authority_hold(&self) -> &AuthorityHold {
        &self.authority_hold
    }
    pub fn authority_hold_mut(&mut self) -> &mut AuthorityHold {
        &mut self.authority_hold
    }
    pub fn book_guard(&self) -> &BookGuard {
        &self.book_guard
    }
    pub fn book_guard_mut(&mut self) -> &mut BookGuard {
        &mut self.book_guard
    }
    pub fn status_guard(&self) -> &StatusGuard {
        &self.status_guard
    }
    pub fn status_guard_mut(&mut self) -> &mut StatusGuard {
        &mut self.status_guard
    }

    /// A journal or post-append consistency error freezes further commits.
    /// Reconstruction from the journal is a separate operation, not a reset of
    /// this latch or a proof that the host has completed recovery.
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    pub fn seed_proven(&self) -> bool {
        self.seed_proven
    }

    /// Consume already converted execution rows from an exhaustive seed walk.
    /// None records a rejected input row; conversions must validate the venue's
    /// account and quantity fields before producing Some. Earlier successful
    /// rows remain committed when a later row fails, but never establish proof.
    /// Journal reconstruction does not restore this process-local proof.
    pub fn commit_seed_walk(
        &mut self,
        cutoff: i64,
        created_ts: i64,
        walked: bool,
        rows: impl IntoIterator<Item = Option<ExecutionEvidence>>,
        journal: &mut dyn OmsJournal,
    ) -> Result<bool, CommitError> {
        self.seed_proven = false;
        self.ensure_active()?;
        if created_ts < cutoff || !walked {
            return Ok(false);
        }
        for row in rows {
            let Some(row) = row else { return Ok(false) };
            if row.scope != self.ledger.scope
                || row.market != self.ledger.market
                || row.source != ExecutionSource::VenueSeed
            {
                return Ok(false);
            }
            self.commit_routed_execution(&row, journal)?;
        }
        self.seed_proven = true;
        Ok(true)
    }

    /// Canonical host readiness, with conflict taking precedence over seed,
    /// stop and unknown-submission blockers. This is not send permission or
    /// proof that the independent admission/transport guards have cleared.
    pub fn host_readiness(&self, fill_apply_failed: bool) -> Readiness {
        if fill_apply_failed
            || matches!(
                self.inventory.view().health,
                InventoryHealth::Conflicted { .. }
            )
        {
            return Readiness::Conflict;
        }
        if !self.seed_proven
            || self.stopped
            || self.orders.values().any(|row| {
                matches!(
                    row.state,
                    OrderState::SubmitUnknown | OrderState::UnknownNoMatch
                )
            })
        {
            return Self::unresolved_host_readiness();
        }
        // A missing/mismatched registry member cannot establish readiness.
        // Preserve fail-closed host semantics; registry_readiness still exposes
        // the original typed error for diagnostics rather than hiding it.
        self.registry_readiness()
            .unwrap_or_else(|_| Self::unresolved_host_readiness())
    }

    fn unresolved_host_readiness() -> Readiness {
        Readiness::Unresolved {
            member_debt: 0,
            wire_residual: 0,
            unresolved_venue_only: 0,
            tracked_scoped: 0,
        }
    }

    pub fn outstanding_fill_debt(&self) -> u64 {
        execution_debt(self.orders.values().map(|row| &row.ctx))
    }

    pub fn outstanding_fill_obligation_rows(&self) -> usize {
        self.orders
            .values()
            .filter(|row| row.ctx.attributed_fill_qty < row.ctx.fill_obligation)
            .count()
    }

    /// Halt fences are best-effort, as distinct from mandatory closure fences.
    /// Return failures in occurrence order for the caller's diagnostics. They
    /// never promote an append acknowledgement into stable-storage authority.
    pub fn take_fence_errors(&mut self) -> Vec<JournalError> {
        std::mem::take(&mut self.fence_errors)
    }

    fn ensure_active(&self) -> Result<(), CommitError> {
        if self.stopped {
            Err(CommitError::Stopped)
        } else {
            Ok(())
        }
    }

    fn write_transition(
        &mut self,
        record: Option<&JournalRecord>,
        effects: &[Effect],
        fence: bool,
        journal: &mut dyn OmsJournal,
    ) -> Result<Vec<DurabilityAck>, CommitError> {
        let result = (|| {
            let mut acknowledgements = Vec::new();
            if let Some(record) = record {
                acknowledgements
                    .push(journal.apply_effects(&[Effect::AppendFsync(record.clone())])?);
            }
            if !effects.is_empty() {
                acknowledgements.push(journal.apply_effects(effects)?);
            }
            if fence && record.is_some() {
                acknowledgements.push(journal.commit_now()?);
            }
            Ok(acknowledgements)
        })();
        result.map_err(|error| {
            self.stopped = true;
            CommitError::Journal(error)
        })
    }

    fn fold_committed(
        &mut self,
        record: Option<&JournalRecord>,
        journal: &dyn OmsJournal,
    ) -> Result<(), CommitError> {
        if let Some(record) = record {
            // Rebuild enumerates records from zero. The just-appended record
            // has index len - 1, including the order/member binding records.
            let pos = journal.len().saturating_sub(1) as u64;
            self.registry
                .fold(pos, record, &self.inventory)
                .map_err(|error| {
                    self.stopped = true;
                    CommitError::Registry(error)
                })?;
        }
        Ok(())
    }

    fn fence_halt(
        &mut self,
        record: Option<&JournalRecord>,
        journal: &mut dyn OmsJournal,
        durability: &mut Vec<DurabilityAck>,
    ) {
        let batch_halted = |outcome: &BatchOutcome| match outcome {
            BatchOutcome::Halt(_) => true,
            BatchOutcome::Accept | BatchOutcome::Reject(_) => false,
        };
        let halt = match record {
            Some(JournalRecord::OrderTxn(order)) => batch_halted(&order.outcome),
            Some(JournalRecord::ExecutionTxn(execution)) => execution
                .order
                .as_ref()
                .is_some_and(|order| batch_halted(&order.outcome)),
            // The canonical commit APIs emit normalized order/execution
            // batches. Enumerate other current records so a future journal
            // kind forces its fence policy to be reviewed at compile time.
            None
            | Some(
                JournalRecord::SubmitPrepared { .. }
                | JournalRecord::SubmitStarted { .. }
                | JournalRecord::SubmitResponse { .. }
                | JournalRecord::SubmitUnknown
                | JournalRecord::SubmitUnknownCid { .. }
                | JournalRecord::Fill { .. }
                | JournalRecord::FillCid { .. }
                | JournalRecord::FeeCorrection { .. }
                | JournalRecord::CancelRequested
                | JournalRecord::CancelOutcome(_)
                | JournalRecord::CancelRequestedCid { .. }
                | JournalRecord::CancelOutcomeCid { .. }
                | JournalRecord::VenueBoundCid { .. }
                | JournalRecord::ImmediateFillUnattributed { .. }
                | JournalRecord::ImmediateFillUnresolved
                | JournalRecord::ImmediateFillUnresolvedCid { .. }
                | JournalRecord::UnknownNoMatch
                | JournalRecord::UnknownNoMatchCid { .. }
                | JournalRecord::Halted { .. }
                | JournalRecord::VenueBound { .. }
                | JournalRecord::ObligationRaised { .. }
                | JournalRecord::AuthorityLatched { .. }
                | JournalRecord::AuthorityInvalidated { .. }
                | JournalRecord::ReconcileObserved { .. }
                | JournalRecord::OrderTerminal { .. }
                | JournalRecord::ObservationTxn(_)
                | JournalRecord::AmendLineage { .. }
                | JournalRecord::UserOrderReceived { .. }
                | JournalRecord::UserOrderQuarantined { .. }
                | JournalRecord::OrderAuthorityHold { .. }
                | JournalRecord::UserOrderStaged { .. }
                | JournalRecord::OrderStateTxn(_)
                | JournalRecord::RestOrderProofTxn(_)
                | JournalRecord::OwnActionCid { .. },
            ) => false,
        };
        if halt {
            match journal.commit_now() {
                Ok(acknowledgement) => durability.push(acknowledgement),
                Err(error) => self.fence_errors.push(error),
            }
        }
    }

    /// Prepare and commit under the exclusive owner borrow. No externally
    /// mutable prepared transaction can cross the journal boundary.
    /// Submission closure is explicitly fenced before its canonical state is
    /// applied. Halt fences are best-effort; failures are separately drainable.
    /// Other events retain the journal's configured append policy.
    pub fn commit_event(
        &mut self,
        client: &ClientOrderId,
        event: &OrderEvent,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<()>, CommitError> {
        self.ensure_active()?;
        let transaction = self
            .prepare_event(client, event)
            .map_err(CommitError::Host)?;
        if let Some(reason) = transaction.rejected {
            return Err(CommitError::Rejected(reason));
        }
        let record = transaction.journal.clone();
        let effects = transaction.effects.clone();
        let mut durability = self.write_transition(
            record.as_ref(),
            &effects,
            matches!(event, OrderEvent::CloseSubmission { .. }),
            journal,
        )?;
        self.fence_halt(record.as_ref(), journal, &mut durability);
        let row = self.orders.get_mut(client).expect("prepared owner exists");
        transaction
            .apply(OrderTarget {
                ledger: &self.ledger,
                state: &mut row.state,
                ctx: &mut row.ctx,
                cursor: &mut row.cursor,
            })
            .map_err(|error| {
                self.stopped = true;
                CommitError::Apply(error)
            })?;
        self.fold_committed(record.as_ref(), journal)?;
        Ok(CommitReceipt {
            outcome: (),
            effects,
            durability,
        })
    }

    pub fn commit_execution(
        &mut self,
        owner: Option<&ClientOrderId>,
        evidence: &ExecutionEvidence,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<Vec<ExecutionOutcome>>, CommitError> {
        self.ensure_active()?;
        let transaction = self
            .prepare_execution(owner, evidence)
            .map_err(CommitError::Host)?;
        self.commit_execution_transaction(owner, transaction, journal)
    }

    /// Source-defined owner selection: a known claimed client must not conflict
    /// with its bound wire; an unknown/mismatched claim never falls back to a
    /// different wire owner. Unclaimed evidence uses the canonical route.
    /// Closed submissions remain NoRow. This commits the execution transaction;
    /// post-fill REST/lineage/terminal convergence remains a separate operation.
    pub fn commit_routed_execution(
        &mut self,
        evidence: &ExecutionEvidence,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<Vec<ExecutionOutcome>>, CommitError> {
        self.ensure_active()?;
        let eligible = |client: &ClientOrderId| {
            self.orders.get(client).is_some_and(|row| {
                let mismatch = match (
                    row.ctx.venue_order_id.as_ref(),
                    evidence.venue_order_id.as_ref(),
                ) {
                    (Some(bound), Some(found)) => bound != found,
                    (None, None) | (None, Some(_)) | (Some(_), None) => false,
                };
                !mismatch && !row.state.is_submission_closed()
            })
        };
        let owner = if let Some(client) = evidence.claimed_client_order_id.as_ref() {
            eligible(client).then(|| client.clone())
        } else if let Some(wire) = evidence.venue_order_id.as_ref() {
            match self
                .registry
                .route(&self.ledger, wire, &evidence.execution_id, &self.inventory)
            {
                Route::ExistingOwner(client)
                | Route::NewUnique(client)
                | Route::UnownedUnique(client) => self
                    .orders
                    .get(&client)
                    .is_some_and(|row| !row.state.is_submission_closed())
                    .then_some(client),
                Route::NewAmbiguous | Route::UnownedAmbiguous | Route::VenueOnly => None,
            }
        } else {
            None
        };
        self.commit_execution(owner.as_ref(), evidence, journal)
    }

    /// Identified fills and their lifecycle event use the existing canonical
    /// batch transaction, including positional evidence validation.
    pub fn commit_execution_batch(
        &mut self,
        owner: &ClientOrderId,
        event: &OrderEvent,
        evidence: &[ExecutionEvidence],
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<Vec<ExecutionOutcome>>, CommitError> {
        self.ensure_active()?;
        let order = self
            .order_ref(owner)
            .ok_or_else(|| CommitError::Host(HostError::MissingClient(owner.clone())))?;
        let transaction = prepare_execution_batch(order, &self.inventory, event, evidence)
            .map_err(|error| CommitError::Host(HostError::Prepare(error)))?;
        self.commit_execution_transaction(Some(owner), transaction, journal)
    }

    fn commit_execution_transaction(
        &mut self,
        owner: Option<&ClientOrderId>,
        transaction: ExecutionTransaction,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<Vec<ExecutionOutcome>>, CommitError> {
        let record = transaction.journal.clone();
        let effects = transaction.effects.clone();
        let mut durability = self.write_transition(record.as_ref(), &effects, false, journal)?;
        self.fence_halt(record.as_ref(), journal, &mut durability);
        let target = owner.map(|client| {
            let row = self.orders.get_mut(client).expect("prepared owner exists");
            OrderTarget {
                ledger: &self.ledger,
                state: &mut row.state,
                ctx: &mut row.ctx,
                cursor: &mut row.cursor,
            }
        });
        let outcome = transaction
            .apply(target, &mut self.inventory)
            .map_err(|error| {
                self.stopped = true;
                CommitError::Apply(error)
            })?;
        self.fold_committed(record.as_ref(), journal)?;
        Ok(CommitReceipt {
            outcome,
            effects,
            durability,
        })
    }

    /// Complete a canonical observation using exactly the nonterminal wire
    /// members. No copied order book or persistent working projection is kept.
    pub fn commit_observation(
        &mut self,
        round: ObservationRound,
        row: RowEvidence,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<ObservationOutcome>, CommitError> {
        self.ensure_active()?;
        if row.ledger != self.ledger {
            return Err(CommitError::Observation(RoundError::ForeignRow));
        }
        let clients: BTreeSet<_> = self
            .registry
            .get(&self.ledger, &row.wire)
            .into_iter()
            .flat_map(|wire| &wire.members)
            .filter(|member| member.role != MemberRole::Terminal)
            .map(|member| member.cid.clone())
            .collect();
        let members = clients
            .iter()
            .map(|client| {
                self.order_ref(client)
                    .ok_or_else(|| CommitError::Host(HostError::MissingClient(client.clone())))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let transaction = round
            .finish(&self.registry, &self.inventory, &members, row)
            .map_err(CommitError::Observation)?;
        let record = transaction.journal.clone();
        let effects = transaction.effects.clone();
        let durability = self.write_transition(record.as_ref(), &effects, false, journal)?;
        let mut targets: Vec<_> = self
            .orders
            .iter_mut()
            .filter(|(client, _)| clients.contains(*client))
            .map(|(_, row)| OrderTarget {
                ledger: &self.ledger,
                state: &mut row.state,
                ctx: &mut row.ctx,
                cursor: &mut row.cursor,
            })
            .collect();
        let outcome = transaction
            .apply(&mut targets, &mut self.registry, &self.inventory)
            .map_err(|error| {
                self.stopped = true;
                CommitError::ObservationApply(error)
            })?;
        self.fold_committed(record.as_ref(), journal)?;
        Ok(CommitReceipt {
            outcome,
            effects,
            durability,
        })
    }

    /// Store a validated current order-state frame without synthesizing fills,
    /// remaining proof or host authority. Duplicate frames append nothing.
    pub fn commit_user_order(
        &mut self,
        evidence: UserOrderEvidence,
        journal: &mut dyn OmsJournal,
    ) -> Result<Option<CommitReceipt<()>>, CommitError> {
        self.ensure_active()?;
        let record = self
            .registry
            .prepare_user_order_received(&self.ledger, &evidence.wire.clone(), evidence)
            .map_err(CommitError::OrderStateProof)?;
        let Some(record) = record else {
            return Ok(None);
        };
        let durability = self.write_transition(Some(&record), &[], true, journal)?;
        self.fold_committed(Some(&record), journal)?;
        Ok(Some(CommitReceipt {
            outcome: (),
            effects: Vec::new(),
            durability,
        }))
    }

    /// Canonical remaining proof, after the frame and execution inventory have
    /// been committed. This proves neither transport liveness nor send authority.
    pub fn commit_resting_proof(
        &mut self,
        wire: &VenueOrderId,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<OrderStateTxnRecord>, CommitError> {
        self.ensure_active()?;
        let transaction = self
            .registry
            .prepare_order_state_resting(&self.ledger, wire, &self.inventory)
            .map_err(CommitError::OrderStateProof)?;
        let record = JournalRecord::OrderStateTxn(Box::new(transaction.clone()));
        let durability = self.write_transition(Some(&record), &[], true, journal)?;
        self.registry
            .apply_order_state_resting(&transaction, &self.inventory)
            .map_err(|error| {
                self.stopped = true;
                CommitError::OrderStateProof(error)
            })?;
        Ok(CommitReceipt {
            outcome: transaction,
            effects: Vec::new(),
            durability,
        })
    }

    /// Commit the existing canonical lineage declaration before external IO.
    /// The canonical preparer validates current/phase/generation bindings;
    /// declaring HttpPending/Unknown is not an accepted child or send permit.
    pub fn commit_amend_lineage(
        &mut self,
        wire: &VenueOrderId,
        lineage: AmendLineage,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<()>, CommitError> {
        self.ensure_active()?;
        let record = self
            .registry
            .prepare_amend_lineage(&self.ledger, wire, lineage)
            .map_err(CommitError::OrderStateProof)?;
        let durability = self.write_transition(Some(&record), &[], true, journal)?;
        self.fold_committed(Some(&record), journal)?;
        Ok(CommitReceipt {
            outcome: (),
            effects: Vec::new(),
            durability,
        })
    }
}
