use std::collections::{BTreeMap, BTreeSet};

use crate::execution::{
    execution_debt, prepare_execution, prepare_execution_batch, prepare_order_event, BatchOutcome,
    ExecutionEvidence, ExecutionOutcome, ExecutionSource, ExecutionTransaction,
    InventoryDisposition, InventoryHealth, LedgerId, MarketInventory, OrderCursor,
    OrderDisposition, OrderRef, OrderTarget, OrderTransaction,
};
use crate::lifecycle::{
    BackfillOrderRecord, BackfillOrderStatus, CancelOutcome, ClientOrderId, Effect, JournalRecord,
    OrderCtx, OrderEvent, OrderState, SubmissionCloseReason, VenueOrderId,
};
use crate::observation::{
    rebuild_order_with_observations, AmendLineage, AmendPhase, MemberRole, ObservationOutcome,
    ObservationRound, OrderStateProofError, OrderStateTxnRecord, PageMeta, Readiness,
    ReadinessError, RestOrderProofTxnRecord, RoundError, Route, RowEvidence, UserOrderEvidence,
    UserOrderStatus, WireMarker, WireRegistry, WireState, WorkingView,
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

/// Source positive-DELETE provenance, not an execution or remaining proof.
/// Like the source row's cancel_gone, this transient fact is not reconstructed.
#[derive(Debug)]
struct CancelGone {
    venue_filled: u64,
    wire: VenueOrderId,
}

#[derive(Clone, Copy)]
enum FoldCoordinate {
    Transaction,
    LastRecord,
    JournalEnd,
}

/// Source authority-read snapshot, scoped to this owner's ledger. This is not
/// raw REST identity authentication or send authority. Callers still validate
/// the returned wire/CID/market/index/subaccount and actual page completeness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityReadToken {
    pub wire: VenueOrderId,
    pub generation: u64,
    pub current_cid: Option<ClientOrderId>,
    pub inventory_generation: u64,
    pub authority_hold_revision: u64,
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
    last_applied_execution_ts_ns: u64,
    late_after_normal_terminal: BTreeSet<ClientOrderId>,
    cancel_gone: BTreeMap<ClientOrderId, CancelGone>,
    cancel_none: BTreeSet<VenueOrderId>,
    fold_pos: u64,
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
            last_applied_execution_ts_ns: 0,
            late_after_normal_terminal: BTreeSet::new(),
            cancel_gone: BTreeMap::new(),
            cancel_none: BTreeSet::new(),
            fold_pos: 0,
        }
    }

    fn visit_replay_identity(
        record: &JournalRecord,
        visit: &mut impl FnMut(&LedgerId, Option<&ClientOrderId>),
    ) {
        match record {
            JournalRecord::OrderTxn(order) => visit(&order.ledger, Some(&order.client_order_id)),
            JournalRecord::ExecutionTxn(execution) => visit(
                &execution.ledger,
                execution.order.as_ref().map(|order| &order.client_order_id),
            ),
            JournalRecord::ObservationTxn(observation) => {
                visit(&observation.ledger, None);
                for member in &observation.members_after {
                    visit(&observation.ledger, Some(&member.client_order_id));
                }
            }
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
            | JournalRecord::AmendLineage { .. }
            | JournalRecord::UserOrderReceived { .. }
            | JournalRecord::UserOrderQuarantined { .. }
            | JournalRecord::OrderAuthorityHold { .. }
            | JournalRecord::UserOrderStaged { .. }
            | JournalRecord::OrderStateTxn(_)
            | JournalRecord::RestOrderProofTxn(_)
            | JournalRecord::OwnActionCid { .. } => {}
        }
    }

    /// Rebuild the already-owned core in source order, preserving a prior stop.
    /// Caller-owned staging/pending-wire replay remains an infallible post-step.
    /// The fresh constructor is a separate operation and remains unchanged.
    pub fn rebuild_in_place(&mut self, records: &[JournalRecord]) -> Result<(), HostError> {
        if records.is_empty() {
            return Ok(());
        }
        let mut found = None;
        super::persistence::for_each_journal_record(records, &mut |record| {
            if found.is_none() {
                Self::visit_replay_identity(record, &mut |ledger, _client| {
                    if found.is_none() {
                        found = Some(ledger.clone());
                    }
                });
            }
        });
        let Some(ledger) = found else {
            return Ok(());
        };
        if ledger.scope != self.ledger.scope {
            return Err(HostError::JournalScopeMismatch {
                expected: self.ledger.scope.clone(),
                found: ledger.scope,
            });
        }
        self.ledger = ledger.clone();
        self.inventory = MarketInventory::rebuild(ledger.clone(), records).map_err(|source| {
            self.stopped = true;
            HostError::Replay {
                stage: super::types::ReplayStage::Inventory,
                source,
            }
        })?;
        let mut rebuilt_registry =
            WireRegistry::rebuild(records, &self.inventory).map_err(|source| {
                self.stopped = true;
                HostError::Replay {
                    stage: super::types::ReplayStage::Registry,
                    source,
                }
            })?;
        rebuilt_registry.carry_ordinal_identity_from(&mut self.registry);
        self.registry = rebuilt_registry;
        let mut clients = BTreeSet::new();
        super::persistence::for_each_journal_record(records, &mut |record| {
            Self::visit_replay_identity(record, &mut |_ledger, client| {
                if let Some(client) = client {
                    clients.insert(client.clone());
                }
            });
        });
        let mut orders = BTreeMap::new();
        for client in clients {
            match rebuild_order_with_observations(&ledger, &client, records) {
                Ok(Some((state, ctx, cursor))) => {
                    orders.insert(client, OrderRow { state, ctx, cursor });
                }
                Ok(None) => {}
                Err(source) => {
                    self.stopped = true;
                    return Err(HostError::Replay {
                        stage: super::types::ReplayStage::Order(client),
                        source,
                    });
                }
            }
        }
        self.orders = orders;
        self.cancel_gone.clear();
        // Retain the native immediate-pre-fill predicate for this field.
        let (_timestamp, late) = Self::rebuild_execution_provenance(&ledger, records);
        self.late_after_normal_terminal = late;
        self.last_applied_execution_ts_ns = 0;
        for record in records {
            if let JournalRecord::ExecutionTxn(execution) = record {
                for outcome in &execution.executions {
                    if let Some(timestamp) = Self::applied_venue_time(outcome) {
                        self.last_applied_execution_ts_ns =
                            self.last_applied_execution_ts_ns.max(timestamp);
                    }
                }
            }
        }
        self.fold_pos = records.len() as u64;
        self.seed_proven = false;
        Ok(())
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
        let (last_applied_execution_ts_ns, late_after_normal_terminal) =
            Self::rebuild_execution_provenance(&ledger, records);
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
            last_applied_execution_ts_ns,
            late_after_normal_terminal,
            cancel_gone: BTreeMap::new(),
            cancel_none: BTreeSet::new(),
            fold_pos: records.len() as u64,
        })
    }

    fn normal_terminal(state: &OrderState) -> bool {
        match state {
            OrderState::Filled | OrderState::Canceled | OrderState::Terminal => true,
            OrderState::New
            | OrderState::SubmitPrepared
            | OrderState::SubmitStarted { .. }
            | OrderState::SubmitUnknown
            | OrderState::Accepted { .. }
            | OrderState::Partial { .. }
            | OrderState::CancelPending { .. }
            | OrderState::ReconcilePending { .. }
            | OrderState::ImmediateFillUnattributed { .. }
            | OrderState::ImmediateFillUnresolved
            | OrderState::UnknownNoMatch
            | OrderState::Halted { .. }
            | OrderState::SubmissionClosed { .. } => false,
        }
    }

    fn late_booked(outcome: &ExecutionOutcome) -> bool {
        match outcome.order {
            OrderDisposition::LateBooked => true,
            OrderDisposition::Attributed
            | OrderDisposition::Enriched
            | OrderDisposition::Duplicate
            | OrderDisposition::Ignored(_)
            | OrderDisposition::NotAttributedByOrder(_)
            | OrderDisposition::NoRow
            | OrderDisposition::Refused(_) => false,
        }
    }

    fn applied_venue_time(outcome: &ExecutionOutcome) -> Option<u64> {
        let ingested = match outcome.inventory {
            InventoryDisposition::Ingested(_) => true,
            InventoryDisposition::Duplicate
            | InventoryDisposition::Upgraded
            | InventoryDisposition::Conflict(_)
            | InventoryDisposition::Unchanged => false,
        };
        let venue_clock = match outcome.evidence.source {
            ExecutionSource::WsFill | ExecutionSource::RestFill => true,
            ExecutionSource::CreateResponseBackfill | ExecutionSource::VenueSeed => false,
        };
        if !ingested || !venue_clock {
            return None;
        }
        // Only a nonnegative venue timestamp may certify this boundary.
        // An invalid timestamp cannot substitute the host clock or advance L;
        // its evidence remains in the canonical outcome/journal for diagnosis.
        u64::try_from(outcome.evidence.ts_ns).ok()
    }

    fn rebuild_execution_provenance(
        ledger: &LedgerId,
        records: &[JournalRecord],
    ) -> (u64, BTreeSet<ClientOrderId>) {
        let update_normal =
            |normal: &mut BTreeSet<ClientOrderId>, client: &ClientOrderId, state: &OrderState| {
                if Self::normal_terminal(state) {
                    normal.insert(client.clone());
                } else {
                    normal.remove(client);
                }
            };
        let mut normal = BTreeSet::new();
        let mut late = BTreeSet::new();
        let mut timestamp = 0;
        for record in records {
            // Startup census, not event dispatch: only owner-bound normalized
            // transactions/observations establish these two provenances.
            match record {
                JournalRecord::OrderTxn(order) if &order.ledger == ledger => {
                    update_normal(&mut normal, &order.client_order_id, &order.state_after);
                }
                JournalRecord::ObservationTxn(observation) if &observation.ledger == ledger => {
                    for member in &observation.members_after {
                        update_normal(&mut normal, &member.client_order_id, &member.state_after);
                    }
                }
                JournalRecord::ExecutionTxn(execution) if &execution.ledger == ledger => {
                    for outcome in &execution.executions {
                        if let Some(value) = Self::applied_venue_time(outcome) {
                            timestamp = timestamp.max(value);
                        }
                    }
                    if let Some(order) = &execution.order {
                        if normal.contains(&order.client_order_id)
                            && execution.executions.iter().any(Self::late_booked)
                        {
                            late.insert(order.client_order_id.clone());
                        }
                        // Match live's immediate pre-fill state, not any earlier
                        // terminal. Update only after the late check; once set,
                        // the separate provenance bit survives later halts.
                        update_normal(&mut normal, &order.client_order_id, &order.state_after);
                    }
                }
                _ => {}
            }
        }
        (timestamp, late)
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

    fn held_terminal_identity(state: &WireState) -> Option<&ClientOrderId> {
        // S held_terminal_identity: last venue-bound member is used only for
        // strict TERMINAL REST identity, never to re-prove resting authority.
        if !state.authority_hold
            || state.proved_current_cid.is_some()
            || state.members.is_empty()
            || state
                .members
                .iter()
                .any(|member| member.role != MemberRole::Terminal)
            || state
                .amend_lineage
                .as_ref()
                .is_some_and(|lineage| match lineage.phase {
                    AmendPhase::HttpPending => true,
                    AmendPhase::AppliedNeedsObservation
                    | AmendPhase::Unknown
                    | AmendPhase::RestDue => false,
                })
        {
            return None;
        }
        state
            .members
            .iter()
            .max_by_key(|member| member.bind_pos)
            .map(|member| &member.cid)
    }

    pub fn authority_terminal_identity(&self, wire: &VenueOrderId) -> Option<&ClientOrderId> {
        self.registry
            .get(&self.ledger, wire)
            .and_then(Self::held_terminal_identity)
    }

    /// Snapshot canonical current identity/generations immediately before the
    /// caller's authority read. A newer hold cannot be cleared by an old token.
    pub fn authority_read_token(&self, wire: &VenueOrderId) -> Option<AuthorityReadToken> {
        let state = self.registry.get(&self.ledger, wire)?;
        let current_cid = state
            .members
            .iter()
            .find(|member| member.role == MemberRole::Current)
            .map(|member| member.cid.clone())
            .or_else(|| {
                // A newer hold may outlive a proved child's terminal claim.
                // Retain only that proved terminal identity; no resting proof
                // or earlier superseded CID is revived by this fallback.
                if state.authority_hold {
                    state
                        .proved_current_cid
                        .as_ref()
                        .filter(|cid| {
                            state.members.iter().any(|member| {
                                &member.cid == *cid && member.role == MemberRole::Terminal
                            })
                        })
                        .cloned()
                } else {
                    None
                }
            })
            .or_else(|| Self::held_terminal_identity(state).cloned());
        Some(AuthorityReadToken {
            wire: wire.clone(),
            generation: state.generation,
            current_cid,
            inventory_generation: self.inventory.generation(),
            authority_hold_revision: state.authority_hold_revision,
        })
    }

    pub fn authority_token_still_current(&self, token: &AuthorityReadToken) -> bool {
        self.authority_read_token(&token.wire).as_ref() == Some(token)
    }

    pub fn last_applied_execution_ts_ns(&self) -> u64 {
        self.last_applied_execution_ts_ns
    }

    /// The latest halt label alone is insufficient. This also requires a fill
    /// actually committed after THIS client's own normal terminal event.
    pub fn post_terminal_booked_after_normal_terminal(&self, client: &ClientOrderId) -> bool {
        self.late_after_normal_terminal.contains(client)
            && self.orders.get(client).is_some_and(|row| {
                matches!(
                    row.state,
                    OrderState::Halted {
                        reason: crate::lifecycle::HaltReason::PostTerminalFill
                    }
                )
            })
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

    /// Coverage quantity facts from the one canonical owner, including the
    /// source positive-DELETE residual. This is still not an admission decision.
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
                let (quantity, unresolvable) = if let Some(gone) = self.cancel_gone.get(client) {
                    // S risk_guards live_reducing_row_qty: unknown held quantity
                    // subtracts zero, preserving the whole known venue residual.
                    // This conservative cover never grants send/remaining proof.
                    (
                        gone.venue_filled
                            .saturating_sub(self.wire_held_quantity(&gone.wire).unwrap_or(0)),
                        false,
                    )
                } else {
                    self.member_working_capacity(client).unwrap_or((0, true))
                };
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
            self.commit_ingested_execution(&row, journal)?;
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
        call_effects: bool,
        fence: bool,
        stop_on_error: bool,
        journal: &mut dyn OmsJournal,
    ) -> Result<Vec<DurabilityAck>, CommitError> {
        let result = (|| {
            let mut acknowledgements = Vec::new();
            if let Some(record) = record {
                acknowledgements
                    .push(journal.apply_effects(&[Effect::AppendFsync(record.clone())])?);
            }
            if call_effects {
                acknowledgements.push(journal.apply_effects(effects)?);
            }
            if fence && record.is_some() {
                acknowledgements.push(journal.commit_now()?);
            }
            Ok(acknowledgements)
        })();
        result.map_err(|error| {
            if stop_on_error {
                self.stopped = true;
            }
            CommitError::Journal(error)
        })
    }

    fn fold_committed(
        &mut self,
        record: Option<&JournalRecord>,
        journal: &dyn OmsJournal,
        coordinate: FoldCoordinate,
    ) -> Result<(), CommitError> {
        if let Some(record) = record {
            let pos = match coordinate {
                FoldCoordinate::Transaction => self.fold_pos,
                FoldCoordinate::LastRecord => journal.len().saturating_sub(1) as u64,
                FoldCoordinate::JournalEnd => journal.len() as u64,
            };
            self.registry
                .fold(pos, record, &self.inventory)
                .map_err(|error| {
                    if matches!(coordinate, FoldCoordinate::Transaction) {
                        self.stopped = true;
                    }
                    CommitError::Registry(error)
                })?;
            if matches!(coordinate, FoldCoordinate::Transaction) {
                self.fold_pos = self.fold_pos.saturating_add(1);
            }
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
            record.is_some() || !matches!(event, OrderEvent::CloseSubmission { .. }),
            matches!(event, OrderEvent::CloseSubmission { .. }),
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
            .map_err(CommitError::Apply)?;
        self.fold_committed(record.as_ref(), journal, FoldCoordinate::Transaction)?;
        if !matches!(event, OrderEvent::CloseSubmission { .. }) {
            if let Some(JournalRecord::OrderTxn(order)) = record.as_ref() {
                if let BatchOutcome::Halt(reason) = &order.outcome {
                    return Err(CommitError::SemanticHalt {
                        event: Box::new(event.clone()),
                        reason: Box::new(reason.clone()),
                    });
                }
            }
        }
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
        let normal_before = owner
            .and_then(|client| self.orders.get(client))
            .is_some_and(|row| Self::normal_terminal(&row.state));
        let record = transaction.journal.clone();
        let effects = transaction.effects.clone();
        let mut durability =
            self.write_transition(record.as_ref(), &effects, true, false, false, journal)?;
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
            .map_err(CommitError::Apply)?;
        if normal_before && outcome.iter().any(Self::late_booked) {
            if let Some(client) = owner {
                self.late_after_normal_terminal.insert(client.clone());
            }
        }
        self.fold_committed(record.as_ref(), journal, FoldCoordinate::Transaction)?;
        for execution in &outcome {
            if let Some(timestamp) = Self::applied_venue_time(execution) {
                self.last_applied_execution_ts_ns =
                    self.last_applied_execution_ts_ns.max(timestamp);
            }
        }
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
        let wire = row.wire.clone();
        let terminal_status = match row.status {
            BackfillOrderStatus::Open | BackfillOrderStatus::Partial => false,
            BackfillOrderStatus::Filled | BackfillOrderStatus::Canceled => true,
        };
        let clients: BTreeSet<_> = self
            .registry
            .get(&self.ledger, &row.wire)
            .into_iter()
            .flat_map(|wire| &wire.members)
            .filter(|member| member.role != MemberRole::Terminal)
            .map(|member| member.cid.clone())
            .collect();
        let collected = clients
            .iter()
            .map(|client| {
                self.order_ref(client)
                    .ok_or_else(|| CommitError::Host(HostError::MissingClient(client.clone())))
            })
            .collect::<Result<Vec<_>, _>>();
        let members = match collected {
            Ok(members) => members,
            Err(error) => {
                self.stopped = true;
                return Err(error);
            }
        };
        let prepared = round.finish(&self.registry, &self.inventory, &members, row);
        let transaction = prepared.map_err(|error| {
            self.stopped = true;
            CommitError::Observation(error)
        })?;
        let record = transaction.journal.clone();
        let mut effects = transaction.effects.clone();
        let mut durability =
            self.write_transition(record.as_ref(), &effects, true, false, true, journal)?;
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
        self.fold_committed(record.as_ref(), journal, FoldCoordinate::Transaction)?;
        // Source complete_terminal_amend_lineage runs after observation/apply:
        // only terminal venue status, every canonical member terminal, no hold.
        let complete = terminal_status
            && self.registry.get(&self.ledger, &wire).is_some_and(|state| {
                state.amend_lineage.is_some()
                    && !state.authority_hold
                    && state
                        .members
                        .iter()
                        .all(|member| member.role == MemberRole::Terminal)
            });
        if complete {
            if let Some(receipt) = self.commit_clear_amend_lineage(&wire, journal)? {
                Self::append_commit(&mut effects, &mut durability, receipt);
            }
        }
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
        let record = self
            .registry
            .prepare_user_order_received(&self.ledger, &evidence.wire.clone(), evidence)
            .map_err(CommitError::OrderStateProof)?;
        let Some(record) = record else {
            return Ok(None);
        };
        let durability = self.write_transition(Some(&record), &[], false, true, false, journal)?;
        self.fold_committed(Some(&record), journal, FoldCoordinate::LastRecord)?;
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
        let transaction = self
            .registry
            .prepare_order_state_resting(&self.ledger, wire, &self.inventory)
            .map_err(CommitError::OrderStateProof)?;
        let record = JournalRecord::OrderStateTxn(Box::new(transaction.clone()));
        let durability = self.write_transition(Some(&record), &[], false, true, false, journal)?;
        self.registry
            .apply_order_state_resting(&transaction, &self.inventory)
            .map_err(CommitError::OrderStateProof)?;
        Ok(CommitReceipt {
            outcome: transaction,
            effects: Vec::new(),
            durability,
        })
    }

    /// Canonical per-wire hold, distinct from the channel/position hold. Append
    /// and fence before folding; errors preserve the existing stop latch.
    pub fn commit_wire_authority_hold(
        &mut self,
        wire: &VenueOrderId,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<()>, CommitError> {
        let record = JournalRecord::OrderAuthorityHold {
            ledger: self.ledger.clone(),
            wire: wire.clone(),
        };
        let durability = self.write_transition(Some(&record), &[], false, true, false, journal)?;
        self.fold_committed(Some(&record), journal, FoldCoordinate::LastRecord)?;
        Ok(CommitReceipt {
            outcome: (),
            effects: Vec::new(),
            durability,
        })
    }

    /// The existing canonical REST proof after caller identity/page validation.
    /// This commits a RestOrderProofTxn, never a fabricated WS frame, timestamp
    /// or subscription identity. The canonical preparer retains exact errors.
    pub fn commit_rest_order_proof(
        &mut self,
        wire: &VenueOrderId,
        client: &ClientOrderId,
        filled: u64,
        remaining: u64,
        initial: u64,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<RestOrderProofTxnRecord>, CommitError> {
        let transaction = self
            .registry
            .prepare_rest_order_proof(
                &self.ledger,
                wire,
                client,
                filled,
                remaining,
                initial,
                &self.inventory,
            )
            .map_err(CommitError::OrderStateProof)?;
        let record = JournalRecord::RestOrderProofTxn(Box::new(transaction.clone()));
        let durability = self.write_transition(Some(&record), &[], false, true, false, journal)?;
        self.registry
            .apply_rest_order_proof(&transaction, &self.inventory)
            .map_err(CommitError::OrderStateProof)?;
        Ok(CommitReceipt {
            outcome: transaction,
            effects: Vec::new(),
            durability,
        })
    }

    /// Source identity-bound REST observation/proof composition. row's CID is
    /// the actual returned CID, not an inferred member. The caller has checked
    /// raw market/index/subaccount and supplies real exhaustive/parse metadata.
    /// A stale token/foreign row/wrong CID rejects before ALL journal operations.
    pub fn commit_rest_order_page(
        &mut self,
        mut row: RowEvidence,
        initial: u64,
        token: &AuthorityReadToken,
        page_outcomes: &[ExecutionOutcome],
        page: PageMeta,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<ObservationOutcome>, CommitError> {
        self.ensure_active()?;
        if row.ledger != self.ledger {
            return Err(CommitError::Observation(RoundError::ForeignRow));
        }
        if !self.authority_token_still_current(token) || token.wire != row.wire {
            return Err(CommitError::OrderStateProof(
                OrderStateProofError::GenerationChanged,
            ));
        }
        if token.current_cid.is_none() || row.client_order_id != token.current_cid {
            return Err(CommitError::OrderStateProof(
                OrderStateProofError::WrongCurrent,
            ));
        }
        // S runtime.rs:3503-3511: this fallback is for terminal evidence only.
        // A lagged resting page must retain the hold, never install Known.
        let resting = match row.status {
            BackfillOrderStatus::Open | BackfillOrderStatus::Partial => true,
            BackfillOrderStatus::Filled | BackfillOrderStatus::Canceled => false,
        };
        if resting
            && token
                .current_cid
                .as_ref()
                .is_some_and(|client| self.authority_terminal_identity(&row.wire) == Some(client))
        {
            return Err(CommitError::OrderStateProof(
                OrderStateProofError::InvalidTransition,
            ));
        }
        row.marker = WireMarker(token.generation);
        let wire = row.wire.clone();
        let client = row.client_order_id.clone().expect("validated actual CID");
        let status = row.status;
        let filled = row.filled;
        let remaining = row.remaining;
        let mut round = ObservationRound::begin(self.ledger.clone(), wire.clone());
        for outcome in page_outcomes {
            // Source page outcomes come from its ledger-validated execution
            // ingestion. Preserve that owner boundary on this public wrapper;
            // ObservationRound::record itself filters wire identity only.
            if outcome.evidence.scope != self.ledger.scope
                || outcome.evidence.market != self.ledger.market
            {
                return Err(CommitError::Observation(RoundError::ForeignRow));
            }
            match round.record(outcome) {
                Ok(()) => {}
                // S observe_rest_order_page_with_identity ignores a foreign
                // wire outcome. It never contributes a covered fill;
                // canonical finish still checks actual held IDs and quantities.
                Err(RoundError::ForeignRow) => {}
                Err(error @ RoundError::MembersInvalid { .. }) => {
                    return Err(CommitError::Observation(error))
                }
            }
        }
        let complete_page = page.exhaustive && page.parse_failures == 0;
        round.page(page);
        let mut result = self.commit_observation(round, row, journal)?;
        let active_amend = self
            .registry
            .get(&self.ledger, &wire)
            .and_then(|state| state.amend_lineage.as_ref())
            .is_some_and(|lineage| lineage.new_cid == client);
        if complete_page
            && result.outcome
                == (ObservationOutcome::Consistent {
                    remaining_fresh: false,
                })
            && status == BackfillOrderStatus::Open
            && active_amend
        {
            let receipt =
                self.commit_rest_order_proof(&wire, &client, filled, remaining, initial, journal)?;
            Self::append_commit(&mut result.effects, &mut result.durability, receipt);
        }
        Ok(result)
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
        let record = self
            .registry
            .prepare_amend_lineage(&self.ledger, wire, lineage)
            .map_err(CommitError::OrderStateProof)?;
        let durability = self.write_transition(Some(&record), &[], false, true, false, journal)?;
        self.fold_committed(Some(&record), journal, FoldCoordinate::JournalEnd)?;
        Ok(CommitReceipt {
            outcome: (),
            effects: Vec::new(),
            durability,
        })
    }

    /// Clear the canonical lineage only after a caller has established its
    /// resolution. The returned former lineage supplies owner identity to the
    /// caller's ordered notification; no clock or resolution source is guessed.
    pub fn commit_clear_amend_lineage(
        &mut self,
        wire: &VenueOrderId,
        journal: &mut dyn OmsJournal,
    ) -> Result<Option<CommitReceipt<AmendLineage>>, CommitError> {
        let before = self
            .registry
            .get(&self.ledger, wire)
            .and_then(|state| state.amend_lineage.clone());
        let Some(before) = before else {
            return Ok(None);
        };
        let record = JournalRecord::AmendLineage {
            ledger: self.ledger.clone(),
            wire: wire.clone(),
            lineage: None,
        };
        let durability = self.write_transition(Some(&record), &[], false, true, false, journal)?;
        self.fold_committed(Some(&record), journal, FoldCoordinate::JournalEnd)?;
        Ok(Some(CommitReceipt {
            outcome: before,
            effects: Vec::new(),
            durability,
        }))
    }

    fn append_commit<T>(
        effects: &mut Vec<Effect>,
        durability: &mut Vec<DurabilityAck>,
        mut receipt: CommitReceipt<T>,
    ) {
        effects.append(&mut receipt.effects);
        durability.append(&mut receipt.durability);
    }

    fn retire_superseded_old(
        &mut self,
        old: &ClientOrderId,
        wire: &VenueOrderId,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<()>, CommitError> {
        let mut result = CommitReceipt {
            outcome: (),
            effects: Vec::new(),
            durability: Vec::new(),
        };
        let Some(row) = self.orders.get(old) else {
            return Ok(result);
        };
        if row.state.is_terminal_or_halt() || row.state.is_submission_closed() {
            return Ok(result);
        }
        let filled = row.ctx.attributed_fill_qty;
        if matches!(
            row.state,
            OrderState::Accepted { .. } | OrderState::Partial { .. }
        ) {
            let receipt = self.commit_event(old, &OrderEvent::CancelRequested, journal)?;
            Self::append_commit(&mut result.effects, &mut result.durability, receipt);
            let receipt = self.commit_event(
                old,
                &OrderEvent::CancelOutcome(CancelOutcome::Canceled),
                journal,
            )?;
            Self::append_commit(&mut result.effects, &mut result.durability, receipt);
        }
        if self.orders.get(old).is_some_and(|row| {
            matches!(
                row.state,
                OrderState::CancelPending { .. } | OrderState::ReconcilePending { .. }
            )
        }) {
            let receipt = self.commit_event(
                old,
                &OrderEvent::ReconcileResult {
                    status: BackfillOrderStatus::Canceled,
                    venue_order_id: Some(wire.clone()),
                    filled_qty: filled,
                    remaining_qty: 0,
                    fills: Vec::new(),
                    authority_complete: true,
                },
                journal,
            )?;
            Self::append_commit(&mut result.effects, &mut result.durability, receipt);
        }
        Ok(result)
    }

    /// A terminal frame creates no fill. Only the same-wire row whose actual
    /// attributed quantity equals the venue cumulative can claim its terminal.
    /// A terminal amend child retires the old member BEFORE lineage clearing.
    pub fn commit_terminal_frame_if_complete(
        &mut self,
        wire: &VenueOrderId,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<bool>, CommitError> {
        self.ensure_active()?;
        let mut result = CommitReceipt {
            outcome: false,
            effects: Vec::new(),
            durability: Vec::new(),
        };
        let Some(frame) = self
            .registry
            .get(&self.ledger, wire)
            .and_then(|state| state.latest_user_order.clone())
        else {
            return Ok(result);
        };
        let executed = match frame.status {
            UserOrderStatus::Canceled => false,
            UserOrderStatus::Executed => true,
            UserOrderStatus::Resting | UserOrderStatus::Unknown => return Ok(result),
        };
        let Some(row) = self.orders.get(&frame.client_order_id) else {
            return Ok(result);
        };
        if !matches!(
            row.state,
            OrderState::Accepted { .. }
                | OrderState::Partial { .. }
                | OrderState::CancelPending { .. }
                | OrderState::ReconcilePending { .. }
        ) || row.ctx.venue_order_id.as_ref() != Some(wire)
            || row.ctx.attributed_fill_qty != frame.filled
        {
            return Ok(result);
        }
        let receipt = self.commit_event(
            &frame.client_order_id,
            &OrderEvent::UserOrderTerminalClaim {
                venue_order_id: wire.clone(),
                filled: frame.filled,
                executed,
            },
            journal,
        )?;
        Self::append_commit(&mut result.effects, &mut result.durability, receipt);
        result.outcome = true;
        if self
            .orders
            .get(&frame.client_order_id)
            .is_some_and(|row| Self::normal_terminal(&row.state))
        {
            let lineage = self
                .registry
                .get(&self.ledger, wire)
                .and_then(|state| state.amend_lineage.clone())
                .filter(|lineage| lineage.new_cid == frame.client_order_id);
            if let Some(lineage) = lineage {
                let receipt = self.retire_superseded_old(&lineage.old_cid, wire, journal)?;
                Self::append_commit(&mut result.effects, &mut result.durability, receipt);
                if let Some(receipt) = self.commit_clear_amend_lineage(wire, journal)? {
                    Self::append_commit(&mut result.effects, &mut result.durability, receipt);
                }
            }
        }
        Ok(result)
    }

    /// Reprove a stored resting frame when the canonical execution ledger has
    /// caught up. A not-yet-valid proof is a no-op, never synthetic authority;
    /// commit_resting_proof still exposes the exact typed error on explicit use.
    pub fn commit_resting_reproof_if_ready(
        &mut self,
        wire: &VenueOrderId,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<bool>, CommitError> {
        self.ensure_active()?;
        let mut result = CommitReceipt {
            outcome: false,
            effects: Vec::new(),
            durability: Vec::new(),
        };
        let Some(state) = self.registry.get(&self.ledger, wire) else {
            return Ok(result);
        };
        if !state
            .latest_user_order
            .as_ref()
            .is_some_and(|frame| frame.status == UserOrderStatus::Resting)
        {
            return Ok(result);
        }
        // Source ingestion retries this proof only when it is valid. A failed
        // preparation changes nothing; it remains independently queryable via
        // the canonical preparer and explicitly committing primitive.
        if self
            .registry
            .prepare_order_state_resting(&self.ledger, wire, &self.inventory)
            .is_err()
        {
            return Ok(result);
        }
        let before = state.amend_lineage.clone();
        let receipt = self.commit_resting_proof(wire, journal)?;
        Self::append_commit(&mut result.effects, &mut result.durability, receipt);
        result.outcome = true;
        if let Some(lineage) = before {
            let proven =
                self.registry.get(&self.ledger, wire).is_some_and(|state| {
                    state.proved_current_cid.as_ref() == Some(&lineage.new_cid)
                }) || self.orders.get(&lineage.new_cid).is_some_and(|row| {
                    row.ctx.attributed_fill_qty > 0 || matches!(row.state, OrderState::Filled)
                });
            if proven {
                let receipt = self.retire_superseded_old(&lineage.old_cid, wire, journal)?;
                Self::append_commit(&mut result.effects, &mut result.durability, receipt);
            }
        }
        Ok(result)
    }

    fn amend_child_proven(&self, child: &ClientOrderId, wire: &VenueOrderId) -> bool {
        self.registry
            .get(&self.ledger, wire)
            .is_some_and(|state| state.proved_current_cid.as_ref() == Some(child))
            || self.orders.get(child).is_some_and(|row| {
                row.ctx.attributed_fill_qty > 0 || matches!(row.state, OrderState::Filled)
            })
    }

    fn clear_terminal_lineages(
        &mut self,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<()>, CommitError> {
        let mut result = CommitReceipt {
            outcome: (),
            effects: Vec::new(),
            durability: Vec::new(),
        };
        let wires: Vec<_> = self
            .orders
            .values()
            .filter_map(|row| row.ctx.venue_order_id.clone())
            .collect();
        let mut seen = BTreeSet::new();
        for wire in wires {
            if !seen.insert(wire.clone()) {
                continue;
            }
            let Some(state) = self.registry.get(&self.ledger, &wire) else {
                continue;
            };
            let Some(lineage) = state.amend_lineage.as_ref() else {
                continue;
            };
            let after_http = match lineage.phase {
                AmendPhase::HttpPending => false,
                AmendPhase::AppliedNeedsObservation | AmendPhase::Unknown | AmendPhase::RestDue => {
                    true
                }
            };
            let child_unknown = lineage.phase == AmendPhase::Unknown
                && self
                    .orders
                    .get(&lineage.new_cid)
                    .is_some_and(|row| matches!(row.state, OrderState::SubmitUnknown))
                && self.amend_child_proven(&lineage.new_cid, &wire);
            if after_http
                && !state.authority_hold
                && state
                    .members
                    .iter()
                    .all(|member| member.role == MemberRole::Terminal)
                && !child_unknown
            {
                if let Some(receipt) = self.commit_clear_amend_lineage(&wire, journal)? {
                    Self::append_commit(&mut result.effects, &mut result.durability, receipt);
                }
            }
        }
        Ok(result)
    }

    fn resolve_fully_filled_unknown_child(
        &mut self,
        child: &ClientOrderId,
        wire: &VenueOrderId,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<()>, CommitError> {
        let empty = || CommitReceipt {
            outcome: (),
            effects: Vec::new(),
            durability: Vec::new(),
        };
        let Some(row) = self.orders.get(child) else {
            return Ok(empty());
        };
        if !matches!(row.state, OrderState::SubmitUnknown)
            || row.ctx.qty == 0
            || row.ctx.attributed_fill_qty < row.ctx.qty
            || row
                .ctx
                .venue_order_id
                .as_ref()
                .is_some_and(|bound| bound != wire)
        {
            return Ok(empty());
        }
        let record = BackfillOrderRecord {
            other_members_filled: 0,
            client_order_id: child.clone(),
            venue_order_id: wire.clone(),
            status: BackfillOrderStatus::Filled,
            filled_qty: row.ctx.qty,
            remaining_qty: 0,
            fills: Vec::new(),
        };
        self.commit_event(
            child,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![record],
            },
            journal,
        )
    }

    fn resolve_child_after_old_filled(
        &mut self,
        wire: &VenueOrderId,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<()>, CommitError> {
        let empty = || CommitReceipt {
            outcome: (),
            effects: Vec::new(),
            durability: Vec::new(),
        };
        let Some(lineage) = self
            .registry
            .get(&self.ledger, wire)
            .and_then(|state| state.amend_lineage.clone())
        else {
            return Ok(empty());
        };
        match lineage.phase {
            AmendPhase::Unknown | AmendPhase::RestDue => {}
            AmendPhase::HttpPending | AmendPhase::AppliedNeedsObservation => return Ok(empty()),
        }
        let (Some(old), Some(new)) = (
            self.orders.get(&lineage.old_cid),
            self.orders.get(&lineage.new_cid),
        ) else {
            return Ok(empty());
        };
        if !matches!(new.state, OrderState::SubmitUnknown)
            || new.ctx.venue_order_id.is_some()
            || new.ctx.attributed_fill_qty != 0
            || new.ctx.qty == 0
            || new.ctx.qty != lineage.declared_remaining
            || old.ctx.qty == 0
            || old
                .ctx
                .venue_order_id
                .as_ref()
                .is_some_and(|bound| bound != wire)
            || old.ctx.attributed_fill_qty < old.ctx.qty.max(lineage.declared_remaining)
        {
            return Ok(empty());
        }
        let mut result = self.commit_event(
            &lineage.new_cid,
            &OrderEvent::CloseSubmission {
                reason: SubmissionCloseReason::ExhaustiveAbsent,
            },
            journal,
        )?;
        let receipt = self.clear_terminal_lineages(journal)?;
        Self::append_commit(&mut result.effects, &mut result.durability, receipt);
        Ok(result)
    }

    /// Source route (ii): a successful positive DELETE without a live total
    /// remains unresolved while any bound row is nonterminal. Transient only;
    /// rebuilding requires fresh venue evidence, as in the source owner.
    pub fn cancel_without_proof_member(&self, wire: &VenueOrderId) -> bool {
        self.cancel_none.contains(wire)
            && self.orders.values().any(|row| {
                row.ctx.venue_order_id.as_ref() == Some(wire) && !row.state.is_terminal_or_halt()
            })
    }

    pub fn deficit_member_wires(&self) -> Vec<&VenueOrderId> {
        self.orders
            .values()
            .filter_map(|row| row.ctx.venue_order_id.as_ref())
            .filter(|wire| self.wire_known_deficit(wire) || self.cancel_without_proof_member(wire))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn positive_cancel_residual(&self, client: &ClientOrderId) -> Option<u64> {
        let row = self.orders.get(client)?;
        if row.state.is_terminal_or_halt() || row.state.is_submission_closed() {
            return None;
        }
        let gone = self.cancel_gone.get(client)?;
        // Unknown held quantity subtracts zero, retaining the known residual
        // rather than declaring a fill booked. Canonical inventory remains exact.
        Some(
            gone.venue_filled
                .saturating_sub(self.wire_held_quantity(&gone.wire).unwrap_or(0)),
        )
    }

    fn commit_cancel_observation(
        &mut self,
        wire: &VenueOrderId,
        filled: u64,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<ObservationOutcome>, CommitError> {
        let mut round = ObservationRound::begin(self.ledger.clone(), wire.clone());
        round.page(PageMeta {
            exhaustive: true,
            parse_failures: 0,
        });
        let row = RowEvidence {
            ledger: self.ledger.clone(),
            wire: wire.clone(),
            status: BackfillOrderStatus::Canceled,
            filled,
            remaining: 0,
            client_order_id: None,
            marker: self.registry.marker(&self.ledger, wire),
        };
        self.commit_observation(round, row, journal)
    }

    /// Empty inventory and successful cancel do not prove fill completeness.
    /// A known venue deficit refuses the source 0/0 synthesis with no journal op.
    pub fn commit_canceled_empty_if_no_deficit(
        &mut self,
        wire: &VenueOrderId,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<ObservationOutcome>, CommitError> {
        self.ensure_active()?;
        if self.wire_known_deficit(wire) {
            return Ok(CommitReceipt {
                outcome: ObservationOutcome::NotCovered,
                effects: Vec::new(),
                durability: Vec::new(),
            });
        }
        self.commit_cancel_observation(wire, 0, journal)
    }

    /// Source Canceled response composition. The caller supplies actual DELETE
    /// classification/reduced-by evidence. Capture the current total and held
    /// inventory BEFORE cancel invalidates the proof; never synthesize a fill.
    pub fn commit_canceled_response(
        &mut self,
        client: &ClientOrderId,
        wire: &VenueOrderId,
        delete_reduced_positive: bool,
        cancel_not_found: bool,
        reduced_by: Option<u64>,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<ObservationOutcome>, CommitError> {
        self.ensure_active()?;
        let mut result = CommitReceipt {
            outcome: ObservationOutcome::NotCovered,
            effects: Vec::new(),
            durability: Vec::new(),
        };
        let row = self
            .orders
            .get(client)
            .ok_or_else(|| CommitError::Host(HostError::MissingClient(client.clone())))?;
        if row.state.is_terminal_or_halt() || row.state.is_submission_closed() {
            return Ok(result);
        }
        let positive = delete_reduced_positive && !cancel_not_found;
        let gone_pre = if positive {
            reduced_by.map(|reduced| (reduced, self.wire_total_at_proof(wire)))
        } else {
            None
        };
        let held_pre = if positive {
            self.wire_held_quantity(wire)
        } else {
            None
        };
        let receipt = self.commit_event(
            client,
            &OrderEvent::CancelOutcome(CancelOutcome::Canceled),
            journal,
        )?;
        Self::append_commit(&mut result.effects, &mut result.durability, receipt);
        if positive {
            let lineage = self
                .registry
                .get(&self.ledger, wire)
                .and_then(|state| state.amend_lineage.clone())
                .filter(|lineage| &lineage.new_cid == client);
            if let Some(lineage) = lineage {
                let receipt = self.retire_superseded_old(&lineage.old_cid, wire, journal)?;
                Self::append_commit(&mut result.effects, &mut result.durability, receipt);
            }
        }
        let bound = self
            .orders
            .get(client)
            .and_then(|row| row.ctx.venue_order_id.clone());
        if let Some(bound) = bound {
            let shared = self.orders.iter().any(|(other, row)| {
                other != client
                    && !row.state.is_terminal_or_halt()
                    && row.ctx.venue_order_id.as_ref() == Some(&bound)
            });
            let held_empty = match self.registry.get(&self.ledger, &bound) {
                Some(state) => state.held.is_empty(),
                None => true,
            };
            if !shared && held_empty {
                // S book_cancel_terminals: missing held baseline uses zero only
                // to preserve a larger venue fill obligation, never book inventory.
                let receipt = match gone_pre {
                    Some((reduced, Some(total)))
                        if reduced <= total && total - reduced > held_pre.unwrap_or(0) =>
                    {
                        let attributed = self
                            .orders
                            .get(client)
                            .expect("committed client exists")
                            .ctx
                            .attributed_fill_qty;
                        self.commit_cancel_observation(
                            &bound,
                            (total - reduced).max(attributed),
                            journal,
                        )?
                    }
                    Some((_, None)) if !self.wire_known_deficit(&bound) => {
                        self.cancel_none.insert(bound.clone());
                        CommitReceipt {
                            outcome: ObservationOutcome::NotCovered,
                            effects: Vec::new(),
                            durability: Vec::new(),
                        }
                    }
                    Some(_) | None => self.commit_canceled_empty_if_no_deficit(&bound, journal)?,
                };
                result.outcome = receipt.outcome;
                Self::append_commit(&mut result.effects, &mut result.durability, receipt);
            }
        }
        if let Some((reduced, Some(total))) = gone_pre {
            let live = self.orders.get(client).is_some_and(|row| {
                !row.state.is_terminal_or_halt() && !row.state.is_submission_closed()
            });
            if reduced <= total && live {
                self.cancel_gone.insert(
                    client.clone(),
                    CancelGone {
                        venue_filled: total - reduced,
                        wire: wire.clone(),
                    },
                );
            }
        }
        Ok(result)
    }

    /// Ordered source ingestion tail: commit/route the fill, terminal frame,
    /// resting reproof, Unknown-child resolution, then superseded-old settlement.
    /// Transport checks, fee queues and external notifications remain caller work.
    pub fn commit_ingested_execution(
        &mut self,
        evidence: &ExecutionEvidence,
        journal: &mut dyn OmsJournal,
    ) -> Result<CommitReceipt<Vec<ExecutionOutcome>>, CommitError> {
        let mut result = self.commit_routed_execution(evidence, journal)?;
        if let Some(wire) = evidence.venue_order_id.as_ref() {
            let receipt = self.commit_terminal_frame_if_complete(wire, journal)?;
            Self::append_commit(&mut result.effects, &mut result.durability, receipt);
        }
        let wire = evidence.venue_order_id.clone().or_else(|| {
            evidence
                .claimed_client_order_id
                .as_ref()
                .and_then(|client| self.orders.get(client))
                .and_then(|row| row.ctx.venue_order_id.clone())
        });
        if let Some(wire) = wire {
            let receipt = self.commit_resting_reproof_if_ready(&wire, journal)?;
            Self::append_commit(&mut result.effects, &mut result.durability, receipt);
        }
        if let (Some(client), Some(wire)) = (
            evidence.claimed_client_order_id.as_ref(),
            evidence.venue_order_id.as_ref(),
        ) {
            let receipt = self.resolve_fully_filled_unknown_child(client, wire, journal)?;
            Self::append_commit(&mut result.effects, &mut result.durability, receipt);
            let is_old = self
                .registry
                .get(&self.ledger, wire)
                .and_then(|state| state.amend_lineage.as_ref())
                .is_some_and(|lineage| &lineage.old_cid == client);
            if is_old {
                let receipt = self.resolve_child_after_old_filled(wire, journal)?;
                Self::append_commit(&mut result.effects, &mut result.durability, receipt);
            }
            let lineage = self
                .registry
                .get(&self.ledger, wire)
                .and_then(|state| state.amend_lineage.clone())
                .filter(|lineage| &lineage.new_cid == client);
            if let Some(lineage) = lineage {
                if self.amend_child_proven(client, wire) {
                    let receipt = self.retire_superseded_old(&lineage.old_cid, wire, journal)?;
                    Self::append_commit(&mut result.effects, &mut result.durability, receipt);
                }
            }
        }
        Ok(result)
    }
}
