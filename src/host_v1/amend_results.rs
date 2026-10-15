//! Applied and uncertain amend outcomes over canonical order state.
use super::close_proof::OmsPriceE4;
use super::live_admission::AdmissionClient;
use super::live_cover::CoverageClient;
use super::live_ingest::ExecutionTrace;
use super::live_state::LiveOmsBookState;
use super::live_transaction::TransactionIo;
use super::session_recovery::SessionRecoveryFields;
use crate::lifecycle::{
    ClientOrderId as OmsClientOrderId, SubmissionCloseReason, VenueOrderId,
};
use crate::observation::AmendPhase;
use anyhow::Result;

pub trait AmendResultTrace<R>: ExecutionTrace<R> {
    fn classify_failed_detail(detail: &str, definite: bool) -> String;
}

#[derive(Debug, Clone, PartialEq)]
pub enum LiveAmendOutcome {
    Applied {
        remaining_centi: u64,
        probe_filled_centi: Option<u64>,
    },
    Killed,
    Gone,
    Failed {
        detail: String,
        definite: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveAmendAck<C, S, R> {
    pub old_client_order_id: C,
    pub new_client_order_id: C,
    pub wire_order_id: String,
    pub side: S,
    pub role: R,
    pub price: rust_decimal::Decimal,
    pub outcome: LiveAmendOutcome,
    pub official_404_send_utc_ns: Option<u64>,
    pub amend_http_status: Option<u16>,
}

pub fn amend_http_answered(status: Option<u16>) -> bool {
    matches!(status, Some(200..=299) | Some(400..=499))
}
impl<C: CoverageClient + Clone + Eq + std::hash::Hash> SessionRecoveryFields<C> {
    pub fn book_amend_results<R, S, Stage: AmendResultTrace<R>>(
        &mut self,
        oms: &mut LiveOmsBookState<R, Stage>,
        amends: &[LiveAmendAck<C, S, R>],
        now_ns: u64,
        journal: &mut dyn TransactionIo,
    ) -> Result<()>
    where
        C: AdmissionClient<R>,
    {
        for a in amends {
            match &a.outcome {
                LiveAmendOutcome::Applied {
                    remaining_centi,
                    probe_filled_centi,
                } => {
                    let declared = oms
                        .by_client
                        .get(a.new_client_order_id.as_str())
                        .map(|o| o.ctx.qty)
                        .unwrap_or(*remaining_centi);
                    oms.on_amend_applied::<C>(
                        &a.new_client_order_id,
                        &a.wire_order_id,
                        declared,
                        *remaining_centi,
                        journal,
                    )?;
                    let child_filled = oms
                        .by_client
                        .get(a.new_client_order_id.as_str())
                        .map(|o| (o.ctx.attributed_fill_qty > 0, o.ctx.qty, o.ctx.price_cents));
                    if let Err(error) = oms.advance_amend_lineage(
                        &a.wire_order_id,
                        AmendPhase::AppliedNeedsObservation,
                        Some(now_ns),
                        journal,
                    ) {
                        let consistent = child_filled.is_some_and(|(filled, qty, price_cents)| {
                            filled
                                && *remaining_centi <= qty
                                && OmsPriceE4::from_dollars(a.price)
                                    .is_some_and(|p| p.into_wire_price_cents() == price_cents)
                        });
                        if !consistent {
                            return Err(error);
                        }
                        oms.clear_amend_lineage_as(&a.wire_order_id, journal, "write_decision")?;
                    }
                    let _ =
                        oms.apply_staged_user_order_claim::<C>(&a.new_client_order_id, journal)?;
                    if let Some(o) = oms.by_client.get_mut(a.new_client_order_id.as_str()) {
                        o.order_age_anchor_ns = Some(now_ns);
                    }
                    let wire = VenueOrderId(a.wire_order_id.clone());
                    let staged_current = oms
                        .public_fields
                        .registry
                        .get(&oms.ledger, &wire)
                        .and_then(|ws| ws.latest_user_order.as_ref())
                        .is_some_and(|e| {
                            e.client_order_id
                                == OmsClientOrderId(a.new_client_order_id.as_str().to_owned())
                        });
                    let mut new_cid_proven = false;
                    if staged_current {
                        new_cid_proven = oms
                            .prove_current_amend_resting::<C>(&a.wire_order_id, journal)
                            .is_ok();
                    }
                    if let Some(f) = probe_filled_centi {
                        if *f > 0 {
                            oms.on_amend_probe_executed::<C>(
                                &a.new_client_order_id,
                                *f,
                                *remaining_centi,
                                journal,
                            )?;
                        }
                    }
                    if oms
                        .by_client
                        .get(a.old_client_order_id.as_str())
                        .is_some_and(|o| {
                            !o.state.is_terminal_or_halt() && !o.state.is_submission_closed()
                        })
                    {
                        oms.on_cancel_requested::<C>(&a.old_client_order_id, journal)?;
                        oms.on_cancel_http_success::<C>(&a.old_client_order_id, journal)?;
                    }
                    let _ = new_cid_proven;
                    oms.settle_superseded_old::<C>(
                        &a.old_client_order_id,
                        &a.new_client_order_id,
                        &VenueOrderId(a.wire_order_id.clone()),
                        journal,
                    )?;
                }
                LiveAmendOutcome::Killed => {
                    oms.clear_amend_lineage_as(&a.wire_order_id, journal, "write_decision")?;
                    let newly = oms.close_submission::<C>(
                        &a.new_client_order_id,
                        SubmissionCloseReason::DefiniteFailure,
                        journal,
                    )?;
                    if newly {
                        self.amend_reject_tombstones
                            .insert(a.new_client_order_id.as_str().to_string(), now_ns);
                    }
                    if oms
                        .by_client
                        .get(a.old_client_order_id.as_str())
                        .is_some_and(|o| {
                            !o.state.is_terminal_or_halt() && !o.state.is_submission_closed()
                        })
                    {
                        oms.on_cancel_requested::<C>(&a.old_client_order_id, journal)?;
                        oms.on_cancel_http_success::<C>(&a.old_client_order_id, journal)?;
                    }
                }
                LiveAmendOutcome::Gone => {
                    oms.clear_amend_lineage_as(&a.wire_order_id, journal, "write_decision")?;
                    let newly = oms.close_submission::<C>(
                        &a.new_client_order_id,
                        SubmissionCloseReason::DefiniteFailure,
                        journal,
                    )?;
                    if newly {
                        self.amend_reject_tombstones
                            .insert(a.new_client_order_id.as_str().to_string(), now_ns);
                    }
                    if oms
                        .by_client
                        .get(a.old_client_order_id.as_str())
                        .is_some_and(|o| {
                            !o.state.is_terminal_or_halt() && !o.state.is_submission_closed()
                        })
                    {
                        oms.on_cancel_requested::<C>(&a.old_client_order_id, journal)?;
                    }
                }
                LiveAmendOutcome::Failed { definite, .. } => {
                    if *definite {
                        oms.clear_amend_lineage_as(&a.wire_order_id, journal, "write_decision")?;
                        let newly = oms.close_submission::<C>(
                            &a.new_client_order_id,
                            SubmissionCloseReason::DefiniteFailure,
                            journal,
                        )?;
                        if newly {
                            self.amend_reject_tombstones
                                .insert(a.new_client_order_id.as_str().to_string(), now_ns);
                        }
                    } else {
                        self.ua.ua_note_unknown(
                            a.new_client_order_id.as_str(),
                            a.old_client_order_id.as_str(),
                            &a.wire_order_id,
                            amend_http_answered(a.amend_http_status),
                            journal.wall_now_ns(),
                        );
                        oms.advance_amend_lineage(
                            &a.wire_order_id,
                            AmendPhase::Unknown,
                            Some(now_ns),
                            journal,
                        )?;
                        oms.on_create_fail::<C>(&a.new_client_order_id, journal)?;
                        let _ = oms.fold_staged_pending_amend_evidence::<C>(
                            &a.new_client_order_id,
                            &VenueOrderId(a.wire_order_id.clone()),
                            journal,
                        )?;
                        let _ = oms.recover_unknown_amend_from_user_order::<C>(
                            &VenueOrderId(a.wire_order_id.clone()),
                            now_ns,
                            journal,
                        )?;
                        let _ = oms.recover_unknown_amend_from_terminal_user_order::<C>(
                            &VenueOrderId(a.wire_order_id.clone()),
                            journal,
                        )?;
                        oms.settle_superseded_old::<C>(
                            &a.old_client_order_id,
                            &a.new_client_order_id,
                            &VenueOrderId(a.wire_order_id.clone()),
                            journal,
                        )?;
                        oms.resolve_unknown_child_if_fully_filled::<C>(
                            &a.new_client_order_id,
                            &VenueOrderId(a.wire_order_id.clone()),
                            journal,
                        )?;
                        oms.resolve_unknown_child_if_old_filled_wire::<C>(
                            &VenueOrderId(a.wire_order_id.clone()),
                            journal,
                        )?;
                    }
                }
            }
            let result = match &a.outcome {
                LiveAmendOutcome::Applied { .. } => "applied",
                LiveAmendOutcome::Killed => "killed",
                LiveAmendOutcome::Gone => "gone",
                LiveAmendOutcome::Failed { .. } => "failed",
            };
            let venue = match &a.outcome {
                LiveAmendOutcome::Failed { detail, definite } => {
                    Some(Stage::classify_failed_detail(detail, *definite))
                }
                _ => None,
            };
            Stage::action_admit_venue(
                oms,
                "amend",
                result,
                a.new_client_order_id.as_str(),
                Some(&a.wire_order_id),
                venue.as_deref(),
            );
        }
        Ok(())
    }
}
