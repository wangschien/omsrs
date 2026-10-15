//! Mutable recovery metadata over the caller's original client identity type.
use super::cancel_ack::{CancelGoneEvidence, CancelGoneResolve, LiveCancelAck};
use super::live_cover::CoverageClient;
use super::live_state::LiveOmsBookState;
use super::unknown_amend::UaState;
use crate::lifecycle::{ClientOrderId as OmsClientOrderId, OrderState, VenueOrderId};
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AmendDowngrades {
    pub pending_drop: u64,
    pub to_cancel: u64,
    pub to_decrease: u64,
    pub unbooked_drop: u64,
}

#[derive(Debug, Clone)]
pub struct GoneLatch {
    pub kind: GoneKind,
    pub wire: String,
    pub next_get_ns: u64,
    pub get_backoff_ns: u64,
    pub probe_after_ns: u64,
    pub order_age_anchor_ns: Option<u64>,
    pub action_send_utc_ns: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoneKind {
    Unconfirmed,
    Contradiction,
}

#[derive(Debug)]
pub struct SessionRecoveryFields<C> {
    pub create_filtered_twin_cross: u64,
    pub gone_latch: std::collections::HashMap<C, GoneLatch>,
    pub gone_get_budget: Vec<u64>,
    pub probe_granted: Vec<C>,
    pub gone_latch_hits: u64,
    pub gone_latch_get_retries: u64,
    pub gone_latch_still_open_resumes: u64,
    pub gone_latch_probe_reopens: u64,
    pub still_open_streak: std::collections::HashMap<C, u32>,
    pub gone_latch_contradictions: u64,
    pub unconfirmed_latch_entered: u64,
    pub resolved_by_fill: u64,
    pub resolved_by_still_open: u64,
    pub resolved_by_probe: u64,
    pub resolved_by_authoritative_get: u64,
    pub resolved_by_other_terminal_or_halt: u64,
    pub amend_reject_tombstones: std::collections::HashMap<String, u64>,
    pub ua: UaState,
    pub decrease_reject_memo: std::collections::BTreeMap<String, ((u64, u64, u64), u64)>,
    pub amend_downgrades: AmendDowngrades,
    pub unbooked_fill_dropped: bool,
}

pub const DECREASE_REJECT_MEMO_TTL_NS: u64 = 1_000_000_000;
pub const GONE_GET_BASE_NS: u64 = 250_000_000;
pub const GONE_GET_CAP_NS: u64 = 2_000_000_000;
pub const GONE_PROBE_TTL_NS: u64 = 10_000_000_000;
pub const GONE_GET_BUDGET_N: usize = 4;
pub const GONE_GET_BUDGET_WINDOW_NS: u64 = 250_000_000;
impl<C: CoverageClient + Clone + Eq + std::hash::Hash> SessionRecoveryFields<C> {
    pub fn gone_latch_filter(&mut self, cancels: Vec<C>, now_ns: u64) -> Vec<C> {
        self.probe_granted.clear();
        let mut out = Vec::with_capacity(cancels.len());
        for cid in cancels {
            match self.gone_latch.get_mut(&cid) {
                None => out.push(cid),
                Some(l) if now_ns >= l.probe_after_ns => {
                    l.probe_after_ns = u64::MAX;
                    self.probe_granted.push(cid.clone());
                    out.push(cid);
                }
                Some(_) => {
                    self.gone_latch_hits = self.gone_latch_hits.saturating_add(1);
                    eprintln!("GONE-LATCH HIT cancel 抑制 cid={}", cid.as_str());
                }
            }
        }
        out
    }

