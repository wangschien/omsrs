//! Order-command preparation over original borrowed command vectors.
use super::close_proof::OmsPriceE4;
use super::journal_projection::resting_wire_for_open_projection;
use super::journal_projection::{
    amend_wire_has_unbooked_evidence, amend_wire_unbooked_user_order_only,
};
use super::live_admission::{AdmissionAmend, AdmissionIntent};
use super::live_cover::CoverageClient;
use super::live_lifecycle::SubmissionIntent;
use super::live_state::LiveOmsBookState;
use super::session_recovery::{AmendDowngrades, SessionRecoveryFields};
use crate::lifecycle::AttemptId;
use crate::lifecycle::{OrderState, Side};
use anyhow::{Context, Result};

pub trait AmendInput<C>: AdmissionAmend<C> + Clone {
    type Intent;
    fn to_order_intent(&self) -> Self::Intent;
    fn market_ticker(&self) -> &String;
    fn price(&self) -> rust_decimal::Decimal;
    fn new_client_order_id(&self) -> &C;
}
pub trait DecisionRead<C, R> {
    type Intent: SubmissionIntent<C, R>;
    type Amend: AmendInput<C, Intent = Self::Intent>;
    fn intents(&self) -> &[Self::Intent];
    fn amends(&self) -> &[Self::Amend];
    fn cancels(&self) -> &[C];
    fn decreases(&self) -> &[(C, u64)];
    fn cancel_all(&self) -> bool;
}
pub struct PreparedMutation<'a, C, I, A> {
    pub intents: &'a mut Vec<I>,
    pub amends: &'a mut Vec<A>,
    pub cancel_client_order_ids: &'a mut Vec<C>,
    pub decrease_to: &'a mut Vec<(C, u64)>,
}
pub trait DecisionWrite<C, R>: DecisionRead<C, R> {
    fn mutable(&mut self) -> PreparedMutation<'_, C, Self::Intent, Self::Amend>;
    fn amend_downgrade_trace(before: AmendDowngrades, now: AmendDowngrades);
}

pub fn validate_amend_decision<C: CoverageClient + std::fmt::Display, R, D: DecisionRead<C, R>>(
    decision: &D,
) -> Result<()> {
    if decision.amends().is_empty() {
        return Ok(());
    }
    if decision.cancel_all() {
        anyhow::bail!("amend decision conflicts with cancel_all");
    }

    let mut old_cids = std::collections::HashSet::new();
    let mut new_cids = std::collections::HashSet::new();
    for amend in decision.amends() {
        let old = amend.old_client_order_id().as_str().to_owned();
        let new = amend.new_client_order_id().as_str().to_owned();
        if old == new {
            anyhow::bail!("amend old and new client ids are identical: {old}");
        }
        if !old_cids.insert(old.clone()) {
            anyhow::bail!("multiple amends target old client id {old}");
        }
        if !new_cids.insert(new.clone()) {
            anyhow::bail!("multiple amends create new client id {new}");
        }
    }
    if let Some(cid) = old_cids.intersection(&new_cids).next() {
        anyhow::bail!("amend chain in one decision is forbidden at client id {cid}");
    }

    let conflicts = |cid: &C| old_cids.contains(cid.as_str()) || new_cids.contains(cid.as_str());
    if let Some(cid) = decision.cancels().iter().find(|cid| conflicts(cid)) {
        anyhow::bail!("amend conflicts with explicit cancel for client id {cid}");
    }
    if let Some((cid, _)) = decision.decreases().iter().find(|(cid, _)| conflicts(cid)) {
        anyhow::bail!("amend conflicts with decrease for client id {cid}");
    }
    if let Some(intent) = decision
        .intents()
        .iter()
        .find(|intent| conflicts(intent.client_order_id()))
    {
        anyhow::bail!(
            "amend conflicts with create intent for client id {}",
            intent.client_order_id()
        );
    }
    Ok(())
}

