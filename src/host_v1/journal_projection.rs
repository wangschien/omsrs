//! Source-shaped restart journal and resting-order projections.

use super::live_state::LiveOmsOrder;
use super::persistence::for_each_journal_record;
use crate::execution::{InventoryDisposition, InventoryProvenance, LedgerId};
use crate::lifecycle::{
    ClientOrderId as OmsClientOrderId, JournalRecord, OrderState, VenueOrderId,
};
use crate::observation::{AmendPhase, WireKind, WireRegistry};
use std::collections::{BTreeMap, BTreeSet};

pub fn last_bound_cid(ws: &crate::observation::WireState) -> Option<&OmsClientOrderId> {
    ws.members.iter().max_by_key(|m| m.bind_pos).map(|m| &m.cid)
}

pub fn held_terminal_identity(ws: &crate::observation::WireState) -> Option<&OmsClientOrderId> {
    if !ws.authority_hold
        || ws.proved_current_cid.is_some()
        || ws.members.is_empty()
        || ws
            .members
            .iter()
            .any(|m| m.role != crate::observation::MemberRole::Terminal)
        || ws
            .amend_lineage
            .as_ref()
            .is_some_and(|l| l.phase == AmendPhase::HttpPending)
    {
        return None;
    }
    last_bound_cid(ws)
}

pub fn journal_ledger(records: &[JournalRecord]) -> Option<LedgerId> {
    let mut found = None;
    for_each_journal_record(records, &mut |r| {
        if found.is_some() {
            return;
        }
        match r {
            JournalRecord::OrderTxn(o) => found = Some(o.ledger.clone()),
            JournalRecord::ExecutionTxn(e) => found = Some(e.ledger.clone()),
            JournalRecord::ObservationTxn(t) => found = Some(t.ledger.clone()),
            _ => {}
        }
    });
    found
}

pub fn journal_order_cids(records: &[JournalRecord]) -> BTreeSet<OmsClientOrderId> {
    let mut cids = BTreeSet::new();
    for_each_journal_record(records, &mut |r| match r {
        JournalRecord::OrderTxn(o) => {
            cids.insert(o.client_order_id.clone());
        }
        JournalRecord::ExecutionTxn(e) => {
            if let Some(o) = e.order.as_ref() {
                cids.insert(o.client_order_id.clone());
            }
        }
        JournalRecord::ObservationTxn(t) => {
            for m in &t.members_after {
                cids.insert(m.client_order_id.clone());
            }
        }
        _ => {}
    });
    cids
}

pub fn halted_with_unaccounted_fills<R>(o: &LiveOmsOrder<R>) -> bool {
    matches!(o.state, OrderState::Halted { .. })
        && o.ctx.attributed_fill_qty < o.ctx.fill_obligation
}