    pub fn gone_latch_lifecycle_sweep<R, Stage>(&mut self, oms: &LiveOmsBookState<R, Stage>) {
        let mut drop = Vec::new();
        self.gone_latch.retain(|cid, l| {
            let keep = oms
                .by_client
                .get(cid.as_str())
                .is_some_and(|o| !o.state.is_terminal_or_halt() && !o.state.is_submission_closed());
            if !keep {
                let filled = oms
                    .by_client
                    .get(cid.as_str())
                    .is_some_and(|o| matches!(o.state, OrderState::Filled));
                drop.push((l.kind, filled));
            }
            keep
        });
        for (kind, filled) in drop {
            if kind != GoneKind::Unconfirmed {
                continue;
            }
            if filled {
                self.resolved_by_fill = self.resolved_by_fill.saturating_add(1);
            } else {
                self.resolved_by_other_terminal_or_halt =
                    self.resolved_by_other_terminal_or_halt.saturating_add(1);
            }
        }
        self.still_open_streak.retain(|cid, _| {
            oms.by_client
                .get(cid.as_str())
                .is_some_and(|o| !o.state.is_terminal_or_halt() && !o.state.is_submission_closed())
        });
    }

    pub fn unconfirmed_unresolved_at_end(&self) -> u64 {
        self.gone_latch
            .values()
            .filter(|l| l.kind == GoneKind::Unconfirmed)
            .count() as u64
    }

    pub fn is_amend_reject_tombstone(&mut self, cid: &str, now_ns: u64) -> bool {
        self.sweep_amend_tombstones(now_ns);
        self.amend_reject_tombstones.contains_key(cid)
    }

    pub fn sweep_amend_tombstones(&mut self, now_ns: u64) {
        const TTL_NS: u64 = 60_000_000_000;
        self.amend_reject_tombstones
            .retain(|_, born| now_ns.saturating_sub(*born) < TTL_NS);
    }

    pub fn keep_prepare_decrease<R, Stage>(
        &self,
        oms: &LiveOmsBookState<R, Stage>,
        cid: &C,
        target: &u64,
        now_ns: u64,
    ) -> bool {
        let pending_on_wire = oms
            .by_client
            .get(cid.as_str())
            .and_then(|o| o.ctx.venue_order_id.as_ref())
            .and_then(|wire| oms.public_fields.registry.get(&oms.ledger, wire))
            .is_some_and(|ws| ws.amend_lineage.is_some());
        !pending_on_wire
            && !oms.decrease_cannot_reduce(cid.as_str(), *target)
            && !self.decrease_rejected_before(oms, cid, *target, now_ns)
    }

    pub fn decrease_memo_key<R, Stage>(
        &self,
        oms: &LiveOmsBookState<R, Stage>,
        cid: &C,
        target: u64,
    ) -> Option<(u64, u64, u64)> {
        let row = oms.by_client.get(cid.as_str())?;
        Some((
            target,
            row.ctx.remaining_qty_checked()?,
            row.ctx.attributed_fill_qty,
        ))
    }

    pub fn decrease_rejected_before<R, Stage>(
        &self,
        oms: &LiveOmsBookState<R, Stage>,
        cid: &C,
        target: u64,
        now_ns: u64,
    ) -> bool {
        self.decrease_reject_memo
            .get(cid.as_str())
            .is_some_and(|(key, at_ns)| {
                now_ns.saturating_sub(*at_ns) < DECREASE_REJECT_MEMO_TTL_NS
                    && Some(*key) == self.decrease_memo_key(oms, cid, target)
            })
    }

    pub fn note_rejected_decreases<R, Stage>(
        &mut self,
        oms: &LiveOmsBookState<R, Stage>,
        rejected: &[(C, u64, u16)],
        now_ns: u64,
    ) {
        for (cid, target, status) in rejected {
            if *status != 400 {
                continue;
            }
            if let Some(key) = self.decrease_memo_key(oms, cid, *target) {
                self.decrease_reject_memo
                    .insert(cid.as_str().to_string(), (key, now_ns));
            }
        }
        self.decrease_reject_memo.retain(|cid, _| {
            C::from_string(cid.to_string())
                .and_then(|c| oms.by_client.get(c.as_str()))
                .is_some_and(|row| !row.state.is_terminal_or_halt())
        });
    }