pub fn append_omitted_decreases<C: Clone + PartialEq>(
    original: &[(C, u64)],
    prepared: &[(C, u64)],
    not_attempted: &mut Vec<C>,
) {
    for (cid, _) in original {
        if prepared.iter().any(|(c, _)| c == cid) {
            continue;
        }
        if not_attempted.iter().any(|c| c == cid) {
            continue;
        }
        not_attempted.push(cid.clone());
    }
}
impl<C: CoverageClient + Clone + Eq + std::hash::Hash> SessionRecoveryFields<C> {
    pub fn filter_prepare_amends<R, Stage, P: DecisionWrite<C, R>>(
        &mut self,
        oms: &LiveOmsBookState<R, Stage>,
        prepared: &mut P,
    ) {
        let mut index = 0;
        let before = self.amend_downgrades;
        while index < prepared.amends().len() {
            let pending = self.amend_prepare_pending(oms, &prepared.amends()[index]);
            let amendable =
                !pending && self.amend_prepare_amendable(oms, &prepared.amends()[index]);
            if amendable
                && amend_wire_has_unbooked_evidence(
                    oms,
                    prepared.amends()[index].old_client_order_id().as_str(),
                )
            {
                if amend_wire_unbooked_user_order_only(
                    oms,
                    prepared.amends()[index].old_client_order_id().as_str(),
                ) {
                    self.unbooked_fill_dropped = true;
                }
                prepared.mutable().amends.remove(index);
                self.amend_downgrades.unbooked_drop += 1;
                continue;
            }
            if pending || !amendable {
                let amend = prepared.amends()[index].clone();
                let old = amend.old_client_order_id().clone();
                let decision = prepared.mutable();
                decision.amends.remove(index);
                if pending {
                    self.amend_downgrades.pending_drop += 1;
                    continue;
                }
                if self.filled_same_price_reduction(oms, &amend) {
                    decision.decrease_to.push((old, amend.qty_centi()));
                    self.amend_downgrades.to_decrease += 1;
                } else {
                    decision.cancel_client_order_ids.push(old);
                    self.amend_downgrades.to_cancel += 1;
                }
            } else {
                index += 1;
            }
        }
        let now = self.amend_downgrades;
        if now != before {
            P::amend_downgrade_trace(before, now);
        }
    }

    pub fn amend_prepare_pending<R, Stage, A: AmendInput<C>>(
        &self,
        oms: &LiveOmsBookState<R, Stage>,
        am: &A,
    ) -> bool {
        oms.by_client
            .get(am.old_client_order_id().as_str())
            .and_then(|o| o.ctx.venue_order_id.as_ref())
            .and_then(|wire| oms.public_fields.registry.get(&oms.ledger, wire))
            .is_some_and(|ws| ws.amend_lineage.is_some())
    }

    pub fn amend_prepare_amendable<R, Stage, A: AmendInput<C>>(
        &self,
        oms: &LiveOmsBookState<R, Stage>,
        am: &A,
    ) -> bool {
        oms.amend_prepare_amendable(am.old_client_order_id().as_str())
    }

    pub fn filled_same_price_reduction<R, Stage, A: AmendInput<C>>(
        &self,
        oms: &LiveOmsBookState<R, Stage>,
        am: &A,
    ) -> bool {
        let Some(order) = oms.by_client.get(am.old_client_order_id().as_str()) else {
            return false;
        };
        if order.ctx.attributed_fill_qty == 0 || am.qty_centi() == 0 {
            return false;
        }
        let resting = order.ctx.qty.saturating_sub(order.ctx.attributed_fill_qty);
        let same_price = OmsPriceE4::from_dollars(am.price())
            .is_some_and(|px| px.into_wire_price_cents() == order.ctx.price_cents);
        same_price && am.qty_centi() < resting
    }

    pub fn filter_prepare_decreases<R, Stage, P: DecisionWrite<C, R>>(
        &self,
        oms: &LiveOmsBookState<R, Stage>,
        prepared: &mut P,
        now_ns: u64,
    ) {
        let drop_any = prepared
            .decreases()
            .iter()
            .any(|entry| !self.keep_prepare_decrease(oms, &entry.0, &entry.1, now_ns));
        if drop_any {
            prepared
                .mutable()
                .decrease_to
                .retain(|entry| self.keep_prepare_decrease(oms, &entry.0, &entry.1, now_ns));
        }
    }