pub fn late_fill_after_terminal_cids(records: &[JournalRecord]) -> BTreeSet<String> {
    fn terminal_in(o: &crate::execution::OrderTxnRecord) -> bool {
        o.core_records.iter().any(|r| {
            matches!(
                r,
                JournalRecord::OrderTerminal {
                    authority_complete: true,
                    ..
                }
            )
        })
    }
    let mut terminal = BTreeSet::<String>::new();
    let mut out = BTreeSet::<String>::new();
    for record in records {
        match record {
            JournalRecord::OrderTxn(o) => {
                if terminal_in(o) {
                    terminal.insert(o.client_order_id.0.clone());
                }
            }

            JournalRecord::ObservationTxn(t) => {
                for m in &t.members_after {
                    if matches!(
                        m.state_after,
                        OrderState::Filled | OrderState::Canceled | OrderState::Terminal
                    ) {
                        terminal.insert(m.client_order_id.0.clone());
                    }
                }
            }
            JournalRecord::ExecutionTxn(e) => {
                if let Some(owner) = e.order.as_ref().map(|o| &o.client_order_id) {
                    if terminal.contains(&owner.0)
                        && e.executions.iter().any(|o| {
                            matches!(o.order, crate::execution::OrderDisposition::LateBooked)
                        })
                    {
                        out.insert(owner.0.clone());
                    }
                }
                if let Some(o) = e.order.as_ref() {
                    if terminal_in(o) {
                        terminal.insert(o.client_order_id.0.clone());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

pub fn backfill_claim_stamps_age(state: &OrderState) -> bool {
    match state {
        OrderState::Accepted { .. } => true,
        OrderState::Partial { remaining_qty, .. } if *remaining_qty > 0 => true,
        _ => false,
    }
}

pub fn resting_wire_for_open_projection(state: &OrderState) -> Option<&VenueOrderId> {
    match state {
        OrderState::Accepted { venue_order_id } => Some(venue_order_id),
        OrderState::Partial {
            venue_order_id,
            remaining_qty,
            ..
        } if *remaining_qty > 0 => Some(venue_order_id),
        OrderState::CancelPending {
            venue_order_id,
            remaining_qty,
            ..
        } if *remaining_qty > 0 => Some(venue_order_id),
        OrderState::ReconcilePending { venue_order_id, .. } => Some(venue_order_id),
        OrderState::ImmediateFillUnattributed { venue_order_id, .. } => Some(venue_order_id),
        _ => None,
    }
}

pub fn reconstruct_pending_wires<R>(
    records: &[JournalRecord],
    registry: &WireRegistry,
    ledger: &LedgerId,
    by_client: &BTreeMap<String, LiveOmsOrder<R>>,
) -> BTreeSet<VenueOrderId> {
    let mut pending = BTreeSet::new();
    let mut last_obs: BTreeMap<String, usize> = BTreeMap::new();
    let mut known_wires: BTreeSet<VenueOrderId> = BTreeSet::new();
    for (i, r) in records.iter().enumerate() {
        match r {
            JournalRecord::ObservationTxn(t) => {
                last_obs.insert(t.wire.0.clone(), i);
                known_wires.insert(t.wire.clone());
            }
            JournalRecord::ExecutionTxn(e) => {
                for o in &e.executions {
                    if let Some(w) = o.evidence.venue_order_id.as_ref() {
                        known_wires.insert(w.clone());
                    }
                    if let Some(w) = o
                        .entry_after
                        .as_ref()
                        .and_then(|en| en.venue_order_id.clone())
                    {
                        known_wires.insert(w);
                    }
                }
            }
            _ => {}
        }
    }
    for_each_journal_record(records, &mut |r| {
        if let JournalRecord::VenueBoundCid { venue_order_id, .. } = r {
            known_wires.insert(venue_order_id.clone());
        }
    });
    for o in by_client.values() {
        if let Some(w) = o.ctx.venue_order_id.as_ref() {
            known_wires.insert(w.clone());
        }
    }

    for (i, r) in records.iter().enumerate() {
        match r {
            JournalRecord::ExecutionTxn(e) => {
                for o in &e.executions {
                    let ingested_owner_none =
                        matches!(o.inventory, InventoryDisposition::Ingested(_))
                            && (o.entry_after.as_ref().is_some_and(|en| en.owner.is_none())
                                || matches!(
                                    o.inventory,
                                    InventoryDisposition::Ingested(InventoryProvenance::NoRow)
                                        | InventoryDisposition::Ingested(
                                            InventoryProvenance::RowForeign
                                        )
                                ));
                    if !ingested_owner_none {
                        continue;
                    }
                    let Some(w) = o
                        .entry_after
                        .as_ref()
                        .and_then(|en| en.venue_order_id.clone())
                        .or_else(|| o.evidence.venue_order_id.clone())
                    else {
                        continue;
                    };
                    let after_obs = last_obs.get(&w.0).map_or(true, |oi| i > *oi);
                    if after_obs {
                        pending.insert(w);
                    }
                }
            }
            JournalRecord::ObservationTxn(t) => match t.wire_after.kind {
                WireKind::VenueOnly if t.wire_after.resolved => {
                    pending.remove(&t.wire);
                }
                WireKind::Scoped if !t.wire_after.tracked => {
                    pending.remove(&t.wire);
                }
                _ => {
                    pending.insert(t.wire.clone());
                }
            },
            _ => {}
        }
    }
    for wire in &known_wires {
        let Some(ws) = registry.get(ledger, wire) else {
            continue;
        };
        match ws.kind {
            WireKind::VenueOnly if !ws.resolved => {
                pending.insert(wire.clone());
            }
            WireKind::Scoped if ws.tracked => {
                pending.insert(wire.clone());
            }
            WireKind::VenueOnly if ws.resolved => {
                pending.remove(wire);
            }
            WireKind::Scoped if !ws.tracked => {
                pending.remove(wire);
            }
            _ => {}
        }

        if ws.amend_lineage.is_some() || ws.authority_hold {
            pending.insert(wire.clone());
        }
    }
    for o in by_client.values() {
        if let OrderState::ReconcilePending { venue_order_id, .. } = &o.state {
            pending.insert(venue_order_id.clone());
        }
    }
    pending
}

pub fn amend_wire_has_unbooked_evidence<R, Stage>(
    oms: &super::live_state::LiveOmsBookState<R, Stage>,
    old_cid: &str,
) -> bool {
    oms.by_client
        .get(old_cid)
        .and_then(|row| row.ctx.venue_order_id.as_ref())
        .and_then(|wire| oms.registry.get(oms.ledger(), wire))
        .is_some_and(|state| {
            state.authority_hold
                || !state.held.is_empty()
                || state
                    .latest_user_order
                    .as_ref()
                    .is_some_and(|seen| seen.filled != 0)
        })
}

pub fn amend_wire_unbooked_user_order_only<R, Stage>(
    oms: &super::live_state::LiveOmsBookState<R, Stage>,
    old_cid: &str,
) -> bool {
    oms.by_client
        .get(old_cid)
        .and_then(|row| row.ctx.venue_order_id.as_ref())
        .and_then(|wire| oms.registry.get(oms.ledger(), wire))
        .is_some_and(|state| {
            !state.authority_hold
                && state.held.is_empty()
                && state
                    .latest_user_order
                    .as_ref()
                    .is_some_and(|seen| seen.filled != 0)
        })
}
