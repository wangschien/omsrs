//! User-order staging, observation and amend recovery over canonical state.
use super::close_proof::OmsPriceE4;
use super::create_ack::{CreateAckInput, create_response_counts};
use super::live_admission::{AdmissionClient, AdmissionIntent};
use super::live_authority::AuthorityReadToken;
use super::live_state::{LineageClear, LiveOmsBookState};
use super::live_transaction::{AuthorityEffectsIo, TransactionIo};
use crate::execution::{
    ExecutionOutcome, ExecutionSource, LedgerId, OrderCursor, OrderRef, OrderTarget,
};
use crate::lifecycle::{
    BackfillOrderRecord, BackfillOrderStatus, CancelOutcome, ClientOrderId as OmsClientOrderId,
    Effect, JournalRecord, OrderCtx, OrderEvent, OrderState, SubmissionCloseReason, VenueOrderId,
};
use crate::observation::{
    AmendPhase, MemberRole, ObservationOutcome, ObservationRound, OrderStateTxnRecord, PageMeta,
    RoundError, RowEvidence, UserOrderEvidence, WireMarker,
};
use anyhow::{Result, bail};
use std::collections::BTreeMap;

/// Existing private telemetry leaves; they never mutate order/authority state.
pub trait ObservationTrace: Sized {
    fn quarantine_trace(
        stage: Option<&Self>,
        client_order_id: &Option<OmsClientOrderId>,
        wire: &Option<VenueOrderId>,
        reason: &str,
    );
    fn ws_terminal(cid: &str);
}
#[derive(Debug, Clone)]
pub struct AuthorityRequest {
    pub ledger: LedgerId,
    pub wire: VenueOrderId,
    pub cids: Vec<OmsClientOrderId>,
}
pub const LIVE_RECOVERY_INTERVAL_NS: u64 = 30_000_000_000;

impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn quarantine_user_order(
        &mut self,
        client_order_id: Option<OmsClientOrderId>,
        wire: Option<VenueOrderId>,
        reason: &str,
        journal: &mut dyn TransactionIo,
    ) -> Result<()>
    where
        Stage: ObservationTrace,
    {
        Stage::quarantine_trace(self.stage.as_ref(), &client_order_id, &wire, reason);
        let hold_current = wire
            .as_ref()
            .and_then(|wire| self.public_fields.registry.get(&self.ledger, wire))
            .is_some_and(|state| {
                client_order_id.as_ref().is_some_and(|cid| {
                    state.members.iter().any(|member| {
                        member.role == crate::observation::MemberRole::Current && &member.cid == cid
                    }) || state
                        .amend_lineage
                        .as_ref()
                        .is_some_and(|lineage| &lineage.new_cid == cid)
                })
            });
        let mut effects = vec![Effect::AppendFsync(JournalRecord::UserOrderQuarantined {
            ledger: self.ledger.clone(),
            wire: wire.clone(),
            client_order_id,
            reason: reason.chars().take(768).collect(),
        })];
        let hold =
            wire.as_ref()
                .filter(|_| hold_current)
                .map(|wire| JournalRecord::OrderAuthorityHold {
                    ledger: self.ledger.clone(),
                    wire: wire.clone(),
                });
        if let Some(record) = &hold {
            effects.push(Effect::AppendFsync(record.clone()));
        }
        journal.apply_effects(&effects)?;
        journal.commit_now()?;
        if let Some(wire) = wire {
            if let Some(record) = hold {
                self.public_fields
                    .registry
                    .fold(
                        journal.len().saturating_sub(1) as u64,
                        &record,
                        &self.public_fields.inventory,
                    )
                    .map_err(|e| anyhow::anyhow!("committed authority hold fold failed: {e:?}"))?;
            }
            self.pending_wires.insert(wire);
        }
        Ok(())
    }

    pub fn record_user_order(
        &mut self,
        wire: &str,
        evidence: UserOrderEvidence,
        journal: &mut dyn TransactionIo,
    ) -> Result<bool> {
        let wire = VenueOrderId(wire.to_string());
        let record = self
            .public_fields
            .registry
            .prepare_user_order_received(&self.ledger, &wire, evidence)
            .map_err(|e| anyhow::anyhow!("user_order admission: {e:?}"))?;
        if let Some(record) = record {
            self.commit_registry_record(record, journal)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn apply_user_order_terminal_if_complete<C: AdmissionClient<R>>(
        &mut self,
        wire: &VenueOrderId,
        journal: &mut dyn TransactionIo,
    ) -> Result<bool>
    where
        Stage: ObservationTrace,
    {
        let Some(evidence) = self
            .public_fields
            .registry
            .get(&self.ledger, wire)
            .and_then(|state| state.latest_user_order.clone())
        else {
            return Ok(false);
        };
        let executed = match evidence.status {
            crate::observation::UserOrderStatus::Canceled => false,
            crate::observation::UserOrderStatus::Executed => true,
            _ => return Ok(false),
        };
        let cid = C::try_from_string(evidence.client_order_id.0)?;
        let Some(row) = self.by_client.get(cid.as_str()) else {
            return Ok(false);
        };
        if !matches!(
            row.state,
            OrderState::Accepted { .. }
                | OrderState::Partial { .. }
                | OrderState::CancelPending { .. }
                | OrderState::ReconcilePending { .. }
        ) || row.ctx.venue_order_id.as_ref() != Some(wire)
            || row.ctx.attributed_fill_qty != evidence.filled
        {
            return Ok(false);
        }
        let cancel_pending = matches!(row.state, OrderState::CancelPending { .. });
        self.apply_for_client(
            cid.as_str(),
            &OrderEvent::UserOrderTerminalClaim {
                venue_order_id: wire.clone(),
                filled: evidence.filled,
                executed,
            },
            journal,
            ExecutionSource::RestFill,
        )?;
        if cancel_pending
            && self.by_client.get(cid.as_str()).is_some_and(|row| {
                matches!(
                    row.state,
                    OrderState::Filled | OrderState::Canceled | OrderState::Terminal
                )
            })
        {
            Stage::ws_terminal(cid.as_str());
        }
        if self.by_client.get(cid.as_str()).is_some_and(|row| {
            matches!(
                row.state,
                OrderState::Filled | OrderState::Canceled | OrderState::Terminal
            )
        }) && self
            .public_fields
            .registry
            .get(&self.ledger, wire)
            .and_then(|state| state.amend_lineage.as_ref())
            .is_some_and(|lineage| lineage.new_cid == OmsClientOrderId(cid.as_str().to_string()))
        {
            let old_cid = self
                .public_fields
                .registry
                .get(&self.ledger, wire)
                .and_then(|state| state.amend_lineage.as_ref())
                .map(|lineage| lineage.old_cid.0.clone());
            if let Some(old_cid) = old_cid {
                let old = C::try_from_string(old_cid)?;
                self.retire_superseded_old::<C>(&old, wire, journal)?;
            }
            self.clear_amend_lineage_as(&wire.0, journal, "ws")?;
        }
        self.sync_pending_wire_from_registry(wire);
        Ok(true)
    }

    pub fn stage_user_order(
        &mut self,
        evidence: UserOrderEvidence,
        journal: &mut dyn TransactionIo,
    ) -> Result<()>
    where
        Stage: ObservationTrace,
    {
        let key = evidence.client_order_id.0.clone();
        if let Some(previous) = self.staged_user_orders.get(&key) {
            if previous == &evidence {
                return Ok(());
            }
            if evidence.updated_ts_ms < previous.updated_ts_ms {
                if evidence.status != previous.status
                    || evidence.filled != previous.filled
                    || evidence.remaining != previous.remaining
                    || evidence.initial != previous.initial
                {
                    self.quarantine_user_order(
                        Some(evidence.client_order_id.clone()),
                        Some(evidence.wire.clone()),
                        "staged_older_state_conflict",
                        journal,
                    )?;
                }
                return Ok(());
            }
            if evidence.updated_ts_ms == previous.updated_ts_ms
                || evidence.filled < previous.filled
                || (previous.status != crate::observation::UserOrderStatus::Resting
                    && evidence.status == crate::observation::UserOrderStatus::Resting)
            {
                self.quarantine_user_order(
                    Some(evidence.client_order_id.clone()),
                    Some(evidence.wire.clone()),
                    "staged_order_version_conflict",
                    journal,
                )?;
                return Ok(());
            }
        }
        journal.apply_effects(&[Effect::AppendFsync(JournalRecord::UserOrderStaged {
            ledger: self.ledger.clone(),
            client_order_id: evidence.client_order_id.clone(),
            evidence: Some(evidence.clone()),
        })])?;
        journal.commit_now()?;
        self.staged_user_orders.insert(key, evidence);
        Ok(())
    }

    pub fn clear_staged_user_order(
        &mut self,
        cid: &OmsClientOrderId,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        if !self.staged_user_orders.contains_key(&cid.0) {
            return Ok(());
        }
        journal.apply_effects(&[Effect::AppendFsync(JournalRecord::UserOrderStaged {
            ledger: self.ledger.clone(),
            client_order_id: cid.clone(),
            evidence: None,
        })])?;
        journal.commit_now()?;
        self.staged_user_orders.remove(&cid.0);
        Ok(())
    }

    pub fn apply_staged_user_order_claim<C: AdmissionClient<R>>(
        &mut self,
        cid: &C,
        journal: &mut dyn TransactionIo,
    ) -> Result<bool> {
        let Some(evidence) = self.staged_user_orders.get(cid.as_str()).cloned() else {
            return Ok(false);
        };
        let Some(row) = self.by_client.get(cid.as_str()) else {
            return Ok(false);
        };
        let qty_matches = if matches!(
            row.state,
            OrderState::Accepted { .. } | OrderState::Partial { .. }
        ) {
            evidence.filled.checked_add(evidence.remaining) == Some(row.ctx.qty)
        } else {
            evidence.filled == 0 && evidence.remaining == row.ctx.qty
        };
        if !matches!(
            row.state,
            OrderState::SubmitUnknown | OrderState::Accepted { .. } | OrderState::Partial { .. }
        ) || evidence.status != crate::observation::UserOrderStatus::Resting
            || !qty_matches
            || evidence.wire
                != row
                    .ctx
                    .venue_order_id
                    .clone()
                    .unwrap_or(evidence.wire.clone())
        {
            return Ok(false);
        }
        let wire = evidence.wire.clone();
        if matches!(row.state, OrderState::SubmitUnknown) {
            self.apply_for_client(
                cid.as_str(),
                &OrderEvent::UserOrderRestingClaim {
                    venue_order_id: wire.clone(),
                    remaining: evidence.remaining,
                },
                journal,
                ExecutionSource::CreateResponseBackfill,
            )?;
        }
        self.record_user_order(&wire.0, evidence.clone(), journal)?;
        self.clear_staged_user_order(&evidence.client_order_id, journal)?;
        Ok(true)
    }

    pub fn apply_staged_create_claim_and_prove<C: AdmissionClient<R>>(
        &mut self,
        cid: &C,
        journal: &mut dyn TransactionIo,
    ) -> Result<bool> {
        let applied = self.apply_staged_user_order_claim::<C>(cid, journal)?;
        if applied {
            if let Some(wire) = self
                .by_client
                .get(cid.as_str())
                .and_then(|row| row.ctx.venue_order_id.clone())
            {
                if self
                    .public_fields
                    .registry
                    .prepare_order_state_resting(&self.ledger, &wire, &self.public_fields.inventory)
                    .is_ok()
                {
                    self.prove_current_amend_resting::<C>(&wire.0, journal)?;
                }
            }
        }
        Ok(applied)
    }

    pub fn fold_staged_pending_amend_evidence<C: AdmissionClient<R>>(
        &mut self,
        cid: &C,
        wire: &VenueOrderId,
        journal: &mut dyn TransactionIo,
    ) -> Result<bool> {
        let Some(evidence) = self.staged_user_orders.get(cid.as_str()).cloned() else {
            return Ok(false);
        };
        let Some(lineage) = self
            .public_fields
            .registry
            .get(&self.ledger, wire)
            .and_then(|state| state.amend_lineage.as_ref())
        else {
            return Ok(false);
        };
        if evidence.wire != *wire
            || evidence.client_order_id != lineage.new_cid
            || evidence.status != crate::observation::UserOrderStatus::Resting
            || evidence.filled != 0
            || evidence.remaining != lineage.declared_remaining
            || evidence.initial != lineage.declared_remaining
        {
            return Ok(false);
        }
        self.record_user_order(&wire.0, evidence.clone(), journal)?;
        self.clear_staged_user_order(&evidence.client_order_id, journal)?;
        Ok(true)
    }

    pub fn recover_unknown_amend_from_user_order<C: AdmissionClient<R>>(
        &mut self,
        wire: &VenueOrderId,
        now_ns: u64,
        journal: &mut dyn TransactionIo,
    ) -> Result<bool> {
        let Some(ws) = self.public_fields.registry.get(&self.ledger, wire) else {
            return Ok(false);
        };
        let Some(lineage) = ws.amend_lineage.clone() else {
            return Ok(false);
        };
        let Some(evidence) = ws.latest_user_order.clone() else {
            return Ok(false);
        };
        if ws.authority_hold
            || !matches!(lineage.phase, AmendPhase::Unknown | AmendPhase::RestDue)
            || evidence.client_order_id != lineage.new_cid
            || evidence.wire != *wire
            || evidence.status != crate::observation::UserOrderStatus::Resting
            || evidence.filled != 0
            || evidence.remaining != lineage.declared_remaining
            || evidence.initial != lineage.declared_remaining
        {
            return Ok(false);
        }
        let cid = C::try_from_string(lineage.new_cid.0.clone())?;
        if lineage.phase == AmendPhase::RestDue
            && lineage.pre_generation.checked_add(1) == Some(ws.generation)
            && self
                .by_client
                .get(cid.as_str())
                .is_some_and(|row| matches!(row.state, OrderState::Accepted { .. }))
        {
            self.advance_amend_lineage(
                &wire.0,
                AmendPhase::AppliedNeedsObservation,
                None,
                journal,
            )?;
            self.prove_current_amend_resting::<C>(&wire.0, journal)?;
            return Ok(true);
        }
        if ws.generation != lineage.pre_generation
            || !self.by_client.get(cid.as_str()).is_some_and(|row| {
                matches!(
                    row.state,
                    OrderState::SubmitUnknown | OrderState::Accepted { .. }
                )
            })
        {
            return Ok(false);
        }
        if self
            .by_client
            .get(cid.as_str())
            .is_some_and(|row| matches!(row.state, OrderState::SubmitUnknown))
        {
            self.apply_for_client(
                cid.as_str(),
                &OrderEvent::UserOrderRestingClaim {
                    venue_order_id: wire.clone(),
                    remaining: evidence.remaining,
                },
                journal,
                ExecutionSource::CreateResponseBackfill,
            )?;
        }
        self.apply_for_client(
            cid.as_str(),
            &OrderEvent::AmendObserved,
            journal,
            ExecutionSource::CreateResponseBackfill,
        )?;
        self.advance_amend_lineage(
            &wire.0,
            AmendPhase::AppliedNeedsObservation,
            Some(now_ns),
            journal,
        )?;
        self.prove_current_amend_resting::<C>(&wire.0, journal)?;
        let old = C::try_from_string(lineage.old_cid.0)?;
        if self.by_client.get(old.as_str()).is_some_and(|row| {
            !row.state.is_terminal_or_halt() && !row.state.is_submission_closed()
        }) {
            self.on_cancel_requested::<C>(&old, journal)?;
            self.on_cancel_http_success::<C>(&old, journal)?;
        }
        self.settle_superseded_old::<C>(&old, &cid, wire, journal)?;
        Ok(true)
    }

    pub fn backfill_member_filled_qty<C: AdmissionClient<R>>(
        &self,
        wire: &VenueOrderId,
        client: &C,
        wire_cumulative: u64,
    ) -> Option<u64> {
        let (other_attributed, other_debt) =
            self.other_members_attribution(wire, client.as_str())?;
        if other_debt {
            return None;
        }
        wire_cumulative.checked_sub(other_attributed)
    }

    pub fn recover_unknown_amend_from_terminal_user_order<C: AdmissionClient<R>>(
        &mut self,
        wire: &VenueOrderId,
        journal: &mut dyn TransactionIo,
    ) -> Result<bool> {
        let Some(ws) = self.public_fields.registry.get(&self.ledger, wire) else {
            return Ok(false);
        };
        let Some(lineage) = ws.amend_lineage.clone() else {
            return Ok(false);
        };
        if ws.authority_hold || !matches!(lineage.phase, AmendPhase::Unknown | AmendPhase::RestDue)
        {
            return Ok(false);
        }
        let Some(evidence) = self
            .staged_user_orders
            .get(lineage.new_cid.0.as_str())
            .cloned()
        else {
            return Ok(false);
        };
        let status = match evidence.status {
            crate::observation::UserOrderStatus::Canceled => BackfillOrderStatus::Canceled,
            crate::observation::UserOrderStatus::Executed => BackfillOrderStatus::Filled,
            _ => return Ok(false),
        };
        if evidence.client_order_id != lineage.new_cid || evidence.wire != *wire {
            return Ok(false);
        }
        let cid = C::try_from_string(lineage.new_cid.0.clone())?;
        if !self
            .by_client
            .get(cid.as_str())
            .is_some_and(|row| matches!(row.state, OrderState::SubmitUnknown))
        {
            return Ok(false);
        }
        let Some(filled_qty) = self.backfill_member_filled_qty::<C>(wire, &cid, evidence.filled)
        else {
            return Ok(false);
        };
        if self
            .by_client
            .get(cid.as_str())
            .is_some_and(|row| filled_qty < row.ctx.attributed_fill_qty)
        {
            return Ok(false);
        }
        let record = BackfillOrderRecord {
            other_members_filled: evidence.filled - filled_qty,
            client_order_id: OmsClientOrderId(cid.as_str().to_string()),
            venue_order_id: wire.clone(),
            status,
            filled_qty: evidence.filled,
            remaining_qty: evidence.remaining,
            fills: Vec::new(),
        };
        self.apply_for_client(
            cid.as_str(),
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![record],
            },
            journal,
            ExecutionSource::CreateResponseBackfill,
        )?;
        self.clear_staged_user_order(&lineage.new_cid, journal)?;
        let old = C::try_from_string(lineage.old_cid.0)?;
        self.retire_superseded_old::<C>(&old, wire, journal)?;
        if self
            .by_client
            .get(cid.as_str())
            .is_some_and(|row| row.state.is_terminal_or_halt())
        {
            self.clear_amend_lineage_as(&wire.0, journal, "ws")?;
            self.sync_pending_wire_from_registry(wire);
        }
        Ok(true)
    }

    pub fn recover_unknown_amend_from_rest<C: AdmissionClient<R>>(
        &mut self,
        wire: &VenueOrderId,
        actual_cid: &OmsClientOrderId,
        filled: u64,
        remaining: u64,
        initial: u64,
        journal: &mut dyn TransactionIo,
    ) -> Result<bool> {
        let Some(lineage) = self
            .public_fields
            .registry
            .get(&self.ledger, wire)
            .and_then(|state| state.amend_lineage.clone())
        else {
            return Ok(false);
        };
        if !matches!(lineage.phase, AmendPhase::Unknown | AmendPhase::RestDue)
            || &lineage.new_cid != actual_cid
            || filled != 0
            || remaining != lineage.declared_remaining
            || !self.amend_initial_ok::<C>(&lineage, initial)
        {
            return Ok(false);
        }
        let cid = C::try_from_string(actual_cid.0.clone())?;
        if !self
            .by_client
            .get(cid.as_str())
            .is_some_and(|row| matches!(row.state, OrderState::SubmitUnknown))
        {
            return Ok(false);
        }
        self.apply_for_client(
            cid.as_str(),
            &OrderEvent::UserOrderRestingClaim {
                venue_order_id: wire.clone(),
                remaining,
            },
            journal,
            ExecutionSource::CreateResponseBackfill,
        )?;
        self.apply_for_client(
            cid.as_str(),
            &OrderEvent::AmendObserved,
            journal,
            ExecutionSource::CreateResponseBackfill,
        )?;
        self.advance_amend_lineage(&wire.0, AmendPhase::AppliedNeedsObservation, None, journal)?;
        let old = C::try_from_string(lineage.old_cid.0)?;
        if self.by_client.get(old.as_str()).is_some_and(|row| {
            !row.state.is_terminal_or_halt() && !row.state.is_submission_closed()
        }) {
            self.on_cancel_requested::<C>(&old, journal)?;
            self.on_cancel_http_success::<C>(&old, journal)?;
        }
        self.settle_superseded_old::<C>(&old, &cid, wire, journal)?;
        Ok(true)
    }

    pub fn reprove_resting_after_fill<C: AdmissionClient<R>>(
        &mut self,
        wire: &VenueOrderId,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let Some(ws) = self.public_fields.registry.get(&self.ledger, wire) else {
            return Ok(());
        };
        if !ws
            .latest_user_order
            .as_ref()
            .is_some_and(|row| row.status == crate::observation::UserOrderStatus::Resting)
        {
            return Ok(());
        }
        if self
            .public_fields
            .registry
            .prepare_order_state_resting(&self.ledger, wire, &self.public_fields.inventory)
            .is_ok()
        {
            self.prove_current_amend_resting::<C>(&wire.0, journal)?;
        }
        Ok(())
    }

    pub fn record_omsrs_lineage_clear(
        &mut self,
        wire: &VenueOrderId,
        before_new_cid: Option<String>,
        source: &'static str,
        journal: &dyn TransactionIo,
    ) {
        let Some(new_cid) = before_new_cid else {
            return;
        };
        if self
            .public_fields
            .registry
            .get(&self.ledger, wire)
            .is_some_and(|ws| ws.amend_lineage.is_some())
        {
            return;
        }
        self.lineage_clears.push(LineageClear {
            new_cid,
            instant_ns: journal.wall_now_ns(),
            source,
        });
    }

    pub fn prove_current_amend_resting<C: AdmissionClient<R>>(
        &mut self,
        wire: &str,
        journal: &mut dyn TransactionIo,
    ) -> Result<OrderStateTxnRecord> {
        let wire = VenueOrderId(wire.to_string());
        let superseded_old = self
            .public_fields
            .registry
            .get(&self.ledger, &wire)
            .and_then(|ws| {
                ws.amend_lineage
                    .as_ref()
                    .map(|l| (l.old_cid.clone(), l.new_cid.clone()))
            });
        let txn = self
            .public_fields
            .registry
            .prepare_order_state_resting(&self.ledger, &wire, &self.public_fields.inventory)
            .map_err(|e| anyhow::anyhow!("user_order remaining proof: {e:?}"))?;
        journal.apply_effects(&[Effect::AppendFsync(JournalRecord::OrderStateTxn(Box::new(
            txn.clone(),
        )))])?;
        journal.commit_now()?;
        self.public_fields
            .registry
            .apply_order_state_resting(&txn, &self.public_fields.inventory)
            .map_err(|e| anyhow::anyhow!("user_order proof apply: {e:?}"))?;
        self.record_omsrs_lineage_clear(
            &wire,
            superseded_old.as_ref().map(|(_, n)| n.0.clone()),
            "ws",
            journal,
        );
        if let Some((old, new)) = superseded_old {
            if let (Ok(old), Ok(new)) = (C::try_from_string(old.0), C::try_from_string(new.0)) {
                self.settle_superseded_old::<C>(&old, &new, &wire, journal)?;
            }
        }
        Ok(txn)
    }

    pub fn settle_superseded_old<C: AdmissionClient<R>>(
        &mut self,
        old: &C,
        new: &C,
        wire: &VenueOrderId,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        if !self.amend_child_proven::<C>(new, wire) {
            return Ok(());
        }
        self.retire_superseded_old::<C>(old, wire, journal)
    }

    pub fn amend_child_proven<C: AdmissionClient<R>>(&self, new: &C, wire: &VenueOrderId) -> bool {
        self.public_fields
            .registry
            .get(&self.ledger, wire)
            .is_some_and(|ws| {
                ws.proved_current_cid
                    .as_ref()
                    .is_some_and(|c| c.0 == new.as_str())
            })
            || self.by_client.get(new.as_str()).is_some_and(|r| {
                r.ctx.attributed_fill_qty > 0 || matches!(r.state, OrderState::Filled)
            })
    }

    pub fn retire_superseded_old<C: AdmissionClient<R>>(
        &mut self,
        old: &C,
        wire: &VenueOrderId,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let Some(row) = self.by_client.get(old.as_str()) else {
            return Ok(());
        };
        if row.state.is_terminal_or_halt() || row.state.is_submission_closed() {
            return Ok(());
        }
        let old_filled = row.ctx.attributed_fill_qty;
        if matches!(
            row.state,
            OrderState::Accepted { .. } | OrderState::Partial { .. }
        ) {
            self.on_cancel_requested::<C>(old, journal)?;
            self.on_cancel_http_success::<C>(old, journal)?;
        }
        if !self.by_client.get(old.as_str()).is_some_and(|o| {
            matches!(
                o.state,
                OrderState::CancelPending { .. } | OrderState::ReconcilePending { .. }
            )
        }) {
            return Ok(());
        }
        self.apply_for_client(
            old.as_str(),
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(wire.clone()),
                filled_qty: old_filled,
                remaining_qty: 0,
                fills: Vec::new(),
                authority_complete: true,
            },
            journal,
            ExecutionSource::RestFill,
        )
        .map(|_| ())
    }

    pub fn prove_current_amend_from_rest(
        &mut self,
        wire: &VenueOrderId,
        cid: &OmsClientOrderId,
        filled: u64,
        remaining: u64,
        initial: u64,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let lineage_before = self
            .public_fields
            .registry
            .get(&self.ledger, wire)
            .and_then(|ws| ws.amend_lineage.as_ref().map(|l| l.new_cid.0.clone()));
        let txn = self
            .public_fields
            .registry
            .prepare_rest_order_proof(
                &self.ledger,
                wire,
                cid,
                filled,
                remaining,
                initial,
                &self.public_fields.inventory,
            )
            .map_err(|e| anyhow::anyhow!("REST amended-row proof: {e:?}"))?;
        journal.apply_effects(&[Effect::AppendFsync(JournalRecord::RestOrderProofTxn(
            Box::new(txn.clone()),
        ))])?;
        journal.commit_now()?;
        self.public_fields
            .registry
            .apply_rest_order_proof(&txn, &self.public_fields.inventory)
            .map_err(|e| anyhow::anyhow!("REST amended-row proof apply: {e:?}"))?;
        self.record_omsrs_lineage_clear(wire, lineage_before, "rest", journal);
        self.sync_pending_wire_from_registry(wire);
        Ok(())
    }

    pub fn drain_authority_requests<C: AdmissionClient<R>, J: AuthorityEffectsIo>(
        &mut self,
        journal: &mut J,
    ) -> Vec<AuthorityRequest> {
        let alpha = journal.drain_authority_effects();
        let mut by_wire: BTreeMap<String, AuthorityRequest> = BTreeMap::new();
        let mut unbound = Vec::new();
        for e in alpha {
            let parsed = match &e {
                Effect::RequestAuthorityReconcile {
                    venue_order_id: Some(w),
                    client_order_id,
                } => Some((w.clone(), Some(client_order_id.clone()))),
                Effect::ReconcileCancel {
                    venue_order_id: Some(w),
                } => Some((w.clone(), None)),
                Effect::BackfillFills { venue_order_id: w } => Some((w.clone(), None)),
                _ => None,
            };
            if let Some((w, cid)) = parsed {
                let req = by_wire
                    .entry(w.0.clone())
                    .or_insert_with(|| AuthorityRequest {
                        ledger: self.ledger.clone(),
                        wire: w.clone(),
                        cids: Vec::new(),
                    });
                if let Some(c) = cid {
                    if !req.cids.iter().any(|x| x == &c) {
                        req.cids.push(c);
                    }
                }
                self.pending_wires.insert(w);
            } else {
                unbound.push(e);
            }
        }
        if !unbound.is_empty() {
            journal.requeue_authority_effects(unbound);
        }
        for w in &self.pending_wires {
            by_wire
                .entry(w.0.clone())
                .or_insert_with(|| AuthorityRequest {
                    ledger: self.ledger.clone(),
                    wire: w.clone(),
                    cids: Vec::new(),
                });
        }
        let mut out = Vec::new();
        for req in by_wire.into_values() {
            if self.replayed_http_pending.contains(&req.wire) {
                let replay_child = self
                    .public_fields
                    .registry
                    .get(&self.ledger, &req.wire)
                    .and_then(|state| state.amend_lineage.as_ref())
                    .map(|lineage| C::try_from_string(lineage.new_cid.0.clone()));
                let Some(Ok(replay_child)) = replay_child else {
                    self.public_fields.stopped = true;
                    continue;
                };
                if self.by_client.get(replay_child.as_str()).is_none() {
                    if self
                        .clear_amend_lineage_as(&req.wire.0, journal, "authority")
                        .is_err()
                    {
                        self.public_fields.stopped = true;
                        continue;
                    }
                    self.replayed_http_pending.remove(&req.wire);
                    continue;
                }
                if self
                    .by_client
                    .get(replay_child.as_str())
                    .is_some_and(|row| matches!(row.state, OrderState::SubmitStarted { .. }))
                    && self.on_create_fail::<C>(&replay_child, journal).is_err()
                {
                    self.public_fields.stopped = true;
                    continue;
                }
                if self
                    .advance_amend_lineage(
                        &req.wire.0,
                        AmendPhase::RestDue,
                        Some(journal.wall_now_ns()),
                        journal,
                    )
                    .is_err()
                {
                    self.public_fields.stopped = true;
                    continue;
                }
                self.replayed_http_pending.remove(&req.wire);
            }
            if self
                .public_fields
                .registry
                .get(&self.ledger, &req.wire)
                .and_then(|state| state.amend_lineage.as_ref())
                .is_some_and(|lineage| lineage.phase == AmendPhase::AppliedNeedsObservation)
            {
                if self.amend_applied_mono.get(&req.wire.0).is_some_and(|at| {
                    at.elapsed().as_nanos() < u128::from(LIVE_RECOVERY_INTERVAL_NS)
                }) {
                    continue;
                }
                if self
                    .advance_amend_lineage(&req.wire.0, AmendPhase::RestDue, None, journal)
                    .is_err()
                {
                    self.public_fields.stopped = true;
                    continue;
                }
            }
            if self.authority_wire_completed(&req.wire) {
                self.pending_wires.remove(&req.wire);
                continue;
            }
            out.push(req);
        }
        out
    }

    pub fn wire_marker(&self, wire: &VenueOrderId) -> WireMarker {
        self.public_fields.registry.marker(&self.ledger, wire)
    }

    pub fn observe_current_resting(
        &mut self,
        wire: &str,
        remaining: u64,
        filled: u64,
        journal: &mut dyn TransactionIo,
    ) -> Result<ObservationOutcome> {
        let vid = VenueOrderId(wire.into());
        let marker = self.wire_marker(&vid);
        self.observe_page(
            &vid,
            BackfillOrderStatus::Open,
            filled,
            remaining,
            marker,
            &[],
            true,
            0,
            journal,
        )
    }

    pub fn observe_canceled_empty(
        &mut self,
        wire: &str,
        journal: &mut dyn TransactionIo,
    ) -> Result<ObservationOutcome> {
        let vid = VenueOrderId(wire.into());
        if self.wire_known_deficit(&vid) {
            return Ok(ObservationOutcome::NotCovered);
        }
        let marker = self.wire_marker(&vid);
        self.observe_page(
            &vid,
            BackfillOrderStatus::Canceled,
            0,
            0,
            marker,
            &[],
            true,
            0,
            journal,
        )
    }

    pub fn observe_row_page_none(
        &mut self,
        wire: &VenueOrderId,
        status: BackfillOrderStatus,
        filled: u64,
        remaining: u64,
        journal: &mut dyn TransactionIo,
    ) -> Result<ObservationOutcome> {
        let marker = self.wire_marker(wire);
        let round = ObservationRound::begin(self.ledger.clone(), wire.clone());
        self.finish_observation_round(
            round, wire, status, filled, remaining, marker, None, journal,
        )
    }

    pub fn observe_page(
        &mut self,
        wire: &VenueOrderId,
        status: BackfillOrderStatus,
        filled: u64,
        remaining: u64,
        marker: WireMarker,
        page_outcomes: &[ExecutionOutcome],
        exhaustive: bool,
        parse_failures: u32,
        journal: &mut dyn TransactionIo,
    ) -> Result<ObservationOutcome> {
        let mut round = ObservationRound::begin(self.ledger.clone(), wire.clone());
        for o in page_outcomes {
            match round.record(o) {
                Ok(()) => {}
                Err(RoundError::ForeignRow) => {}
                Err(e) => {
                    self.public_fields.stopped = true;
                    bail!("observation record STOP: {e:?}");
                }
            }
        }
        round.page(PageMeta {
            exhaustive,
            parse_failures,
        });
        self.finish_observation_round(
            round, wire, status, filled, remaining, marker, None, journal,
        )
    }

    pub fn observe_rest_order_page_with_identity(
        &mut self,
        wire: &VenueOrderId,
        actual_cid: &OmsClientOrderId,
        status: BackfillOrderStatus,
        filled: u64,
        remaining: u64,
        initial: u64,
        token: &AuthorityReadToken,
        page_outcomes: &[ExecutionOutcome],
        exhaustive: bool,
        parse_failures: u32,
        journal: &mut dyn TransactionIo,
    ) -> Result<ObservationOutcome> {
        if !self.authority_token_still_current(token)
            || token.wire != *wire
            || token.current_cid.as_ref() != Some(actual_cid)
        {
            bail!("amended REST authority token or returned CID changed");
        }
        let mut round = ObservationRound::begin(self.ledger.clone(), wire.clone());
        for outcome in page_outcomes {
            if let Err(error) = round.record(outcome) {
                if !matches!(error, RoundError::ForeignRow) {
                    bail!("amended REST fills page: {error:?}");
                }
            }
        }
        round.page(PageMeta {
            exhaustive,
            parse_failures,
        });
        let observed = self.finish_observation_round(
            round,
            wire,
            status,
            filled,
            remaining,
            WireMarker(token.generation),
            Some(actual_cid.clone()),
            journal,
        )?;
        let active_amend_for_cid = self
            .public_fields
            .registry
            .get(&self.ledger, wire)
            .and_then(|state| state.amend_lineage.as_ref())
            .is_some_and(|lineage| lineage.new_cid == *actual_cid);
        if exhaustive
            && parse_failures == 0
            && observed
                == (ObservationOutcome::Consistent {
                    remaining_fresh: false,
                })
            && status == BackfillOrderStatus::Open
            && active_amend_for_cid
        {
            self.prove_current_amend_from_rest(
                wire, actual_cid, filled, remaining, initial, journal,
            )?;
        }
        Ok(observed)
    }

    pub fn finish_observation_round(
        &mut self,
        round: ObservationRound,
        wire: &VenueOrderId,
        status: BackfillOrderStatus,
        filled: u64,
        remaining: u64,
        marker: WireMarker,
        actual_cid: Option<OmsClientOrderId>,
        journal: &mut dyn TransactionIo,
    ) -> Result<ObservationOutcome> {
        if self.public_fields.stopped {
            bail!("oms STOP: no further A/C activity until rebuild");
        }
        self.ensure_ledger("");
        let member_cids: Vec<OmsClientOrderId> = self
            .public_fields
            .registry
            .get(&self.ledger, wire)
            .map(|ws| {
                ws.members
                    .iter()
                    .filter(|m| m.role != MemberRole::Terminal)
                    .map(|m| m.cid.clone())
                    .collect()
            })
            .unwrap_or_default();
        let mut cloned: Vec<(String, OrderState, OrderCtx, OrderCursor)> = Vec::new();
        for cid in &member_cids {
            let o = self.by_client.get(&cid.0).ok_or_else(|| {
                self.public_fields.stopped = true;
                anyhow::anyhow!("observation MissingMember {}", cid.0)
            })?;
            cloned.push((cid.0.clone(), o.state.clone(), o.ctx.clone(), o.cursor));
        }
        let ledger = self.ledger.clone();
        let refs: Vec<OrderRef<'_>> = cloned
            .iter()
            .map(|(_, s, c, cur)| OrderRef {
                ledger: &ledger,
                cursor: *cur,
                state: s,
                ctx: c,
            })
            .collect();
        let row = RowEvidence {
            ledger: ledger.clone(),
            wire: wire.clone(),
            status,
            filled,
            remaining,
            client_order_id: actual_cid.or_else(|| {
                cloned
                    .first()
                    .map(|(k, _, _, _)| OmsClientOrderId(k.clone()))
            }),
            marker,
        };
        let txn = round
            .finish(
                &self.public_fields.registry,
                &self.public_fields.inventory,
                &refs,
                row,
            )
            .map_err(|e| {
                self.public_fields.stopped = true;
                anyhow::anyhow!("observation finish STOP: {e:?}")
            })?;
        drop(refs);
        let rec = txn.journal.clone();
        if let Some(r) = rec.as_ref() {
            journal
                .apply_effects(&[Effect::AppendFsync(r.clone())])
                .map_err(|e| {
                    self.public_fields.stopped = true;
                    e
                })?;
        }
        journal.apply_effects(&txn.effects).map_err(|e| {
            self.public_fields.stopped = true;
            e
        })?;
        let outcome = {
            let mut targets: Vec<OrderTarget<'_>> = cloned
                .iter_mut()
                .map(|(_, s, c, cur)| OrderTarget {
                    ledger: &ledger,
                    state: s,
                    ctx: c,
                    cursor: cur,
                })
                .collect();
            txn.apply(
                &mut targets,
                &mut self.public_fields.registry,
                &self.public_fields.inventory,
            )
            .map_err(|e| {
                self.public_fields.stopped = true;
                anyhow::anyhow!("ObservationApplyMismatch: {e:?}")
            })?
        };
        for (k, s, c, cur) in cloned {
            if let Some(o) = self.by_client.get_mut(&k) {
                o.state = s;
                o.ctx = c;
                o.cursor = cur;
            }
        }
        if let Some(r) = rec.as_ref() {
            self.fold_record(r)?;
        }
        self.complete_terminal_amend_lineage(wire, status, journal)?;
        self.sync_pending_wire_from_registry(wire);
        Ok(outcome)
    }

    pub fn sweep_terminal_amend_lineages<C: AdmissionClient<R>>(
        &mut self,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let wires: Vec<VenueOrderId> = self
            .by_client
            .values()
            .filter_map(|row| row.ctx.venue_order_id.clone())
            .collect();
        let mut seen = std::collections::HashSet::new();
        for wire in wires {
            if !seen.insert(wire.clone()) {
                continue;
            }
            let done = self
                .public_fields
                .registry
                .get(&self.ledger, &wire)
                .is_some_and(|ws| {
                    ws.amend_lineage
                        .as_ref()
                        .is_some_and(|l| l.phase != AmendPhase::HttpPending)
                        && !ws.authority_hold
                        && ws.members.iter().all(|m| m.role == MemberRole::Terminal)
                });
            let child_unknown = self
                .public_fields
                .registry
                .get(&self.ledger, &wire)
                .and_then(|ws| ws.amend_lineage.as_ref())
                .is_some_and(|l| {
                    l.phase == AmendPhase::Unknown
                        && self
                            .by_client
                            .get(&l.new_cid.0)
                            .is_some_and(|r| matches!(r.state, OrderState::SubmitUnknown))
                        && C::try_from_string(l.new_cid.0.clone())
                            .is_ok_and(|n| self.amend_child_proven::<C>(&n, &wire))
                });
            if done && !child_unknown {
                self.clear_amend_lineage_as(&wire.0, journal, "sweep")?;
            }
        }
        Ok(())
    }

    pub fn complete_terminal_amend_lineage(
        &mut self,
        wire: &VenueOrderId,
        status: BackfillOrderStatus,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        if matches!(
            status,
            BackfillOrderStatus::Open | BackfillOrderStatus::Partial
        ) {
            return Ok(());
        }
        let done = self
            .public_fields
            .registry
            .get(&self.ledger, wire)
            .is_some_and(|ws| {
                ws.amend_lineage.is_some()
                    && !ws.authority_hold
                    && ws.members.iter().all(|m| m.role == MemberRole::Terminal)
            });
        if done {
            self.clear_amend_lineage_as(&wire.0, journal, "rest_terminal")?;
        }
        Ok(())
    }

    pub fn on_create_ack<C: AdmissionClient<R>, I: AdmissionIntent<C>, A: CreateAckInput>(
        &mut self,
        intent: &I,
        ack: &A,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let key = intent.client_order_id().as_str();
        let venue_order_id = ack.venue_order_id();
        let (fill_count, remaining_count, avg_price_e4, fee_cents) =
            create_response_counts::<C, I, A>(intent, ack)?;
        self.apply_for_client(
            key,
            &OrderEvent::SubmitResponse {
                venue_order_id,
                fill_count,
                remaining_count,
                avg_price_cents: avg_price_e4.map(OmsPriceE4::into_wire_price_cents),
                fee_cents,
                snapshot_boundary: None,
            },
            journal,
            ExecutionSource::CreateResponseBackfill,
        )?;
        Ok(())
    }

    pub fn on_amend_applied<C: AdmissionClient<R>>(
        &mut self,
        client: &C,
        wire: &str,
        declared_qty_centi: u64,
        venue_remaining_centi: u64,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let key = client.as_str();
        self.apply_for_client(
            key,
            &OrderEvent::SubmitResponse {
                venue_order_id: VenueOrderId(wire.to_string()),
                fill_count: 0,
                remaining_count: declared_qty_centi,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
            journal,
            ExecutionSource::CreateResponseBackfill,
        )?;
        self.apply_for_client(
            key,
            &OrderEvent::AmendObserved,
            journal,
            ExecutionSource::CreateResponseBackfill,
        )?;
        let _ = venue_remaining_centi;
        Ok(())
    }

    pub fn on_create_fail<C: AdmissionClient<R>>(
        &mut self,
        client: &C,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let key = client.as_str();
        self.apply_for_client(
            key,
            &OrderEvent::SubmitTimeout,
            journal,
            ExecutionSource::CreateResponseBackfill,
        )
        .map(|_| ())
    }

    pub fn close_submission<C: AdmissionClient<R>>(
        &mut self,
        client: &C,
        reason: SubmissionCloseReason,
        journal: &mut dyn TransactionIo,
    ) -> Result<bool> {
        if self.public_fields.stopped {
            bail!("oms STOP: no further A/C activity until rebuild");
        }
        let key = client.as_str();
        let already = self
            .by_client
            .get(key)
            .ok_or_else(|| anyhow::anyhow!("close_submission: unknown client {key}"))?
            .state
            .is_submission_closed();
        self.apply_for_client(
            key,
            &OrderEvent::CloseSubmission { reason },
            journal,
            ExecutionSource::CreateResponseBackfill,
        )?;
        Ok(!already)
    }

    pub fn on_cancel_requested<C: AdmissionClient<R>>(
        &mut self,
        client: &C,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let key = client.as_str();
        self.apply_for_client(
            key,
            &OrderEvent::CancelRequested,
            journal,
            ExecutionSource::RestFill,
        )
        .map(|_| ())
    }

    pub fn on_cancel_outcome<C: AdmissionClient<R>>(
        &mut self,
        client: &C,
        outcome: CancelOutcome,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let key = client.as_str();
        self.apply_for_client(
            key,
            &OrderEvent::CancelOutcome(outcome),
            journal,
            ExecutionSource::RestFill,
        )
        .map(|_| ())
    }

    pub fn on_cancel_http_success<C: AdmissionClient<R>>(
        &mut self,
        client: &C,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let key = client.as_str();
        self.apply_for_client(
            key,
            &OrderEvent::CancelOutcome(CancelOutcome::Canceled),
            journal,
            ExecutionSource::RestFill,
        )
        .map(|_| ())
    }

    pub fn on_cancel_404_executed<C: AdmissionClient<R>>(
        &mut self,
        client: &C,
        filled_qty_centi: u64,
        remaining_qty_centi: u64,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let key = client.as_str();
        let vid = self
            .by_client
            .get(key)
            .ok_or_else(|| anyhow::anyhow!("on_cancel_404_executed: unknown client {key}"))?
            .ctx
            .venue_order_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("on_cancel_404_executed: no wire {key}"))?;
        let marker = self.wire_marker(&vid);
        self.observe_page(
            &vid,
            BackfillOrderStatus::Filled,
            filled_qty_centi,
            remaining_qty_centi,
            marker,
            &[],
            true,
            0,
            journal,
        )
        .map(|_| ())
    }

    // Retain the original explicit borrow-end marker in this relocated body.
    #[allow(dropping_references)]
    pub fn on_amend_probe_executed<C: AdmissionClient<R>>(
        &mut self,
        client: &C,
        filled_qty_centi: u64,
        remaining_qty_centi: u64,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let key = client.as_str();
        let order = self
            .by_client
            .get_mut(key)
            .ok_or_else(|| anyhow::anyhow!("on_amend_probe_executed: unknown client {key}"))?;
        let qty = order.ctx.qty;
        if filled_qty_centi == qty && remaining_qty_centi == 0 {
            drop(order);
            self.on_cancel_requested::<C>(client, journal)?;
            let vid = self
                .by_client
                .get(key)
                .ok_or_else(|| anyhow::anyhow!("on_amend_probe_executed: row vanished {key}"))?
                .ctx
                .venue_order_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("on_amend_probe_executed: no wire {key}"))?;
            let marker = self.wire_marker(&vid);
            self.observe_page(
                &vid,
                BackfillOrderStatus::Filled,
                filled_qty_centi,
                0,
                marker,
                &[],
                true,
                0,
                journal,
            )?;
            eprintln!(
                "AMEND PROBE-EXEC booked cid={key} filled={filled_qty_centi}(全量,义务机制入账)"
            );
        } else {
            eprintln!(
                "AMEND PROBE-EXEC partial-declare cid={key} filled={filled_qty_centi}/{qty} rem={remaining_qty_centi} —— D2b 禁令不发 Filled,fill 走通道"
            );
        }
        Ok(())
    }

    pub fn on_cancel_canceled_with_fill<C: AdmissionClient<R>>(
        &mut self,
        client: &C,
        filled_qty_centi: u64,
        remaining_qty_centi: u64,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let key = client.as_str();
        let (vid, attributed) = {
            let order = self.by_client.get(key).ok_or_else(|| {
                anyhow::anyhow!("on_cancel_canceled_with_fill: unknown client {key}")
            })?;
            (
                order.ctx.venue_order_id.clone(),
                order.ctx.attributed_fill_qty,
            )
        };
        let filled_qty = filled_qty_centi.max(attributed);
        let vid =
            vid.ok_or_else(|| anyhow::anyhow!("on_cancel_canceled_with_fill: no wire {key}"))?;
        let marker = self.wire_marker(&vid);
        self.observe_page(
            &vid,
            BackfillOrderStatus::Canceled,
            filled_qty,
            remaining_qty_centi,
            marker,
            &[],
            true,
            0,
            journal,
        )
        .map(|_| ())
    }
}
