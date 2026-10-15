//! Ordered cancellation terminal booking over the original acknowledgement slice.
use super::cancel_ack::LiveCancelAck;
use super::live_admission::AdmissionClient;
use super::live_ingest::ExecutionTrace;
use super::live_lifecycle::LiveCancelTerminalKind;
use super::live_state::{CancelGone, LiveOmsBookState};
use super::live_transaction::TransactionIo;
use crate::lifecycle::{CancelOutcome, VenueOrderId};
use anyhow::Result;

pub trait CancelBookingIo<C>: TransactionIo {
    fn has_cancel_not_found(&self, cid: &C) -> bool;
}
/// Existing diagnostic leaves; they do not select transitions or mutate state.
pub trait CancelBookingTrace<R>: ExecutionTrace<R> {
    fn cancel_evidence(
        cid: &str,
        gone_pre: &Option<(u64, VenueOrderId, Option<u64>)>,
        held_pre: Option<u64>,
        path: &str,
    );
    fn cancel_gone(cid: &str, r: u64, t: u64, held: Option<u64>);
    fn cancel_gone_mismatch(cid: &str, r: u64, total: Option<u64>);
}

impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn book_cancel_terminals<C: AdmissionClient<R>, J: CancelBookingIo<C>>(
        &mut self,
        cancels: &mut [LiveCancelAck<C>],
        journal: &mut J,
    ) -> Result<()>
    where
        Stage: CancelBookingTrace<R>,
    {
        let mut emitted = std::collections::HashSet::<String>::new();
        for c in cancels {
            if !self
                .by_client
                .get(c.client_order_id.as_str())
                .is_some_and(|o| !o.state.is_terminal_or_halt() && !o.state.is_submission_closed())
            {
                let result = match self.by_client.get(c.client_order_id.as_str()) {
                    None => Some("row_absent"),
                    Some(o) if o.state.is_terminal_or_halt() => Some("already_terminal"),
                    Some(_) => None,
                };
                if let Some(result) = result {
                    if emitted.insert(c.client_order_id.as_str().to_string()) {
                        Stage::action_admit_venue(
                            self,
                            "cancel",
                            result,
                            c.client_order_id.as_str(),
                            Some(&c.wire_order_id),
                            None,
                        );
                    }
                }
                continue;
            }
            if c.cancel_not_found && !journal.has_cancel_not_found(&c.client_order_id) {
                self.on_cancel_outcome(&c.client_order_id, CancelOutcome::NotFound, journal)?;
            }
            match c.terminal {
                LiveCancelTerminalKind::Canceled => {
                    let positive_wire = (c.delete_reduced_positive && !c.cancel_not_found)
                        .then(|| VenueOrderId(c.wire_order_id.clone()));
                    let gone_pre = match (positive_wire.as_ref(), c.delete_reduced_by_centi) {
                        (Some(w), Some(r)) => {
                            Some((r, w.clone(), self.wire_total_at_proof_centi(w)))
                        }
                        _ => None,
                    };
                    let held_pre = positive_wire
                        .as_ref()
                        .and_then(|w| self.wire_held_inventory_centi(w));
                    let positive = positive_wire.is_some();
                    self.on_cancel_http_success(&c.client_order_id, journal)?;
                    if let Some(w) = positive_wire {
                        self.retire_old_on_positive_current_cancel(
                            &c.client_order_id,
                            &w,
                            journal,
                        )?;
                    }
                    let mut path = "keep";
                    if let Some(wire) = self.unique_live_wire(&c.client_order_id) {
                        if self.wire_held_empty(&wire) {
                            match &gone_pre {
                                Some((r, _, Some(t)))
                                    if *r <= *t && *t - *r > held_pre.unwrap_or(0) =>
                                {
                                    path = "with_fill";
                                    self.on_cancel_canceled_with_fill(
                                        &c.client_order_id,
                                        *t - *r,
                                        0,
                                        journal,
                                    )?;
                                }
                                Some((_, _, None))
                                    if !self.wire_known_deficit(&VenueOrderId(wire.clone())) =>
                                {
                                    path = "refuse_none";
                                    self.d3_cancel_none.insert(VenueOrderId(wire));
                                }
                                _ => {
                                    path = "synth";
                                    self.observe_canceled_empty(&wire, journal)?;
                                }
                            }
                        }
                    }
                    if positive {
                        Stage::cancel_evidence(
                            c.client_order_id.as_str(),
                            &gone_pre,
                            held_pre,
                            path,
                        );
                    }
                    if let Some((r, w, total)) = gone_pre {
                        let cid = c.client_order_id.as_str();
                        let live =
                            self.by_client
                                .get(c.client_order_id.as_str())
                                .is_some_and(|o| {
                                    !o.state.is_terminal_or_halt()
                                        && !o.state.is_submission_closed()
                                });
                        match total {
                            Some(t) if r <= t => {
                                if live {
                                    let held = self.wire_held_inventory_centi(&w);
                                    Stage::cancel_gone(cid, r, t, held);
                                    if let Some(o) =
                                        self.by_client.get_mut(c.client_order_id.as_str())
                                    {
                                        o.cancel_gone = Some(CancelGone {
                                            venue_filled_centi: t - r,
                                            wire: w,
                                        });
                                    }
                                }
                            }
                            _ => {
                                Stage::cancel_gone_mismatch(cid, r, total);
                            }
                        }
                    }
                }
                LiveCancelTerminalKind::Filled {
                    filled_qty_centi,
                    remaining_qty_centi,
                } => {
                    self.on_cancel_404_executed(
                        &c.client_order_id,
                        filled_qty_centi,
                        remaining_qty_centi,
                        journal,
                    )?;
                }
                LiveCancelTerminalKind::CanceledWithFill {
                    filled_qty_centi,
                    remaining_qty_centi,
                } => {
                    self.on_cancel_canceled_with_fill(
                        &c.client_order_id,
                        filled_qty_centi,
                        remaining_qty_centi,
                        journal,
                    )?;
                }
            }
            if emitted.insert(c.client_order_id.as_str().to_string()) {
                Stage::action_admit_venue(
                    self,
                    "cancel",
                    "terminal",
                    c.client_order_id.as_str(),
                    Some(&c.wire_order_id),
                    None,
                );
            }
            self.mark_hold_slot_if_open::<C, _>(c);
        }
        Ok(())
    }
}
