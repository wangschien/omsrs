//! OMS-A scoped order/execution transactions and durable market inventory.
//!
//! Pure API only: no I/O. Shell owns durability / critical section (work order §2.6).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::lifecycle::{
    apply_event, event_name, halt_with_reason, is_restart_frozen, BackfillOrderRecord,
    ClientOrderId, Effect, FillId, FillPayload, FillRecord, HaltReason, JournalRecord, OrderCtx,
    OrderEvent, OrderState, RejectReason, Side, TransitionOutcome, VenueOrderId,
};

// ─── §2.2 types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ExecutionScope(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LedgerId {
    pub scope: ExecutionScope,
    pub market: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionSource {
    WsFill,
    RestFill,
    CreateResponseBackfill,
    VenueSeed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionEvidence {
    pub scope: ExecutionScope,
    pub market: String,
    pub execution_id: FillId,
    pub side: Side,
    pub qty: u64,
    pub price_cents: u64,
    pub ts_ns: i64,
    pub venue_order_id: Option<VenueOrderId>,
    pub claimed_client_order_id: Option<ClientOrderId>,
    pub fee_cents: Option<u64>,
    pub source: ExecutionSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderCursor {
    pub seq: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct OrderRef<'a> {
    pub ledger: &'a LedgerId,
    pub cursor: OrderCursor,
    pub state: &'a OrderState,
    pub ctx: &'a OrderCtx,
}

pub struct OrderTarget<'a> {
    pub ledger: &'a LedgerId,
    pub state: &'a mut OrderState,
    pub ctx: &'a mut OrderCtx,
    pub cursor: &'a mut OrderCursor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryEntry {
    pub side: Side,
    pub qty: u64,
    pub price_cents: u64,
    pub first_seen_ts_ns: i64,
    pub venue_order_id: Option<VenueOrderId>,
    pub claimed_client_order_id: Option<ClientOrderId>,
    pub owner: Option<ClientOrderId>,
    pub fee_cents: Option<u64>,
    pub source: ExecutionSource,
    pub provenance: InventoryProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BatchOutcome {
    Accept,
    Halt(HaltReason),
    Reject(RejectReason),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InventoryProvenance {
    Attributed,
    Recovered,
    LateBooked,
    NotAttributedByOrder(BatchOutcome),
    Refused(RefusalReason),
    NoRow,
    RowForeign,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictWitness {
    Entry(InventoryEntry),
    OrderApplied {
        client_order_id: ClientOrderId,
        payload: FillPayload,
    },
    Both {
        entry: InventoryEntry,
        client_order_id: ClientOrderId,
        payload: FillPayload,
    },
    Staged(InventoryEntry),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictClass {
    Core,
    Ownership,
    Fee,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictFact {
    pub execution_id: FillId,
    pub class: ConflictClass,
    pub witness: ConflictWitness,
    pub evidence: ExecutionEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketInventory {
    ledger: LedgerId,
    generation: u64,
    entries: BTreeMap<FillId, InventoryEntry>,
    conflicts: BTreeMap<FillId, ConflictFact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryView {
    pub net: i128,
    pub executions: usize,
    pub health: InventoryHealth,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InventoryHealth {
    Clean,
    Conflicted {
        core: usize,
        ownership: usize,
        fee: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChecksumDiagnostic {
    Match,
    Mismatch { ledger: i128, observed: i128 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderDisposition {
    Attributed,
    Enriched,
    Duplicate,
    Ignored(IgnoreReason),
    LateBooked,
    NotAttributedByOrder(BatchOutcome),
    NoRow,
    Refused(RefusalReason),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefusalReason {
    DuplicateOtherOwner { owner: ClientOrderId },
    ConflictingExecution(ConflictClass),
    FeeConflict,
    SideMismatch { ctx_side: Side, evidence_side: Side },
    RowForeign {
        row_ledger: LedgerId,
        row_ctx_market: String,
        inventory_ledger: LedgerId,
    },
    InBatchDuplicate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InventoryDisposition {
    Ingested(InventoryProvenance),
    Duplicate,
    Upgraded,
    Conflict(ConflictClass),
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IgnoreReason {
    ZeroQty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionOutcome {
    pub evidence: ExecutionEvidence,
    pub order: OrderDisposition,
    pub inventory: InventoryDisposition,
    pub entry_after: Option<InventoryEntry>,
    pub conflict: Option<ConflictFact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderTxnRecord {
    pub ledger: LedgerId,
    pub client_order_id: ClientOrderId,
    pub seq: u64,
    pub event: String,
    pub outcome: BatchOutcome,
    pub core_records: Vec<JournalRecord>,
    pub state_after: OrderState,
    pub ctx_after: OrderCtx,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionTxnRecord {
    pub ledger: LedgerId,
    pub inventory_generation_before: u64,
    pub executions: Vec<ExecutionOutcome>,
    pub order: Option<OrderTxnRecord>,
}

#[derive(Debug)]
pub struct OrderTransaction {
    pub journal: Option<JournalRecord>,
    pub effects: Vec<Effect>,
    pub rejected: Option<RejectReason>,
    prepared_ledger: LedgerId,
    expected_cursor: OrderCursor,
    expected_state: OrderState,
    expected_ctx: OrderCtx,
    state_after: OrderState,
    ctx_after: OrderCtx,
    advance_cursor: bool,
    next_seq: u64,
}

#[derive(Debug)]
pub struct ExecutionTransaction {
    pub outcomes: Vec<ExecutionOutcome>,
    pub journal: Option<JournalRecord>,
    pub effects: Vec<Effect>,
    prepared_inventory_ledger: LedgerId,
    expected_generation: u64,
    /// Ledger the row was prepared under (Some iff prepare received Some(OrderRef)).
    prepared_order_ledger: Option<LedgerId>,
    expected_cursor: Option<OrderCursor>,
    expected_state: Option<OrderState>,
    expected_ctx: Option<OrderCtx>,
    order_state_after: Option<OrderState>,
    order_ctx_after: Option<OrderCtx>,
    advance_cursor: bool,
    next_seq: u64,
    /// Planned inventory mutations (applied on `apply`).
    planned_entries: BTreeMap<FillId, InventoryEntry>,
    planned_conflicts: BTreeMap<FillId, ConflictFact>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareError {
    ForeignLedger {
        inventory: LedgerId,
        evidence: LedgerId,
    },
    FillsMustUseExecutionApi {
        event: String,
        fills: usize,
    },
    EvidenceFillMismatch {
        position: usize,
        detail: String,
    },
    MixedLedgerBatch,
    OrderRefInconsistent {
        ledger_market: String,
        ctx_market: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyMismatch {
    LedgerIdentity,
    InventoryGeneration { expected: u64, found: u64 },
    OrderCursor { expected: u64, found: u64 },
    OrderStateOrCtx,
    OrderTarget { expected: bool, found: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebuildError {
    SeqGap {
        client_order_id: ClientOrderId,
        expected: u64,
        found: u64,
    },
    Divergent {
        client_order_id: ClientOrderId,
        seq: u64,
    },
    DuplicateExecutionRecord(FillId),
}

// ─── helpers ──────────────────────────────────────────────────────────────────

fn evidence_ledger(e: &ExecutionEvidence) -> LedgerId {
    LedgerId {
        scope: e.scope.clone(),
        market: e.market.clone(),
    }
}

fn split_effects(effects: &[Effect]) -> (Vec<JournalRecord>, Vec<Effect>) {
    let mut records = Vec::new();
    let mut non_journal = Vec::new();
    for e in effects {
        match e {
            Effect::AppendFsync(r) => records.push(r.clone()),
            other => non_journal.push(other.clone()),
        }
    }
    (records, non_journal)
}

fn outcome_from_transition(t: &TransitionOutcome) -> BatchOutcome {
    match t {
        TransitionOutcome::Accept { .. } => BatchOutcome::Accept,
        TransitionOutcome::Halt { reason, .. } => BatchOutcome::Halt(reason.clone()),
        TransitionOutcome::Reject { reason } => BatchOutcome::Reject(reason.clone()),
    }
}

fn state_after_transition(t: &TransitionOutcome, prior: &OrderState) -> OrderState {
    match t {
        TransitionOutcome::Accept { new_state, .. } | TransitionOutcome::Halt { new_state, .. } => {
            new_state.clone()
        }
        TransitionOutcome::Reject { .. } => prior.clone(),
    }
}

fn count_event_fills(event: &OrderEvent) -> usize {
    match event {
        OrderEvent::Fill { .. } => 1,
        OrderEvent::ImmediateFillBackfillResult { fills } => fills.len(),
        OrderEvent::UnknownBackfillResult { matched, .. } => {
            matched.iter().map(|m| m.fills.len()).sum()
        }
        OrderEvent::ReconcileResult { fills, .. } => fills.len(),
        _ => 0,
    }
}

fn event_has_identified_fills(event: &OrderEvent) -> bool {
    count_event_fills(event) > 0
        && matches!(
            event,
            OrderEvent::Fill { .. }
                | OrderEvent::ImmediateFillBackfillResult { .. }
                | OrderEvent::UnknownBackfillResult { .. }
                | OrderEvent::ReconcileResult { .. }
        )
}

fn flatten_event_fills(event: &OrderEvent) -> Vec<FillRecord> {
    match event {
        OrderEvent::Fill {
            fill_id,
            qty,
            price_cents,
            ts_ns,
            venue_order_id,
            fee_cents,
        } => vec![FillRecord {
            fill_id: fill_id.clone(),
            qty: *qty,
            price_cents: *price_cents,
            ts_ns: *ts_ns,
            venue_order_id: venue_order_id.clone(),
            fee_cents: *fee_cents,
        }],
        OrderEvent::ImmediateFillBackfillResult { fills } => fills.clone(),
        OrderEvent::UnknownBackfillResult { matched, .. } => {
            matched.iter().flat_map(|m| m.fills.clone()).collect()
        }
        OrderEvent::ReconcileResult { fills, .. } => fills.clone(),
        _ => Vec::new(),
    }
}

fn check_evidence_matches_fills(
    fills: &[FillRecord],
    evidences: &[ExecutionEvidence],
) -> Result<(), PrepareError> {
    if fills.len() != evidences.len() {
        return Err(PrepareError::EvidenceFillMismatch {
            position: 0,
            detail: format!(
                "length mismatch: event fills {} vs evidences {}",
                fills.len(),
                evidences.len()
            ),
        });
    }
    for (i, (f, e)) in fills.iter().zip(evidences.iter()).enumerate() {
        if f.fill_id != e.execution_id {
            return Err(PrepareError::EvidenceFillMismatch {
                position: i,
                detail: format!(
                    "execution_id: event {:?} vs evidence {:?}",
                    f.fill_id, e.execution_id
                ),
            });
        }
        if f.qty != e.qty {
            return Err(PrepareError::EvidenceFillMismatch {
                position: i,
                detail: format!("qty: event {} vs evidence {}", f.qty, e.qty),
            });
        }
        if f.price_cents != e.price_cents {
            return Err(PrepareError::EvidenceFillMismatch {
                position: i,
                detail: format!(
                    "price_cents: event {} vs evidence {}",
                    f.price_cents, e.price_cents
                ),
            });
        }
        // venue: event Some vs evidence None, or both Some unequal ⇒ mismatch
        // evidence Some vs event None is allowed
        match (&f.venue_order_id, &e.venue_order_id) {
            (Some(a), Some(b)) if a != b => {
                return Err(PrepareError::EvidenceFillMismatch {
                    position: i,
                    detail: format!("venue_order_id both Some unequal: {a:?} vs {b:?}"),
                });
            }
            (Some(_), None) => {
                return Err(PrepareError::EvidenceFillMismatch {
                    position: i,
                    detail: "venue_order_id: event Some vs evidence None".into(),
                });
            }
            _ => {}
        }
        match (&f.fee_cents, &e.fee_cents) {
            (Some(a), Some(b)) if a != b => {
                return Err(PrepareError::EvidenceFillMismatch {
                    position: i,
                    detail: format!("fee_cents both Some unequal: {a} vs {b}"),
                });
            }
            (Some(_), None) => {
                return Err(PrepareError::EvidenceFillMismatch {
                    position: i,
                    detail: "fee_cents: event Some vs evidence None".into(),
                });
            }
            _ => {}
        }
    }
    Ok(())
}

#[derive(Clone)]
struct StagedPayload {
    side: Side,
    qty: u64,
    price_cents: u64,
    first_seen_ts_ns: i64,
    venue_order_id: Option<VenueOrderId>,
    claimed_client_order_id: Option<ClientOrderId>,
    fee_cents: Option<u64>,
    source: ExecutionSource,
}

impl StagedPayload {
    fn to_entry(&self, owner: Option<ClientOrderId>, provenance: InventoryProvenance) -> InventoryEntry {
        InventoryEntry {
            side: self.side,
            qty: self.qty,
            price_cents: self.price_cents,
            first_seen_ts_ns: self.first_seen_ts_ns,
            venue_order_id: self.venue_order_id.clone(),
            claimed_client_order_id: self.claimed_client_order_id.clone(),
            owner,
            fee_cents: self.fee_cents,
            source: self.source.clone(),
            provenance,
        }
    }

    fn as_fill_record(&self, id: &FillId) -> FillRecord {
        FillRecord {
            fill_id: id.clone(),
            qty: self.qty,
            price_cents: self.price_cents,
            ts_ns: self.first_seen_ts_ns,
            venue_order_id: self.venue_order_id.clone(),
            fee_cents: self.fee_cents,
        }
    }
}

fn core_disagree_qty_price_venue(
    qty_a: u64,
    price_a: u64,
    venue_a: &Option<VenueOrderId>,
    qty_b: u64,
    price_b: u64,
    venue_b: &Option<VenueOrderId>,
) -> bool {
    if qty_a != qty_b || price_a != price_b {
        return true;
    }
    match (venue_a, venue_b) {
        (Some(a), Some(b)) if a != b => true,
        _ => false,
    }
}

fn fee_conflict(a: Option<u64>, b: Option<u64>) -> bool {
    matches!((a, b), (Some(x), Some(y)) if x != y)
}

fn build_conflict_witness(
    staged: Option<&StagedPayload>,
    entry: Option<&InventoryEntry>,
    q: Option<(&ClientOrderId, &FillPayload)>,
) -> ConflictWitness {
    if let Some(s) = staged {
        return ConflictWitness::Staged(s.to_entry(None, InventoryProvenance::NoRow));
    }
    match (entry, q) {
        (Some(e), Some((cid, p))) => ConflictWitness::Both {
            entry: e.clone(),
            client_order_id: cid.clone(),
            payload: p.clone(),
        },
        (Some(e), None) => ConflictWitness::Entry(e.clone()),
        (None, Some((cid, p))) => ConflictWitness::OrderApplied {
            client_order_id: cid.clone(),
            payload: p.clone(),
        },
        (None, None) => ConflictWitness::Staged(InventoryEntry {
            side: Side::BuyYes,
            qty: 0,
            price_cents: 0,
            first_seen_ts_ns: 0,
            venue_order_id: None,
            claimed_client_order_id: None,
            owner: None,
            fee_cents: None,
            source: ExecutionSource::WsFill,
            provenance: InventoryProvenance::NoRow,
        }),
    }
}

// ─── MarketInventory ──────────────────────────────────────────────────────────

impl MarketInventory {
    pub fn new(ledger: LedgerId) -> Self {
        Self {
            ledger,
            generation: 0,
            entries: BTreeMap::new(),
            conflicts: BTreeMap::new(),
        }
    }

    pub fn ledger(&self) -> &LedgerId {
        &self.ledger
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn net(&self) -> i128 {
        let mut n: i128 = 0;
        for e in self.entries.values() {
            match e.side {
                Side::BuyYes => n += e.qty as i128,
                Side::SellYes => n -= e.qty as i128,
            }
        }
        n
    }

    pub fn view(&self) -> InventoryView {
        let mut core = 0usize;
        let mut ownership = 0usize;
        let mut fee = 0usize;
        for c in self.conflicts.values() {
            match c.class {
                ConflictClass::Core => core += 1,
                ConflictClass::Ownership => ownership += 1,
                ConflictClass::Fee => fee += 1,
            }
        }
        let health = if core + ownership + fee == 0 {
            InventoryHealth::Clean
        } else {
            InventoryHealth::Conflicted {
                core,
                ownership,
                fee,
            }
        };
        InventoryView {
            net: self.net(),
            executions: self.entries.len(),
            health,
        }
    }

    pub fn get(&self, id: &FillId) -> Option<&InventoryEntry> {
        self.entries.get(id)
    }

    pub fn conflicts(&self) -> impl Iterator<Item = &ConflictFact> {
        self.conflicts.values()
    }

    pub fn checksum_diagnostic(&self, observed_net: i128) -> ChecksumDiagnostic {
        let ledger = self.net();
        if ledger == observed_net {
            ChecksumDiagnostic::Match
        } else {
            ChecksumDiagnostic::Mismatch {
                ledger,
                observed: observed_net,
            }
        }
    }

    pub fn rebuild(ledger: LedgerId, records: &[JournalRecord]) -> Result<Self, RebuildError> {
        // Count generation exactly as apply: per TRANSACTION, one bump per distinct
        // mutated entry id + one per distinct conflict id (no cross-txn de-dup).
        let mut entries: BTreeMap<FillId, InventoryEntry> = BTreeMap::new();
        let mut conflicts: BTreeMap<FillId, ConflictFact> = BTreeMap::new();
        let mut gen = 0u64;
        let mut ingested_ids: BTreeSet<FillId> = BTreeSet::new();
        for rec in records {
            let JournalRecord::ExecutionTxn(txn) = rec else {
                continue;
            };
            if txn.ledger != ledger {
                continue;
            }
            let mut mutated_entry_ids: BTreeSet<FillId> = BTreeSet::new();
            let mut conflict_ids: BTreeSet<FillId> = BTreeSet::new();
            for outcome in &txn.executions {
                let id = &outcome.evidence.execution_id;
                if let Some(c) = &outcome.conflict {
                    // last conflict fact for an id is preserved
                    conflicts.insert(c.execution_id.clone(), c.clone());
                    conflict_ids.insert(c.execution_id.clone());
                }
                match &outcome.inventory {
                    InventoryDisposition::Ingested(_) => {
                        if ingested_ids.contains(id) {
                            return Err(RebuildError::DuplicateExecutionRecord(id.clone()));
                        }
                        if let Some(e) = &outcome.entry_after {
                            ingested_ids.insert(id.clone());
                            entries.insert(id.clone(), e.clone());
                            mutated_entry_ids.insert(id.clone());
                        }
                    }
                    InventoryDisposition::Upgraded => {
                        if let Some(e) = &outcome.entry_after {
                            entries.insert(id.clone(), e.clone());
                            mutated_entry_ids.insert(id.clone());
                        }
                    }
                    InventoryDisposition::Duplicate
                    | InventoryDisposition::Conflict(_)
                    | InventoryDisposition::Unchanged => {}
                }
            }
            gen = gen
                .saturating_add(mutated_entry_ids.len() as u64)
                .saturating_add(conflict_ids.len() as u64);
        }
        Ok(Self {
            ledger,
            generation: gen,
            entries,
            conflicts,
        })
    }
}

pub fn rebuild_order(
    ledger: &LedgerId,
    client_order_id: &ClientOrderId,
    records: &[JournalRecord],
) -> Result<Option<(OrderState, OrderCtx, OrderCursor)>, RebuildError> {
    let mut snaps: BTreeMap<u64, OrderTxnRecord> = BTreeMap::new();
    for rec in records {
        let order_part = match rec {
            JournalRecord::OrderTxn(o) => Some(o.as_ref()),
            JournalRecord::ExecutionTxn(e) => e.order.as_ref(),
            _ => None,
        };
        let Some(o) = order_part else { continue };
        if &o.ledger != ledger || &o.client_order_id != client_order_id {
            continue;
        }
        if let Some(prev) = snaps.get(&o.seq) {
            if prev != o {
                return Err(RebuildError::Divergent {
                    client_order_id: client_order_id.clone(),
                    seq: o.seq,
                });
            }
            // equal duplicate idempotent
            continue;
        }
        snaps.insert(o.seq, o.clone());
    }
    if snaps.is_empty() {
        return Ok(None);
    }
    let mut expected = 1u64;
    let mut last: Option<&OrderTxnRecord> = None;
    for (seq, rec) in &snaps {
        if *seq != expected {
            return Err(RebuildError::SeqGap {
                client_order_id: client_order_id.clone(),
                expected,
                found: *seq,
            });
        }
        last = Some(rec);
        expected += 1;
    }
    let last = last.expect("non-empty");
    Ok(Some((
        last.state_after.clone(),
        last.ctx_after.clone(),
        OrderCursor { seq: last.seq },
    )))
}

pub fn legacy_fill_ids(records: &[JournalRecord]) -> BTreeSet<FillId> {
    let mut out = BTreeSet::new();
    for rec in records {
        match rec {
            JournalRecord::Fill { fill_id, .. } | JournalRecord::FillCid { fill_id, .. } => {
                out.insert(fill_id.clone());
            }
            _ => {}
        }
    }
    out
}

pub fn execution_debt<'a>(ctxs: impl Iterator<Item = &'a OrderCtx>) -> u64 {
    let mut total = 0u64;
    for ctx in ctxs {
        let debt = ctx.fill_obligation.saturating_sub(ctx.attributed_fill_qty);
        total = total.saturating_add(debt);
    }
    total
}

// ─── prepare_order_event ──────────────────────────────────────────────────────

pub fn prepare_order_event(
    order: OrderRef<'_>,
    event: &OrderEvent,
) -> Result<OrderTransaction, PrepareError> {
    if order.ctx.market != order.ledger.market {
        return Err(PrepareError::OrderRefInconsistent {
            ledger_market: order.ledger.market.clone(),
            ctx_market: order.ctx.market.clone(),
        });
    }
    if event_has_identified_fills(event) {
        return Err(PrepareError::FillsMustUseExecutionApi {
            event: event_name(event).to_string(),
            fills: count_event_fills(event),
        });
    }

    let mut scratch_ctx = order.ctx.clone();
    let prior_state = order.state.clone();
    let transition = apply_event(order.state, &mut scratch_ctx, event);

    if let TransitionOutcome::Reject { reason } = &transition {
        return Ok(OrderTransaction {
            journal: None,
            effects: vec![],
            rejected: Some(reason.clone()),
            prepared_ledger: order.ledger.clone(),
            expected_cursor: order.cursor,
            expected_state: order.state.clone(),
            expected_ctx: order.ctx.clone(),
            state_after: prior_state,
            ctx_after: order.ctx.clone(),
            advance_cursor: false,
            next_seq: order.cursor.seq,
        });
    }

    let state_after = state_after_transition(&transition, &prior_state);
    let (core_records, non_journal) = split_effects(transition.effects());
    let batch_outcome = outcome_from_transition(&transition);
    let changed = state_after != *order.state
        || scratch_ctx != *order.ctx
        || !core_records.is_empty()
        || !non_journal.is_empty()
        || !transition.effects().is_empty();

    // Pin: no AccountFill from prepare_order_event
    debug_assert!(
        !transition
            .effects()
            .iter()
            .any(|e| matches!(e, Effect::AccountFill { .. })),
        "prepare_order_event must not yield AccountFill"
    );

    if !changed {
        return Ok(OrderTransaction {
            journal: None,
            effects: vec![],
            rejected: None,
            prepared_ledger: order.ledger.clone(),
            expected_cursor: order.cursor,
            expected_state: order.state.clone(),
            expected_ctx: order.ctx.clone(),
            state_after: order.state.clone(),
            ctx_after: order.ctx.clone(),
            advance_cursor: false,
            next_seq: order.cursor.seq,
        });
    }

    let seq = order.cursor.seq + 1;
    let record = OrderTxnRecord {
        ledger: order.ledger.clone(),
        client_order_id: order.ctx.client_order_id.clone(),
        seq,
        event: event_name(event).to_string(),
        outcome: batch_outcome,
        core_records,
        state_after: state_after.clone(),
        ctx_after: scratch_ctx.clone(),
    };

    Ok(OrderTransaction {
        journal: Some(JournalRecord::OrderTxn(Box::new(record))),
        effects: non_journal,
        rejected: None,
        prepared_ledger: order.ledger.clone(),
        expected_cursor: order.cursor,
        expected_state: order.state.clone(),
        expected_ctx: order.ctx.clone(),
        state_after,
        ctx_after: scratch_ctx,
        advance_cursor: true,
        next_seq: seq,
    })
}

impl OrderTransaction {
    pub fn apply(self, target: OrderTarget<'_>) -> Result<(), ApplyMismatch> {
        if target.ledger != &self.prepared_ledger {
            return Err(ApplyMismatch::LedgerIdentity);
        }
        if target.cursor.seq != self.expected_cursor.seq {
            return Err(ApplyMismatch::OrderCursor {
                expected: self.expected_cursor.seq,
                found: target.cursor.seq,
            });
        }
        if *target.state != self.expected_state || *target.ctx != self.expected_ctx {
            return Err(ApplyMismatch::OrderStateOrCtx);
        }
        if self.advance_cursor {
            *target.state = self.state_after;
            *target.ctx = self.ctx_after;
            target.cursor.seq = self.next_seq;
        }
        Ok(())
    }
}

// continued in part 2...

// ─── prepare_execution / prepare_execution_batch ──────────────────────────────

pub fn prepare_execution(
    order: Option<OrderRef<'_>>,
    inventory: &MarketInventory,
    evidence: &ExecutionEvidence,
) -> Result<ExecutionTransaction, PrepareError> {
    match order {
        None => prepare_execution_norow(inventory, evidence),
        Some(o) => {
            let event = OrderEvent::Fill {
                fill_id: evidence.execution_id.clone(),
                qty: evidence.qty,
                price_cents: evidence.price_cents,
                ts_ns: evidence.ts_ns,
                venue_order_id: evidence.venue_order_id.clone(),
                fee_cents: evidence.fee_cents,
            };
            prepare_execution_batch(o, inventory, &event, std::slice::from_ref(evidence))
        }
    }
}

fn prepare_execution_norow(
    inventory: &MarketInventory,
    evidence: &ExecutionEvidence,
) -> Result<ExecutionTransaction, PrepareError> {
    let el = evidence_ledger(evidence);
    if el != inventory.ledger {
        return Err(PrepareError::ForeignLedger {
            inventory: inventory.ledger.clone(),
            evidence: el,
        });
    }
    let gen_before = inventory.generation();
    let mut outcomes = Vec::new();
    let mut planned_entries: BTreeMap<FillId, InventoryEntry> = BTreeMap::new();
    let mut planned_conflicts: BTreeMap<FillId, ConflictFact> = BTreeMap::new();

    if evidence.qty == 0 {
        outcomes.push(ExecutionOutcome {
            evidence: evidence.clone(),
            order: OrderDisposition::Ignored(IgnoreReason::ZeroQty),
            inventory: InventoryDisposition::Unchanged,
            entry_after: None,
            conflict: None,
        });
        return Ok(ExecutionTransaction {
            outcomes,
            journal: None,
            effects: vec![],
            prepared_inventory_ledger: inventory.ledger.clone(),
            expected_generation: gen_before,
            prepared_order_ledger: None,
            expected_cursor: None,
            expected_state: None,
            expected_ctx: None,
            order_state_after: None,
            order_ctx_after: None,
            advance_cursor: false,
            next_seq: 0,
            planned_entries,
            planned_conflicts,
        });
    }

    // Global identity vs P only (no Q, no S)
    if let Some(p) = inventory.get(&evidence.execution_id) {
        if core_disagree_qty_price_venue(
            evidence.qty,
            evidence.price_cents,
            &evidence.venue_order_id,
            p.qty,
            p.price_cents,
            &p.venue_order_id,
        ) || evidence.side != p.side
        {
            let fact = ConflictFact {
                execution_id: evidence.execution_id.clone(),
                class: ConflictClass::Core,
                witness: ConflictWitness::Entry(p.clone()),
                evidence: evidence.clone(),
            };
            planned_conflicts.insert(evidence.execution_id.clone(), fact.clone());
            outcomes.push(ExecutionOutcome {
                evidence: evidence.clone(),
                order: OrderDisposition::Refused(RefusalReason::ConflictingExecution(
                    ConflictClass::Core,
                )),
                inventory: InventoryDisposition::Conflict(ConflictClass::Core),
                entry_after: Some(p.clone()),
                conflict: Some(fact),
            });
        } else if fee_conflict(evidence.fee_cents, p.fee_cents) {
            let fact = ConflictFact {
                execution_id: evidence.execution_id.clone(),
                class: ConflictClass::Fee,
                witness: ConflictWitness::Entry(p.clone()),
                evidence: evidence.clone(),
            };
            planned_conflicts.insert(evidence.execution_id.clone(), fact.clone());
            outcomes.push(ExecutionOutcome {
                evidence: evidence.clone(),
                order: OrderDisposition::Refused(RefusalReason::FeeConflict),
                inventory: InventoryDisposition::Conflict(ConflictClass::Fee),
                entry_after: Some(p.clone()),
                conflict: Some(fact),
            });
        } else {
            // duplicate / maybe upgrade metadata None→Some (but NoRow has no owner upgrade from row)
            let mut entry = p.clone();
            let mut upgraded = false;
            if entry.venue_order_id.is_none() && evidence.venue_order_id.is_some() {
                entry.venue_order_id = evidence.venue_order_id.clone();
                upgraded = true;
            }
            if entry.fee_cents.is_none() && evidence.fee_cents.is_some() {
                entry.fee_cents = evidence.fee_cents;
                upgraded = true;
            }
            if entry.claimed_client_order_id.is_none()
                && evidence.claimed_client_order_id.is_some()
            {
                entry.claimed_client_order_id = evidence.claimed_client_order_id.clone();
                upgraded = true;
            }
            if upgraded {
                planned_entries.insert(evidence.execution_id.clone(), entry.clone());
                outcomes.push(ExecutionOutcome {
                    evidence: evidence.clone(),
                    order: OrderDisposition::NoRow,
                    inventory: InventoryDisposition::Upgraded,
                    entry_after: Some(entry),
                    conflict: None,
                });
            } else {
                outcomes.push(ExecutionOutcome {
                    evidence: evidence.clone(),
                    order: OrderDisposition::NoRow,
                    inventory: InventoryDisposition::Duplicate,
                    entry_after: Some(entry),
                    conflict: None,
                });
            }
        }
    } else {
        let entry = InventoryEntry {
            side: evidence.side,
            qty: evidence.qty,
            price_cents: evidence.price_cents,
            first_seen_ts_ns: evidence.ts_ns,
            venue_order_id: evidence.venue_order_id.clone(),
            claimed_client_order_id: evidence.claimed_client_order_id.clone(),
            owner: None, // NoRow: owner always None
            fee_cents: evidence.fee_cents,
            source: evidence.source.clone(),
            provenance: InventoryProvenance::NoRow,
        };
        planned_entries.insert(evidence.execution_id.clone(), entry.clone());
        outcomes.push(ExecutionOutcome {
            evidence: evidence.clone(),
            order: OrderDisposition::NoRow,
            inventory: InventoryDisposition::Ingested(InventoryProvenance::NoRow),
            entry_after: Some(entry),
            conflict: None,
        });
    }

    let need_journal = outcomes.iter().any(|o| {
        matches!(
            o.inventory,
            InventoryDisposition::Ingested(_)
                | InventoryDisposition::Upgraded
                | InventoryDisposition::Conflict(_)
        )
    });
    let journal = if need_journal {
        Some(JournalRecord::ExecutionTxn(Box::new(ExecutionTxnRecord {
            ledger: inventory.ledger.clone(),
            inventory_generation_before: gen_before,
            executions: outcomes.clone(),
            order: None,
        })))
    } else {
        None
    };

    Ok(ExecutionTransaction {
        outcomes,
        journal,
        effects: vec![],
        prepared_inventory_ledger: inventory.ledger.clone(),
        expected_generation: gen_before,
        prepared_order_ledger: None,
        expected_cursor: None,
        expected_state: None,
        expected_ctx: None,
        order_state_after: None,
        order_ctx_after: None,
        advance_cursor: false,
        next_seq: 0,
        planned_entries,
        planned_conflicts,
    })
}

pub fn prepare_execution_batch(
    order: OrderRef<'_>,
    inventory: &MarketInventory,
    event: &OrderEvent,
    evidences: &[ExecutionEvidence],
) -> Result<ExecutionTransaction, PrepareError> {
    // Positional evidence match
    let fills = flatten_event_fills(event);
    check_evidence_matches_fills(&fills, evidences)?;

    // Batch-level checks
    if evidences.is_empty() {
        // structured empty-fill events still allowed through
    } else {
        let first = evidence_ledger(&evidences[0]);
        for e in &evidences[1..] {
            if evidence_ledger(e) != first {
                return Err(PrepareError::MixedLedgerBatch);
            }
        }
        if first != inventory.ledger {
            return Err(PrepareError::ForeignLedger {
                inventory: inventory.ledger.clone(),
                evidence: first,
            });
        }
    }

    let row_foreign = *order.ledger != inventory.ledger
        || order.ctx.market != order.ledger.market;
    let gen_before = inventory.generation();

    // Per-evidence adjudication against P, Q, S
    #[derive(Clone)]
    enum PreCore {
        IgnoredZero,
        Refused {
            order: OrderDisposition,
            inventory: InventoryDisposition,
            entry_after: Option<InventoryEntry>,
            conflict: Option<ConflictFact>,
        },
        Admitted {
            staged: StagedPayload,
        },
    }

    let mut staging: BTreeMap<FillId, StagedPayload> = BTreeMap::new();
    let mut pre: Vec<(ExecutionEvidence, PreCore)> = Vec::new();
    let mut planned_conflicts: BTreeMap<FillId, ConflictFact> = BTreeMap::new();

    if row_foreign {
        let refusal = RefusalReason::RowForeign {
            row_ledger: order.ledger.clone(),
            row_ctx_market: order.ctx.market.clone(),
            inventory_ledger: inventory.ledger.clone(),
        };
        for e in evidences {
            if e.qty == 0 {
                pre.push((
                    e.clone(),
                    PreCore::IgnoredZero,
                ));
                continue;
            }
            // Row-less adjudication over {E, S, P}; Q unread. ORDER always Refused(RowForeign).
            let p = inventory.get(&e.execution_id);
            let s = staging.get(&e.execution_id).cloned();

            // core disagreement among {E, S, P}
            let mut core_hit = false;
            if let Some(ref sp) = s {
                if e.side != sp.side
                    || core_disagree_qty_price_venue(
                        e.qty,
                        e.price_cents,
                        &e.venue_order_id,
                        sp.qty,
                        sp.price_cents,
                        &sp.venue_order_id,
                    )
                {
                    core_hit = true;
                }
            }
            if let Some(p) = p {
                if e.side != p.side
                    || core_disagree_qty_price_venue(
                        e.qty,
                        e.price_cents,
                        &e.venue_order_id,
                        p.qty,
                        p.price_cents,
                        &p.venue_order_id,
                    )
                {
                    core_hit = true;
                }
            }
            if let (Some(ref sp), Some(p)) = (&s, p) {
                if sp.side != p.side
                    || core_disagree_qty_price_venue(
                        sp.qty,
                        sp.price_cents,
                        &sp.venue_order_id,
                        p.qty,
                        p.price_cents,
                        &p.venue_order_id,
                    )
                {
                    core_hit = true;
                }
            }
            if core_hit {
                let witness = build_conflict_witness(s.as_ref(), p, None);
                let fact = ConflictFact {
                    execution_id: e.execution_id.clone(),
                    class: ConflictClass::Core,
                    witness,
                    evidence: e.clone(),
                };
                planned_conflicts.insert(e.execution_id.clone(), fact.clone());
                pre.push((
                    e.clone(),
                    PreCore::Refused {
                        order: OrderDisposition::Refused(refusal.clone()),
                        inventory: InventoryDisposition::Conflict(ConflictClass::Core),
                        entry_after: p.cloned(),
                        conflict: Some(fact),
                    },
                ));
                continue;
            }

            // fee both-Some-unequal over any pair among {E, S, P}
            let fee_e = e.fee_cents;
            let fee_s = s.as_ref().map(|sp| sp.fee_cents).unwrap_or(None);
            let fee_p = p.map(|pp| pp.fee_cents).unwrap_or(None);
            let fee_hit = fee_conflict(fee_e, fee_s)
                || fee_conflict(fee_e, fee_p)
                || fee_conflict(fee_s, fee_p);
            if fee_hit {
                let witness = build_conflict_witness(s.as_ref(), p, None);
                let fact = ConflictFact {
                    execution_id: e.execution_id.clone(),
                    class: ConflictClass::Fee,
                    witness,
                    evidence: e.clone(),
                };
                planned_conflicts.insert(e.execution_id.clone(), fact.clone());
                pre.push((
                    e.clone(),
                    PreCore::Refused {
                        order: OrderDisposition::Refused(refusal.clone()),
                        inventory: InventoryDisposition::Conflict(ConflictClass::Fee),
                        entry_after: p.cloned(),
                        conflict: Some(fact),
                    },
                ));
                continue;
            }

            // S present and checks passed => Duplicate with None->Some merge into S
            if let Some(mut sp) = s {
                if sp.venue_order_id.is_none() && e.venue_order_id.is_some() {
                    sp.venue_order_id = e.venue_order_id.clone();
                }
                if sp.fee_cents.is_none() && e.fee_cents.is_some() {
                    sp.fee_cents = e.fee_cents;
                }
                if sp.claimed_client_order_id.is_none() && e.claimed_client_order_id.is_some() {
                    sp.claimed_client_order_id = e.claimed_client_order_id.clone();
                }
                staging.insert(e.execution_id.clone(), sp);
                pre.push((
                    e.clone(),
                    PreCore::Refused {
                        order: OrderDisposition::Refused(refusal.clone()),
                        inventory: InventoryDisposition::Duplicate,
                        entry_after: p.cloned(),
                        conflict: None,
                    },
                ));
                continue;
            }

            // S absent, P present and consistent => Duplicate, or Upgraded under venue/fee None->Some
            // (never erase existing P owner/provenance/first_seen/source; no standalone claimed-cid upgrade)
            if let Some(p_entry) = p {
                let mut entry = p_entry.clone();
                let mut upgraded = false;
                if entry.venue_order_id.is_none() && e.venue_order_id.is_some() {
                    entry.venue_order_id = e.venue_order_id.clone();
                    upgraded = true;
                }
                if entry.fee_cents.is_none() && e.fee_cents.is_some() {
                    entry.fee_cents = e.fee_cents;
                    upgraded = true;
                }
                let (inv_disp, entry_after) = if upgraded {
                    (InventoryDisposition::Upgraded, Some(entry))
                } else {
                    (InventoryDisposition::Duplicate, Some(p_entry.clone()))
                };
                pre.push((
                    e.clone(),
                    PreCore::Refused {
                        order: OrderDisposition::Refused(refusal.clone()),
                        inventory: inv_disp,
                        entry_after,
                        conflict: None,
                    },
                ));
                continue;
            }

            // S absent and P absent => stage canonical payload and Ingested(RowForeign) EXACTLY ONCE
            // (presence over inventory U staging). entry = final staged; owner None; first_seen = E.ts_ns.
            let staged = StagedPayload {
                side: e.side,
                qty: e.qty,
                price_cents: e.price_cents,
                first_seen_ts_ns: e.ts_ns,
                venue_order_id: e.venue_order_id.clone(),
                claimed_client_order_id: e.claimed_client_order_id.clone(),
                fee_cents: e.fee_cents,
                source: e.source.clone(),
            };
            staging.insert(e.execution_id.clone(), staged.clone());
            let entry = InventoryEntry {
                side: staged.side,
                qty: staged.qty,
                price_cents: staged.price_cents,
                first_seen_ts_ns: e.ts_ns,
                venue_order_id: staged.venue_order_id.clone(),
                claimed_client_order_id: staged.claimed_client_order_id.clone(),
                owner: None,
                fee_cents: staged.fee_cents,
                source: staged.source.clone(),
                provenance: InventoryProvenance::RowForeign,
            };
            pre.push((
                e.clone(),
                PreCore::Refused {
                    order: OrderDisposition::Refused(refusal.clone()),
                    inventory: InventoryDisposition::Ingested(InventoryProvenance::RowForeign),
                    entry_after: Some(entry),
                    conflict: None,
                },
            ));
        }
    } else {
        for e in evidences {
            // 1. zero qty
            if e.qty == 0 {
                pre.push((e.clone(), PreCore::IgnoredZero));
                continue;
            }

            let p = inventory.get(&e.execution_id);
            let q = order.ctx.applied_fills.get(&e.execution_id);
            let s = staging.get(&e.execution_id).cloned();

            // 2. Global identity — core among {E,S,P,Q}
            let mut core_hit = false;
            // E vs S
            if let Some(ref sp) = s {
                if e.side != sp.side
                    || core_disagree_qty_price_venue(
                        e.qty, e.price_cents, &e.venue_order_id,
                        sp.qty, sp.price_cents, &sp.venue_order_id,
                    )
                {
                    core_hit = true;
                }
            }
            // E vs P
            if let Some(p) = p {
                if e.side != p.side
                    || core_disagree_qty_price_venue(
                        e.qty, e.price_cents, &e.venue_order_id,
                        p.qty, p.price_cents, &p.venue_order_id,
                    )
                {
                    core_hit = true;
                }
            }
            // E vs Q (no side)
            if let Some(q) = q {
                if core_disagree_qty_price_venue(
                    e.qty, e.price_cents, &e.venue_order_id,
                    q.qty, q.price_cents, &q.venue_order_id,
                ) {
                    core_hit = true;
                }
            }
            // S vs P
            if let (Some(ref sp), Some(p)) = (&s, p) {
                if sp.side != p.side
                    || core_disagree_qty_price_venue(
                        sp.qty, sp.price_cents, &sp.venue_order_id,
                        p.qty, p.price_cents, &p.venue_order_id,
                    )
                {
                    core_hit = true;
                }
            }
            // S vs Q
            if let (Some(ref sp), Some(q)) = (&s, q) {
                if core_disagree_qty_price_venue(
                    sp.qty, sp.price_cents, &sp.venue_order_id,
                    q.qty, q.price_cents, &q.venue_order_id,
                ) {
                    core_hit = true;
                }
            }
            // P vs Q
            if let (Some(p), Some(q)) = (p, q) {
                if core_disagree_qty_price_venue(
                    p.qty, p.price_cents, &p.venue_order_id,
                    q.qty, q.price_cents, &q.venue_order_id,
                ) {
                    core_hit = true;
                }
            }

            if core_hit {
                let witness = build_conflict_witness(
                    s.as_ref(),
                    p,
                    q.map(|qp| (&order.ctx.client_order_id, qp)),
                );
                let fact = ConflictFact {
                    execution_id: e.execution_id.clone(),
                    class: ConflictClass::Core,
                    witness,
                    evidence: e.clone(),
                };
                planned_conflicts.insert(e.execution_id.clone(), fact.clone());
                pre.push((
                    e.clone(),
                    PreCore::Refused {
                        order: OrderDisposition::Refused(RefusalReason::ConflictingExecution(
                            ConflictClass::Core,
                        )),
                        inventory: InventoryDisposition::Conflict(ConflictClass::Core),
                        entry_after: p.cloned(),
                        conflict: Some(fact),
                    },
                ));
                continue;
            }

            // Ownership: P.owner Some(c1) and row cid c2 != c1
            if let Some(p) = p {
                if let Some(owner) = &p.owner {
                    if owner != &order.ctx.client_order_id {
                        let fact = ConflictFact {
                            execution_id: e.execution_id.clone(),
                            class: ConflictClass::Ownership,
                            witness: ConflictWitness::Entry(p.clone()),
                            evidence: e.clone(),
                        };
                        planned_conflicts.insert(e.execution_id.clone(), fact.clone());
                        pre.push((
                            e.clone(),
                            PreCore::Refused {
                                order: OrderDisposition::Refused(
                                    RefusalReason::DuplicateOtherOwner {
                                        owner: owner.clone(),
                                    },
                                ),
                                inventory: InventoryDisposition::Conflict(ConflictClass::Ownership),
                                entry_after: Some(p.clone()),
                                conflict: Some(fact),
                            },
                        ));
                        continue;
                    }
                }
            }

            // Fee both-Some unequal among {E,S,P,Q}
            let fee_e = e.fee_cents;
            let fee_s = s.as_ref().map(|sp| sp.fee_cents).unwrap_or(None);
            let fee_p = p.map(|pp| pp.fee_cents).unwrap_or(None);
            let fee_q = q.map(|qq| qq.fee).unwrap_or(None);
            let fee_hit = fee_conflict(fee_e, fee_s)
                || fee_conflict(fee_e, fee_p)
                || fee_conflict(fee_e, fee_q)
                || fee_conflict(fee_s, fee_p)
                || fee_conflict(fee_s, fee_q)
                || fee_conflict(fee_p, fee_q);

            if fee_hit {
                let witness = build_conflict_witness(
                    s.as_ref(),
                    p,
                    q.map(|qp| (&order.ctx.client_order_id, qp)),
                );
                let fact = ConflictFact {
                    execution_id: e.execution_id.clone(),
                    class: ConflictClass::Fee,
                    witness,
                    evidence: e.clone(),
                };
                planned_conflicts.insert(e.execution_id.clone(), fact.clone());
                pre.push((
                    e.clone(),
                    PreCore::Refused {
                        order: OrderDisposition::Refused(RefusalReason::FeeConflict),
                        inventory: InventoryDisposition::Conflict(ConflictClass::Fee),
                        entry_after: p.cloned(),
                        conflict: Some(fact),
                    },
                ));
                continue;
            }

            // S present ⇒ InBatchDuplicate; merge None→Some into S
            if let Some(mut sp) = s {
                if sp.venue_order_id.is_none() && e.venue_order_id.is_some() {
                    sp.venue_order_id = e.venue_order_id.clone();
                }
                if sp.fee_cents.is_none() && e.fee_cents.is_some() {
                    sp.fee_cents = e.fee_cents;
                }
                if sp.claimed_client_order_id.is_none() && e.claimed_client_order_id.is_some() {
                    sp.claimed_client_order_id = e.claimed_client_order_id.clone();
                }
                staging.insert(e.execution_id.clone(), sp);
                pre.push((
                    e.clone(),
                    PreCore::Refused {
                        order: OrderDisposition::Refused(RefusalReason::InBatchDuplicate),
                        inventory: InventoryDisposition::Duplicate,
                        entry_after: p.cloned(),
                        conflict: None,
                    },
                ));
                continue;
            }

            // Stage E with canonical merge
            let ts = p
                .map(|pp| pp.first_seen_ts_ns)
                .or_else(|| q.map(|qq| qq.ts_ns))
                .unwrap_or(e.ts_ns);
            let venue = p
                .and_then(|pp| pp.venue_order_id.clone())
                .or_else(|| q.and_then(|qq| qq.venue_order_id.clone()))
                .or_else(|| e.venue_order_id.clone());
            let fee = p
                .and_then(|pp| pp.fee_cents)
                .or_else(|| q.and_then(|qq| qq.fee))
                .or(e.fee_cents);
            let staged = StagedPayload {
                side: e.side,
                qty: e.qty,
                price_cents: e.price_cents,
                first_seen_ts_ns: ts,
                venue_order_id: venue,
                claimed_client_order_id: e.claimed_client_order_id.clone(),
                fee_cents: fee,
                source: e.source.clone(),
            };
            staging.insert(e.execution_id.clone(), staged.clone());
            pre.push((e.clone(), PreCore::Admitted { staged }));
        }

        // 3. Side cross-check on admitted
        for item in pre.iter_mut() {
            if let (e, PreCore::Admitted { .. }) = item {
                if order.ctx.side != e.side {
                    let refusal = RefusalReason::SideMismatch {
                        ctx_side: order.ctx.side,
                        evidence_side: e.side,
                    };
                    // remove from staging for core feed — keep staged payload for inventory
                    let staged = staging.remove(&e.execution_id);
                    let entry_absent = inventory.get(&e.execution_id).is_none();
                    let (inv, entry_after) = if let Some(p) = inventory.get(&e.execution_id) {
                        (InventoryDisposition::Duplicate, Some(p.clone()))
                    } else if let Some(sp) = staged {
                        let entry = sp.to_entry(None, InventoryProvenance::Refused(refusal.clone()));
                        (
                            InventoryDisposition::Ingested(InventoryProvenance::Refused(
                                refusal.clone(),
                            )),
                            Some(entry),
                        )
                    } else {
                        (InventoryDisposition::Unchanged, None)
                    };
                    // note: entry_absent used for ingest; if present Duplicate
                    let _ = entry_absent;
                    *item = (
                        e.clone(),
                        PreCore::Refused {
                            order: OrderDisposition::Refused(refusal),
                            inventory: inv,
                            entry_after,
                            conflict: None,
                        },
                    );
                }
            }
        }
    }

    // Collect admitted fills: one per Admitted id, payload from FINAL staging
    // (after in-batch None->Some merges), not the stage-time clone kept in `pre`.
    let admitted: Vec<(FillId, StagedPayload)> = pre
        .iter()
        .filter_map(|(e, pc)| match pc {
            PreCore::Admitted { staged } => {
                let final_staged = staging
                    .get(&e.execution_id)
                    .cloned()
                    .unwrap_or_else(|| staged.clone());
                Some((e.execution_id.clone(), final_staged))
            }
            _ => None,
        })
        .collect();

    let all_side_mismatch = !evidences.is_empty()
        && pre.iter().all(|(_, pc)| match pc {
            PreCore::Refused {
                order: OrderDisposition::Refused(RefusalReason::SideMismatch { .. }),
                ..
            } => true,
            PreCore::IgnoredZero => true,
            _ => false,
        })
        && pre.iter().any(|(_, pc)| {
            matches!(
                pc,
                PreCore::Refused {
                    order: OrderDisposition::Refused(RefusalReason::SideMismatch { .. }),
                    ..
                }
            )
        })
        && admitted.is_empty();

    let structured = matches!(
        event,
        OrderEvent::ImmediateFillBackfillResult { .. }
            | OrderEvent::UnknownBackfillResult { .. }
            | OrderEvent::ReconcileResult { .. }
    );

    // 4. Core call
    let mut scratch_state = order.state.clone();
    let mut scratch_ctx = order.ctx.clone();
    let mut core_transition: Option<TransitionOutcome> = None;

    if row_foreign {
        // no core call
    } else if all_side_mismatch && !is_restart_frozen(order.state) {
        // synthetic halt — live row; this IS the order part (no further core call)
        let detail = "execution side mismatch with order ctx.side".to_string();
        core_transition = Some(halt_with_reason(
            HaltReason::CrossCheckMismatch { detail },
            vec![],
        ));
        if let TransitionOutcome::Halt { new_state, .. } = core_transition.as_ref().unwrap() {
            scratch_state = new_state.clone();
        }
    } else if all_side_mismatch && is_restart_frozen(order.state) {
        // frozen: no order part, no core call
    } else if !admitted.is_empty() || structured {
        // rebuild event with admitted fills only
        let admitted_at: Vec<usize> = pre
            .iter()
            .enumerate()
            .filter_map(|(i, (_, pc))| matches!(pc, PreCore::Admitted { .. }).then_some(i))
            .collect();
        let core_event = rebuild_event_with_fills(event, &admitted, &admitted_at);
        core_transition = Some(apply_event(
            order.state,
            &mut scratch_ctx,
            &core_event,
        ));
        scratch_state = state_after_transition(core_transition.as_ref().unwrap(), order.state);
    } else if matches!(event, OrderEvent::Fill { .. }) && admitted.is_empty() {
        // bare Fill all refused: no core call
    }

    let batch_outcome = core_transition
        .as_ref()
        .map(outcome_from_transition)
        .unwrap_or(BatchOutcome::Accept);

    // Per admitted fill: attribution from ctx_after.applied_fills
    let mut outcomes: Vec<ExecutionOutcome> = Vec::new();
    let mut planned_entries: BTreeMap<FillId, InventoryEntry> = BTreeMap::new();

    for (e, pc) in &pre {
        match pc {
            PreCore::IgnoredZero => {
                outcomes.push(ExecutionOutcome {
                    evidence: e.clone(),
                    order: OrderDisposition::Ignored(IgnoreReason::ZeroQty),
                    inventory: InventoryDisposition::Unchanged,
                    entry_after: None,
                    conflict: None,
                });
            }
            PreCore::Refused {
                order: od,
                inventory: idisp,
                entry_after,
                conflict,
            } => {
                if let Some(c) = conflict {
                    planned_conflicts.insert(c.execution_id.clone(), c.clone());
                }
                // Newly ingested RowForeign: publish the final staged payload
                // (later in-batch None→Some venue/fee/claimed-cid), keeping
                // first-seen/source, owner None, and RowForeign provenance.
                let mut entry_after = entry_after.clone();
                if matches!(
                    idisp,
                    InventoryDisposition::Ingested(InventoryProvenance::RowForeign)
                ) {
                    if let (Some(ent), Some(sp)) =
                        (entry_after.as_mut(), staging.get(&e.execution_id))
                    {
                        ent.venue_order_id = sp.venue_order_id.clone();
                        ent.fee_cents = sp.fee_cents;
                        ent.claimed_client_order_id = sp.claimed_client_order_id.clone();
                    }
                }
                if matches!(
                    idisp,
                    InventoryDisposition::Ingested(_) | InventoryDisposition::Upgraded
                ) {
                    if let Some(ent) = entry_after.as_ref() {
                        planned_entries.insert(e.execution_id.clone(), ent.clone());
                    }
                }
                // For side-mismatch ingest when entry was absent
                if let InventoryDisposition::Ingested(_) = idisp {
                    if let Some(ent) = entry_after.as_ref() {
                        planned_entries.insert(e.execution_id.clone(), ent.clone());
                    }
                }
                outcomes.push(ExecutionOutcome {
                    evidence: e.clone(),
                    order: od.clone(),
                    inventory: idisp.clone(),
                    entry_after,
                    conflict: conflict.clone(),
                });
            }
            PreCore::Admitted { staged } => {
                let id = &e.execution_id;
                let in_before = order.ctx.applied_fills.contains_key(id);
                let in_after = scratch_ctx.applied_fills.contains_key(id);
                let payload_before = order.ctx.applied_fills.get(id);
                let payload_after = scratch_ctx.applied_fills.get(id);

                let order_disp = if !in_after {
                    OrderDisposition::NotAttributedByOrder(batch_outcome.clone())
                } else if !in_before {
                    if matches!(
                        &batch_outcome,
                        BatchOutcome::Halt(HaltReason::PostTerminalFill)
                    ) {
                        OrderDisposition::LateBooked
                    } else {
                        OrderDisposition::Attributed
                    }
                } else if payload_before != payload_after {
                    OrderDisposition::Enriched
                } else {
                    OrderDisposition::Duplicate
                };

                // 5. Inventory part
                let existing = inventory.get(id);
                let (inv_disp, entry_after, conflict) = if existing.is_none() {
                    // Recovered: Q present consistent + core Duplicate/Enriched
                    let provenance = if row_foreign {
                        InventoryProvenance::RowForeign
                    } else if matches!(
                        order_disp,
                        OrderDisposition::Duplicate | OrderDisposition::Enriched
                    ) && in_before
                    {
                        InventoryProvenance::Recovered
                    } else {
                        match &order_disp {
                            OrderDisposition::Attributed => InventoryProvenance::Attributed,
                            OrderDisposition::LateBooked => InventoryProvenance::LateBooked,
                            OrderDisposition::NotAttributedByOrder(o) => {
                                InventoryProvenance::NotAttributedByOrder(o.clone())
                            }
                            OrderDisposition::Refused(r) => InventoryProvenance::Refused(r.clone()),
                            OrderDisposition::NoRow => InventoryProvenance::NoRow,
                            _ => InventoryProvenance::Attributed,
                        }
                    };
                    let owner = owner_for(
                        &order_disp,
                        &order.ctx.client_order_id,
                        row_foreign,
                        in_after,
                    );
                    // Use final staged (after in-batch merges)
                    let final_staged = staging.get(id).cloned().unwrap_or_else(|| staged.clone());
                    let entry = InventoryEntry {
                        side: final_staged.side,
                        qty: final_staged.qty,
                        price_cents: final_staged.price_cents,
                        first_seen_ts_ns: e.ts_ns, // first_seen_ts_ns = E.ts_ns per §2.5.5
                        venue_order_id: final_staged.venue_order_id.clone(),
                        claimed_client_order_id: e.claimed_client_order_id.clone(),
                        owner,
                        fee_cents: final_staged.fee_cents,
                        source: e.source.clone(),
                        provenance: provenance.clone(),
                    };
                    planned_entries.insert(id.clone(), entry.clone());
                    (
                        InventoryDisposition::Ingested(provenance),
                        Some(entry),
                        None,
                    )
                } else {
                    let mut entry = existing.unwrap().clone();
                    let mut upgraded = false;
                    // owner None→Some
                    if entry.owner.is_none()
                        && !row_foreign
                        && in_after
                        && matches!(
                            order_disp,
                            OrderDisposition::Attributed
                                | OrderDisposition::LateBooked
                                | OrderDisposition::Duplicate
                                | OrderDisposition::Enriched
                        )
                    {
                        entry.owner = Some(order.ctx.client_order_id.clone());
                        upgraded = true;
                    }
                    // venue/fee None→Some from staged
                    let final_staged = staging.get(id).cloned().unwrap_or_else(|| staged.clone());
                    if entry.venue_order_id.is_none() && final_staged.venue_order_id.is_some() {
                        entry.venue_order_id = final_staged.venue_order_id.clone();
                        upgraded = true;
                    }
                    if entry.fee_cents.is_none() && final_staged.fee_cents.is_some() {
                        entry.fee_cents = final_staged.fee_cents;
                        upgraded = true;
                    }
                    if upgraded {
                        planned_entries.insert(id.clone(), entry.clone());
                        (InventoryDisposition::Upgraded, Some(entry), None)
                    } else {
                        (InventoryDisposition::Duplicate, Some(entry), None)
                    }
                };

                outcomes.push(ExecutionOutcome {
                    evidence: e.clone(),
                    order: order_disp,
                    inventory: inv_disp,
                    entry_after,
                    conflict,
                });
            }
        }
    }

    // Order part
    let (core_records, non_journal) = if let Some(ref t) = core_transition {
        split_effects(t.effects())
    } else {
        (vec![], vec![])
    };
    let order_changed = core_transition.is_some()
        && (scratch_state != *order.state
            || scratch_ctx != *order.ctx
            || !core_records.is_empty()
            || !non_journal.is_empty()
            || core_transition
                .as_ref()
                .map(|t| !t.effects().is_empty())
                .unwrap_or(false));

    let order_part = if order_changed {
        let seq = order.cursor.seq + 1;
        Some(OrderTxnRecord {
            ledger: order.ledger.clone(),
            client_order_id: order.ctx.client_order_id.clone(),
            seq,
            event: event_name(event).to_string(),
            outcome: batch_outcome.clone(),
            core_records,
            state_after: scratch_state.clone(),
            ctx_after: scratch_ctx.clone(),
        })
    } else {
        None
    };

    let need_journal = order_part.is_some()
        || outcomes.iter().any(|o| {
            matches!(
                o.inventory,
                InventoryDisposition::Ingested(_)
                    | InventoryDisposition::Upgraded
                    | InventoryDisposition::Conflict(_)
            )
        });

    let journal = if need_journal {
        Some(JournalRecord::ExecutionTxn(Box::new(ExecutionTxnRecord {
            ledger: inventory.ledger.clone(),
            inventory_generation_before: gen_before,
            executions: outcomes.clone(),
            order: order_part.clone(),
        })))
    } else {
        None
    };

    let advance = order_part.is_some();
    let next_seq = if advance {
        order.cursor.seq + 1
    } else {
        order.cursor.seq
    };

    Ok(ExecutionTransaction {
        outcomes,
        journal,
        effects: non_journal,
        prepared_inventory_ledger: inventory.ledger.clone(),
        expected_generation: gen_before,
        prepared_order_ledger: Some(order.ledger.clone()),
        expected_cursor: Some(order.cursor),
        expected_state: Some(order.state.clone()),
        expected_ctx: Some(order.ctx.clone()),
        order_state_after: if advance {
            Some(scratch_state)
        } else {
            None
        },
        order_ctx_after: if advance {
            Some(scratch_ctx)
        } else {
            None
        },
        advance_cursor: advance,
        next_seq,
        planned_entries,
        planned_conflicts,
    })
}

fn owner_for(
    order_disp: &OrderDisposition,
    cid: &ClientOrderId,
    row_foreign: bool,
    in_after: bool,
) -> Option<ClientOrderId> {
    if row_foreign || !in_after {
        return None;
    }
    match order_disp {
        OrderDisposition::Attributed
        | OrderDisposition::LateBooked
        | OrderDisposition::Duplicate
        | OrderDisposition::Enriched => Some(cid.clone()),
        _ => None,
    }
}

fn rebuild_event_with_fills(
    event: &OrderEvent,
    admitted: &[(FillId, StagedPayload)],
    admitted_at: &[usize],
) -> OrderEvent {
    // Emit from per-position admission results (flattened input order), not
    // from a rescan of raw event positions. Each admitted id is fed exactly
    // once with its final staging payload.
    let fills_from_admitted: Vec<FillRecord> = admitted
        .iter()
        .map(|(id, s)| s.as_fill_record(id))
        .collect();
    match event {
        OrderEvent::Fill { .. } => {
            let (id, s) = &admitted[0];
            OrderEvent::Fill {
                fill_id: id.clone(),
                qty: s.qty,
                price_cents: s.price_cents,
                ts_ns: s.first_seen_ts_ns,
                venue_order_id: s.venue_order_id.clone(),
                fee_cents: s.fee_cents,
            }
        }
        OrderEvent::ImmediateFillBackfillResult { .. } => OrderEvent::ImmediateFillBackfillResult {
            fills: fills_from_admitted,
        },
        OrderEvent::UnknownBackfillResult { exhaustive, matched } => {
            // Keep every matched record's position/status/qty, even if its
            // fills become empty. Place each admitted fill in the record that
            // contained that id's first-admitted occurrence.
            let mut rec_fills: Vec<Vec<FillRecord>> = vec![Vec::new(); matched.len()];
            for (&flatten_idx, (id, staged)) in admitted_at.iter().zip(admitted.iter()) {
                let mut n = 0usize;
                let mut placed = false;
                for (ri, m) in matched.iter().enumerate() {
                    let next = n + m.fills.len();
                    if flatten_idx < next {
                        rec_fills[ri].push(staged.as_fill_record(id));
                        placed = true;
                        break;
                    }
                    n = next;
                }
                if !placed && !matched.is_empty() {
                    rec_fills[matched.len() - 1].push(staged.as_fill_record(id));
                }
            }
            let new_matched: Vec<BackfillOrderRecord> = matched
                .iter()
                .zip(rec_fills)
                .map(|(m, fills)| BackfillOrderRecord {
                    client_order_id: m.client_order_id.clone(),
                    venue_order_id: m.venue_order_id.clone(),
                    status: m.status,
                    filled_qty: m.filled_qty,
                    remaining_qty: m.remaining_qty,
                    fills,
                })
                .collect();
            OrderEvent::UnknownBackfillResult {
                exhaustive: *exhaustive,
                matched: new_matched,
            }
        }
        OrderEvent::ReconcileResult {
            status,
            venue_order_id,
            filled_qty,
            remaining_qty,
            authority_complete,
            ..
        } => OrderEvent::ReconcileResult {
            status: *status,
            venue_order_id: venue_order_id.clone(),
            filled_qty: *filled_qty,
            remaining_qty: *remaining_qty,
            fills: fills_from_admitted,
            authority_complete: *authority_complete,
        },
        other => other.clone(),
    }
}

impl ExecutionTransaction {
    pub fn apply(
        self,
        order: Option<OrderTarget<'_>>,
        inventory: &mut MarketInventory,
    ) -> Result<Vec<ExecutionOutcome>, ApplyMismatch> {
        let expect_order = self.prepared_order_ledger.is_some();
        let found_order = order.is_some();
        if expect_order != found_order {
            return Err(ApplyMismatch::OrderTarget {
                expected: expect_order,
                found: found_order,
            });
        }
        if inventory.ledger != self.prepared_inventory_ledger {
            return Err(ApplyMismatch::LedgerIdentity);
        }
        if inventory.generation != self.expected_generation {
            return Err(ApplyMismatch::InventoryGeneration {
                expected: self.expected_generation,
                found: inventory.generation,
            });
        }
        if let Some(target) = order {
            let prepared_ledger = self.prepared_order_ledger.as_ref().unwrap();
            if target.ledger != prepared_ledger {
                return Err(ApplyMismatch::LedgerIdentity);
            }
            let expected_cursor = self.expected_cursor.unwrap();
            if target.cursor.seq != expected_cursor.seq {
                return Err(ApplyMismatch::OrderCursor {
                    expected: expected_cursor.seq,
                    found: target.cursor.seq,
                });
            }
            if *target.state != *self.expected_state.as_ref().unwrap()
                || *target.ctx != *self.expected_ctx.as_ref().unwrap()
            {
                return Err(ApplyMismatch::OrderStateOrCtx);
            }
            if self.advance_cursor {
                *target.state = self.order_state_after.unwrap();
                *target.ctx = self.order_ctx_after.unwrap();
                target.cursor.seq = self.next_seq;
            }
        }

        // Apply inventory mutations
        for (id, entry) in self.planned_entries {
            inventory.entries.insert(id, entry);
            inventory.generation = inventory.generation.saturating_add(1);
        }
        for (id, fact) in self.planned_conflicts {
            inventory.conflicts.insert(id, fact);
            inventory.generation = inventory.generation.saturating_add(1);
        }

        Ok(self.outcomes)
    }
}

#[cfg(test)]
mod tests {
    #![allow(unused_variables, unused_mut, unused_assignments)]
    use super::*;
    use crate::lifecycle::{AttemptId, rebuild_ctx_from_journal};

    fn scope(s: &str) -> ExecutionScope {
        ExecutionScope(s.into())
    }
    fn ledger(s: &str, m: &str) -> LedgerId {
        LedgerId {
            scope: scope(s),
            market: m.into(),
        }
    }
    fn cid(s: &str) -> ClientOrderId {
        ClientOrderId(s.into())
    }
    fn fid(s: &str) -> FillId {
        FillId(s.into())
    }
    fn vid(s: &str) -> VenueOrderId {
        VenueOrderId(s.into())
    }

    fn base_ctx(market: &str, side: Side, qty: u64) -> OrderCtx {
        OrderCtx::new(cid("c1"), market, "strat", side, 50, qty)
    }

    fn evidence(
        market: &str,
        id: &str,
        side: Side,
        qty: u64,
        price: u64,
        fee: Option<u64>,
    ) -> ExecutionEvidence {
        ExecutionEvidence {
            scope: scope("sub"),
            market: market.into(),
            execution_id: fid(id),
            side,
            qty,
            price_cents: price,
            ts_ns: 1000,
            venue_order_id: Some(vid("W1")),
            claimed_client_order_id: None,
            fee_cents: fee,
            source: ExecutionSource::WsFill,
        }
    }

    /// Drive New → Accepted via prepare_order_event path (no fills).
    fn to_accepted(qty: u64) -> (LedgerId, OrderState, OrderCtx, OrderCursor) {
        let led = ledger("sub", "MKT");
        let mut state = OrderState::New;
        let mut ctx = base_ctx("MKT", Side::BuyYes, qty);
        ctx.qty = qty;
        let mut cursor = OrderCursor { seq: 0 };
        for ev in [
            OrderEvent::PrepareSubmit,
            OrderEvent::StartSubmit {
                attempt_id: AttemptId("a1".into()),
            },
            OrderEvent::SubmitResponse {
                venue_order_id: vid("W1"),
                fill_count: 0,
                remaining_count: qty,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        ] {
            let oref = OrderRef {
                ledger: &led,
                cursor,
                state: &state,
                ctx: &ctx,
            };
            let txn = prepare_order_event(oref, &ev).expect("prep");
            let target = OrderTarget {
                ledger: &led,
                state: &mut state,
                ctx: &mut ctx,
                cursor: &mut cursor,
            };
            txn.apply(target).expect("apply");
        }
        (led, state, ctx, cursor)
    }

    fn apply_exec(
        led: &LedgerId,
        state: &mut OrderState,
        ctx: &mut OrderCtx,
        cursor: &mut OrderCursor,
        inv: &mut MarketInventory,
        event: &OrderEvent,
        evidences: &[ExecutionEvidence],
    ) -> ExecutionTransaction {
        let oref = OrderRef {
            ledger: led,
            cursor: *cursor,
            state,
            ctx,
        };
        let txn = prepare_execution_batch(oref, inv, event, evidences).expect("prep batch");
        // We need to return txn after apply — clone outcomes via apply
        let journal = txn.journal.clone();
        let effects = txn.effects.clone();
        let outcomes_before = txn.outcomes.clone();
        let target = OrderTarget {
            ledger: led,
            state,
            ctx,
            cursor,
        };
        let outcomes = txn.apply(Some(target), inv).expect("apply");
        assert_eq!(outcomes, outcomes_before);
        // reconstruct a lightweight view — tests mostly need outcomes/journal/effects/cursor
        ExecutionTransaction {
            outcomes,
            journal,
            effects,
            prepared_inventory_ledger: led.clone(),
            expected_generation: 0,
            prepared_order_ledger: Some(led.clone()),
            expected_cursor: None,
            expected_state: None,
            expected_ctx: None,
            order_state_after: None,
            order_ctx_after: None,
            advance_cursor: false,
            next_seq: 0,
            planned_entries: BTreeMap::new(),
            planned_conflicts: BTreeMap::new(),
        }
    }

    fn core_fill_cids(t: &ExecutionTransaction) -> Vec<FillId> {
        let Some(JournalRecord::ExecutionTxn(txn)) = t.journal.as_ref() else {
            return Vec::new();
        };
        let Some(order) = txn.order.as_ref() else {
            return Vec::new();
        };
        order
            .core_records
            .iter()
            .filter_map(|r| match r {
                JournalRecord::FillCid { fill_id, .. } | JournalRecord::Fill { fill_id, .. } => {
                    Some(fill_id.clone())
                }
                _ => None,
            })
            .collect()
    }

    // ── 1. permutation invariance ───────────────────────────────────────────
    #[test]
    fn a01_permutation_invariant_net_and_conflicts() {
        let (led, mut state, mut ctx, mut cursor) = to_accepted(1000);
        let mut inv = MarketInventory::new(led.clone());
        let e1 = evidence("MKT", "f1", Side::BuyYes, 100, 50, None);
        let e1b = {
            let mut e = e1.clone();
            e.price_cents = 200;
            e.ts_ns = 2000;
            e
        };
        // order A: f1 p100 then f1 p200
        let ev = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 100,
            price_cents: 50,
            ts_ns: 1000,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        // first delivery ok
        let t1 = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[e1.clone()]);
        assert!(matches!(t1.outcomes[0].order, OrderDisposition::Attributed));
        // conflict on second
        let ev2 = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 100,
            price_cents: 200,
            ts_ns: 2000,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let t2 = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev2, &[e1b.clone()]);
        assert!(matches!(
            t2.outcomes[0].inventory,
            InventoryDisposition::Conflict(ConflictClass::Core)
        ));
        let set_a: BTreeSet<_> = inv.conflicts().map(|c| c.execution_id.clone()).collect();
        assert_eq!(set_a, BTreeSet::from([fid("f1")]));

        // reverse order on fresh ledger
        let (led2, mut st2, mut cx2, mut cu2) = to_accepted(1000);
        let mut inv2 = MarketInventory::new(led2.clone());
        let t1r = apply_exec(&led2, &mut st2, &mut cx2, &mut cu2, &mut inv2, &ev2, &[e1b]);
        assert!(matches!(t1r.outcomes[0].order, OrderDisposition::Attributed));
        let t2r = apply_exec(&led2, &mut st2, &mut cx2, &mut cu2, &mut inv2, &ev, &[e1]);
        assert!(matches!(
            t2r.outcomes[0].inventory,
            InventoryDisposition::Conflict(ConflictClass::Core)
        ));
        let set_b: BTreeSet<_> = inv2.conflicts().map(|c| c.execution_id.clone()).collect();
        assert_eq!(set_a, set_b);
        // net only counts entries — conflicted second delivery not entered twice
        assert_eq!(inv.net(), inv2.net());
    }

    // ── 2. rebuild equals live ──────────────────────────────────────────────
    #[test]
    fn a02_rebuild_equals_live_and_duplicate_line() {
        let (led, mut state, mut ctx, mut cursor) = to_accepted(1000);
        let mut inv = MarketInventory::new(led.clone());
        let mut journals = Vec::new();
        for (id, qty) in [("f1", 100u64), ("f2", 50u64)] {
            let e = evidence("MKT", id, Side::BuyYes, qty, 50, None);
            let ev = OrderEvent::Fill {
                fill_id: fid(id),
                qty,
                price_cents: 50,
                ts_ns: 1000,
                venue_order_id: Some(vid("W1")),
                fee_cents: None,
            };
            let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[e]);
            if let Some(j) = t.journal {
                journals.push(j);
            }
        }
        // upgrade path: fee None→Some
        let mut e = evidence("MKT", "f1", Side::BuyYes, 100, 50, Some(3));
        e.ts_ns = 3000;
        let ev = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 100,
            price_cents: 50,
            ts_ns: 3000,
            venue_order_id: Some(vid("W1")),
            fee_cents: Some(3),
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[e]);
        if let Some(j) = t.journal {
            journals.push(j);
        }
        let rebuilt = MarketInventory::rebuild(led.clone(), &journals).expect("rebuild");
        assert_eq!(rebuilt.entries, inv.entries);
        assert_eq!(rebuilt.conflicts, inv.conflicts);
        assert_eq!(rebuilt.generation, inv.generation);
        // duplicated ingest line
        let mut dup = journals.clone();
        dup.push(journals[0].clone());
        let err = MarketInventory::rebuild(led, &dup).unwrap_err();
        assert!(matches!(err, RebuildError::DuplicateExecutionRecord(_)));
    }

    // ── 3. two-source conflict Both witness ─────────────────────────────────
    #[test]
    fn a03_two_source_conflict_both_witness() {
        let (led, mut state, mut ctx, mut cursor) = to_accepted(1000);
        let mut inv = MarketInventory::new(led.clone());
        // seed entry via NoRow
        let e0 = evidence("MKT", "f1", Side::BuyYes, 100, 50, None);
        let t0 = prepare_execution(None, &inv, &e0).unwrap();
        t0.apply(None, &mut inv).unwrap();
        // put conflicting Q: same id different price in ctx
        ctx.applied_fills.insert(
            fid("f1"),
            FillPayload {
                qty: 100,
                price_cents: 99,
                fee: None,
                venue_order_id: Some(vid("W1")),
                ts_ns: 1000,
            },
        );
        // evidence agrees with P (50) not Q (99)
        let e = evidence("MKT", "f1", Side::BuyYes, 100, 50, None);
        let ev = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 100,
            price_cents: 50,
            ts_ns: 1000,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let oref = OrderRef {
            ledger: &led,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let txn = prepare_execution_batch(oref, &inv, &ev, &[e]).unwrap();
        assert!(matches!(
            txn.outcomes[0].inventory,
            InventoryDisposition::Conflict(ConflictClass::Core)
        ));
        assert!(matches!(
            txn.outcomes[0].conflict.as_ref().map(|c| &c.witness),
            Some(ConflictWitness::Both { .. })
        ));
        assert!(txn.effects.iter().all(|e| !matches!(e, Effect::AccountFill { .. })));
        let net_before = inv.net();
        let _ = txn; // drop without apply still — apply to confirm no net change intent
        // re-prepare and apply
        let oref = OrderRef {
            ledger: &led,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let txn = prepare_execution_batch(oref, &inv, &ev, &[evidence("MKT", "f1", Side::BuyYes, 100, 50, None)]).unwrap();
        txn.apply(
            Some(OrderTarget {
                ledger: &led,
                state: &mut state,
                ctx: &mut ctx,
                cursor: &mut cursor,
            }),
            &mut inv,
        )
        .unwrap();
        assert_eq!(inv.net(), net_before);
    }

    // ── 4. metadata fee/venue ───────────────────────────────────────────────
    #[test]
    fn a04_metadata_fee_venue_rules() {
        let (led, mut state, mut ctx, mut cursor) = to_accepted(1000);
        let mut inv = MarketInventory::new(led.clone());
        let e = evidence("MKT", "f1", Side::BuyYes, 100, 50, None);
        let ev = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 100,
            price_cents: 50,
            ts_ns: 1000,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[e]);
        let seq_after = cursor.seq;
        // ts differs, same core ⇒ Duplicate, journal None
        let mut e2 = evidence("MKT", "f1", Side::BuyYes, 100, 50, None);
        e2.ts_ns = 9999;
        let ev2 = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 100,
            price_cents: 50,
            ts_ns: 9999,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev2, &[e2]);
        assert!(matches!(t.outcomes[0].order, OrderDisposition::Duplicate));
        assert!(t.journal.is_none());
        assert_eq!(cursor.seq, seq_after);
        // fee None→Some ⇒ Enriched + Upgraded
        let e3 = evidence("MKT", "f1", Side::BuyYes, 100, 50, Some(7));
        let ev3 = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 100,
            price_cents: 50,
            ts_ns: 1000,
            venue_order_id: Some(vid("W1")),
            fee_cents: Some(7),
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev3, &[e3]);
        assert!(matches!(t.outcomes[0].order, OrderDisposition::Enriched));
        assert!(matches!(t.outcomes[0].inventory, InventoryDisposition::Upgraded));
        assert!(t.journal.is_some());
        assert!(
            t.effects
                .iter()
                .any(|e| matches!(e, Effect::AccountFeeCorrection { .. })),
            "fee None->Some must emit AccountFeeCorrection effect"
        );
        assert!(
            t.journal.as_ref().map(|j| matches!(j, JournalRecord::ExecutionTxn(x) if x.order.as_ref().map(|o| o.core_records.iter().any(|r| matches!(r, JournalRecord::FeeCorrection{..}))).unwrap_or(false))).unwrap_or(false),
            "fee None->Some must record FeeCorrection in order part"
        );
        // fee Some(a)->Some(b) => FeeConflict
        let e4 = evidence("MKT", "f1", Side::BuyYes, 100, 50, Some(9));
        let ev4 = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 100,
            price_cents: 50,
            ts_ns: 1000,
            venue_order_id: Some(vid("W1")),
            fee_cents: Some(9),
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev4, &[e4]);
        assert!(matches!(
            t.outcomes[0].order,
            OrderDisposition::Refused(RefusalReason::FeeConflict)
        ));
        assert!(matches!(
            t.outcomes[0].inventory,
            InventoryDisposition::Conflict(ConflictClass::Fee)
        ));
        // fee Some->None re-delivery => Duplicate (Some->None never shown to core)
        let e5 = evidence("MKT", "f1", Side::BuyYes, 100, 50, None);
        let ev5 = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 100,
            price_cents: 50,
            ts_ns: 1000,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let net_before = inv.net();
        let seq_before_e5 = cursor.seq;
        let state_before_e5 = state.clone();
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev5, &[e5]);
        assert!(matches!(t.outcomes[0].order, OrderDisposition::Duplicate));
        assert!(matches!(
            t.outcomes[0].inventory,
            InventoryDisposition::Duplicate
        ));
        assert_eq!(inv.get(&fid("f1")).unwrap().fee_cents, Some(7));
        assert_eq!(inv.net(), net_before);
        // Canonical Some→None is a true duplicate: no new halt/order record or state change.
        assert!(
            t.journal.is_none(),
            "fee Some->None must not write a new ExecutionTxn/halt/order record"
        );
        assert_eq!(cursor.seq, seq_before_e5);
        assert_eq!(state, state_before_e5);
        // venue Some->None re-delivery => Duplicate
        let mut e6 = evidence("MKT", "f1", Side::BuyYes, 100, 50, Some(7));
        e6.venue_order_id = None;
        let ev6 = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 100,
            price_cents: 50,
            ts_ns: 1000,
            venue_order_id: None,
            fee_cents: Some(7),
        };
        let seq_before_e6 = cursor.seq;
        let state_before_e6 = state.clone();
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev6, &[e6]);
        assert!(matches!(t.outcomes[0].order, OrderDisposition::Duplicate));
        assert!(matches!(
            t.outcomes[0].inventory,
            InventoryDisposition::Duplicate
        ));
        assert_eq!(
            inv.get(&fid("f1")).unwrap().venue_order_id,
            Some(vid("W1"))
        );
        assert!(
            t.journal.is_none(),
            "venue Some->None must not write a new ExecutionTxn/halt/order record"
        );
        assert_eq!(cursor.seq, seq_before_e6);
        assert_eq!(state, state_before_e6);
        // E fee None with P fee 1 and Q fee 2 => Conflict(Fee), no core call
        let (led2, mut state2, mut ctx2, mut cursor2) = to_accepted(1000);
        let mut inv2 = MarketInventory::new(led2.clone());
        let e_p = evidence("MKT", "fx", Side::BuyYes, 10, 50, Some(1));
        let ev_p = OrderEvent::Fill {
            fill_id: fid("fx"),
            qty: 10,
            price_cents: 50,
            ts_ns: 1,
            venue_order_id: Some(vid("W1")),
            fee_cents: Some(1),
        };
        apply_exec(
            &led2,
            &mut state2,
            &mut ctx2,
            &mut cursor2,
            &mut inv2,
            &ev_p,
            &[e_p],
        );
        // Force Q fee to 2 while P stays at 1 (enrichment path would sync; patch ctx directly)
        let payload = ctx2.applied_fills.get_mut(&fid("fx")).expect("q");
        payload.fee = Some(2);
        let e_none = evidence("MKT", "fx", Side::BuyYes, 10, 50, None);
        let ev_none = OrderEvent::Fill {
            fill_id: fid("fx"),
            qty: 10,
            price_cents: 50,
            ts_ns: 2,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let seq_before = cursor2.seq;
        let t = apply_exec(
            &led2,
            &mut state2,
            &mut ctx2,
            &mut cursor2,
            &mut inv2,
            &ev_none,
            &[e_none],
        );
        assert!(matches!(
            t.outcomes[0].order,
            OrderDisposition::Refused(RefusalReason::FeeConflict)
        ));
        assert!(matches!(
            t.outcomes[0].inventory,
            InventoryDisposition::Conflict(ConflictClass::Fee)
        ));
        assert_eq!(cursor2.seq, seq_before, "fee conflict must not call core / advance cursor");
        assert_eq!(inv2.get(&fid("fx")).unwrap().fee_cents, Some(1));
    }

    // ── 5. pins ─────────────────────────────────────────────────────────────
    #[test]
    fn a05_pins_no_conflicting_payload_halt_no_account_fill_on_order_event() {
        let (led, state, ctx, cursor) = to_accepted(100);
        let oref = OrderRef {
            ledger: &led,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let ev = OrderEvent::PrepareSubmit; // already prepared — reject or illegal
        // use CancelRequested on Accepted — no fills
        let ev = OrderEvent::CancelRequested;
        let txn = prepare_order_event(oref, &ev).unwrap();
        assert!(txn
            .effects
            .iter()
            .all(|e| !matches!(e, Effect::AccountFill { .. })));
        // FillsMustUseExecutionApi for fill-bearing backfill
        let oref = OrderRef {
            ledger: &led,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let bad = OrderEvent::ImmediateFillBackfillResult {
            fills: vec![FillRecord {
                fill_id: fid("f"),
                qty: 1,
                price_cents: 50,
                ts_ns: 1,
                venue_order_id: Some(vid("W1")),
                fee_cents: None,
            }],
        };
        let err = prepare_order_event(oref, &bad).unwrap_err();
        assert!(matches!(err, PrepareError::FillsMustUseExecutionApi { .. }));
    }

    // ── 6. backfill bypass ──────────────────────────────────────────────────
    #[test]
    fn a06_backfill_bypass_and_evidence_mismatch() {
        let (led, state, ctx, cursor) = to_accepted(100);
        let oref = OrderRef {
            ledger: &led,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        for (name, ev) in [
            (
                "imm",
                OrderEvent::ImmediateFillBackfillResult {
                    fills: vec![FillRecord {
                        fill_id: fid("f"),
                        qty: 1,
                        price_cents: 50,
                        ts_ns: 1,
                        venue_order_id: Some(vid("W1")),
                        fee_cents: None,
                    }],
                },
            ),
            (
                "unk",
                OrderEvent::UnknownBackfillResult {
                    exhaustive: true,
                    matched: vec![BackfillOrderRecord {
                        client_order_id: cid("c1"),
                        venue_order_id: vid("W1"),
                        status: crate::lifecycle::BackfillOrderStatus::Partial,
                        filled_qty: 1,
                        remaining_qty: 99,
                        fills: vec![FillRecord {
                            fill_id: fid("f"),
                            qty: 1,
                            price_cents: 50,
                            ts_ns: 1,
                            venue_order_id: Some(vid("W1")),
                            fee_cents: None,
                        }],
                    }],
                },
            ),
            (
                "rec",
                OrderEvent::ReconcileResult {
                    status: crate::lifecycle::BackfillOrderStatus::Canceled,
                    venue_order_id: Some(vid("W1")),
                    filled_qty: 1,
                    remaining_qty: 0,
                    fills: vec![FillRecord {
                        fill_id: fid("f"),
                        qty: 1,
                        price_cents: 50,
                        ts_ns: 1,
                        venue_order_id: Some(vid("W1")),
                        fee_cents: None,
                    }],
                    authority_complete: true,
                },
            ),
        ] {
            let _ = name;
            let err = prepare_order_event(oref, &ev).unwrap_err();
            assert!(matches!(err, PrepareError::FillsMustUseExecutionApi { .. }));
        }
        // empty fills ⇒ OrderTxn
        let oref = OrderRef {
            ledger: &led,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let empty = OrderEvent::ImmediateFillBackfillResult { fills: vec![] };
        let txn = prepare_order_event(oref, &empty).unwrap();
        // may be no-change or records — either way not FillsMustUse
        let _ = txn;
        // through prepare_execution_batch (Fill on Accepted)
        let (led, mut state, mut ctx, mut cursor) = to_accepted(100);
        let mut inv = MarketInventory::new(led.clone());
        let e = evidence("MKT", "f", Side::BuyYes, 1, 50, None);
        let ev = OrderEvent::Fill {
            fill_id: fid("f"),
            qty: 1,
            price_cents: 50,
            ts_ns: 1000,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[e]);
        assert!(matches!(t.outcomes[0].order, OrderDisposition::Attributed), "{:?}", t.outcomes[0].order);
        assert!(matches!(
            t.outcomes[0].inventory,
            InventoryDisposition::Ingested(InventoryProvenance::Attributed)
        ));
        // qty mismatch
        let e_bad = evidence("MKT", "f2", Side::BuyYes, 2, 50, None);
        let ev = OrderEvent::Fill {
            fill_id: fid("f2"),
            qty: 1,
            price_cents: 50,
            ts_ns: 1,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let oref = OrderRef {
            ledger: &led,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let err = prepare_execution_batch(oref, &inv, &ev, &[e_bad]).unwrap_err();
        assert!(matches!(err, PrepareError::EvidenceFillMismatch { .. }));
    }

    // ── 7. in-batch ─────────────────────────────────────────────────────────
    #[test]
    fn a07_in_batch_dedup_and_fee_merge() {
        let (led, mut state, mut ctx, mut cursor) = to_accepted(1000);
        let mut inv = MarketInventory::new(led.clone());
        let e1 = evidence("MKT", "e1", Side::BuyYes, 10, 50, None);
        let e1d = e1.clone();
        let ev = OrderEvent::ImmediateFillBackfillResult {
            fills: vec![
                FillRecord {
                    fill_id: fid("e1"),
                    qty: 10,
                    price_cents: 50,
                    ts_ns: 1000,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: None,
                },
                FillRecord {
                    fill_id: fid("e1"),
                    qty: 10,
                    price_cents: 50,
                    ts_ns: 1000,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: None,
                },
            ],
        };
        let t = apply_exec(
            &led,
            &mut state,
            &mut ctx,
            &mut cursor,
            &mut inv,
            &ev,
            &[e1, e1d],
        );
        assert!(matches!(
            t.outcomes[0].inventory,
            InventoryDisposition::Ingested(_)
        ));
        assert!(matches!(
            t.outcomes[1].order,
            OrderDisposition::Refused(RefusalReason::InBatchDuplicate)
        ));
        assert_eq!(inv.net(), 10);
        // qty conflict in batch
        let (led, mut state, mut ctx, mut cursor) = to_accepted(1000);
        let mut inv = MarketInventory::new(led.clone());
        let a = evidence("MKT", "e1", Side::BuyYes, 10, 50, None);
        let mut b = evidence("MKT", "e1", Side::BuyYes, 20, 50, None);
        b.ts_ns = 2;
        let ev = OrderEvent::ImmediateFillBackfillResult {
            fills: vec![
                FillRecord {
                    fill_id: fid("e1"),
                    qty: 10,
                    price_cents: 50,
                    ts_ns: 1000,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: None,
                },
                FillRecord {
                    fill_id: fid("e1"),
                    qty: 20,
                    price_cents: 50,
                    ts_ns: 2,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: None,
                },
            ],
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[a, b]);
        assert!(matches!(
            t.outcomes[1].inventory,
            InventoryDisposition::Conflict(ConflictClass::Core)
        ));
        assert!(matches!(
            t.outcomes[1].conflict.as_ref().map(|c| &c.witness),
            Some(ConflictWitness::Staged(_))
        ));
        assert_eq!(inv.net(), 10);
        // qty0 second
        let (led, mut state, mut ctx, mut cursor) = to_accepted(1000);
        let mut inv = MarketInventory::new(led.clone());
        let a = evidence("MKT", "e1", Side::BuyYes, 10, 50, None);
        let mut z = evidence("MKT", "e1", Side::BuyYes, 0, 50, None);
        let ev = OrderEvent::ImmediateFillBackfillResult {
            fills: vec![
                FillRecord {
                    fill_id: fid("e1"),
                    qty: 10,
                    price_cents: 50,
                    ts_ns: 1000,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: None,
                },
                FillRecord {
                    fill_id: fid("e1"),
                    qty: 0,
                    price_cents: 50,
                    ts_ns: 1000,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: None,
                },
            ],
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[a, z]);
        assert!(matches!(
            t.outcomes[1].order,
            OrderDisposition::Ignored(IgnoreReason::ZeroQty)
        ));
        // fee None then Some(3) merge
        let (led, mut state, mut ctx, mut cursor) = to_accepted(1000);
        let mut inv = MarketInventory::new(led.clone());
        let a = evidence("MKT", "e1", Side::BuyYes, 10, 50, None);
        let mut b = evidence("MKT", "e1", Side::BuyYes, 10, 50, Some(3));
        let ev = OrderEvent::ImmediateFillBackfillResult {
            fills: vec![
                FillRecord {
                    fill_id: fid("e1"),
                    qty: 10,
                    price_cents: 50,
                    ts_ns: 1000,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: None,
                },
                FillRecord {
                    fill_id: fid("e1"),
                    qty: 10,
                    price_cents: 50,
                    ts_ns: 1000,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: Some(3),
                },
            ],
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[a, b]);
        assert_eq!(inv.get(&fid("e1")).unwrap().fee_cents, Some(3));
        assert!(matches!(
            t.outcomes[1].order,
            OrderDisposition::Refused(RefusalReason::InBatchDuplicate)
        ));
        // fee Some(1) vs Some(2)
        let (led, mut state, mut ctx, mut cursor) = to_accepted(1000);
        let mut inv = MarketInventory::new(led.clone());
        let a = evidence("MKT", "e1", Side::BuyYes, 10, 50, Some(1));
        let b = evidence("MKT", "e1", Side::BuyYes, 10, 50, Some(2));
        let ev = OrderEvent::ImmediateFillBackfillResult {
            fills: vec![
                FillRecord {
                    fill_id: fid("e1"),
                    qty: 10,
                    price_cents: 50,
                    ts_ns: 1000,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: Some(1),
                },
                FillRecord {
                    fill_id: fid("e1"),
                    qty: 10,
                    price_cents: 50,
                    ts_ns: 1000,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: Some(2),
                },
            ],
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[a, b]);
        assert_eq!(inv.get(&fid("e1")).unwrap().fee_cents, Some(1));
        assert!(matches!(
            t.outcomes[1].order,
            OrderDisposition::Refused(RefusalReason::FeeConflict)
        ));
    }

    // ── 8. owner ────────────────────────────────────────────────────────────
    #[test]
    fn a08_owner_rules_norow_upgrade_recovered() {
        let led = ledger("sub", "MKT");
        let mut inv = MarketInventory::new(led.clone());
        let mut e = evidence("MKT", "f1", Side::BuyYes, 10, 50, None);
        e.claimed_client_order_id = Some(cid("c"));
        let t = prepare_execution(None, &inv, &e).unwrap();
        t.apply(None, &mut inv).unwrap();
        assert!(inv.get(&fid("f1")).unwrap().owner.is_none());
        // later claim by row c
        let (led, mut state, mut ctx, mut cursor) = to_accepted(100);
        ctx.client_order_id = cid("c");
        let mut inv = MarketInventory::new(led.clone());
        let mut e = evidence("MKT", "f1", Side::BuyYes, 10, 50, None);
        e.claimed_client_order_id = Some(cid("c"));
        prepare_execution(None, &inv, &e)
            .unwrap()
            .apply(None, &mut inv)
            .unwrap();
        let ev = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 10,
            price_cents: 50,
            ts_ns: 1000,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[e]);
        assert!(matches!(t.outcomes[0].order, OrderDisposition::Attributed));
        assert!(matches!(t.outcomes[0].inventory, InventoryDisposition::Upgraded));
        assert_eq!(inv.get(&fid("f1")).unwrap().owner, Some(cid("c")));
        // other owner
        let mut state2 = state.clone();
        let mut ctx2 = ctx.clone();
        ctx2.client_order_id = cid("c2");
        let mut cursor2 = cursor;
        let e2 = evidence("MKT", "f1", Side::BuyYes, 10, 50, None);
        let t = apply_exec(&led, &mut state2, &mut ctx2, &mut cursor2, &mut inv, &ev, &[e2]);
        assert!(matches!(
            t.outcomes[0].order,
            OrderDisposition::Refused(RefusalReason::DuplicateOtherOwner { .. })
        ));
        // Recovered
        let (led, mut state, mut ctx, mut cursor) = to_accepted(100);
        ctx.applied_fills.insert(
            fid("fx"),
            FillPayload {
                qty: 5,
                price_cents: 50,
                fee: None,
                venue_order_id: Some(vid("W1")),
                ts_ns: 1,
            },
        );
        let mut inv = MarketInventory::new(led.clone());
        let e = evidence("MKT", "fx", Side::BuyYes, 5, 50, None);
        let ev = OrderEvent::Fill {
            fill_id: fid("fx"),
            qty: 5,
            price_cents: 50,
            ts_ns: 1,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[e]);
        assert!(matches!(t.outcomes[0].order, OrderDisposition::Duplicate));
        assert!(matches!(
            t.outcomes[0].inventory,
            InventoryDisposition::Ingested(InventoryProvenance::Recovered)
        ));
        assert_eq!(inv.get(&fid("fx")).unwrap().owner, Some(cid("c1")));
    }

    // ── 9. side mismatch ────────────────────────────────────────────────────
    #[test]
    fn a09_side_mismatch_live_and_frozen() {
        let (led, mut state, mut ctx, mut cursor) = to_accepted(100);
        let mut inv = MarketInventory::new(led.clone());
        let e = evidence("MKT", "f1", Side::SellYes, 10, 50, None); // opposite
        let ev = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 10,
            price_cents: 50,
            ts_ns: 1000,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[e]);
        assert!(matches!(
            t.outcomes[0].order,
            OrderDisposition::Refused(RefusalReason::SideMismatch { .. })
        ));
        assert!(matches!(state, OrderState::Halted { .. }));
        assert!(!ctx.applied_fills.contains_key(&fid("f1")));
        assert!(matches!(
            t.outcomes[0].inventory,
            InventoryDisposition::Ingested(InventoryProvenance::Refused(_))
        ));
        assert!(inv.get(&fid("f1")).unwrap().owner.is_none());
        // entry PRESENT + side mismatch => Duplicate, net unchanged (§4.9 / T2)
        // Seed via entry-absent SideMismatch ingest (stores evidence side), then re-deliver.
        {
            let (led, mut state, mut ctx, mut cursor) = to_accepted(100);
            let mut inv = MarketInventory::new(led.clone());
            let e_bad = evidence("MKT", "present", Side::SellYes, 10, 50, None);
            let ev_bad = OrderEvent::Fill {
                fill_id: fid("present"),
                qty: 10,
                price_cents: 50,
                ts_ns: 1,
                venue_order_id: Some(vid("W1")),
                fee_cents: None,
            };
            let t0 = apply_exec(
                &led,
                &mut state,
                &mut ctx,
                &mut cursor,
                &mut inv,
                &ev_bad,
                &[e_bad.clone()],
            );
            assert!(matches!(
                t0.outcomes[0].inventory,
                InventoryDisposition::Ingested(InventoryProvenance::Refused(_))
            ));
            let net_before = inv.net();
            let gen_before = inv.generation();
            let entry_before = inv.get(&fid("present")).unwrap().clone();
            let e_bad2 = evidence("MKT", "present", Side::SellYes, 10, 50, None);
            let ev_bad2 = OrderEvent::Fill {
                fill_id: fid("present"),
                qty: 10,
                price_cents: 50,
                ts_ns: 2,
                venue_order_id: Some(vid("W1")),
                fee_cents: None,
            };
            let t = apply_exec(
                &led,
                &mut state,
                &mut ctx,
                &mut cursor,
                &mut inv,
                &ev_bad2,
                &[e_bad2],
            );
            assert!(matches!(
                t.outcomes[0].order,
                OrderDisposition::Refused(RefusalReason::SideMismatch { .. })
            ));
            assert!(matches!(
                t.outcomes[0].inventory,
                InventoryDisposition::Duplicate
            ));
            assert_eq!(inv.net(), net_before);
            assert_eq!(inv.generation(), gen_before);
            assert_eq!(inv.get(&fid("present")).unwrap().side, entry_before.side);
        }
        // frozen Filled
        let (led, mut state, mut ctx, mut cursor) = to_accepted(10);
        // fill fully
        let mut inv = MarketInventory::new(led.clone());
        let e = evidence("MKT", "ff", Side::BuyYes, 10, 50, None);
        let ev = OrderEvent::Fill {
            fill_id: fid("ff"),
            qty: 10,
            price_cents: 50,
            ts_ns: 1,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[e]);
        assert!(matches!(state, OrderState::Filled));
        let seq = cursor.seq;
        let e2 = evidence("MKT", "f2", Side::SellYes, 1, 50, None);
        let ev2 = OrderEvent::Fill {
            fill_id: fid("f2"),
            qty: 1,
            price_cents: 50,
            ts_ns: 2,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev2, &[e2]);
        assert!(matches!(
            t.outcomes[0].order,
            OrderDisposition::Refused(RefusalReason::SideMismatch { .. })
        ));
        assert!(matches!(state, OrderState::Filled));
        assert!(t.journal.as_ref().map(|j| match j {
            JournalRecord::ExecutionTxn(x) => x.order.is_none(),
            _ => false,
        }).unwrap_or(true));
        assert_eq!(cursor.seq, seq); // no order part ⇒ cursor unchanged when only inventory ingest
        // Actually ingest of side-mismatch creates journal with inventory — order part none, cursor unchanged
    }

    // ── 10. overfill ────────────────────────────────────────────────────────
    #[test]
    fn a10_overfill_not_attributed_still_ingested() {
        let (led, mut state, mut ctx, mut cursor) = to_accepted(10);
        let mut inv = MarketInventory::new(led.clone());
        let e = evidence("MKT", "f1", Side::BuyYes, 20, 50, None); // over
        let ev = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 20,
            price_cents: 50,
            ts_ns: 1,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[e]);
        assert!(matches!(
            t.outcomes[0].order,
            OrderDisposition::NotAttributedByOrder(BatchOutcome::Halt(HaltReason::OverFill { .. }))
        ));
        assert!(matches!(
            t.outcomes[0].inventory,
            InventoryDisposition::Ingested(InventoryProvenance::NotAttributedByOrder(_))
        ));
        assert!(inv.get(&fid("f1")).unwrap().owner.is_none());
        assert_eq!(inv.net(), 20);
    }

    // ── 11. post terminal late booked ───────────────────────────────────────
    #[test]
    fn a11_post_terminal_late_booked() {
        let (led, mut state, mut ctx, mut cursor) = to_accepted(100);
        // Fixture a Canceled/released row directly (cancel finalize needs authority latch).
        state = OrderState::Canceled;
        ctx.venue_order_id = Some(vid("W1"));
        ctx.latch_authority_complete();
        let mut inv = MarketInventory::new(led.clone());
        let e = evidence("MKT", "late", Side::BuyYes, 5, 50, None);
        let ev = OrderEvent::Fill {
            fill_id: fid("late"),
            qty: 5,
            price_cents: 50,
            ts_ns: 1,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[e]);
        assert!(matches!(t.outcomes[0].order, OrderDisposition::LateBooked));
        assert!(matches!(
            t.outcomes[0].inventory,
            InventoryDisposition::Ingested(InventoryProvenance::LateBooked)
        ));
        assert_eq!(inv.get(&fid("late")).unwrap().owner, Some(cid("c1")));
        assert!(t.effects.iter().any(|e| matches!(e, Effect::ReserveFull))
            || t.journal.as_ref().map(|j| match j {
                JournalRecord::ExecutionTxn(x) => x
                    .order
                    .as_ref()
                    .map(|o| matches!(o.outcome, BatchOutcome::Halt(HaltReason::PostTerminalFill)))
                    .unwrap_or(false),
                _ => false,
            }).unwrap_or(false));
    }

    // ── 12. ledger foreign / row foreign ────────────────────────────────────
    #[test]
    fn a12_ledger_and_row_foreign() {
        let (led, state, ctx, cursor) = to_accepted(100);
        let inv = MarketInventory::new(ledger("other", "MKT"));
        let e = evidence("MKT", "f1", Side::BuyYes, 1, 50, None);
        // evidence ledger is sub/MKT, inventory other/MKT
        let oref = OrderRef {
            ledger: &led,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let ev = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 1,
            price_cents: 50,
            ts_ns: 1,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let err = prepare_execution_batch(oref, &inv, &ev, &[e]).unwrap_err();
        assert!(matches!(err, PrepareError::ForeignLedger { .. }));
        // RowForeign: order ledger A, ctx market B, inventory B
        let led_a = ledger("sub", "A");
        let inv_b = MarketInventory::new(ledger("sub", "B"));
        let mut ctx = base_ctx("B", Side::BuyYes, 100);
        let state = OrderState::Accepted {
            venue_order_id: vid("W1"),
        };
        let cursor = OrderCursor { seq: 1 };
        let e = ExecutionEvidence {
            scope: scope("sub"),
            market: "B".into(),
            execution_id: fid("f1"),
            side: Side::BuyYes,
            qty: 1,
            price_cents: 50,
            ts_ns: 1,
            venue_order_id: Some(vid("W1")),
            claimed_client_order_id: None,
            fee_cents: None,
            source: ExecutionSource::WsFill,
        };
        let oref = OrderRef {
            ledger: &led_a,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let ev = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 1,
            price_cents: 50,
            ts_ns: 1,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let mut inv_b = inv_b;
        let txn = prepare_execution_batch(oref, &inv_b, &ev, &[e]).unwrap();
        assert!(matches!(
            txn.outcomes[0].order,
            OrderDisposition::Refused(RefusalReason::RowForeign { .. })
        ));
        assert!(matches!(
            txn.outcomes[0].inventory,
            InventoryDisposition::Ingested(InventoryProvenance::RowForeign)
        ));
        assert!(txn.outcomes[0].conflict.is_none());
        // OrderRefInconsistent
        let led = ledger("sub", "MKT");
        let ctx = base_ctx("OTHER", Side::BuyYes, 1);
        let oref = OrderRef {
            ledger: &led,
            cursor: OrderCursor { seq: 0 },
            state: &OrderState::New,
            ctx: &ctx,
        };
        let err = prepare_order_event(oref, &OrderEvent::PrepareSubmit).unwrap_err();
        assert!(matches!(err, PrepareError::OrderRefInconsistent { .. }));
    }

    // ── 13. zero qty ────────────────────────────────────────────────────────
    #[test]
    fn a13_zero_qty_ignored() {
        let (led, state, ctx, cursor) = to_accepted(100);
        let mut inv = MarketInventory::new(led.clone());
        let e = evidence("MKT", "z", Side::BuyYes, 0, 50, None);
        let oref = OrderRef {
            ledger: &led,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let ev = OrderEvent::Fill {
            fill_id: fid("z"),
            qty: 0,
            price_cents: 50,
            ts_ns: 1,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let txn = prepare_execution_batch(oref, &inv, &ev, &[e]).unwrap();
        assert!(matches!(
            txn.outcomes[0].order,
            OrderDisposition::Ignored(IgnoreReason::ZeroQty)
        ));
        assert!(txn.journal.is_none());
        assert!(txn.effects.is_empty());
        txn.apply(
            Some(OrderTarget {
                ledger: &led,
                state: &mut state.clone(),
                ctx: &mut ctx.clone(),
                cursor: &mut cursor.clone(),
            }),
            &mut inv,
        )
        .unwrap();
        assert!(inv.get(&fid("z")).is_none());
    }

    // ── 14. apply mismatches ────────────────────────────────────────────────
    #[test]
    fn a14_apply_guards() {
        let (led, mut state, mut ctx, mut cursor) = to_accepted(100);
        let mut inv = MarketInventory::new(led.clone());
        let e = evidence("MKT", "f1", Side::BuyYes, 1, 50, None);
        let ev = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 1,
            price_cents: 50,
            ts_ns: 1,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let oref = OrderRef {
            ledger: &led,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let txn = prepare_execution_batch(oref, &inv, &ev, &[e.clone()]).unwrap();
        // interleaved inventory mutation
        let e0 = evidence("MKT", "seed", Side::BuyYes, 1, 50, None);
        prepare_execution(None, &inv, &e0)
            .unwrap()
            .apply(None, &mut inv)
            .unwrap();
        let err = txn
            .apply(
                Some(OrderTarget {
                    ledger: &led,
                    state: &mut state,
                    ctx: &mut ctx,
                    cursor: &mut cursor,
                }),
                &mut inv,
            )
            .unwrap_err();
        assert!(matches!(err, ApplyMismatch::InventoryGeneration { .. }));
        // OrderTarget mismatch
        let oref = OrderRef {
            ledger: &led,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let txn = prepare_execution_batch(oref, &inv, &ev, &[e.clone()]).unwrap();
        let err = txn.apply(None, &mut inv).unwrap_err();
        assert!(matches!(err, ApplyMismatch::OrderTarget { .. }));
        // LedgerIdentity on order target
        let led_b = ledger("other", "MKT");
        let oref = OrderRef {
            ledger: &led,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let txn = prepare_order_event(oref, &OrderEvent::CancelRequested).unwrap();
        let err = txn
            .apply(OrderTarget {
                ledger: &led_b,
                state: &mut state,
                ctx: &mut ctx,
                cursor: &mut cursor,
            })
            .unwrap_err();
        assert!(matches!(err, ApplyMismatch::LedgerIdentity));
        // RowForeign apply with correct foreign target ledger
        let led_a = ledger("sub", "A");
        let mut inv_b = MarketInventory::new(ledger("sub", "B"));
        let mut ctx_rf = base_ctx("B", Side::BuyYes, 100);
        let mut state_rf = OrderState::Accepted {
            venue_order_id: vid("W1"),
        };
        let mut cursor_rf = OrderCursor { seq: 1 };
        let e = ExecutionEvidence {
            scope: scope("sub"),
            market: "B".into(),
            execution_id: fid("rf1"),
            side: Side::BuyYes,
            qty: 1,
            price_cents: 50,
            ts_ns: 1,
            venue_order_id: Some(vid("W1")),
            claimed_client_order_id: None,
            fee_cents: None,
            source: ExecutionSource::WsFill,
        };
        let oref = OrderRef {
            ledger: &led_a,
            cursor: cursor_rf,
            state: &state_rf,
            ctx: &ctx_rf,
        };
        let ev = OrderEvent::Fill {
            fill_id: fid("rf1"),
            qty: 1,
            price_cents: 50,
            ts_ns: 1,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let txn = prepare_execution_batch(oref, &inv_b, &ev, &[e]).unwrap();
        txn.apply(
            Some(OrderTarget {
                ledger: &led_a,
                state: &mut state_rf,
                ctx: &mut ctx_rf,
                cursor: &mut cursor_rf,
            }),
            &mut inv_b,
        )
        .unwrap();
        assert!(inv_b.get(&fid("rf1")).is_some());
        assert_eq!(cursor_rf.seq, 1);
        // T3 / §4.14: RowForeign txn + (s,A) target after cursor advanced => OrderCursor
        {
            let led_a = ledger("sub", "A");
            let mut inv_b = MarketInventory::new(ledger("sub", "B"));
            let mut ctx_rf = base_ctx("B", Side::BuyYes, 100);
            let mut state_rf = OrderState::Accepted {
                venue_order_id: vid("W1"),
            };
            let mut cursor_rf = OrderCursor { seq: 1 };
            let e = ExecutionEvidence {
                scope: scope("sub"),
                market: "B".into(),
                execution_id: fid("rf_adv"),
                side: Side::BuyYes,
                qty: 1,
                price_cents: 50,
                ts_ns: 1,
                venue_order_id: Some(vid("W1")),
                claimed_client_order_id: None,
                fee_cents: None,
                source: ExecutionSource::WsFill,
            };
            let oref = OrderRef {
                ledger: &led_a,
                cursor: cursor_rf,
                state: &state_rf,
                ctx: &ctx_rf,
            };
            let ev = OrderEvent::Fill {
                fill_id: fid("rf_adv"),
                qty: 1,
                price_cents: 50,
                ts_ns: 1,
                venue_order_id: Some(vid("W1")),
                fee_cents: None,
            };
            let txn = prepare_execution_batch(oref, &inv_b, &ev, &[e]).unwrap();
            cursor_rf.seq += 1; // advance after prepare
            let err = txn
                .apply(
                    Some(OrderTarget {
                        ledger: &led_a,
                        state: &mut state_rf,
                        ctx: &mut ctx_rf,
                        cursor: &mut cursor_rf,
                    }),
                    &mut inv_b,
                )
                .unwrap_err();
            assert!(matches!(err, ApplyMismatch::OrderCursor { .. }));
            assert!(inv_b.get(&fid("rf_adv")).is_none());
        }
        // T3 / §4.14: txn prepared on ledger A applied to equally empty ledger B => LedgerIdentity
        {
            let led_a = ledger("sub", "LA");
            let mut inv_a = MarketInventory::new(led_a.clone());
            let mut inv_b = MarketInventory::new(ledger("sub", "LB"));
            assert_eq!(inv_a.generation(), inv_b.generation());
            assert!(inv_a.entries.is_empty() && inv_b.entries.is_empty());
            let e = evidence("LA", "la1", Side::BuyYes, 1, 50, None);
            let (led, mut state, mut ctx, mut cursor) = to_accepted(100);
            // rebind to led_a market LA — build fresh accepted under LA
            let led = led_a.clone();
            let mut state = OrderState::Accepted {
                venue_order_id: vid("W1"),
            };
            let mut ctx = base_ctx("LA", Side::BuyYes, 100);
            let mut cursor = OrderCursor { seq: 1 };
            let oref = OrderRef {
                ledger: &led,
                cursor,
                state: &state,
                ctx: &ctx,
            };
            let ev = OrderEvent::Fill {
                fill_id: fid("la1"),
                qty: 1,
                price_cents: 50,
                ts_ns: 1,
                venue_order_id: Some(vid("W1")),
                fee_cents: None,
            };
            let txn = prepare_execution_batch(oref, &inv_a, &ev, &[e]).unwrap();
            let err = txn
                .apply(
                    Some(OrderTarget {
                        ledger: &led,
                        state: &mut state,
                        ctx: &mut ctx,
                        cursor: &mut cursor,
                    }),
                    &mut inv_b,
                )
                .unwrap_err();
            assert!(matches!(err, ApplyMismatch::LedgerIdentity));
            assert!(inv_b.entries.is_empty());
            assert_eq!(inv_b.generation(), 0);
        }
    }

    // ── 15. prepare_order_event differential ────────────────────────────────
    #[test]
    fn a15_prepare_order_event_differential() {
        let led = ledger("sub", "MKT");
        let state = OrderState::New;
        let ctx = base_ctx("MKT", Side::BuyYes, 10);
        let cursor = OrderCursor { seq: 0 };
        let oref = OrderRef {
            ledger: &led,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let txn = prepare_order_event(oref, &OrderEvent::PrepareSubmit).unwrap();
        let mut ctx2 = ctx.clone();
        let direct = apply_event(&state, &mut ctx2, &OrderEvent::PrepareSubmit);
        match (&txn.journal, &direct) {
            (Some(JournalRecord::OrderTxn(r)), TransitionOutcome::Accept { new_state, effects }) => {
                assert_eq!(&r.state_after, new_state);
                let (recs, _) = split_effects(effects);
                assert_eq!(r.core_records, recs);
            }
            _ => panic!("expected Accept OrderTxn"),
        }
        // Reject
        let state = OrderState::New;
        let oref = OrderRef {
            ledger: &led,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let txn = prepare_order_event(oref, &OrderEvent::CancelRequested).unwrap();
        assert!(txn.rejected.is_some());
        assert!(txn.journal.is_none());
    }

    // ── 16. rebuild_order ───────────────────────────────────────────────────
    #[test]
    fn a16_rebuild_order_seq_and_ledger_filter() {
        let (led, mut state, mut ctx, mut cursor) = to_accepted(100);
        let mut journals = Vec::new();
        // capture order txns from to_accepted path — recreate
        let led = ledger("sub", "MKT");
        let mut state = OrderState::New;
        let mut ctx = base_ctx("MKT", Side::BuyYes, 100);
        let mut cursor = OrderCursor { seq: 0 };
        for ev in [
            OrderEvent::PrepareSubmit,
            OrderEvent::StartSubmit {
                attempt_id: AttemptId("a1".into()),
            },
            OrderEvent::SubmitResponse {
                venue_order_id: vid("W1"),
                fill_count: 0,
                remaining_count: 100,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        ] {
            let oref = OrderRef {
                ledger: &led,
                cursor,
                state: &state,
                ctx: &ctx,
            };
            let txn = prepare_order_event(oref, &ev).unwrap();
            if let Some(j) = &txn.journal {
                journals.push(j.clone());
            }
            txn.apply(OrderTarget {
                ledger: &led,
                state: &mut state,
                ctx: &mut ctx,
                cursor: &mut cursor,
            })
            .unwrap();
        }
        let rebuilt = rebuild_order(&led, &cid("c1"), &journals).unwrap().unwrap();
        assert_eq!(rebuilt.0, state);
        assert_eq!(rebuilt.2.seq, cursor.seq);
        // gap
        let mut gapped = vec![journals[0].clone(), journals[2].clone()];
        let err = rebuild_order(&led, &cid("c1"), &gapped).unwrap_err();
        assert!(matches!(err, RebuildError::SeqGap { .. }));
        // other ledger ignored
        let other = ledger("x", "MKT");
        assert!(rebuild_order(&other, &cid("c1"), &journals)
            .unwrap()
            .is_none());
        // legacy
        let legacy = vec![JournalRecord::Fill {
            fill_id: fid("L1"),
            qty: 1,
            price_cents: 1,
            ts_ns: 1,
            fee_cents: None,
            venue_order_id: None,
        }];
        assert!(rebuild_order(&led, &cid("c1"), &legacy).unwrap().is_none());
        assert!(legacy_fill_ids(&legacy).contains(&fid("L1")));
    }

    // ── 17. serde roundtrip ─────────────────────────────────────────────────
    #[test]
    fn a17_serde_roundtrip_and_legacy_fold_ignores_new() {
        let (led, mut state, mut ctx, mut cursor) = to_accepted(100);
        let mut inv = MarketInventory::new(led.clone());
        let e = evidence("MKT", "f1", Side::BuyYes, 1, 50, None);
        let ev = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 1,
            price_cents: 50,
            ts_ns: 1,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[e]);
        let j = t.journal.unwrap();
        let bytes = serde_json::to_vec(&j).unwrap();
        let back: JournalRecord = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(j, back);
        // OrderCtx u128 fields
        ctx.attributed_notional_cents = 12345678901234567890;
        let oref = OrderRef {
            ledger: &led,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        // may reject on frozen — use Halted path via order txn from cancel on accepted earlier state
        let base = base_ctx("MKT", Side::BuyYes, 10);
        let folded = rebuild_ctx_from_journal(base.clone(), &[j.clone()]).unwrap();
        let folded2 = rebuild_ctx_from_journal(base, &[]).unwrap();
        // ignoring ExecutionTxn ⇒ same as empty for fill attribution from legacy fold
        assert_eq!(folded.applied_fills, folded2.applied_fills);
    }

    // ── 19. execution_debt ──────────────────────────────────────────────────
    #[test]
    fn a19_execution_debt_counts_once_saturating() {
        let mut a = base_ctx("MKT", Side::BuyYes, 100);
        a.fill_obligation = 50;
        a.attributed_fill_qty = 20;
        let mut b = base_ctx("MKT", Side::BuyYes, 100);
        b.fill_obligation = 10;
        b.attributed_fill_qty = 10;
        assert_eq!(execution_debt([&a, &b].into_iter()), 30);
        a.fill_obligation = u64::MAX;
        a.attributed_fill_qty = 0;
        assert_eq!(execution_debt([&a].into_iter()), u64::MAX);
    }

    // ── 20. net and health ──────────────────────────────────────────────────
    #[test]
    fn a20_net_health_checksum() {
        let (led, mut state, mut ctx, mut cursor) = to_accepted(1000);
        let mut inv = MarketInventory::new(led.clone());
        let buy = evidence("MKT", "b", Side::BuyYes, 300, 50, None);
        let sell = evidence("MKT", "s", Side::SellYes, 100, 50, None);
        // need sell side order for sell — use NoRow ingest for sell to simplify net
        prepare_execution(None, &inv, &buy)
            .unwrap()
            .apply(None, &mut inv)
            .unwrap();
        prepare_execution(None, &inv, &sell)
            .unwrap()
            .apply(None, &mut inv)
            .unwrap();
        assert_eq!(inv.net(), 200);
        assert_eq!(inv.view().executions, 2);
        assert_eq!(inv.checksum_diagnostic(200), ChecksumDiagnostic::Match);
        assert_eq!(
            inv.checksum_diagnostic(300),
            ChecksumDiagnostic::Mismatch {
                ledger: 200,
                observed: 300
            }
        );
        // conflict on f3
        let e = evidence("MKT", "f3", Side::SellYes, 100, 100, None);
        prepare_execution(None, &inv, &e)
            .unwrap()
            .apply(None, &mut inv)
            .unwrap();
        let mut e2 = evidence("MKT", "f3", Side::SellYes, 100, 200, None);
        let t = prepare_execution(None, &inv, &e2).unwrap();
        t.apply(None, &mut inv).unwrap();
        assert_eq!(inv.net(), 100); // 200 - 100 from f3 first
        assert_eq!(
            inv.view().health,
            InventoryHealth::Conflicted {
                core: 1,
                ownership: 0,
                fee: 0
            }
        );
    }

    // ── 21. all-refused structured still core-calls ─────────────────────────
    #[test]
    fn a21_all_refused_structured_core_call() {
        let (led, mut state, mut ctx, mut cursor) = to_accepted(100);
        // cancel pending
        let oref = OrderRef {
            ledger: &led,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let txn = prepare_order_event(oref, &OrderEvent::CancelRequested).unwrap();
        txn.apply(OrderTarget {
            ledger: &led,
            state: &mut state,
            ctx: &mut ctx,
            cursor: &mut cursor,
        })
        .unwrap();
        let mut inv = MarketInventory::new(led.clone());
        // seed f1 fee 1
        let e0 = evidence("MKT", "f1", Side::BuyYes, 1, 50, Some(1));
        prepare_execution(None, &inv, &e0)
            .unwrap()
            .apply(None, &mut inv)
            .unwrap();
        let e = evidence("MKT", "f1", Side::BuyYes, 1, 50, Some(2));
        let ev = OrderEvent::ReconcileResult {
            status: crate::lifecycle::BackfillOrderStatus::Canceled,
            venue_order_id: Some(vid("W1")),
            filled_qty: 1,
            remaining_qty: 0,
            fills: vec![FillRecord {
                fill_id: fid("f1"),
                qty: 1,
                price_cents: 50,
                ts_ns: 1000,
                venue_order_id: Some(vid("W1")),
                fee_cents: Some(2),
            }],
            authority_complete: true,
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[e]);
        assert!(matches!(
            t.outcomes[0].order,
            OrderDisposition::Refused(RefusalReason::FeeConflict)
        ));
        // order part present from empty-fill reconcile
        assert!(t.journal.as_ref().map(|j| match j {
            JournalRecord::ExecutionTxn(x) => x.order.is_some(),
            _ => false,
        }).unwrap_or(false));
        // bare Fill refused ⇒ no order part
        let (led, mut state, mut ctx, mut cursor) = to_accepted(100);
        let mut inv = MarketInventory::new(led.clone());
        let e0 = evidence("MKT", "f1", Side::BuyYes, 1, 50, Some(1));
        prepare_execution(None, &inv, &e0)
            .unwrap()
            .apply(None, &mut inv)
            .unwrap();
        let e = evidence("MKT", "f1", Side::BuyYes, 1, 50, Some(2));
        let ev = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 1,
            price_cents: 50,
            ts_ns: 1000,
            venue_order_id: Some(vid("W1")),
            fee_cents: Some(2),
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[e]);
        assert!(t.journal.as_ref().map(|j| match j {
            JournalRecord::ExecutionTxn(x) => x.order.is_none(),
            _ => true,
        }).unwrap_or(true));
    }

    // ── 22. mixed batch side mismatch filtered ──────────────────────────────
    #[test]
    fn a22_mixed_batch_side_mismatch_filtered() {
        // ImmediateFillUnattributed so ImmediateFillBackfillResult is legal.
        let led = ledger("sub", "MKT");
        let mut state = OrderState::New;
        let mut ctx = base_ctx("MKT", Side::BuyYes, 1000);
        let mut cursor = OrderCursor { seq: 0 };
        for ev in [
            OrderEvent::PrepareSubmit,
            OrderEvent::StartSubmit {
                attempt_id: AttemptId("a1".into()),
            },
            OrderEvent::SubmitResponse {
                venue_order_id: vid("W1"),
                fill_count: 20,
                remaining_count: 980,
                avg_price_cents: Some(50),
                fee_cents: None,
                snapshot_boundary: None,
            },
        ] {
            let oref = OrderRef {
                ledger: &led,
                cursor,
                state: &state,
                ctx: &ctx,
            };
            let txn = prepare_order_event(oref, &ev).unwrap();
            txn.apply(OrderTarget {
                ledger: &led,
                state: &mut state,
                ctx: &mut ctx,
                cursor: &mut cursor,
            })
            .unwrap();
        }
        assert!(
            matches!(state, OrderState::ImmediateFillUnattributed { .. }),
            "{state:?}"
        );
        let mut inv = MarketInventory::new(led.clone());
        let good = evidence("MKT", "ok", Side::BuyYes, 10, 50, None);
        let bad = evidence("MKT", "bad", Side::SellYes, 10, 50, None);
        let ev = OrderEvent::ImmediateFillBackfillResult {
            fills: vec![
                FillRecord {
                    fill_id: fid("ok"),
                    qty: 10,
                    price_cents: 50,
                    ts_ns: 1,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: None,
                },
                FillRecord {
                    fill_id: fid("bad"),
                    qty: 10,
                    price_cents: 50,
                    ts_ns: 1,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: None,
                },
            ],
        };
        let t = apply_exec(
            &led,
            &mut state,
            &mut ctx,
            &mut cursor,
            &mut inv,
            &ev,
            &[good, bad],
        );
        assert!(
            matches!(t.outcomes[0].order, OrderDisposition::Attributed),
            "{:?}",
            t.outcomes[0].order
        );
        assert!(matches!(
            t.outcomes[1].order,
            OrderDisposition::Refused(RefusalReason::SideMismatch { .. })
        ));
        assert!(!ctx.applied_fills.contains_key(&fid("bad")));
        assert!(ctx.applied_fills.contains_key(&fid("ok")));
    }

    // ── 23. positional metadata ─────────────────────────────────────────────
    #[test]
    fn a23_positional_metadata_mismatch() {
        let (led, state, ctx, cursor) = to_accepted(100);
        let inv = MarketInventory::new(led.clone());
        // event venue Some, evidence None
        let mut e = evidence("MKT", "f1", Side::BuyYes, 1, 50, None);
        e.venue_order_id = None;
        let ev = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 1,
            price_cents: 50,
            ts_ns: 1,
            venue_order_id: Some(vid("A")),
            fee_cents: None,
        };
        let oref = OrderRef {
            ledger: &led,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let err = prepare_execution_batch(oref, &inv, &ev, &[e]).unwrap_err();
        assert!(matches!(err, PrepareError::EvidenceFillMismatch { .. }));
        // event fee Some(1) evidence Some(2)
        let e = evidence("MKT", "f1", Side::BuyYes, 1, 50, Some(2));
        let ev = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 1,
            price_cents: 50,
            ts_ns: 1,
            venue_order_id: Some(vid("W1")),
            fee_cents: Some(1),
        };
        let oref = OrderRef {
            ledger: &led,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let err = prepare_execution_batch(oref, &inv, &ev, &[e]).unwrap_err();
        assert!(matches!(err, PrepareError::EvidenceFillMismatch { .. }));
        // event fee Some evidence None
        let e = evidence("MKT", "f1", Side::BuyYes, 1, 50, None);
        let ev = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 1,
            price_cents: 50,
            ts_ns: 1,
            venue_order_id: Some(vid("W1")),
            fee_cents: Some(1),
        };
        let oref = OrderRef {
            ledger: &led,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let err = prepare_execution_batch(oref, &inv, &ev, &[e]).unwrap_err();
        assert!(matches!(err, PrepareError::EvidenceFillMismatch { position: 0, .. }));
        // evidence Some event None accepted
        let e = evidence("MKT", "f1", Side::BuyYes, 1, 50, Some(5));
        let ev = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 1,
            price_cents: 50,
            ts_ns: 1,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let mut inv = inv;
        let mut state = state;
        let mut ctx = ctx;
        let mut cursor = cursor;
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[e]);
        assert!(matches!(t.outcomes[0].order, OrderDisposition::Attributed));
    }

    // ── T4 / §4.7 core-fill observables + §4.2 rebuild with repeated conflict ─
    fn to_immediate_fill_unattributed(qty: u64, fill_count: u64) -> (LedgerId, OrderState, OrderCtx, OrderCursor) {
        let led = ledger("sub", "MKT");
        let mut state = OrderState::New;
        let mut ctx = base_ctx("MKT", Side::BuyYes, qty);
        let mut cursor = OrderCursor { seq: 0 };
        let remaining = qty.saturating_sub(fill_count);
        for ev in [
            OrderEvent::PrepareSubmit,
            OrderEvent::StartSubmit {
                attempt_id: AttemptId("a1".into()),
            },
            OrderEvent::SubmitResponse {
                venue_order_id: vid("W1"),
                fill_count,
                remaining_count: remaining,
                avg_price_cents: Some(50),
                fee_cents: None,
                snapshot_boundary: None,
            },
        ] {
            let oref = OrderRef {
                ledger: &led,
                cursor,
                state: &state,
                ctx: &ctx,
            };
            let txn = prepare_order_event(oref, &ev).unwrap();
            txn.apply(OrderTarget {
                ledger: &led,
                state: &mut state,
                ctx: &mut ctx,
                cursor: &mut cursor,
            })
            .unwrap();
        }
        assert!(matches!(state, OrderState::ImmediateFillUnattributed { .. }));
        (led, state, ctx, cursor)
    }

    #[test]
    fn t4_core_fill_final_staging_fee_and_one_per_id() {
        // ImmediateFillUnattributed (a22 shape): [e1 fee None, e1 fee Some(3)]
        // => exactly ONE core fill carrying fee Some(3)
        let (led, mut state, mut ctx, mut cursor) = to_immediate_fill_unattributed(1000, 20);
        let mut inv = MarketInventory::new(led.clone());
        let a = evidence("MKT", "e1", Side::BuyYes, 10, 50, None);
        let b = evidence("MKT", "e1", Side::BuyYes, 10, 50, Some(3));
        let ev = OrderEvent::ImmediateFillBackfillResult {
            fills: vec![
                FillRecord {
                    fill_id: fid("e1"),
                    qty: 10,
                    price_cents: 50,
                    ts_ns: 1000,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: None,
                },
                FillRecord {
                    fill_id: fid("e1"),
                    qty: 10,
                    price_cents: 50,
                    ts_ns: 1000,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: Some(3),
                },
            ],
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[a, b]);
        assert!(matches!(
            t.outcomes[0].inventory,
            InventoryDisposition::Ingested(_)
        ));
        assert!(matches!(
            t.outcomes[1].order,
            OrderDisposition::Refused(RefusalReason::InBatchDuplicate)
        ));
        assert_eq!(inv.get(&fid("e1")).unwrap().fee_cents, Some(3));
        let core_fill = ctx.applied_fills.get(&fid("e1")).expect("one core fill");
        assert_eq!(core_fill.fee, Some(3), "core must see final staging fee Some(3)");
        assert_eq!(ctx.applied_fills.len(), 1);

        // [e1, e1] => one core fill
        let (led, mut state, mut ctx, mut cursor) = to_immediate_fill_unattributed(1000, 20);
        let mut inv = MarketInventory::new(led.clone());
        let a = evidence("MKT", "e1", Side::BuyYes, 10, 50, None);
        let b = a.clone();
        let ev = OrderEvent::ImmediateFillBackfillResult {
            fills: vec![
                FillRecord {
                    fill_id: fid("e1"),
                    qty: 10,
                    price_cents: 50,
                    ts_ns: 1000,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: None,
                },
                FillRecord {
                    fill_id: fid("e1"),
                    qty: 10,
                    price_cents: 50,
                    ts_ns: 1000,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: None,
                },
            ],
        };
        let _t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[a, b]);
        assert_eq!(ctx.applied_fills.len(), 1);
        assert!(ctx.applied_fills.contains_key(&fid("e1")));
    }

    #[test]
    fn t4_rebuild_equals_live_with_upgrades_and_repeated_conflict() {
        // §4.2: upgrades AND conflicts incl. same-id repeated conflict in one txn
        let (led, mut state, mut ctx, mut cursor) = to_accepted(1000);
        let mut inv = MarketInventory::new(led.clone());
        let mut journals = Vec::new();
        // ingest f1, f2
        for (id, qty) in [("f1", 100u64), ("f2", 50u64)] {
            let e = evidence("MKT", id, Side::BuyYes, qty, 50, None);
            let ev = OrderEvent::Fill {
                fill_id: fid(id),
                qty,
                price_cents: 50,
                ts_ns: 1000,
                venue_order_id: Some(vid("W1")),
                fee_cents: None,
            };
            let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[e]);
            if let Some(j) = t.journal {
                journals.push(j);
            }
        }
        // upgrade f1 fee None->Some
        let e = evidence("MKT", "f1", Side::BuyYes, 100, 50, Some(3));
        let ev = OrderEvent::Fill {
            fill_id: fid("f1"),
            qty: 100,
            price_cents: 50,
            ts_ns: 2000,
            venue_order_id: Some(vid("W1")),
            fee_cents: Some(3),
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[e]);
        if let Some(j) = t.journal {
            journals.push(j);
        }
        // same-id repeated conflict within one transaction (two disagreeing redeliveries)
        let mut c1 = evidence("MKT", "f2", Side::BuyYes, 50, 99, None);
        c1.ts_ns = 3001;
        let mut c2 = evidence("MKT", "f2", Side::BuyYes, 50, 88, None);
        c2.ts_ns = 3002;
        let ev = OrderEvent::ImmediateFillBackfillResult {
            fills: vec![
                FillRecord {
                    fill_id: fid("f2"),
                    qty: 50,
                    price_cents: 99,
                    ts_ns: 3001,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: None,
                },
                FillRecord {
                    fill_id: fid("f2"),
                    qty: 50,
                    price_cents: 88,
                    ts_ns: 3002,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: None,
                },
            ],
        };
        let t = apply_exec(
            &led,
            &mut state,
            &mut ctx,
            &mut cursor,
            &mut inv,
            &ev,
            &[c1, c2],
        );
        assert!(matches!(
            t.outcomes[0].inventory,
            InventoryDisposition::Conflict(ConflictClass::Core)
        ));
        assert!(matches!(
            t.outcomes[1].inventory,
            InventoryDisposition::Conflict(ConflictClass::Core)
        ));
        if let Some(j) = t.journal {
            journals.push(j);
        }
        let rebuilt = MarketInventory::rebuild(led.clone(), &journals).expect("rebuild");
        assert_eq!(rebuilt.entries, inv.entries);
        assert_eq!(rebuilt.conflicts, inv.conflicts);
        assert_eq!(
            rebuilt.generation, inv.generation,
            "rebuild generation must match live (per-txn distinct conflict id)"
        );
    }

    #[test]
    fn t4_c2_input_order_and_unknown_backfill_dedupe() {
        // C2: admitted fills keep input order (e2 before e1 in event) and
        // UnknownBackfillResult keeps matched order/status/counts with deduped fills.
        let (led, mut state, mut ctx, mut cursor) = to_immediate_fill_unattributed(1000, 20);
        let mut inv = MarketInventory::new(led.clone());
        let e2 = evidence("MKT", "e2", Side::BuyYes, 5, 50, None);
        let e1 = evidence("MKT", "e1", Side::BuyYes, 7, 50, None);
        let ev = OrderEvent::ImmediateFillBackfillResult {
            fills: vec![
                FillRecord {
                    fill_id: fid("e2"),
                    qty: 5,
                    price_cents: 50,
                    ts_ns: 1,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: None,
                },
                FillRecord {
                    fill_id: fid("e1"),
                    qty: 7,
                    price_cents: 50,
                    ts_ns: 2,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: None,
                },
            ],
        };
        let t = apply_exec(
            &led,
            &mut state,
            &mut ctx,
            &mut cursor,
            &mut inv,
            &ev,
            &[e2, e1],
        );
        // both attributed; rebuild_event must have fed e2 then e1 (not map-sorted e1 then e2)
        assert!(ctx.applied_fills.contains_key(&fid("e2")));
        assert!(ctx.applied_fills.contains_key(&fid("e1")));
        assert_eq!(ctx.applied_fills[&fid("e2")].qty, 5);
        assert_eq!(ctx.applied_fills[&fid("e1")].qty, 7);
        assert_eq!(core_fill_cids(&t), vec![fid("e2"), fid("e1")]);

        // Legal qty-1 structured case [(x,0),(y,1),(x,1)]: first-admitted order is
        // y then x. Core overfill halt attributes y; x stays not attributed.
        let (led, mut state, mut ctx, mut cursor) = to_immediate_fill_unattributed(1, 1);
        let mut inv = MarketInventory::new(led.clone());
        let x0 = evidence("MKT", "x", Side::BuyYes, 0, 50, None);
        let y1 = evidence("MKT", "y", Side::BuyYes, 1, 50, None);
        let x1 = evidence("MKT", "x", Side::BuyYes, 1, 50, None);
        let ev = OrderEvent::ImmediateFillBackfillResult {
            fills: vec![
                FillRecord {
                    fill_id: fid("x"),
                    qty: 0,
                    price_cents: 50,
                    ts_ns: 1000,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: None,
                },
                FillRecord {
                    fill_id: fid("y"),
                    qty: 1,
                    price_cents: 50,
                    ts_ns: 1000,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: None,
                },
                FillRecord {
                    fill_id: fid("x"),
                    qty: 1,
                    price_cents: 50,
                    ts_ns: 1000,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: None,
                },
            ],
        };
        let t = apply_exec(
            &led,
            &mut state,
            &mut ctx,
            &mut cursor,
            &mut inv,
            &ev,
            &[x0, y1, x1],
        );
        assert!(matches!(
            t.outcomes[0].order,
            OrderDisposition::Ignored(IgnoreReason::ZeroQty)
        ));
        assert!(matches!(t.outcomes[1].order, OrderDisposition::Attributed));
        assert!(matches!(
            t.outcomes[2].order,
            OrderDisposition::NotAttributedByOrder(BatchOutcome::Halt(HaltReason::OverFill { .. }))
        ));
        assert!(ctx.applied_fills.contains_key(&fid("y")));
        assert!(
            !ctx.applied_fills.contains_key(&fid("x")),
            "x must not be attributed after overfill halt; first-admitted order is y then x"
        );
        assert_eq!(inv.get(&fid("y")).unwrap().owner, Some(cid("c1")));
        assert_eq!(inv.get(&fid("x")).unwrap().owner, None);
        assert_eq!(
            core_fill_cids(&t),
            vec![fid("y")],
            "core FillCid order must be first-admitted y (not first-raw x)"
        );

        // UnknownBackfillResult: matched keep status/qty; fills filtered+deduped
        // Drive to SubmitUnknown so UnknownBackfillResult is a legal core event.
        let led = ledger("sub", "MKT");
        let mut state = OrderState::New;
        let mut ctx = base_ctx("MKT", Side::BuyYes, 1000);
        let mut cursor = OrderCursor { seq: 0 };
        for ev in [
            OrderEvent::PrepareSubmit,
            OrderEvent::StartSubmit {
                attempt_id: AttemptId("a1".into()),
            },
            OrderEvent::SubmitTimeout,
        ] {
            let oref = OrderRef {
                ledger: &led,
                cursor,
                state: &state,
                ctx: &ctx,
            };
            let txn = prepare_order_event(oref, &ev).unwrap();
            txn.apply(OrderTarget {
                ledger: &led,
                state: &mut state,
                ctx: &mut ctx,
                cursor: &mut cursor,
            })
            .unwrap();
        }
        assert!(
            matches!(state, OrderState::SubmitUnknown { .. }),
            "{state:?}"
        );
        let mut inv = MarketInventory::new(led.clone());
        let a = evidence("MKT", "u1", Side::BuyYes, 3, 50, None);
        let b = a.clone();
        let ev = OrderEvent::UnknownBackfillResult {
            exhaustive: true,
            matched: vec![BackfillOrderRecord {
                client_order_id: cid("c1"),
                venue_order_id: vid("W1"),
                status: crate::lifecycle::BackfillOrderStatus::Filled,
                filled_qty: 3,
                remaining_qty: 0,
                fills: vec![
                    FillRecord {
                        fill_id: fid("u1"),
                        qty: 3,
                        price_cents: 50,
                        ts_ns: 1,
                        venue_order_id: Some(vid("W1")),
                        fee_cents: None,
                    },
                    FillRecord {
                        fill_id: fid("u1"),
                        qty: 3,
                        price_cents: 50,
                        ts_ns: 1,
                        venue_order_id: Some(vid("W1")),
                        fee_cents: None,
                    },
                ],
            }],
        };
        let t = apply_exec(&led, &mut state, &mut ctx, &mut cursor, &mut inv, &ev, &[a, b]);
        assert!(matches!(
            t.outcomes[0].inventory,
            InventoryDisposition::Ingested(_)
        ));
        assert!(matches!(
            t.outcomes[1].order,
            OrderDisposition::Refused(RefusalReason::InBatchDuplicate)
        ));
        assert_eq!(ctx.applied_fills.len(), 1, "one fill after dedupe across batch");
        assert_eq!(inv.get(&fid("u1")).unwrap().qty, 3);
        let Some(JournalRecord::ExecutionTxn(txn)) = t.journal.as_ref() else {
            panic!("UnknownBackfill must journal");
        };
        let order = txn.order.as_ref().expect("order part");
        assert!(
            order.core_records.iter().any(|r| matches!(
                r,
                JournalRecord::ReconcileObserved {
                    venue_filled_qty: 3,
                    venue_remaining_qty: 0,
                    ..
                }
            )),
            "matched record status/qty must be kept: {order:?}"
        );
        assert_eq!(
            core_fill_cids(&t),
            vec![fid("u1")],
            "exactly-once filtering must leave one FillCid"
        );
    }

    #[test]
    fn t4_c3_row_foreign_once_and_preserves_p_identity() {
        // C3: repeated foreign id with None→Some fee/venue/claimed-cid ⇒
        // one Ingested(RowForeign) + one Duplicate; published entry is final staging.
        let led_a = ledger("sub", "A");
        let mut inv_b = MarketInventory::new(ledger("sub", "B"));
        let mut ctx = base_ctx("B", Side::BuyYes, 100);
        let mut state = OrderState::Accepted {
            venue_order_id: vid("W1"),
        };
        let mut cursor = OrderCursor { seq: 1 };
        let state_before = state.clone();
        let ctx_before = ctx.clone();
        let seq_before = cursor.seq;
        let e1 = ExecutionEvidence {
            scope: scope("sub"),
            market: "B".into(),
            execution_id: fid("e1"),
            side: Side::BuyYes,
            qty: 2,
            price_cents: 50,
            ts_ns: 10,
            venue_order_id: None,
            claimed_client_order_id: None,
            fee_cents: None,
            source: ExecutionSource::WsFill,
        };
        let e1b = ExecutionEvidence {
            scope: scope("sub"),
            market: "B".into(),
            execution_id: fid("e1"),
            side: Side::BuyYes,
            qty: 2,
            price_cents: 50,
            ts_ns: 11,
            venue_order_id: Some(vid("W1")),
            claimed_client_order_id: Some(cid("claimed")),
            fee_cents: Some(7),
            source: ExecutionSource::RestFill,
        };
        let oref = OrderRef {
            ledger: &led_a,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let ev = OrderEvent::ImmediateFillBackfillResult {
            fills: vec![
                FillRecord {
                    fill_id: fid("e1"),
                    qty: 2,
                    price_cents: 50,
                    ts_ns: 10,
                    venue_order_id: None,
                    fee_cents: None,
                },
                FillRecord {
                    fill_id: fid("e1"),
                    qty: 2,
                    price_cents: 50,
                    ts_ns: 11,
                    venue_order_id: Some(vid("W1")),
                    fee_cents: Some(7),
                },
            ],
        };
        // prepare with two evidences — use batch
        let txn = prepare_execution_batch(oref, &inv_b, &ev, &[e1, e1b]).unwrap();
        assert_eq!(txn.outcomes.len(), 2);
        assert!(matches!(
            txn.outcomes[0].inventory,
            InventoryDisposition::Ingested(InventoryProvenance::RowForeign)
        ));
        assert!(matches!(
            txn.outcomes[1].inventory,
            InventoryDisposition::Duplicate
        ));
        assert!(matches!(
            txn.outcomes[0].order,
            OrderDisposition::Refused(RefusalReason::RowForeign { .. })
        ));
        assert!(matches!(
            txn.outcomes[1].order,
            OrderDisposition::Refused(RefusalReason::RowForeign { .. })
        ));
        let ingested = txn.outcomes[0].entry_after.as_ref().expect("ingested entry");
        assert_eq!(ingested.fee_cents, Some(7));
        assert_eq!(ingested.venue_order_id, Some(vid("W1")));
        assert_eq!(ingested.claimed_client_order_id, Some(cid("claimed")));
        assert_eq!(ingested.owner, None);
        assert_eq!(ingested.provenance, InventoryProvenance::RowForeign);
        assert_eq!(ingested.first_seen_ts_ns, 10);
        assert_eq!(ingested.source, ExecutionSource::WsFill);
        assert_eq!(txn.planned_entries.len(), 1);
        let journal = txn.journal.clone();
        txn.apply(
            Some(OrderTarget {
                ledger: &led_a,
                state: &mut state,
                ctx: &mut ctx,
                cursor: &mut cursor,
            }),
            &mut inv_b,
        )
        .unwrap();
        assert_eq!(inv_b.net(), 2);
        assert_eq!(state, state_before, "foreign row must stay unchanged");
        assert_eq!(ctx, ctx_before, "foreign row must stay unchanged");
        assert_eq!(cursor.seq, seq_before, "foreign row must stay unchanged");
        let live = inv_b.get(&fid("e1")).expect("live foreign entry");
        assert_eq!(live.fee_cents, Some(7));
        assert_eq!(live.venue_order_id, Some(vid("W1")));
        assert_eq!(live.claimed_client_order_id, Some(cid("claimed")));
        assert_eq!(live.owner, None);
        assert_eq!(live.provenance, InventoryProvenance::RowForeign);
        assert_eq!(live.first_seen_ts_ns, 10);
        assert_eq!(live.source, ExecutionSource::WsFill);
        let journals = vec![journal.expect("journal")];
        let rebuilt = MarketInventory::rebuild(ledger("sub", "B"), &journals).unwrap();
        assert_eq!(rebuilt.entries, inv_b.entries);
        assert_eq!(rebuilt.conflicts, inv_b.conflicts);
        assert_eq!(rebuilt.generation, inv_b.generation);

        // C3 P-present foreign: keep owner/provenance/first_seen/source
        let prior = inv_b.get(&fid("e1")).unwrap().clone();
        let e_again = ExecutionEvidence {
            scope: scope("sub"),
            market: "B".into(),
            execution_id: fid("e1"),
            side: Side::BuyYes,
            qty: 2,
            price_cents: 50,
            ts_ns: 99,
            venue_order_id: Some(vid("W1")),
            claimed_client_order_id: Some(cid("claimed")),
            fee_cents: None,
            source: ExecutionSource::RestFill,
        };
        let oref = OrderRef {
            ledger: &led_a,
            cursor,
            state: &state,
            ctx: &ctx,
        };
        let ev = OrderEvent::Fill {
            fill_id: fid("e1"),
            qty: 2,
            price_cents: 50,
            ts_ns: 99,
            venue_order_id: Some(vid("W1")),
            fee_cents: None,
        };
        let txn = prepare_execution_batch(oref, &inv_b, &ev, &[e_again]).unwrap();
        assert!(matches!(
            txn.outcomes[0].inventory,
            InventoryDisposition::Duplicate
        ));
        txn.apply(
            Some(OrderTarget {
                ledger: &led_a,
                state: &mut state,
                ctx: &mut ctx,
                cursor: &mut cursor,
            }),
            &mut inv_b,
        )
        .unwrap();
        let after = inv_b.get(&fid("e1")).unwrap();
        assert_eq!(after.owner, prior.owner);
        assert_eq!(after.provenance, prior.provenance);
        assert_eq!(after.first_seen_ts_ns, prior.first_seen_ts_ns);
        assert_eq!(after.source, prior.source);
    }

    // ── 18 covered by running full suite; lightweight marker ────────────────
    #[test]
    fn a18_lifecycle_additive_smoke() {
        // new variants ignore in rebuild
        let base = base_ctx("MKT", Side::BuyYes, 1);
        let rec = JournalRecord::OrderTxn(Box::new(OrderTxnRecord {
            ledger: ledger("sub", "MKT"),
            client_order_id: cid("c1"),
            seq: 1,
            event: "PrepareSubmit".into(),
            outcome: BatchOutcome::Accept,
            core_records: vec![],
            state_after: OrderState::SubmitPrepared,
            ctx_after: base.clone(),
        }));
        let out = rebuild_ctx_from_journal(base.clone(), &[rec]).unwrap();
        assert_eq!(out, base);
    }
}
