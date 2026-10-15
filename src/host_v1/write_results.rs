//! Ordered create, cancellation evidence and rollback result reductions.
use super::amend_results::{LiveAmendAck, LiveAmendOutcome};
use super::cancel_ack::{CancelGoneEvidence, CancelGoneResolve, LiveCancelAck};
use super::cancel_booking::CancelBookingIo;
use super::create_ack::CreateAckInput;
use super::live_admission::{AdmissionAmend, AdmissionClient, AdmissionIntent};
use super::live_ingest::ExecutionTrace;
use super::live_state::LiveOmsBookState;
use super::live_transaction::TransactionIo;
use super::prepare_decision::{AmendInput, DecisionRead};
use super::session_recovery::post_action_resting;
use crate::lifecycle::{
    CancelOutcome, ClientOrderId as OmsClientOrderId, Effect, JournalRecord, OrderState,
    SubmissionCloseReason,
};
use anyhow::Result;

pub trait CreateResultInput<C>: CreateAckInput {
    fn client_order_id(&self) -> &C;
    fn wire_order_id(&self) -> &String;
}
pub trait WriteResultTrace<R>: ExecutionTrace<R> {
    fn create_fail_venue_detail(class: CreateFailClass, detail: &str) -> String;
    fn cancel_not_found_outcome(cid: &str, result: &'static str);
    fn cancel_not_attempted(cid: &str);
    fn rollback_failed(cid: &str, error: &anyhow::Error);
}

#[derive(Debug, Clone)]
pub struct LiveCreateFail<C> {
    pub client_order_id: C,
    pub class: CreateFailClass,
    pub detail: String,
    pub post_only_cross: bool,
    pub local_budget_backoff_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateFailClass {
    Unknown,
    RateLimited,
    LocalBudget,
    DefiniteReject,
}
impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn book_create_fail_class<C: AdmissionClient<R>>(
        &mut self,
        cid: &C,
        class: CreateFailClass,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        match class {
            CreateFailClass::RateLimited
            | CreateFailClass::LocalBudget
            | CreateFailClass::DefiniteReject => {
                self.close_submission(cid, SubmissionCloseReason::DefiniteFailure, journal)?;
            }
            CreateFailClass::Unknown => {
                if let Some(o) = self.by_client.get(cid.as_str()) {
                    if matches!(o.state, OrderState::SubmitStarted { .. }) {
                        self.on_create_fail(cid, journal)?;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn fail_unacked_creates_except<C: AdmissionClient<R>, D: DecisionRead<C, R>>(
        &mut self,
        decision: &D,
        created: &std::collections::HashSet<String>,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        for am in decision.amends() {
            if created.contains(am.new_client_order_id().as_str()) {
                continue;
            }
            if let Some(o) = self.by_client.get(am.new_client_order_id().as_str()) {
                if matches!(o.state, OrderState::SubmitStarted { .. }) {
                    self.close_submission(
                        am.new_client_order_id(),
                        SubmissionCloseReason::LocalNotSent,
                        journal,
                    )?;
                    self.clear_unsent_amend_lineage(
                        am.old_client_order_id().as_str(),
                        am.new_client_order_id().as_str(),
                        journal,
                    )?;
                }
            }
        }
        for intent in decision.intents() {
            if created.contains(intent.client_order_id().as_str()) {
                continue;
            }
            if let Some(o) = self.by_client.get(intent.client_order_id().as_str()) {
                if matches!(o.state, OrderState::SubmitStarted { .. }) {
                    self.on_create_fail(intent.client_order_id(), journal)?;
                }
            }
        }
        Ok(())
    }

    pub fn veto_post_action_evidence<C: AdmissionClient<R>, S>(
        &self,
        cancels: &mut Vec<LiveCancelAck<C>>,
        cancels_venue_gone: &mut Vec<CancelGoneEvidence<C>>,
        decreases_gone: &mut Vec<LiveCancelAck<C>>,
        amends: &mut [LiveAmendAck<C, S, R>],
    ) {
        let mut vetoed = Vec::new();
        cancels.retain(|ack| {
            if !ack.cancel_not_found {
                return true;
            }
            let live = post_action_resting(
                self,
                &ack.wire_order_id,
                &ack.client_order_id,
                ack.cancel_404_send_utc_ns,
            );
            if live {
                vetoed.push(CancelGoneEvidence {
                    client_order_id: ack.client_order_id.clone(),
                    wire: ack.wire_order_id.clone(),
                    resolve: CancelGoneResolve::Unconfirmed,
                    order_age_anchor_ns: self
                        .by_client
                        .get(ack.client_order_id.as_str())
                        .and_then(|row| row.order_age_anchor_ns),
                    action_send_utc_ns: ack.cancel_404_send_utc_ns,
                });
                false
            } else {
                true
            }
        });
        cancels_venue_gone.extend(vetoed);
        decreases_gone.retain(|ack| {
            !ack.cancel_404_send_utc_ns.is_some_and(|send| {
                post_action_resting(self, &ack.wire_order_id, &ack.client_order_id, Some(send))
            })
        });
        for ack in amends {
            if ack.official_404_send_utc_ns.is_some_and(|send| {
                post_action_resting(
                    self,
                    &ack.wire_order_id,
                    &ack.old_client_order_id,
                    Some(send),
                )
            }) {
                ack.outcome = LiveAmendOutcome::Failed {
                    detail: "post-action old-CID resting frame vetoed 404 GET".into(),
                    definite: false,
                };
            }
        }
    }

    pub fn book_create_results<
        C: AdmissionClient<R>,
        D: DecisionRead<C, R>,
        A: CreateResultInput<C>,
    >(
        &mut self,
        decision: &D,
        creates: &[A],
        now_ns: u64,
        journal: &mut dyn TransactionIo,
    ) -> Result<std::collections::HashSet<String>>
    where
        Stage: ExecutionTrace<R>,
    {
        let created: std::collections::HashSet<String> = creates
            .iter()
            .map(|a| a.client_order_id().as_str().to_string())
            .collect();
        for ack in creates {
            if let Some(intent) = decision
                .intents()
                .iter()
                .find(|i| i.client_order_id() == ack.client_order_id())
            {
                self.on_create_ack::<C, _, _>(intent, ack, journal)?;
                let _ =
                    self.apply_staged_create_claim_and_prove::<C>(ack.client_order_id(), journal)?;
                Stage::action_admit_venue(
                    self,
                    "create",
                    "accepted",
                    ack.client_order_id().as_str(),
                    Some(ack.wire_order_id()),
                    None,
                );
                if let Some(o) = self.by_client.get_mut(ack.client_order_id().as_str()) {
                    o.order_age_anchor_ns = Some(now_ns);
                }
            }
        }
        Ok(created)
    }

    pub fn book_create_fail_results<C: AdmissionClient<R>>(
        &mut self,
        create_fails: &[LiveCreateFail<C>],
        journal: &mut dyn TransactionIo,
    ) -> Result<std::collections::HashSet<String>>
    where
        Stage: WriteResultTrace<R>,
    {
        let mut handled_fails: std::collections::HashSet<String> = std::collections::HashSet::new();
        for fail in create_fails {
            handled_fails.insert(fail.client_order_id.as_str().to_string());
            self.book_create_fail_class(&fail.client_order_id, fail.class, journal)?;
            let venue = Stage::create_fail_venue_detail(fail.class, &fail.detail);
            Stage::action_admit_venue(
                self,
                "create",
                "failed",
                fail.client_order_id.as_str(),
                None,
                Some(&venue),
            );
            if self
                .by_client
                .get(fail.client_order_id.as_str())
                .is_some_and(|row| matches!(row.state, OrderState::SubmitUnknown))
            {
                let _ =
                    self.apply_staged_create_claim_and_prove::<C>(&fail.client_order_id, journal)?;
            }
            let wire = self
                .by_client
                .get(fail.client_order_id.as_str())
                .and_then(|row| {
                    row.ctx.venue_order_id.clone().or_else(|| {
                        row.ctx
                            .applied_fills
                            .values()
                            .find_map(|f| f.venue_order_id.clone())
                    })
                });
            if let Some(wire) = wire {
                self.resolve_unknown_child_if_fully_filled::<C>(
                    &fail.client_order_id,
                    &wire,
                    journal,
                )?;
            }
        }
        Ok(handled_fails)
    }

    pub fn persist_cancel_not_found_observations<C: AdmissionClient<R>, J: CancelBookingIo<C>>(
        &mut self,
        cancels_venue_gone: &[CancelGoneEvidence<C>],
        journal: &mut J,
    ) -> Result<()>
    where
        Stage: WriteResultTrace<R>,
    {
        for evidence in cancels_venue_gone {
            if journal.has_cancel_not_found(&evidence.client_order_id) {
                continue;
            }
            journal.apply_effects(&[Effect::AppendFsync(JournalRecord::CancelOutcomeCid {
                client_order_id: OmsClientOrderId(evidence.client_order_id.as_str().to_owned()),
                outcome: CancelOutcome::NotFound,
            })])?;
            Stage::cancel_not_found_outcome(
                evidence.client_order_id.as_str(),
                if self
                    .by_client
                    .get(evidence.client_order_id.as_str())
                    .is_some_and(|row| matches!(row.state, OrderState::Filled))
                {
                    "filled"
                } else {
                    "gone"
                },
            );
        }
        Ok(())
    }

    pub fn rollback_unattempted_cancels<C: AdmissionClient<R> + std::fmt::Display>(
        &mut self,
        cancels_not_attempted: &[C],
        errors: &mut Vec<String>,
        journal: &mut dyn TransactionIo,
    ) where
        Stage: WriteResultTrace<R>,
    {
        for cid in cancels_not_attempted {
            Stage::cancel_not_attempted(cid.as_str());
            if self.by_client.get(cid.as_str()).is_none()
                || self
                    .by_client
                    .get(cid.as_str())
                    .is_some_and(|o| o.state.is_submission_closed())
            {
                continue;
            }
            if let Err(e) = self.on_cancel_outcome(cid, CancelOutcome::Rejected, journal) {
                Stage::rollback_failed(cid.as_str(), &e);
                errors.push(format!("cancel rollback failed {cid}: {e:#}"));
            }
        }
    }
}
