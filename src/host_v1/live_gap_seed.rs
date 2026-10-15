//! Channel-gap obligations, seed admission and role projections.
use super::live_admission::AdmissionClient;
use super::live_ingest::ExecutionTrace;
use super::live_state::LiveOmsBookState;
use super::live_transaction::TransactionIo;
use super::risk::reducing_side;
use crate::execution::{ExecutionEvidence, ExecutionSource};
use crate::lifecycle::{JournalRecord, OrderState, Side, VenueOrderId};
use crate::observation::AmendPhase;
use anyhow::Result;
use std::collections::BTreeSet;

pub trait SeedAdmission {
    type Row;
    type Error;
    fn admit_seed_row(
        raw: &Self::Row,
        expected_subaccount: i64,
        market: &str,
        source: ExecutionSource,
    ) -> std::result::Result<ExecutionEvidence, Self::Error>;
}
/// Typed calls to the existing pure strategy classifier; no host state is copied.
pub trait RoleClient<R>: AdmissionClient<R> {
    type Tag: Copy;
    fn tag_for_bot(value: &str, bot: &str) -> Self::Tag;
    fn effective_venue_role(tag: Self::Tag, net_known: bool, reduces: bool) -> (Self::Tag, bool);
    fn conservative_role(tag: Self::Tag) -> R;
    fn tag_is_entry(tag: Self::Tag) -> bool;
    fn is_flatten_role(role: &R) -> bool;
}

impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn note_user_orders_channel_gap(&mut self, journal: &mut dyn TransactionIo) -> Result<()> {
        self.public_fields.user_orders_sid = None;
        self.public_fields.user_orders_resubscribe_required = true;
        let wires: BTreeSet<VenueOrderId> = self
            .by_client
            .values()
            .filter(|row| {
                !matches!(
                    row.state,
                    OrderState::Filled | OrderState::Canceled | OrderState::Terminal
                )
            })
            .filter_map(|row| row.ctx.venue_order_id.clone())
            .collect();
        for wire in wires {
            if self
                .public_fields
                .registry
                .get(&self.ledger, &wire)
                .and_then(|state| state.amend_lineage.as_ref())
                .is_some_and(|lineage| lineage.phase == AmendPhase::AppliedNeedsObservation)
            {
                self.advance_amend_lineage(&wire.0, AmendPhase::RestDue, None, journal)?;
            }
            self.commit_registry_record(
                JournalRecord::OrderAuthorityHold {
                    ledger: self.ledger.clone(),
                    wire: wire.clone(),
                },
                journal,
            )?;
            self.pending_wires.insert(wire);
        }
        Ok(())
    }

    pub fn d3_cancel_none_member(&self, wire: &VenueOrderId) -> bool {
        self.d3_cancel_none.contains(wire)
            && self.by_client.values().any(|o| {
                o.ctx.venue_order_id.as_ref() == Some(wire) && !o.state.is_terminal_or_halt()
            })
    }

    pub fn d3_member_wires(&self) -> Vec<String> {
        let mut wires = BTreeSet::new();
        for row in self.by_client.values() {
            let Some(wire) = row.ctx.venue_order_id.as_ref() else {
                continue;
            };
            if self.wire_known_deficit(wire) || self.d3_cancel_none_member(wire) {
                wires.insert(wire.0.clone());
            }
        }
        wires.into_iter().collect()
    }

    pub fn apply_g2_seed_rows<C: AdmissionClient<R> + SeedAdmission>(
        &mut self,
        cutoff: i64,
        created_ts: i64,
        rows: &[C::Row],
        walked: bool,
        market: &str,
        journal: &mut dyn TransactionIo,
    ) -> Result<bool>
    where
        Stage: ExecutionTrace<R>,
    {
        self.public_fields.g2_proven = false;
        if created_ts < cutoff || !walked {
            return Ok(false);
        }
        self.ensure_ledger(market);
        for raw in rows {
            match C::admit_seed_row(
                raw,
                self.public_fields.expected_subaccount,
                market,
                ExecutionSource::VenueSeed,
            ) {
                Ok(ev) => {
                    if let Err(e) = self.ingest_execution::<C>(ev, journal) {
                        self.public_fields.g2_proven = false;
                        return Err(e);
                    }
                }
                Err(_) => {
                    self.public_fields.g2_proven = false;
                    return Ok(false);
                }
            }
        }
        self.public_fields.g2_proven = true;
        Ok(true)
    }

    pub fn apply_venue_cid_role<C: RoleClient<R>>(
        &mut self,
        client: &C,
        bot_id: &str,
        venue_net_centi: Option<i64>,
    ) -> (C::Tag, bool) {
        let tag = C::tag_for_bot(client.as_str(), bot_id);
        let mut stale_exit_on_increasing_side = false;
        if let Some(row) = self.by_client.get_mut(client.as_str()) {
            let reduces = venue_net_centi.is_some_and(|net| reducing_side(row.ctx.side, net));
            let (effective, stale) =
                C::effective_venue_role(tag, venue_net_centi.is_some(), reduces);
            stale_exit_on_increasing_side = stale;
            row.role = C::conservative_role(effective);
            row.reduce_only = !C::tag_is_entry(effective);
        }
        (tag, stale_exit_on_increasing_side)
    }

    pub fn outstanding_flatten_fill_rows<C: RoleClient<R>>(&self) -> Vec<(C, Option<String>)> {
        self.by_client
            .values()
            .filter(|o| {
                C::is_flatten_role(&o.role) && o.ctx.attributed_fill_qty < o.ctx.fill_obligation
            })
            .filter_map(|o| {
                C::from_string(o.ctx.client_order_id.as_str().to_string())
                    .map(|c| (c, o.ctx.venue_order_id.as_ref().map(|v| v.0.clone())))
            })
            .collect()
    }

    pub fn unbooked_flatten_reduce_centi<C: RoleClient<R>>(&self, net_centi: i64) -> u64 {
        self.by_client
            .values()
            .filter(|o| C::is_flatten_role(&o.role))
            .filter(|o| {
                reducing_side(
                    match o.ctx.side {
                        Side::BuyYes => Side::BuyYes,
                        Side::SellYes => Side::SellYes,
                    },
                    net_centi,
                )
            })
            .fold(0u64, |sum, o| {
                sum.saturating_add(
                    o.ctx
                        .fill_obligation
                        .saturating_sub(o.ctx.attributed_fill_qty),
                )
            })
    }

    pub fn outstanding_fill_obligation_rows(&self) -> usize {
        self.by_client
            .values()
            .filter(|o| o.ctx.attributed_fill_qty < o.ctx.fill_obligation)
            .count()
    }
}
