//! Live authority, quantity and terminal proofs over canonical order state.
use super::journal_projection::{halted_with_unaccounted_fills, last_bound_cid};
use super::live_cover::CoverageClient;
use super::live_state::{LiveOmsBookState, LiveOmsOrder};
use crate::execution::{InventoryHealth, InventoryProvenance};
use crate::lifecycle::{ClientOrderId as OmsClientOrderId, OrderState, VenueOrderId};
use crate::observation::{
    AmendLineage, AmendPhase, MemberRole, ObservationOutcome, Readiness, WireKind, WorkingView,
};
impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn amend_prepare_amendable(&self, cid: &str) -> bool {
        self.by_client.get(cid).is_some_and(|o| {
            let wire = o.ctx.venue_order_id.as_ref();
            let generation_proven = wire.is_some_and(|w| {
                self.public_fields
                    .registry
                    .get(self.ledger(), w)
                    .is_some_and(|ws| {
                        ws.amend_lineage.is_none()
                            && (ws.generation == 0
                                || self.public_fields.registry.member_working_view(
                                    self.ledger(),
                                    &OmsClientOrderId(cid.to_string()),
                                ) == WorkingView::Known(o.ctx.qty))
                    })
            });
            matches!(o.state, OrderState::Accepted { .. })
                && o.ctx.attributed_fill_qty == 0
                && o.ctx
                    .last_venue_remaining_qty
                    .map_or(true, |w| w == o.ctx.qty)
                && generation_proven
        })
    }

    pub fn applied_entry_amend_old_leg_fresh_detail(
        &self,
        row: &LiveOmsOrder<R>,
        applied_witness: impl Fn(&str) -> Option<u64>,
        now_ns: u64,
        bound_ns: u64,
    ) -> Option<(String, u64)> {
        let OrderState::ReconcilePending { target, .. } = &row.state else {
            return None;
        };
        if target.terminal != crate::lifecycle::ReconcileTerminal::Canceled
            || target.venue_filled_qty != 0
            || row.ctx.attributed_fill_qty != 0
        {
            return None;
        }
        let wire = row.ctx.venue_order_id.as_ref()?;
        let ws = self.public_fields.registry.get(&self.ledger, wire)?;
        let lineage = ws.amend_lineage.as_ref()?;
        let t_a = lineage.applied_at_utc_ns?;
        if lineage.old_cid != row.ctx.client_order_id
            || lineage.phase != AmendPhase::AppliedNeedsObservation
        {
            return None;
        }
        let child = self.by_client.get(&lineage.new_cid.0)?;
        if !matches!(child.state, OrderState::Accepted { .. })
            || child.ctx.attributed_fill_qty != 0
            || child.order_age_anchor_ns.is_none()
            || child.state.is_submission_closed()
            || last_bound_cid(ws) != Some(&lineage.new_cid)
            || self.wire_held_inventory_centi(wire) != Some(0)
            || now_ns.saturating_sub(t_a) >= bound_ns
            || applied_witness(&lineage.new_cid.0).is_none()
        {
            return None;
        }
        Some((lineage.new_cid.0.clone(), t_a))
    }

    pub fn is_normal_terminal(&self, client: &str) -> bool {
        self.by_client.get(client).is_some_and(|o| {
            matches!(
                o.state,
                OrderState::Filled | OrderState::Canceled | OrderState::Terminal
            )
        })
    }

    pub fn user_order_initial_ok<C: CoverageClient>(
        &self,
        row: &LiveOmsOrder<R>,
        wire: &VenueOrderId,
        initial: u64,
    ) -> bool {
        if initial == row.ctx.qty
            || row.venue_initial_centi == Some(initial)
            || self.wire_initial_on_lineage(wire, row.ctx.client_order_id.0.as_str(), initial)
        {
            return true;
        }
        let cid = row.ctx.client_order_id.clone();
        self.public_fields
            .registry
            .get(&self.ledger, wire)
            .and_then(|state| state.amend_lineage.as_ref())
            .is_some_and(|lineage| {
                lineage.new_cid.0 == cid.as_str() && self.amend_initial_ok::<C>(lineage, initial)
            })
    }

    pub fn wire_initial_on_lineage(&self, wire: &VenueOrderId, cid: &str, initial: u64) -> bool {
        const LINEAGE_ROW_BOUND: usize = 64;
        if self
            .by_client
            .get(cid)
            .map_or(true, |row| row.ctx.venue_order_id.as_ref() != Some(wire))
        {
            return false;
        }
        if self
            .public_fields
            .registry
            .get(&self.ledger, wire)
            .and_then(|state| state.amend_lineage.as_ref())
            .is_some_and(|lineage| lineage.declared_remaining == initial)
        {
            return true;
        }
        self.by_client
            .values()
            .filter(|row| row.ctx.venue_order_id.as_ref() == Some(wire))
            .take(LINEAGE_ROW_BOUND)
            .any(|row| row.ctx.qty == initial || row.venue_initial_centi == Some(initial))
    }

    pub fn backfill_quantity_valid<C: CoverageClient>(
        &self,
        wire: &VenueOrderId,
        current_cid: Option<&OmsClientOrderId>,
        st: crate::lifecycle::BackfillOrderStatus,
        filled: u64,
        remaining: u64,
        initial: u64,
    ) -> bool {
        use crate::lifecycle::BackfillOrderStatus as B;
        let sum = filled.checked_add(remaining);
        let on_lineage = |initial: u64| {
            current_cid
                .is_some_and(|cid| self.wire_initial_on_lineage(wire, cid.0.as_str(), initial))
        };
        match st {
            B::Open | B::Partial => {
                sum == Some(initial)
                    || (sum.is_some_and(|sum| sum < initial)
                        && remaining > 0
                        && on_lineage(initial)
                        && current_cid
                            .and_then(|cid| C::from_string(cid.0.clone()))
                            .and_then(|cid| self.by_client.get(cid.as_str()))
                            .is_some_and(|row| Some(row.ctx.qty) == sum))
            }
            B::Filled => {
                remaining == 0
                    && (filled == initial
                        || (filled > 0 && filled < initial && on_lineage(initial)))
            }
            B::Canceled => remaining == 0 && filled <= initial,
        }
    }

    pub fn amend_initial_ok<C: CoverageClient>(
        &self,
        lineage: &AmendLineage,
        initial: u64,
    ) -> bool {
        initial == lineage.declared_remaining
            || C::from_string(lineage.old_cid.0.clone())
                .and_then(|old| {
                    self.by_client
                        .get(old.as_str())
                        .map(|row| row.venue_initial_centi.unwrap_or(row.ctx.qty))
                })
                .is_some_and(|qty| qty == initial)
    }

    pub fn decrease_cannot_reduce(&self, cid: &str, target: u64) -> bool {
        if target == 0 {
            return false;
        }
        if !matches!(self.n_readiness(false), Readiness::Ready) {
            return false;
        }
        let has_unbound_non_terminal = self.by_client.values().any(|o| {
            o.ctx.venue_order_id.is_none()
                && !o.state.is_submission_closed()
                && !matches!(
                    o.state,
                    OrderState::Filled | OrderState::Canceled | OrderState::Terminal
                )
        });
        if has_unbound_non_terminal {
            return false;
        }
        let Some(row) = self.by_client.get(cid) else {
            return false;
        };
        if row.ctx.client_order_id.0 != cid {
            return false;
        }
        if row.ctx.market != self.ledger.market || self.ledger.market.is_empty() {
            return false;
        }
        let OrderState::Partial {
            venue_order_id: state_wire,
            filled_qty,
            remaining_qty,
        } = &row.state
        else {
            return false;
        };
        let Some(ctx_wire) = row.ctx.venue_order_id.as_ref() else {
            return false;
        };
        if ctx_wire.0.is_empty() || ctx_wire != state_wire {
            return false;
        }
        let Some(ws) = self.public_fields.registry.get(&self.ledger, ctx_wire) else {
            return false;
        };
        if ws.kind != WireKind::Scoped || ws.generation != 0 || ws.members.len() != 1 {
            return false;
        }
        let m = &ws.members[0];
        if m.role != MemberRole::Current || m.cid.0 != cid {
            return false;
        }
        if *filled_qty != row.ctx.attributed_fill_qty {
            return false;
        }
        if !(row.ctx.attributed_fill_qty > 0 && row.ctx.attributed_fill_qty < row.ctx.qty) {
            return false;
        }
        let Some(upper) = row.ctx.remaining_qty_checked() else {
            return false;
        };
        if *remaining_qty != upper {
            return false;
        }
        target >= upper
    }

    pub fn attributed_coverage_centi_for_wire(&self, wire: &str) -> Option<u64> {
        let w = wire.trim();
        if w.is_empty() {
            return None;
        }
        if matches!(
            self.public_fields.inventory.view().health,
            InventoryHealth::Conflicted { .. }
        ) {
            return None;
        }
        let vid = VenueOrderId(w.to_string());
        if let Some(ws) = self.public_fields.registry.get(&self.ledger, &vid) {
            for m in &ws.members {
                if m.role == MemberRole::Current {
                    let Some(row) = self.by_client.get(&m.cid.0) else {
                        return None;
                    };
                    if halted_with_unaccounted_fills(row) {
                        return None;
                    }
                }
            }
        }
        let mut seen = std::collections::BTreeSet::<String>::new();
        let mut tot: u64 = 0;
        for o in self.by_client.values() {
            let Some(bound) = o.ctx.venue_order_id.as_ref() else {
                continue;
            };
            if bound.0 != w {
                continue;
            }
            if halted_with_unaccounted_fills(o) {
                return None;
            }
            for (fid, payload) in &o.ctx.applied_fills {
                if payload.venue_order_id.as_ref().is_some_and(|pv| pv.0 != w) {
                    continue;
                }
                if !seen.insert(fid.0.clone()) {
                    continue;
                }
                if let Some(entry) = self.public_fields.inventory.get(fid) {
                    if matches!(entry.provenance, InventoryProvenance::Refused(_)) {
                        return None;
                    }
                    if entry.qty != payload.qty {
                        return None;
                    }
                }
                tot = tot.checked_add(payload.qty)?;
            }
        }
        if let Some(ws) = self.public_fields.registry.get(&self.ledger, &vid) {
            if ws.filled_hw > tot {
                return None;
            }
        }
        Some(tot)
    }

    pub fn gone_filled_is_accounted(
        &self,
        client: &str,
        wire: &str,
        filled_qty_centi: u64,
    ) -> bool {
        if filled_qty_centi == 0 {
            return false;
        }
        let w = wire.trim();
        if w.is_empty() {
            return false;
        }
        let Some(row) = self.by_client.get(client) else {
            return false;
        };
        let Some(bound) = row.ctx.venue_order_id.as_ref() else {
            return false;
        };
        if bound.0 != w {
            return false;
        }
        if !self.is_normal_terminal(client) {
            return false;
        }
        if !row.ctx.authority_is_fresh() {
            return false;
        }
        if row.ctx.attributed_fill_qty != row.ctx.fill_obligation {
            return false;
        }
        let Some(covered) = self.attributed_coverage_centi_for_wire(w) else {
            return false;
        };
        covered >= filled_qty_centi
    }

    pub fn authority_wire_completed(&self, wire: &VenueOrderId) -> bool {
        let Some(ws) = self.public_fields.registry.get(&self.ledger, wire) else {
            return false;
        };
        if ws.amend_lineage.is_some() {
            return false;
        }
        if ws.authority_hold {
            return false;
        }
        if ws.kind == WireKind::Scoped
            && self.pending_fee_fill_ids.is_empty()
            && ws.latest_user_order.as_ref().is_some_and(|order| {
                order.status == crate::observation::UserOrderStatus::Resting
                    && ws.proved_current_cid.as_ref() == Some(&order.client_order_id)
                    && ws.last
                        == Some(ObservationOutcome::Consistent {
                            remaining_fresh: true,
                        })
            })
        {
            return true;
        }
        if ws.kind == WireKind::Scoped
            && ws.generation > 0
            && self.pending_fee_fill_ids.is_empty()
            && ws
                .members
                .iter()
                .all(|member| member.role != MemberRole::Current)
            && ws.latest_user_order.as_ref().is_some_and(|order| {
                matches!(
                    order.status,
                    crate::observation::UserOrderStatus::Canceled
                        | crate::observation::UserOrderStatus::Executed
                ) && ws.proved_current_cid.as_ref() == Some(&order.client_order_id)
                    && ws.members.iter().any(|member| {
                        member.cid == order.client_order_id && member.role == MemberRole::Terminal
                    })
                    && self
                        .by_client
                        .get(&order.client_order_id.0)
                        .is_some_and(|row| {
                            matches!(
                                row.state,
                                OrderState::Filled | OrderState::Canceled | OrderState::Terminal
                            ) && row.ctx.venue_order_id.as_ref() == Some(wire)
                        })
            })
        {
            return true;
        }
        if ws.kind == WireKind::Scoped
            && !ws.members.is_empty()
            && self.pending_fee_fill_ids.is_empty()
            && ws.latest_user_order.as_ref().is_some_and(|order| {
                matches!(
                    order.status,
                    crate::observation::UserOrderStatus::Canceled
                        | crate::observation::UserOrderStatus::Executed
                ) && self
                    .by_client
                    .get(&order.client_order_id.0)
                    .is_some_and(|row| {
                        matches!(
                            row.state,
                            OrderState::Filled | OrderState::Canceled | OrderState::Terminal
                        )
                    })
            })
            && ws.members.iter().all(|member| {
                self.by_client.get(&member.cid.0).is_some_and(|row| {
                    matches!(
                        row.state,
                        OrderState::Filled | OrderState::Canceled | OrderState::Terminal
                    )
                })
            })
        {
            return true;
        }
        match ws.kind {
            WireKind::VenueOnly => ws.resolved,
            WireKind::Scoped => {
                if ws.tracked {
                    return false;
                }
                if !matches!(ws.last, Some(ObservationOutcome::Consistent { .. })) {
                    return false;
                }
                if ws.members.is_empty() {
                    return false;
                }
                for m in &ws.members {
                    if m.role != MemberRole::Terminal {
                        return false;
                    }
                    let Some(row) = self.by_client.get(&m.cid.0) else {
                        return false;
                    };
                    if !matches!(
                        row.state,
                        OrderState::Filled | OrderState::Canceled | OrderState::Terminal
                    ) {
                        return false;
                    }
                    match row.ctx.venue_order_id.as_ref() {
                        Some(w) if w == wire => {}
                        _ => return false,
                    }
                }
                true
            }
        }
    }

    pub fn sync_pending_wire_from_registry(&mut self, wire: &VenueOrderId) {
        match self.public_fields.registry.get(&self.ledger, wire) {
            Some(ws) if ws.amend_lineage.is_some() || ws.authority_hold => {
                self.pending_wires.insert(wire.clone());
            }
            Some(ws) => match ws.kind {
                WireKind::VenueOnly if ws.resolved => {
                    self.pending_wires.remove(wire);
                }
                WireKind::Scoped if !ws.tracked => {
                    self.pending_wires.remove(wire);
                }
                _ => {
                    self.pending_wires.insert(wire.clone());
                }
            },
            None => {
                self.pending_wires.insert(wire.clone());
            }
        }
    }
}