    pub fn gone_latch_due(&mut self, now_ns: u64) -> Vec<(C, String, Option<u64>, Option<u64>)> {
        self.gone_get_budget
            .retain(|t| now_ns.saturating_sub(*t) < GONE_GET_BUDGET_WINDOW_NS);
        let mut due: Vec<(u64, C, String)> = self
            .gone_latch
            .iter()
            .filter(|(_, l)| now_ns >= l.next_get_ns)
            .map(|(c, l)| (l.next_get_ns, c.clone(), l.wire.clone()))
            .collect();
        due.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.as_str().cmp(b.1.as_str())));
        let mut out = Vec::new();
        for (_, cid, wire) in due {
            if self.gone_get_budget.len() >= GONE_GET_BUDGET_N {
                break;
            }
            self.gone_get_budget.push(now_ns);
            self.gone_latch_get_retries = self.gone_latch_get_retries.saturating_add(1);
            let latch = self.gone_latch.get(&cid);
            let age = latch.and_then(|l| l.order_age_anchor_ns);
            let action_send_utc_ns = latch.and_then(|l| l.action_send_utc_ns);
            out.push((cid, wire, age, action_send_utc_ns));
        }
        out
    }
}

impl<C: CoverageClient + Clone + Eq + std::hash::Hash> SessionRecoveryFields<C> {
    pub fn gone_latch_consume_report(
        &mut self,
        cancels: &[LiveCancelAck<C>],
        cancels_venue_gone: &[CancelGoneEvidence<C>],
        now_ns: u64,
    ) {
        for c in cancels {
            if let Some(l) = self.gone_latch.remove(&c.client_order_id) {
                if l.kind == GoneKind::Unconfirmed {
                    let real_reduce = self.probe_granted.contains(&c.client_order_id)
                        && c.delete_reduced_positive;
                    if real_reduce {
                        self.resolved_by_probe = self.resolved_by_probe.saturating_add(1);
                    } else {
                        self.resolved_by_authoritative_get =
                            self.resolved_by_authoritative_get.saturating_add(1);
                    }
                }
                if self.probe_granted.contains(&c.client_order_id) {
                    self.gone_latch_probe_reopens = self.gone_latch_probe_reopens.saturating_add(1);
                }
            }
            self.still_open_streak.remove(&c.client_order_id);
        }
        for ev in cancels_venue_gone {
            match ev.resolve {
                CancelGoneResolve::StillOpen => {
                    match self.gone_latch.get(&ev.client_order_id).map(|l| l.kind) {
                        Some(GoneKind::Contradiction) => {}
                        Some(GoneKind::Unconfirmed) => {
                            self.gone_latch.remove(&ev.client_order_id);
                            self.gone_latch_still_open_resumes =
                                self.gone_latch_still_open_resumes.saturating_add(1);
                            self.resolved_by_still_open =
                                self.resolved_by_still_open.saturating_add(1);
                        }
                        None => {
                            let sk = self
                                .still_open_streak
                                .entry(ev.client_order_id.clone())
                                .or_insert(0);
                            *sk = sk.saturating_add(1);
                            if *sk >= 2 {
                                self.gone_latch.insert(
                                    ev.client_order_id.clone(),
                                    GoneLatch {
                                        kind: GoneKind::Contradiction,
                                        wire: ev.wire.clone(),
                                        next_get_ns: now_ns.saturating_add(GONE_GET_BASE_NS),
                                        get_backoff_ns: GONE_GET_BASE_NS,
                                        probe_after_ns: now_ns.saturating_add(GONE_PROBE_TTL_NS),
                                        order_age_anchor_ns: ev.order_age_anchor_ns,
                                        action_send_utc_ns: ev.action_send_utc_ns,
                                    },
                                );
                                self.gone_latch_contradictions =
                                    self.gone_latch_contradictions.saturating_add(1);
                                self.still_open_streak.remove(&ev.client_order_id);
                            }
                        }
                    }
                }
                CancelGoneResolve::Unconfirmed => {
                    self.still_open_streak.remove(&ev.client_order_id);
                    if !self.gone_latch.contains_key(&ev.client_order_id) {
                        self.gone_latch.insert(
                            ev.client_order_id.clone(),
                            GoneLatch {
                                kind: GoneKind::Unconfirmed,
                                wire: ev.wire.clone(),
                                next_get_ns: now_ns.saturating_add(GONE_GET_BASE_NS),
                                get_backoff_ns: GONE_GET_BASE_NS,
                                probe_after_ns: now_ns.saturating_add(GONE_PROBE_TTL_NS),
                                order_age_anchor_ns: ev.order_age_anchor_ns,
                                action_send_utc_ns: ev.action_send_utc_ns,
                            },
                        );
                        self.unconfirmed_latch_entered =
                            self.unconfirmed_latch_entered.saturating_add(1);
                    }
                }
                CancelGoneResolve::AckCanceled
                | CancelGoneResolve::AckFilled { .. }
                | CancelGoneResolve::AckCanceledWithFill { .. } => {}
            }
        }
        let granted = std::mem::take(&mut self.probe_granted);
        for cid in granted {
            if let Some(l) = self.gone_latch.get_mut(&cid) {
                let in_gone = cancels_venue_gone.iter().any(|e| e.client_order_id == cid);
                if !in_gone || l.kind == GoneKind::Contradiction {
                    l.probe_after_ns = now_ns.saturating_add(GONE_PROBE_TTL_NS);
                }
            }
        }
    }
}