    pub fn filter_prepare_growing_amends_behind_cancels<R, Stage, P: DecisionWrite<C, R>>(
        &self,
        oms: &LiveOmsBookState<R, Stage>,
        prepared: &mut P,
    ) {
        let net = oms.net_yes_centi();
        if net == 0 || prepared.amends().is_empty() {
            return;
        }
        let reducing = if net > 0 { Side::SellYes } else { Side::BuyYes };
        let cancels: Vec<C> = prepared.cancels().to_vec();
        let defer = |am: &P::Amend| -> bool {
            let Some(old) = oms.by_client.get(am.old_client_order_id().as_str()) else {
                return false;
            };
            if old.ctx.side != reducing {
                return false;
            }
            let current = old.ctx.qty.saturating_sub(old.ctx.attributed_fill_qty);
            if am.qty_centi() <= current {
                return false;
            }
            let cancel_this_decision = cancels.iter().any(|cid| {
                cid != am.old_client_order_id()
                    && oms.by_client.get(cid.as_str()).is_some_and(|row| {
                        row.ctx.side == reducing
                            && !row.state.is_terminal_or_halt()
                            && !row.state.is_submission_closed()
                    })
            });
            let cancel_pending = oms.by_client.values().any(|row| {
                row.ctx.side == reducing
                    && row.ctx.client_order_id.as_str() != am.old_client_order_id().as_str()
                    && matches!(
                        row.state,
                        OrderState::CancelPending { .. } | OrderState::ReconcilePending { .. }
                    )
            });
            cancel_this_decision || cancel_pending
        };
        if prepared.amends().iter().any(|am| defer(am)) {
            prepared.mutable().amends.retain(|am| !defer(am));
        }
    }

    pub fn filter_prepare_pending_creates<R, Stage, P: DecisionWrite<C, R>>(
        &self,
        oms: &LiveOmsBookState<R, Stage>,
        prepared: &mut P,
    ) {
        let drop_any = prepared
            .intents()
            .iter()
            .any(|intent| !self.keep_prepare_pending_create(oms, intent));
        if drop_any {
            prepared
                .mutable()
                .intents
                .retain(|intent| self.keep_prepare_pending_create(oms, intent));
        }
    }

    pub fn keep_prepare_pending_create<R, Stage, I: SubmissionIntent<C, R>>(
        &self,
        oms: &LiveOmsBookState<R, Stage>,
        intent: &I,
    ) -> bool {
        let side = match intent.side() {
            Side::BuyYes => Side::BuyYes,
            Side::SellYes => Side::SellYes,
        };
        !oms.by_client.values().any(|row| {
            row.ctx.market == *intent.market_ticker()
                && row.ctx.side == side
                && row.ctx.venue_order_id.as_ref().is_some_and(|wire| {
                    oms.public_fields
                        .registry
                        .get(&oms.ledger, wire)
                        .is_some_and(|state| state.amend_lineage.is_some())
                })
        })
    }
}

