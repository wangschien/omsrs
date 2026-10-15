//! Recovery fill-chain proof and host-only confirmation reductions.
use super::live_admission::AdmissionClient;
use super::live_cover::CoverageClient;
use super::live_lifecycle::ReapOutcome;
use super::live_state::LiveOmsBookState;
use super::live_transaction::TransactionIo;
use crate::lifecycle::OrderState;

pub trait RecoveryReadIo {
    fn wire_fill_qty(&self, wire: &str) -> Option<u64>;
}
pub trait RecoveryTrace {
    fn invalid_terminal_state(cid: &str, state: &Option<OrderState>);
    fn terminal_error(cid: &str, error: &anyhow::Error);
    fn keep_projected(cid: &str, filled: bool, venue_filled_centi: u64);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostOnlyConfirm {
    pub filled: bool,
    pub venue_filled_centi: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOnlyOmsDisposition {
    Terminal,
    KeepProjected,
}
pub fn oms_attributed_qty_on_wire<R, Stage>(oms: &LiveOmsBookState<R, Stage>, wire: &str) -> u64 {
    let w = wire.trim();
    if w.is_empty() {
        return 0;
    }
    oms.by_client
        .values()
        .filter(|o| o.ctx.venue_order_id.as_ref().is_some_and(|v| v.0 == w))
        .map(|o| o.ctx.attributed_fill_qty)
        .fold(0u64, u64::saturating_add)
}

pub fn journal_fill_qty_for_wire<J: RecoveryReadIo>(journal: &J, wire: &str) -> u64 {
    let w = wire.trim();
    if w.is_empty() {
        return 0;
    }
    journal.wire_fill_qty(w).unwrap_or(0)
}

pub fn journal_has_fill_for_wire<J: RecoveryReadIo>(journal: &J, wire: &str) -> bool {
    let w = wire.trim();
    !w.is_empty() && journal.wire_fill_qty(w).is_some()
}

pub fn ghost_fill_chain_closed<C: CoverageClient, R, Stage, J: RecoveryReadIo>(
    oms: &LiveOmsBookState<R, Stage>,
    journal: &J,
    cid: &C,
    wire: &str,
    venue_filled_centi: u64,
) -> bool {
    if venue_filled_centi == 0 {
        return false;
    }
    let oms_qty = oms
        .by_client
        .get(cid.as_str())
        .map(|o| o.ctx.attributed_fill_qty)
        .unwrap_or(0)
        .max(oms_attributed_qty_on_wire(oms, wire));
    let booked = oms_qty.max(journal_fill_qty_for_wire(journal, wire));
    booked >= venue_filled_centi
}

pub fn host_only_oms_disposition(
    confirm: HostOnlyConfirm,
    fill_chain_closed: bool,
) -> HostOnlyOmsDisposition {
    match (confirm.filled, fill_chain_closed) {
        (true, true) => HostOnlyOmsDisposition::Terminal,
        (true, false) | (false, _) => HostOnlyOmsDisposition::KeepProjected,
    }
}
impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn apply_host_only_confirm<
        C: AdmissionClient<R> + std::fmt::Display,
        J: RecoveryReadIo + TransactionIo,
    >(
        &mut self,
        cid: &C,
        confirm: HostOnlyConfirm,
        positive_fill_evidence: bool,
        bot_only_ghosts_closed: &mut Vec<C>,
        bot_only_ghost_filled: &mut bool,
        keep_projected: &mut Vec<C>,
        keep_projected_terminals: &mut Vec<C>,
        wire: &str,
        journal: &mut J,
    ) where
        Stage: RecoveryTrace,
    {
        bot_only_ghosts_closed.push(cid.clone());
        if confirm.filled {
            *bot_only_ghost_filled = true;
        }
        let filled = confirm.filled;
        let venue_filled_centi = confirm.venue_filled_centi;
        let chain = ghost_fill_chain_closed(self, journal, cid, wire, venue_filled_centi);
        match host_only_oms_disposition(confirm, chain) {
            HostOnlyOmsDisposition::Terminal => {
                let st = self.by_client.get(cid.as_str()).map(|o| o.state.clone());
                if st
                    .as_ref()
                    .is_some_and(|s| !Self::can_canceled_terminal_host_only(s))
                {
                    Stage::invalid_terminal_state(cid.as_str(), &st);
                } else if let Err(e) = self.terminal_host_only_ghost(cid, journal) {
                    Stage::terminal_error(cid.as_str(), &e);
                } else {
                    keep_projected_terminals.push(cid.clone());
                }
            }
            HostOnlyOmsDisposition::KeepProjected => {
                keep_projected.push(cid.clone());
                Stage::keep_projected(cid.as_str(), filled, venue_filled_centi);
            }
        }
        if positive_fill_evidence {
            *bot_only_ghost_filled = true;
        }
    }

    pub fn recovery_outcome_counts<C: CoverageClient>(
        &self,
        outcomes: &[(C, ReapOutcome)],
        wires: &[(C, String)],
    ) -> (u64, u64) {
        let unattributed_fills = outcomes
            .iter()
            .zip(wires.iter())
            .filter(|((_, o), (cid, wire))| match o {
                ReapOutcome::GoneFilled {
                    filled_qty_centi, ..
                } => !self.gone_filled_is_accounted(cid.as_str(), wire, *filled_qty_centi),
                _ => false,
            })
            .count() as u64;
        let canceled = outcomes.iter().filter(|(_, o)| o.gone()).count() as u64;
        (unattributed_fills, canceled)
    }
}

pub fn venue_only_bot_orphans_to_cancel<C: CoverageClient + Clone>(
    drifts: &[super::open_projection::OpenSetDrift<C>],
    bot_id: &str,
    dedicated_key: bool,
    cid_belongs_to_bot: impl Fn(&str, &str) -> bool,
) -> Vec<(C, String)> {
    let mut out = Vec::new();
    for d in drifts {
        if let super::open_projection::OpenSetDrift::VenueOnly {
            wire_order_id,
            client_order_id: Some(cid),
        } = d
        {
            if dedicated_key || cid_belongs_to_bot(cid.as_str(), bot_id) {
                let w = wire_order_id.trim();
                if !w.is_empty() {
                    out.push((cid.clone(), w.to_string()));
                }
            }
        }
    }
    out
}

pub fn venue_only_gone_filled_unattributed(
    oms_already_attributed: bool,
    journal_has_fill_for_wire: bool,
) -> bool {
    !oms_already_attributed && !journal_has_fill_for_wire
}

pub fn should_terminal_host_only_ghost_oms(filled: bool, fill_chain_closed: bool) -> bool {
    matches!(
        host_only_oms_disposition(
            HostOnlyConfirm {
                filled,
                venue_filled_centi: 0,
            },
            fill_chain_closed
        ),
        HostOnlyOmsDisposition::Terminal
    )
}

pub fn oms_row_already_attributed<C: CoverageClient, R, Stage>(
    oms: &LiveOmsBookState<R, Stage>,
    cid: &C,
) -> bool {
    oms.by_client.get(cid.as_str()).is_some_and(|row| {
        row.ctx.attributed_fill_qty > 0 || matches!(row.state, OrderState::Filled)
    })
}

pub fn host_only_bot_ghosts<C: CoverageClient + Clone>(
    drifts: &[super::open_projection::OpenSetDrift<C>],
    bot_id: &str,
    dedicated_key: bool,
    cid_belongs_to_bot: impl Fn(&str, &str) -> bool,
) -> Vec<(C, String)> {
    let mut out = Vec::new();
    for d in drifts {
        if let super::open_projection::OpenSetDrift::HostOnly {
            client_order_id: cid,
            wire_order_id,
        } = d
        {
            if dedicated_key || cid_belongs_to_bot(cid.as_str(), bot_id) {
                let w = wire_order_id.trim();
                if !w.is_empty() {
                    out.push((cid.clone(), w.to_string()));
                }
            }
        }
    }
    out
}

pub fn recon_host_only_obsolete<C: CoverageClient, R, Stage>(
    oms: &LiveOmsBookState<R, Stage>,
    cid: &C,
) -> bool {
    match oms.by_client.get(cid.as_str()) {
        None => true,
        Some(row) => {
            let working = match row.venue_working_centi {
                Some(w) => w,
                None => row.ctx.qty.saturating_sub(row.ctx.attributed_fill_qty),
            };
            if working > 0 {
                return false;
            }
            let fully_attributed = matches!(row.state, OrderState::Filled)
                || (row.ctx.attributed_fill_qty >= row.ctx.qty && row.ctx.qty > 0);
            fully_attributed
                && !matches!(
                    row.state,
                    OrderState::Halted { .. } | OrderState::ReconcilePending { .. }
                )
        }
    }
}

pub fn recovery_pass_due(last_ns: u64, now_ns: u64, interval_ns: u64) -> bool {
    now_ns.saturating_sub(last_ns) >= interval_ns.max(1)
}
impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn book_cancel_wire_outcomes<C: AdmissionClient<R>>(
        &mut self,
        outs: Vec<(String, Option<C>, ReapOutcome)>,
        journal: &mut dyn TransactionIo,
    ) -> anyhow::Result<Vec<(String, ReapOutcome)>>
    where
        Stage: super::live_ingest::ExecutionTrace<R>,
    {
        let mut res = Vec::with_capacity(outs.len());
        for (wire, cid, out) in outs {
            if let Some(cid) = cid.as_ref().filter(|c| {
                self.by_client.get(c.as_str()).is_some_and(|o| {
                    !o.state.is_terminal_or_halt() && !o.state.is_submission_closed()
                })
            }) {
                let booked = !matches!(out, ReapOutcome::StillThere);
                if booked {
                    if matches!(
                        self.by_client.get(cid.as_str()).map(|o| o.state.clone()),
                        Some(
                            OrderState::Accepted { .. }
                                | OrderState::Partial { .. }
                                | OrderState::ImmediateFillUnattributed { .. }
                        )
                    ) {
                        self.on_cancel_requested(cid, journal)?;
                    }
                }
                match out {
                    ReapOutcome::GoneCanceled => {
                        self.on_cancel_http_success(cid, journal)?;
                        if let Some(w) = self.unique_live_wire(cid) {
                            if self.wire_held_empty(&w) {
                                self.observe_canceled_empty(&w, journal)?;
                            }
                        }
                    }
                    ReapOutcome::GoneFilled {
                        filled_qty_centi,
                        remaining_qty_centi,
                    } => {
                        let qty = self.by_client.get(cid.as_str()).map(|o| o.ctx.qty);
                        if qty == Some(filled_qty_centi) && remaining_qty_centi == 0 {
                            self.on_cancel_404_executed(
                                cid,
                                filled_qty_centi,
                                remaining_qty_centi,
                                journal,
                            )?;
                        } else {
                            self.on_cancel_canceled_with_fill(
                                cid,
                                filled_qty_centi,
                                remaining_qty_centi,
                                journal,
                            )?;
                        }
                    }
                    ReapOutcome::StillThere => {}
                }
                if booked {
                    Stage::action_admit_venue(
                        self,
                        "cancel",
                        "terminal",
                        cid.as_str(),
                        Some(&wire),
                        None,
                    );
                }
            }
            res.push((wire, out));
        }
        Ok(res)
    }
}
