//! Live working quantity and per-wire cover over the canonical order state.
use super::live_state::{LiveOmsBookState, LiveOmsOrder};
use super::risk::reducing_side;
use crate::lifecycle::{ClientOrderId as OmsClientOrderId, Side, VenueOrderId};
use crate::observation::WorkingView;
use std::collections::{BTreeMap, BTreeSet};

/// The caller's existing validated client identifier, preserved without changing
/// its validation or representation at the transport boundary.
pub trait CoverageClient: Sized {
    fn from_string(value: String) -> Option<Self>;
    fn try_from_string(value: String) -> anyhow::Result<Self>;
    fn as_str(&self) -> &str;
}

impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn other_members_attribution(&self, wire: &VenueOrderId, client: &str) -> Option<(u64, bool)> {
        let Some(ws) = self.registry.get(&self.ledger, wire) else {
            return Some((0, false));
        };
        let mut other_attributed = 0u64;
        let mut other_debt = false;
        for member in ws.members.iter().filter(|m| m.cid.0 != client) {
            let other = self.by_client.get(&member.cid.0)?;
            other_debt |= other.ctx.fill_obligation > other.ctx.attributed_fill_qty;
            other_attributed = other_attributed.checked_add(other.ctx.attributed_fill_qty)?;
        }
        Some((other_attributed, other_debt))
    }

    pub fn row_working_centi<C: CoverageClient>(&self, row: &LiveOmsOrder<R>) -> u64 {
        let cid = C::from_string(row.ctx.client_order_id.0.clone());
        row.venue_working_centi
            .or_else(|| cid.and_then(|c| self.slot_projection_remaining(c.as_str())))
            .unwrap_or_else(|| self.row_working_fallback(row).0)
    }

    fn row_working_fallback(&self, row: &LiveOmsOrder<R>) -> (u64, bool) {
        let client = row.ctx.client_order_id.as_str();
        let wire = row.ctx.venue_order_id.as_ref().or_else(|| {
            self.registry
                .wire_for_client(&self.ledger, &row.ctx.client_order_id)
        });
        let (other_attributed, other_debt) = match wire {
            Some(wire) => match self.other_members_attribution(wire, client) {
                Some((filled, debt)) => (filled, debt),
                None => return (0, true),
            },
            None if self
                .registry
                .has_client(&self.ledger, &row.ctx.client_order_id) =>
            {
                return (0, true);
            }
            None => (0, false),
        };
        match row
            .ctx
            .qty
            .checked_sub(other_attributed)
            .and_then(|capacity| capacity.checked_sub(row.ctx.attributed_fill_qty))
        {
            Some(qty) => (qty, other_debt),
            None => (0, true),
        }
    }

    pub fn row_working_unresolvable<C: CoverageClient>(&self, row: &LiveOmsOrder<R>) -> bool {
        let cid = C::from_string(row.ctx.client_order_id.0.clone());
        if row.venue_working_centi.is_some()
            || cid
                .and_then(|c| self.slot_projection_remaining(c.as_str()))
                .is_some()
        {
            false
        } else {
            self.row_working_fallback(row).1
        }
    }

    pub fn slot_projection_remaining(&self, client: &str) -> Option<u64> {
        let cid = OmsClientOrderId(client.to_string());
        match self.registry.member_working_view(&self.ledger, &cid) {
            WorkingView::Known(r) => Some(r),
            WorkingView::Unresolved => None,
        }
    }

    pub fn cover_wire_keys(&self) -> BTreeMap<String, String> {
        let mut lineage_wire = BTreeMap::<String, String>::new();
        for row in self.by_client.values() {
            let Some(wire) = row.ctx.venue_order_id.as_ref() else {
                continue;
            };
            let Some(lineage) = self
                .registry
                .get(&self.ledger, wire)
                .and_then(|ws| ws.amend_lineage.as_ref())
            else {
                continue;
            };
            lineage_wire.insert(lineage.old_cid.0.clone(), wire.0.clone());
            lineage_wire.insert(lineage.new_cid.0.clone(), wire.0.clone());
        }
        self.by_client
            .iter()
            .map(|(cid, row)| {
                let key = match row
                    .ctx
                    .venue_order_id
                    .as_ref()
                    .map(|w| w.0.clone())
                    .or_else(|| lineage_wire.get(cid).cloned())
                {
                    Some(w) => format!("w:{w}"),
                    None => format!("c:{cid}"),
                };
                (cid.clone(), key)
            })
            .collect()
    }

    pub fn cover_superseded_cids(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::<String>::new();
        for row in self.by_client.values() {
            let Some(wire) = row.ctx.venue_order_id.as_ref() else {
                continue;
            };
            let Some(lineage) = self
                .registry
                .get(&self.ledger, wire)
                .and_then(|ws| ws.amend_lineage.as_ref())
            else {
                continue;
            };
            let new_bound = self
                .by_client
                .get(&lineage.new_cid.0)
                .and_then(|n| n.ctx.venue_order_id.as_ref())
                .is_some_and(|w| w == wire);
            if new_bound {
                out.insert(lineage.old_cid.0.clone());
            }
        }
        out
    }
}