pub fn post_action_resting<C: CoverageClient, R, Stage>(
    oms: &LiveOmsBookState<R, Stage>,
    wire: &str,
    cid: &C,
    action_send_utc_ns: Option<u64>,
) -> bool {
    oms.public_fields
        .registry
        .get(&oms.ledger, &VenueOrderId(wire.to_owned()))
        .and_then(|state| state.latest_user_order.as_ref())
        .is_some_and(|frame| {
            frame.client_order_id == OmsClientOrderId(cid.as_str().to_string())
                && frame.status == crate::observation::UserOrderStatus::Resting
                && frame.remaining > 0
                && action_send_utc_ns.map_or(true, |send| frame.updated_ts_ms >= send / 1_000_000)
        })
}
impl<C: CoverageClient + Clone + Eq + std::hash::Hash> SessionRecoveryFields<C> {
    pub fn apply_gone_get_resolve<R, Stage, E>(
        &mut self,
        oms: &LiveOmsBookState<R, Stage>,
        cancels: &mut Vec<LiveCancelAck<C>>,
        cid: C,
        wire: String,
        mut resolve: std::result::Result<CancelGoneResolve, E>,
        now_ns: u64,
    ) where
        C: std::fmt::Display,
    {
        if self.oms_target_resolved(oms, &cid) {
            let filled = oms
                .by_client
                .get(cid.as_str())
                .is_some_and(|o| matches!(o.state, OrderState::Filled));
            if self
                .gone_latch
                .remove(&cid)
                .is_some_and(|l| l.kind == GoneKind::Unconfirmed)
            {
                if filled {
                    self.resolved_by_fill = self.resolved_by_fill.saturating_add(1);
                } else {
                    self.resolved_by_other_terminal_or_halt =
                        self.resolved_by_other_terminal_or_halt.saturating_add(1);
                }
            }
            self.still_open_streak.remove(&cid);
            if let Ok(
                ref r @ (CancelGoneResolve::AckCanceled
                | CancelGoneResolve::AckFilled { .. }
                | CancelGoneResolve::AckCanceledWithFill { .. }),
            ) = resolve
            {
                if oms
                    .by_client
                    .get(cid.as_str())
                    .map_or(true, |o| o.state.is_terminal_or_halt())
                {
                    if let Some(ack) = super::cancel_ack::cancel_ack_from_get(r, cid.clone(), &wire)
                        .map(LiveCancelAck::after_cancel_not_found)
                    {
                        cancels.push(ack);
                    }
                }
            }
            return;
        }
        if matches!(
            &resolve,
            Ok(CancelGoneResolve::AckCanceled
                | CancelGoneResolve::AckFilled { .. }
                | CancelGoneResolve::AckCanceledWithFill { .. })
        ) && post_action_resting(
            oms,
            &wire,
            &cid,
            self.gone_latch.get(&cid).and_then(|l| l.action_send_utc_ns),
        ) {
            resolve = Ok(CancelGoneResolve::Unconfirmed);
        }
        match resolve {
            Ok(ref r @ CancelGoneResolve::AckCanceled)
            | Ok(ref r @ CancelGoneResolve::AckFilled { .. })
            | Ok(ref r @ CancelGoneResolve::AckCanceledWithFill { .. }) => {
                match super::cancel_ack::cancel_ack_from_get(r, cid.clone(), &wire)
                    .map(LiveCancelAck::after_cancel_not_found)
                {
                    Some(ack) => {
                        cancels.push(ack);
                        if self
                            .gone_latch
                            .remove(&cid)
                            .is_some_and(|l| l.kind == GoneKind::Unconfirmed)
                        {
                            self.resolved_by_authoritative_get =
                                self.resolved_by_authoritative_get.saturating_add(1);
                        }
                        self.still_open_streak.remove(&cid);
                    }
                    None => {
                        eprintln!(
                            "★ gone-latch GET:终局映射返回 None(不该发生)cid={cid} wire={wire} \
                             —— 保持闩,不入账"
                        );
                    }
                }
            }
            Ok(CancelGoneResolve::StillOpen) => match self.gone_latch.get(&cid).map(|l| l.kind) {
                Some(GoneKind::Contradiction) => {
                    if let Some(l) = self.gone_latch.get_mut(&cid) {
                        l.get_backoff_ns =
                            (l.get_backoff_ns.saturating_mul(2)).min(GONE_GET_CAP_NS);
                        l.next_get_ns = now_ns.saturating_add(l.get_backoff_ns);
                    }
                }
                _ => {
                    self.gone_latch.remove(&cid);
                    self.gone_latch_still_open_resumes =
                        self.gone_latch_still_open_resumes.saturating_add(1);
                    self.resolved_by_still_open = self.resolved_by_still_open.saturating_add(1);
                }
            },
            Ok(CancelGoneResolve::Unconfirmed) | Err(_) => {
                if let Some(l) = self.gone_latch.get_mut(&cid) {
                    l.get_backoff_ns = (l.get_backoff_ns.saturating_mul(2)).min(GONE_GET_CAP_NS);
                    l.next_get_ns = now_ns.saturating_add(l.get_backoff_ns);
                }
            }
        }
    }

