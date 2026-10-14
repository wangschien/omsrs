use std::collections::{BTreeMap, BTreeSet};

use crate::execution::{
    prepare_execution, prepare_execution_batch, prepare_order_event, ExecutionEvidence,
    ExecutionOutcome, ExecutionTransaction, LedgerId, MarketInventory, OrderCursor, OrderRef,
    OrderTarget, OrderTransaction,
};
use crate::lifecycle::{
    ClientOrderId, Effect, JournalRecord, OrderCtx, OrderEvent, OrderState, VenueOrderId,
};
use crate::observation::{
    rebuild_order_with_observations, MemberRole, ObservationOutcome, ObservationRound,
    OrderStateTxnRecord, Readiness, ReadinessError, RowEvidence, UserOrderEvidence, WireRegistry,
    WorkingView,
};

use super::identity::{LocalOrdinal, OrdinalView};
use super::persistence::OmsJournal;
use super::recovery::RecoveryCadence;
use super::risk::{AdmissionReasons, AuthorityHold, BookGuard, StatusGuard};
use super::types::{
    CommitError, CommitReceipt, DurabilityAck, HostError, OrderHostView, OrderView,
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
            let pos = if matches!(
                record,
                JournalRecord::OrderAuthorityHold { .. } | JournalRecord::UserOrderReceived { .. }
            ) {
                journal.len().saturating_sub(1) as u64
            } else {
                journal.len() as u64
            };
            self.registry
                .fold(pos, record, &self.inventory)
                .map_err(|error| {
                    self.stopped = true;
                    CommitError::Registry(error)
                })?;
        }
        Ok(())
    }

    /// Prepare and commit under the exclusive owner borrow. No externally
    /// mutable prepared transaction can cross the journal boundary.
    /// Submission closure is explicitly fenced before its canonical state is
    /// applied. Other events retain the journal's configured append policy.
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
        let durability = self.write_transition(
            record.as_ref(),
            &effects,
            matches!(event, OrderEvent::CloseSubmission { .. }),
            journal,
        )?;
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
        let durability = self.write_transition(record.as_ref(), &effects, false, journal)?;
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
}
