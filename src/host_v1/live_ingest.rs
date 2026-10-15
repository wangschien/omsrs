//! Execution routing and amend settlement over canonical live state.
use super::live_admission::AdmissionClient;
use super::live_observation::ObservationTrace;
use super::live_state::LiveOmsBookState;
use super::live_transaction::TransactionIo;
use crate::execution::{ExecutionEvidence, ExecutionOutcome, ExecutionSource, OrderDisposition};
use crate::lifecycle::{
    BackfillOrderRecord, BackfillOrderStatus, ClientOrderId as OmsClientOrderId, OrderEvent,
    OrderState, SubmissionCloseReason, VenueOrderId,
};
use crate::observation::{AmendPhase, Route};
use anyhow::{Result, bail};

/// Existing trace and event leaves; no order or authority state is written here.
pub trait ExecutionTrace<R>: ObservationTrace {
    fn execution_admission(
        core: &LiveOmsBookState<R, Self>,
        result: &'static str,
        evidence: &ExecutionEvidence,
        net: Option<i64>,
    );
    fn execution_result(
        core: &LiveOmsBookState<R, Self>,
        net_before: i64,
        evidence: &ExecutionEvidence,
        out: &Result<Vec<ExecutionOutcome>>,
    );
    fn order_filled(cid: &str);
    fn action_admit_venue(
        core: &LiveOmsBookState<R, Self>,
        kind: &'static str,
        result: &'static str,
        cid: &str,
        wire: Option<&str>,
        venue: Option<&str>,
    );
}

impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn ingest_execution<C: AdmissionClient<R>>(
        &mut self,
        evidence: ExecutionEvidence,
        journal: &mut dyn TransactionIo,
    ) -> Result<Vec<ExecutionOutcome>>
    where
        Stage: ExecutionTrace<R>,
    {
        if self.public_fields.stopped {
            Stage::execution_admission(self, "stopped", &evidence, None);
            bail!("oms STOP: no further A/C activity until rebuild");
        }
        if evidence.scope.0 != self.public_fields.expected_subaccount.to_string() {
            Stage::execution_admission(self, "foreign_scope", &evidence, None);
            bail!("ingest_execution: foreign scope {}", evidence.scope.0);
        }
        let net_before = i64::try_from(self.public_fields.inventory.net()).unwrap_or(i64::MAX);
        self.ensure_ledger(&evidence.market);
        let mut owner: Option<(String, bool)> = None;
        let mut out = if let Some(cid) = evidence.claimed_client_order_id.as_ref() {
            if self.by_client.contains_key(&cid.0) {
                let mismatch = {
                    let order = self.by_client.get(&cid.0).expect("contains_key");
                    match (
                        order.ctx.venue_order_id.as_ref(),
                        evidence.venue_order_id.as_ref(),
                    ) {
                        (Some(bound), Some(got)) if bound != got => true,
                        _ => false,
                    }
                };
                if mismatch {
                    self.apply_norow_execution(&evidence, journal)
                } else {
                    owner = Some((cid.0.clone(), self.row_is_normal_terminal(&cid.0)));
                    self.apply_identified_execution(&cid.0, &evidence, journal)
                }
            } else {
                self.apply_norow_execution(&evidence, journal)
            }
        } else {
            let Some(wire) = evidence.venue_order_id.as_ref() else {
                let r = self.apply_norow_execution(&evidence, journal);
                Stage::execution_result(self, net_before, &evidence, &r);
                return r;
            };
            match self.public_fields.registry.route(
                &self.ledger,
                wire,
                &evidence.execution_id,
                &self.public_fields.inventory,
            ) {
                Route::ExistingOwner(c) | Route::UnownedUnique(c) | Route::NewUnique(c) => {
                    if self.by_client.contains_key(&c.0) {
                        owner = Some((c.0.clone(), self.row_is_normal_terminal(&c.0)));
                        self.apply_identified_execution(&c.0, &evidence, journal)
                    } else {
                        self.apply_norow_execution(&evidence, journal)
                    }
                }
                Route::VenueOnly | Route::NewAmbiguous | Route::UnownedAmbiguous => {
                    self.apply_norow_execution(&evidence, journal)
                }
            }
        };
        if let Some((cid, true)) = owner.as_ref() {
            let late_booked = match &out {
                Ok(outcomes) => outcomes
                    .iter()
                    .any(|o| matches!(o.order, OrderDisposition::LateBooked)),
                Err(_) => {
                    self.by_client
                        .get(cid)
                        .is_some_and(|order| order.is_post_terminal_booked())
                        && journal.has_fill_id(&evidence.execution_id.0)
                }
            };
            if late_booked {
                self.late_after_terminal.insert(cid.clone());
            }
        }
        if out.is_ok() {
            if let Some(wire) = evidence.venue_order_id.as_ref() {
                if let Err(err) = self.apply_user_order_terminal_if_complete::<C>(wire, journal) {
                    out = Err(err);
                }
            }
        }
        if out.is_ok() {
            let wire = evidence.venue_order_id.clone().or_else(|| {
                evidence
                    .claimed_client_order_id
                    .as_ref()
                    .and_then(|cid| self.by_client.get(&cid.0))
                    .and_then(|row| row.ctx.venue_order_id.clone())
            });
            if let Some(wire) = wire {
                if let Err(err) = self.reprove_resting_after_fill::<C>(&wire, journal) {
                    out = Err(err);
                }
            }
        }
        if out.is_ok() {
            if let (Some(cid), Some(wire)) = (
                evidence.claimed_client_order_id.as_ref(),
                evidence.venue_order_id.as_ref(),
            ) {
                if let Ok(cid) = C::try_from_string(cid.0.clone()) {
                    if let Err(err) =
                        self.resolve_unknown_child_if_fully_filled::<C>(&cid, wire, journal)
                    {
                        out = Err(err);
                    }
                }
            }
        }
        if out.is_ok() {
            if let (Some(cid), Some(wire)) = (
                evidence.claimed_client_order_id.as_ref(),
                evidence.venue_order_id.as_ref(),
            ) {
                let is_old = self
                    .public_fields
                    .registry
                    .get(&self.ledger, wire)
                    .and_then(|ws| ws.amend_lineage.as_ref())
                    .is_some_and(|l| l.old_cid == *cid);
                if is_old {
                    if let Err(err) =
                        self.resolve_unknown_child_if_old_filled_wire::<C>(wire, journal)
                    {
                        out = Err(err);
                    }
                }
            }
        }
        if out.is_ok() {
            let superseded = evidence.venue_order_id.as_ref().and_then(|wire| {
                let lineage = self
                    .public_fields
                    .registry
                    .get(&self.ledger, wire)?
                    .amend_lineage
                    .clone()?;
                (evidence.claimed_client_order_id.as_ref() == Some(&lineage.new_cid)).then(|| {
                    (
                        lineage.old_cid.0.clone(),
                        lineage.new_cid.0.clone(),
                        wire.clone(),
                    )
                })
            });
            if let Some((old, new, wire)) = superseded {
                if let (Ok(old), Ok(new)) = (C::try_from_string(old), C::try_from_string(new)) {
                    if let Err(err) = self.settle_superseded_old::<C>(&old, &new, &wire, journal) {
                        out = Err(err);
                    }
                }
            }
        }
        if out.is_ok() {
            if let Some((cid, _)) = owner.as_ref() {
                if self
                    .by_client
                    .get(cid)
                    .is_some_and(|row| matches!(row.state, OrderState::Filled))
                {
                    Stage::order_filled(cid);
                }
            }
        }
        Stage::execution_result(self, net_before, &evidence, &out);
        out
    }

    pub fn resolve_unknown_child_if_fully_filled<C: AdmissionClient<R>>(
        &mut self,
        child: &C,
        wire: &VenueOrderId,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let Some(row) = self.by_client.get(child.as_str()) else {
            return Ok(());
        };
        if !matches!(row.state, OrderState::SubmitUnknown)
            || row.ctx.qty == 0
            || row.ctx.attributed_fill_qty < row.ctx.qty
            || row.ctx.venue_order_id.as_ref().is_some_and(|v| v != wire)
        {
            return Ok(());
        }
        let qty = row.ctx.qty;
        let record = BackfillOrderRecord {
            other_members_filled: 0,
            client_order_id: OmsClientOrderId(child.as_str().to_string()),
            venue_order_id: wire.clone(),
            status: BackfillOrderStatus::Filled,
            filled_qty: qty,
            remaining_qty: 0,
            fills: Vec::new(),
        };
        self.apply_for_client(
            child.as_str(),
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![record],
            },
            journal,
            ExecutionSource::CreateResponseBackfill,
        )
        .map(|_| ())
    }

    pub fn resolve_unknown_child_if_old_filled_wire<C: AdmissionClient<R>>(
        &mut self,
        wire: &VenueOrderId,
        journal: &mut dyn TransactionIo,
    ) -> Result<bool>
    where
        Stage: ObservationTrace,
    {
        let Some(lineage) = self
            .public_fields
            .registry
            .get(&self.ledger, wire)
            .and_then(|ws| ws.amend_lineage.clone())
        else {
            return Ok(false);
        };
        if !matches!(lineage.phase, AmendPhase::Unknown | AmendPhase::RestDue) {
            return Ok(false);
        }
        let (Ok(old), Ok(new)) = (
            C::try_from_string(lineage.old_cid.0.clone()),
            C::try_from_string(lineage.new_cid.0.clone()),
        ) else {
            return Ok(false);
        };
        let c_n = lineage.declared_remaining;
        let (Some(o), Some(n)) = (
            self.by_client.get(old.as_str()),
            self.by_client.get(new.as_str()),
        ) else {
            return Ok(false);
        };
        if !matches!(n.state, OrderState::SubmitUnknown)
            || n.ctx.venue_order_id.is_some()
            || n.ctx.attributed_fill_qty != 0
            || n.ctx.qty == 0
            || n.ctx.qty != c_n
            || o.ctx.qty == 0
            || o.ctx.venue_order_id.as_ref().is_some_and(|v| v != wire)
        {
            return Ok(false);
        }
        let need = o.ctx.qty.max(c_n);
        let filled = o.ctx.attributed_fill_qty;
        if filled < need {
            return Ok(false);
        }
        let old_qty = o.ctx.qty;
        self.close_submission::<C>(&new, SubmissionCloseReason::ExhaustiveAbsent, journal)?;
        self.sweep_terminal_amend_lineages::<C>(journal)?;
        self.sync_pending_wire_from_registry(wire);
        eprintln!(
            "amend child {} resolved: old {} filled {} >= max({},{}) on wire {}",
            new.as_str(),
            old.as_str(),
            filled,
            old_qty,
            c_n,
            wire.0
        );
        Ok(true)
    }

    pub fn retire_old_on_positive_current_cancel<C: AdmissionClient<R>>(
        &mut self,
        cancelled: &C,
        wire: &VenueOrderId,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let Some(old_cid) = self
            .public_fields
            .registry
            .get(&self.ledger, wire)
            .and_then(|ws| {
                ws.amend_lineage
                    .as_ref()
                    .filter(|l| l.new_cid.0 == cancelled.as_str())
                    .map(|l| l.old_cid.0.clone())
            })
        else {
            return Ok(());
        };
        let old = C::try_from_string(old_cid)?;
        self.retire_superseded_old::<C>(&old, wire, journal)
    }

    pub fn settle_lineage_after_unknown_backfill<C: AdmissionClient<R>>(
        &mut self,
        wire: &VenueOrderId,
        child: &C,
        old: &C,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        if self
            .by_client
            .get(child.as_str())
            .is_some_and(|row| row.state.is_terminal_or_halt())
        {
            self.retire_superseded_old::<C>(old, wire, journal)?;
            self.clear_amend_lineage_as(&wire.0, journal, "backfill")?;
            self.sync_pending_wire_from_registry(wire);
        } else {
            self.settle_superseded_old::<C>(old, child, wire, journal)?;
        }
        Ok(())
    }
}