pub fn live_reducing_row_qty<C: CoverageClient, R, Stage>(
    oms: &LiveOmsBookState<R, Stage>,
    row: &LiveOmsOrder<R>,
    net: i64,
) -> Option<u64> {
    if row.state.is_terminal_or_halt() || row.state.is_submission_closed() {
        return None;
    }
    let side = match row.ctx.side {
        crate::lifecycle::Side::BuyYes => Side::BuyYes,
        crate::lifecycle::Side::SellYes => Side::SellYes,
    };
    if !reducing_side(side, net) {
        return None;
    }
    if let Some(g) = &row.cancel_gone {
        return Some(
            g.venue_filled_centi
                .saturating_sub(oms.wire_held_inventory_centi(&g.wire).unwrap_or(0)),
        );
    }
    Some(oms.row_working_centi::<C>(row))
}

pub fn live_reducing_row_unresolvable<C: CoverageClient, R, Stage>(
    oms: &LiveOmsBookState<R, Stage>,
    row: &LiveOmsOrder<R>,
    net: i64,
) -> bool {
    if row.state.is_terminal_or_halt() || row.state.is_submission_closed() {
        return false;
    }
    let side = match row.ctx.side {
        crate::lifecycle::Side::BuyYes => Side::BuyYes,
        crate::lifecycle::Side::SellYes => Side::SellYes,
    };
    if !reducing_side(side, net) || row.cancel_gone.is_some() {
        return false;
    }
    oms.row_working_unresolvable::<C>(row)
}

pub fn reducing_cover_upper<C: CoverageClient, R, Stage>(
    oms: &LiveOmsBookState<R, Stage>,
    net: i64,
) -> Option<u64> {
    let key_of = oms.cover_wire_keys();
    let mut per_wire = std::collections::BTreeMap::<String, u64>::new();
    for row in oms.by_client.values() {
        let Some(q) = live_reducing_row_qty::<C, _, _>(oms, row, net) else {
            continue;
        };
        if live_reducing_row_unresolvable::<C, _, _>(oms, row, net) {
            return None;
        }
        let key = key_of
            .get(&row.ctx.client_order_id.0)
            .cloned()
            .unwrap_or_else(|| format!("c:{}", row.ctx.client_order_id.0));
        per_wire
            .entry(key)
            .and_modify(|c| *c = (*c).max(q))
            .or_insert(q);
    }
    Some(per_wire.values().sum())
}

pub fn row_cancelling<R>(row: &LiveOmsOrder<R>) -> bool {
    matches!(
        row.state,
        crate::lifecycle::OrderState::CancelPending { .. }
            | crate::lifecycle::OrderState::ReconcilePending { .. }
    )
}

pub struct WireCover<C> {
    pub key_of: std::collections::BTreeMap<String, String>,
    pub contrib: std::collections::BTreeMap<String, u64>,
    client: std::marker::PhantomData<C>,
}

impl<C: CoverageClient> WireCover<C> {
    pub fn new<R, Stage>(
        oms: &LiveOmsBookState<R, Stage>,
        net: i64,
        zero_cancelling: bool,
    ) -> Self {
        let key_of = oms.cover_wire_keys();
        let superseded = oms.cover_superseded_cids();
        let mut contrib = std::collections::BTreeMap::<String, u64>::new();
        for row in oms.by_client.values() {
            if superseded.contains(&row.ctx.client_order_id.0) {
                continue;
            }
            let Some(q) = live_reducing_row_qty::<C, _, _>(oms, row, net) else {
                continue;
            };
            let q = if live_reducing_row_unresolvable::<C, _, _>(oms, row, net) {
                0
            } else {
                q
            };
            let q = if zero_cancelling && row_cancelling(row) {
                0
            } else {
                q
            };
            let key = key_of
                .get(&row.ctx.client_order_id.0)
                .cloned()
                .unwrap_or_else(|| format!("c:{}", row.ctx.client_order_id.0));
            contrib
                .entry(key)
                .and_modify(|c| *c = (*c).min(q))
                .or_insert(q);
        }
        Self {
            key_of,
            contrib,
            client: std::marker::PhantomData,
        }
    }

    pub fn total(&self) -> u64 {
        self.contrib.values().sum()
    }

    pub fn key(&self, cid: &C) -> Option<&String> {
        self.key_of
            .get(cid.as_str())
            .filter(|k| self.contrib.contains_key(*k))
    }

    pub fn cancelled<'a>(&self, cids: impl Iterator<Item = &'a C>) -> u64
    where
        C: 'a,
    {
        let mut seen = BTreeSet::<&String>::new();
        let mut sum = 0u64;
        for cid in cids {
            if let Some(k) = self.key(cid) {
                if seen.insert(k) {
                    sum += self.contrib[k];
                }
            }
        }
        sum
    }
}
