//! Ordered lifecycle and execution transactions over canonical state.
use super::live_state::{LineageClear, LiveOmsBookState};
use crate::execution::{
    BatchOutcome, ExecutionEvidence, ExecutionOutcome, ExecutionSource, InventoryDisposition,
    InventoryProvenance, MarketInventory, OrderCursor, OrderRef, OrderTarget, PrepareError,
    prepare_execution, prepare_execution_batch, prepare_order_event,
};
use crate::lifecycle::{
    ClientOrderId as OmsClientOrderId, Effect, JournalRecord, OrderCtx, OrderEvent, OrderState,
    VenueOrderId,
};
use crate::observation::{AmendPhase, WireKind};
use anyhow::{Context, Result, bail};

/// The caller's existing journal writes. Each method forwards once and retains
/// the original error value; no buffering, retries or barriers are added.
pub trait TransactionIo {
    fn apply_effects(&mut self, effects: &[Effect]) -> Result<()>;
    fn commit_now(&mut self) -> Result<()>;
    fn len(&self) -> usize;
    fn has_fill_id(&self, id: &str) -> bool;
    fn wall_now_ns(&self) -> u64;
}

pub fn fill_event_from_evidence(evidence: &ExecutionEvidence) -> OrderEvent {
    OrderEvent::Fill {
        fill_id: evidence.execution_id.clone(),
        qty: evidence.qty,
        price_cents: evidence.price_cents,
        ts_ns: evidence.ts_ns,
        venue_order_id: evidence.venue_order_id.clone(),
        fee_cents: evidence.fee_cents,
    }
}

impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn ensure_ledger(&mut self, market: &str) {
        if self.ledger.market.is_empty() && !market.is_empty() {
            self.ledger.market = market.to_string();
            self.public_fields.inventory = MarketInventory::new(self.ledger.clone());
        }
    }

    pub fn fold_record(&mut self, rec: &JournalRecord) -> Result<()> {
        self.public_fields
            .registry
            .fold(self.fold_pos, rec, &self.public_fields.inventory)
            .map_err(|e| {
                self.public_fields.stopped = true;
                anyhow::anyhow!("registry fold STOP: {e:?}")
            })?;
        self.fold_pos = self.fold_pos.saturating_add(1);
        Ok(())
    }

    pub fn apply_and_journal(
        &mut self,
        state: &mut OrderState,
        ctx: &mut OrderCtx,
        cursor: &mut OrderCursor,
        event: &OrderEvent,
        journal: &mut dyn TransactionIo,
        source: ExecutionSource,
    ) -> Result<Vec<ExecutionOutcome>> {
        if self.public_fields.stopped {
            bail!("oms STOP: no further A/C activity until rebuild");
        }
        self.ensure_ledger(&ctx.market);
        let fill_ev = self.fill_evidences(ctx, event, source);
        if !fill_ev.is_empty() {
            return self.apply_execution_txn(state, ctx, cursor, event, &fill_ev, journal);
        }
        let oref = OrderRef {
            ledger: &self.ledger,
            cursor: *cursor,
            state,
            ctx,
        };
        let txn = prepare_order_event(oref, event).map_err(|e| match e {
            PrepareError::FillsMustUseExecutionApi { event, fills } => {
                anyhow::anyhow!("FillsMustUseExecutionApi event={event} fills={fills}")
            }
            other => anyhow::anyhow!("prepare_order_event: {other:?}"),
        })?;
        if let Some(reason) = txn.rejected {
            bail!("oms reject on {event:?}: {reason:?}");
        }
        let rec = txn.journal.clone();
        if matches!(event, OrderEvent::CloseSubmission { .. }) {
            if let Some(r) = rec.as_ref() {
                if let Err(e) = journal.apply_effects(&[Effect::AppendFsync(r.clone())]) {
                    self.public_fields.stopped = true;
                    return Err(e).context("oms journal CloseSubmission append failed");
                }
                if let Err(e) = journal.apply_effects(&txn.effects) {
                    self.public_fields.stopped = true;
                    return Err(e).context("oms CloseSubmission effects failed");
                }
                if let Err(e) = journal.commit_now() {
                    self.public_fields.stopped = true;
                    return Err(e).context("oms CloseSubmission commit_now failed");
                }
                let ledger = self.ledger.clone();
                let target = OrderTarget {
                    ledger: &ledger,
                    state,
                    ctx,
                    cursor,
                };
                txn.apply(target)
                    .map_err(|e| anyhow::anyhow!("OrderTxn apply mismatch: {e:?}"))?;
                self.fold_record(r)?;
                return Ok(Vec::new());
            }
            let ledger = self.ledger.clone();
            let target = OrderTarget {
                ledger: &ledger,
                state,
                ctx,
                cursor,
            };
            txn.apply(target)
                .map_err(|e| anyhow::anyhow!("OrderTxn apply mismatch: {e:?}"))?;
            return Ok(Vec::new());
        }
        let halt_reason = rec.as_ref().and_then(|r| match r {
            JournalRecord::OrderTxn(o) => match &o.outcome {
                BatchOutcome::Halt(h) => Some(h.clone()),
                _ => None,
            },
            _ => None,
        });
        if let Some(r) = rec.as_ref() {
            journal
                .apply_effects(&[Effect::AppendFsync(r.clone())])
                .with_context(|| format!("oms journal OrderTxn failed"))?;
        }
        journal.apply_effects(&txn.effects)?;
        if halt_reason.is_some() {
            if let Err(e) = journal.commit_now() {
                eprintln!("★ halt 证据落盘失败({halt_reason:?}):{e:#} —— 重启后 journal 可能缺尾");
            }
        }
        let ledger = self.ledger.clone();
        let target = OrderTarget {
            ledger: &ledger,
            state,
            ctx,
            cursor,
        };
        txn.apply(target)
            .map_err(|e| anyhow::anyhow!("OrderTxn apply mismatch: {e:?}"))?;
        if let Some(r) = rec.as_ref() {
            self.fold_record(r)?;
        }
        if let Some(reason) = halt_reason {
            bail!("oms halt on {event:?}: {reason:?}");
        }
        Ok(Vec::new())
    }

    pub fn apply_execution_txn(
        &mut self,
        state: &mut OrderState,
        ctx: &mut OrderCtx,
        cursor: &mut OrderCursor,
        event: &OrderEvent,
        evidences: &[ExecutionEvidence],
        journal: &mut dyn TransactionIo,
    ) -> Result<Vec<ExecutionOutcome>> {
        if state.is_submission_closed() {
            let _ = (event, ctx, cursor);
            let mut all = Vec::new();
            for evidence in evidences {
                all.extend(self.apply_norow_execution(evidence, journal)?);
            }
            return Ok(all);
        }
        let oref = OrderRef {
            ledger: &self.ledger,
            cursor: *cursor,
            state,
            ctx,
        };
        let txn = prepare_execution_batch(oref, &self.public_fields.inventory, event, evidences)
            .map_err(|e| anyhow::anyhow!("prepare_execution_batch: {e:?}"))?;
        let rec = txn.journal.clone();
        let halt_reason = rec.as_ref().and_then(|r| match r {
            JournalRecord::ExecutionTxn(e) => e.order.as_ref().and_then(|o| match &o.outcome {
                BatchOutcome::Halt(h) => Some(h.clone()),
                _ => None,
            }),
            _ => None,
        });
        if let Some(r) = rec.as_ref() {
            journal
                .apply_effects(&[Effect::AppendFsync(r.clone())])
                .with_context(|| "oms journal ExecutionTxn failed")?;
        }
        journal.apply_effects(&txn.effects)?;
        if halt_reason.is_some() {
            if let Err(e) = journal.commit_now() {
                eprintln!("★ halt 证据落盘失败({halt_reason:?}):{e:#} —— 重启后 journal 可能缺尾");
            }
        }
        let ledger = self.ledger.clone();
        let target = OrderTarget {
            ledger: &ledger,
            state,
            ctx,
            cursor,
        };
        let outcomes = txn
            .apply(Some(target), &mut self.public_fields.inventory)
            .map_err(|e| anyhow::anyhow!("ExecutionTxn apply mismatch: {e:?}"))?;
        if let Some(r) = rec.as_ref() {
            self.fold_record(r)?;
        }
        self.admit_from_outcomes(&outcomes);
        self.note_applied_exec_ts(&outcomes);
        let _ = (event, halt_reason);
        Ok(outcomes)
    }

    pub fn apply_identified_execution(
        &mut self,
        key: &str,
        evidence: &ExecutionEvidence,
        journal: &mut dyn TransactionIo,
    ) -> Result<Vec<ExecutionOutcome>> {
        if self.public_fields.stopped {
            bail!("oms STOP: no further A/C activity until rebuild");
        }
        self.ensure_ledger(&evidence.market);
        let (mut state, mut ctx, mut cursor) = {
            let o = self
                .by_client
                .get_mut(key)
                .ok_or_else(|| anyhow::anyhow!("oms unknown client {key}"))?;
            (o.state.clone(), o.ctx.clone(), o.cursor)
        };
        let event = fill_event_from_evidence(evidence);
        let r = self.apply_execution_txn(
            &mut state,
            &mut ctx,
            &mut cursor,
            &event,
            std::slice::from_ref(evidence),
            journal,
        );
        if let Some(o) = self.by_client.get_mut(key) {
            o.state = state;
            o.ctx = ctx;
            o.cursor = cursor;
        }
        r
    }

    pub fn apply_for_client(
        &mut self,
        key: &str,
        event: &OrderEvent,
        journal: &mut dyn TransactionIo,
        source: ExecutionSource,
    ) -> Result<Vec<ExecutionOutcome>> {
        let (mut state, mut ctx, mut cursor) = {
            let o = self
                .by_client
                .get_mut(key)
                .ok_or_else(|| anyhow::anyhow!("oms unknown client {key}"))?;
            (o.state.clone(), o.ctx.clone(), o.cursor)
        };
        let r = self.apply_and_journal(&mut state, &mut ctx, &mut cursor, event, journal, source);
        if let Some(o) = self.by_client.get_mut(key) {
            o.state = state;
            o.ctx = ctx;
            o.cursor = cursor;
        }
        r
    }

    pub fn apply_norow_execution(
        &mut self,
        evidence: &ExecutionEvidence,
        journal: &mut dyn TransactionIo,
    ) -> Result<Vec<ExecutionOutcome>> {
        if self.public_fields.stopped {
            bail!("oms STOP: no further A/C activity until rebuild");
        }
        self.ensure_ledger(&evidence.market);
        let txn = prepare_execution(None, &self.public_fields.inventory, evidence)
            .map_err(|e| anyhow::anyhow!("prepare_execution NoRow: {e:?}"))?;
        let rec = txn.journal.clone();
        if let Some(r) = rec.as_ref() {
            journal.apply_effects(&[Effect::AppendFsync(r.clone())])?;
        }
        journal.apply_effects(&txn.effects)?;
        let outcomes = txn
            .apply(None, &mut self.public_fields.inventory)
            .map_err(|e| anyhow::anyhow!("NoRow ExecutionTxn apply mismatch: {e:?}"))?;
        if let Some(r) = rec.as_ref() {
            self.fold_record(r)?;
        }
        self.admit_from_outcomes(&outcomes);
        self.note_applied_exec_ts(&outcomes);
        Ok(outcomes)
    }

    pub fn admit_from_outcomes(&mut self, outcomes: &[ExecutionOutcome]) {
        for o in outcomes {
            if !matches!(o.inventory, InventoryDisposition::Ingested(_)) {
                continue;
            }
            let Some(w) = o.evidence.venue_order_id.as_ref() else {
                continue;
            };
            let owner_none = o.entry_after.as_ref().is_some_and(|e| e.owner.is_none())
                || matches!(
                    o.inventory,
                    InventoryDisposition::Ingested(InventoryProvenance::NoRow)
                        | InventoryDisposition::Ingested(InventoryProvenance::RowForeign)
                );
            let venue_only_unresolved = self
                .public_fields
                .registry
                .get(&self.ledger, w)
                .is_some_and(|ws| matches!(ws.kind, WireKind::VenueOnly) && !ws.resolved);
            if !owner_none && !venue_only_unresolved {
                continue;
            }
            self.pending_wires.insert(w.clone());
        }
    }

    pub fn commit_registry_record(
        &mut self,
        record: JournalRecord,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        journal.apply_effects(&[Effect::AppendFsync(record.clone())])?;
        journal.commit_now()?;
        let pos = if matches!(
            record,
            JournalRecord::OrderAuthorityHold { .. } | JournalRecord::UserOrderReceived { .. }
        ) {
            journal.len().saturating_sub(1) as u64
        } else {
            journal.len() as u64
        };
        self.public_fields
            .registry
            .fold(pos, &record, &self.public_fields.inventory)
            .map_err(|e| anyhow::anyhow!("committed registry fold failed: {e:?}"))?;
        Ok(())
    }

    pub fn stage_amend_lineage_for_presend_group(
        &mut self,
        old_cid: &str,
        new_cid: &str,
        wire: &str,
        desired_remaining: u64,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        if self.public_fields.stopped {
            bail!("oms STOP: cannot stage amend lineage");
        }
        let record =
            self.prepare_amend_lineage_record(old_cid, new_cid, wire, desired_remaining)?;
        journal.apply_effects(&[Effect::AppendFsync(record.clone())])?;
        let pos = journal.len() as u64;
        self.public_fields
            .registry
            .fold(pos, &record, &self.public_fields.inventory)
            .map_err(|e| anyhow::anyhow!("staged amend lineage fold failed: {e:?}"))
    }

    pub fn advance_amend_lineage(
        &mut self,
        wire: &str,
        phase: AmendPhase,
        applied_at_utc_ns: Option<u64>,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let wire = VenueOrderId(wire.to_string());
        let mut lineage = self
            .public_fields
            .registry
            .get(&self.ledger, &wire)
            .and_then(|ws| ws.amend_lineage.clone())
            .ok_or_else(|| anyhow::anyhow!("amend lineage missing"))?;
        lineage.phase = phase;
        lineage.applied_at_utc_ns = applied_at_utc_ns.or(lineage.applied_at_utc_ns);
        let record = self
            .public_fields
            .registry
            .prepare_amend_lineage(&self.ledger, &wire, lineage)
            .map_err(|e| anyhow::anyhow!("amend lineage transition: {e:?}"))?;
        self.commit_registry_record(record, journal)?;
        if phase == AmendPhase::AppliedNeedsObservation {
            self.amend_applied_mono
                .insert(wire.0, std::time::Instant::now());
        }
        Ok(())
    }

    pub fn clear_unsent_amend_lineage(
        &mut self,
        old_cid: &str,
        new_cid: &str,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let Some(wire) = self
            .by_client
            .get(old_cid)
            .and_then(|o| o.ctx.venue_order_id.clone())
        else {
            return Ok(());
        };
        let matches = self
            .public_fields
            .registry
            .get(&self.ledger, &wire)
            .and_then(|ws| ws.amend_lineage.as_ref())
            .is_some_and(|l| {
                l.phase == AmendPhase::HttpPending
                    && l.old_cid == OmsClientOrderId(old_cid.to_string())
                    && l.new_cid == OmsClientOrderId(new_cid.to_string())
            });
        if matches {
            self.clear_amend_lineage_as(&wire.0, journal, "unsent")?;
        }
        Ok(())
    }

    pub fn clear_amend_lineage_as(
        &mut self,
        wire: &str,
        journal: &mut dyn TransactionIo,
        source: &'static str,
    ) -> Result<()> {
        let wire = VenueOrderId(wire.to_string());
        let Some(new_cid) = self
            .public_fields
            .registry
            .get(&self.ledger, &wire)
            .and_then(|ws| ws.amend_lineage.as_ref())
            .map(|l| l.new_cid.0.clone())
        else {
            return Ok(());
        };
        self.lineage_clears.push(LineageClear {
            new_cid,
            instant_ns: journal.wall_now_ns(),
            source,
        });
        self.commit_registry_record(
            JournalRecord::AmendLineage {
                ledger: self.ledger.clone(),
                wire: wire.clone(),
                lineage: None,
            },
            journal,
        )?;
        self.amend_applied_mono.remove(&wire.0);
        Ok(())
    }
}

/// The same authority-effect queue owned by the caller's journal.
pub trait AuthorityEffectsIo: TransactionIo {
    fn drain_authority_effects(&mut self) -> Vec<Effect>;
    fn requeue_authority_effects(&mut self, effects: Vec<Effect>);
}