impl<C: CoverageClient + Clone + Eq + std::hash::Hash> SessionRecoveryFields<C> {
    pub fn filter_prepare_twin_cross<R, Stage, P: DecisionWrite<C, R>>(
        &mut self,
        oms: &LiveOmsBookState<R, Stage>,
        prepared: &mut P,
    ) {
        let mut acted_cids = std::collections::HashSet::new();
        acted_cids.extend(prepared.cancels().iter().cloned());
        acted_cids.extend(prepared.decreases().iter().map(|(cid, _)| cid.clone()));
        acted_cids.extend(
            prepared
                .amends()
                .iter()
                .map(|am| am.old_client_order_id().clone()),
        );
        if prepared.cancel_all() {
            acted_cids.extend(oms.by_client.values().filter_map(|row| {
                (!row.state.is_terminal_or_halt() && !row.state.is_submission_closed())
                    .then(|| C::try_from_string(row.ctx.client_order_id.as_str().to_owned()).ok())
                    .flatten()
            }));
        }
        let mut acted_quotes = Vec::new();
        for cid in acted_cids {
            if let Some(row) = oms.by_client.get(cid.as_str()) {
                let side = match row.ctx.side {
                    Side::BuyYes => Side::BuyYes,
                    Side::SellYes => Side::SellYes,
                };
                acted_quotes.push((
                    row.ctx.market.clone(),
                    side,
                    OmsPriceE4::from_wire_price_cents(row.ctx.price_cents).to_dollars(),
                ));
            }
        }
        for am in prepared.amends() {
            acted_quotes.push((am.market_ticker().clone(), am.side(), am.price()));
        }
        let conflicts_twin_cross = |intent: &P::Intent| {
            let conflicts = acted_quotes.iter().any(|(market, side, price)| {
                if *market != *intent.market_ticker() {
                    return false;
                }
                if *side == intent.side() {
                    return true;
                }
                match intent.side() {
                    Side::BuyYes => intent.price() >= *price,
                    Side::SellYes => *price >= intent.price(),
                }
            });
            conflicts
        };
        let mut filtered_twin_cross = 0u64;
        if prepared.intents().iter().any(&conflicts_twin_cross) {
            prepared.mutable().intents.retain(|intent| {
                let conflicts = conflicts_twin_cross(intent);
                if conflicts {
                    filtered_twin_cross = filtered_twin_cross.saturating_add(1);
                }
                !conflicts
            });
        }
        self.create_filtered_twin_cross = self
            .create_filtered_twin_cross
            .saturating_add(filtered_twin_cross);
    }
}
impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn prepare_write_rows<
        C: super::live_admission::AdmissionClient<R>,
        D: DecisionRead<C, R>,
    >(
        &mut self,
        decision: &D,
        now_ns: u64,
        strategy: &str,
        journal: &mut dyn super::live_transaction::TransactionIo,
    ) -> Result<()> {
        for intent in decision.intents() {
            let attempt = AttemptId(format!("{}@{}", intent.client_order_id().as_str(), now_ns));
            self.begin_submit(intent, strategy, attempt, journal)?;
        }
        let presend_amends = (|| -> Result<()> {
            for am in decision.amends() {
                let wire = self
                    .by_client
                    .get(am.old_client_order_id().as_str())
                    .and_then(|o| o.ctx.venue_order_id.as_ref())
                    .ok_or_else(|| anyhow::anyhow!("amend old CID has no bound wire"))?
                    .0
                    .clone();
                self.stage_amend_lineage_for_presend_group(
                    am.old_client_order_id().as_str(),
                    am.new_client_order_id().as_str(),
                    &wire,
                    am.qty_centi(),
                    journal,
                )?;
                let attempt =
                    AttemptId(format!("{}@{}", am.new_client_order_id().as_str(), now_ns));
                self.begin_submit(&am.to_order_intent(), strategy, attempt, journal)?;
            }
            if !decision.amends().is_empty() {
                journal.commit_now()?;
            }
            Ok(())
        })();
        if let Err(error) = presend_amends {
            self.stopped = true;
            return Err(error).context("pre-send amend group stopped before HTTP");
        }
        for cid in decision.cancels() {
            if self
                .by_client
                .get(cid.as_str())
                .is_some_and(|o| !o.state.is_terminal_or_halt() && !o.state.is_submission_closed())
            {
                self.on_cancel_requested(cid, journal)?;
            }
        }
        if decision.cancel_all() {
            let resting: Vec<C> = self
                .by_client
                .values()
                .filter(|o| resting_wire_for_open_projection(&o.state).is_some())
                .filter_map(|o| C::try_from_string(o.ctx.client_order_id.as_str().to_owned()).ok())
                .collect();
            for cid in resting {
                let _ = self.on_cancel_requested(&cid, journal);
            }
        }

        Ok(())
    }
}