    pub fn oms_target_resolved<R, Stage>(&self, oms: &LiveOmsBookState<R, Stage>, cid: &C) -> bool {
        match oms.by_client.get(cid.as_str()) {
            None => true,
            Some(o) => {
                o.state.is_submission_closed()
                    || o.state.is_terminal_or_halt()
                    || o.venue_working_centi == Some(0)
                    || (o.ctx.attributed_fill_qty >= o.ctx.qty && o.ctx.qty > 0)
            }
        }
    }

    pub fn should_skip_cancel_http(state: &OrderState, order_qty_centi: u64) -> bool {
        matches!(
            state,
            OrderState::ReconcilePending { target, filled_qty, .. }
                if target.terminal == crate::lifecycle::ReconcileTerminal::Filled
                    && target.venue_filled_qty == order_qty_centi
                    && target.venue_remaining_qty == Some(0)
                    && *filled_qty < order_qty_centi
        )
    }

    pub fn cancel_cids_skipping_reconcile_pending<R, Stage>(
        oms: &LiveOmsBookState<R, Stage>,
        cancels: &[C],
    ) -> Vec<C> {
        cancels
            .iter()
            .filter(|cid| {
                !oms.by_client
                    .get(cid.as_str())
                    .is_some_and(|o| Self::should_skip_cancel_http(&o.state, o.ctx.qty))
            })
            .cloned()
            .collect()
    }
}
