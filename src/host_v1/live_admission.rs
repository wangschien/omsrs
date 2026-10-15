//! Admission and cover enforcement over borrowed caller commands.
use super::compat_admission::{AdmissionReasons, B4Reason};
use super::live_cover::{
    CoverageClient, WireCover, live_reducing_row_qty, live_reducing_row_unresolvable,
    row_cancelling,
};
use super::live_state::LiveOmsBookState;
use super::risk::{
    AdmissionAction, AdmissionFacts, GuardReason, exit_reduce_action, reducing_side,
};
use crate::lifecycle::Side;
use anyhow::Result;
use std::collections::BTreeSet;

pub trait AdmissionClient<R>: CoverageClient + Clone + Eq {
    fn permits_entry_cancel(&self, role: &R) -> bool;
    fn is_exit_tag(value: &str) -> bool;
    fn is_entry_tag(value: &str) -> bool;
    fn restored_role(value: &str) -> (R, bool);
}
pub trait AdmissionIntent<C> {
    fn side(&self) -> Side;
    fn qty_centi(&self) -> u64;
    fn reduce_only(&self) -> bool;
    fn client_order_id(&self) -> &C;
}
pub trait AdmissionAmend<C> {
    fn side(&self) -> Side;
    fn qty_centi(&self) -> u64;
    fn reduce_only(&self) -> bool;
    fn old_client_order_id(&self) -> &C;
}
/// Mutable borrows of the original command vectors; no order/policy mirror.
pub struct Decision<'a, C, I, A> {
    pub intents: &'a mut Vec<I>,
    pub amends: &'a mut Vec<A>,
    pub cancel_client_order_ids: &'a mut Vec<C>,
    pub decrease_to: &'a mut Vec<(C, u64)>,
    pub cancel_all: &'a mut bool,
    pub net_centi: i64,
}
pub struct ReadDecision<'a, C, I, A> {
    pub intents: &'a [I],
    pub amends: &'a [A],
    pub cancel_client_order_ids: &'a [C],
    pub net_centi: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExitAdmitNote {
    pub invariant_refused: bool,
    pub pair_withheld: bool,
    pub pair_admitted_under_guard: bool,
    pub surplus_cancel_admitted: bool,
    pub decrease_admitted: bool,
}

pub fn owned_entry<C: AdmissionClient<R>, R, Stage>(
    oms: &LiveOmsBookState<R, Stage>,
    cid: &C,
) -> bool {
    let Some(row) = super::risk::entry_identity_row(oms, cid.as_str()) else {
        return false;
    };
    if !cid.permits_entry_cancel(&row.role) {
        return false;
    }
    super::risk::entry_matches_net(row.ctx.side, oms.net_yes_centi())
}

pub fn admit_decision_with_b4_proven_cancels<
    C: AdmissionClient<R>,
    I: AdmissionIntent<C>,
    A: AdmissionAmend<C>,
    R,
    Stage,
>(
    decision: &mut Decision<'_, C, I, A>,
    oms: &LiveOmsBookState<R, Stage>,
    reasons: &AdmissionReasons,
    facts: AdmissionFacts,
    b4_proven_cancels: &[C],
) -> Result<()> {
    if reasons.is_empty() {
        return Ok(());
    }
    let admitted_cancel_entry = |cid: &C| {
        if reasons.b4.contains(&B4Reason::AuthorityHold) {
            b4_proven_cancels.contains(cid)
        } else {
            owned_entry(oms, cid)
        }
    };
    let net = decision.net_centi;
    decision.intents.retain(|intent| {
        reasons.admits(
            exit_reduce_action(intent.side(), intent.qty_centi(), net, intent.reduce_only()),
            facts,
        )
    });
    let mut demoted = Vec::new();
    decision.amends.retain(|amend| {
        let action = exit_reduce_action(amend.side(), amend.qty_centi(), net, amend.reduce_only());
        if reasons.admits(action, facts) {
            true
        } else {
            if action == AdmissionAction::New && owned_entry(oms, amend.old_client_order_id()) {
                demoted.push(amend.old_client_order_id().clone());
            }
            false
        }
    });
    decision.cancel_client_order_ids.extend(demoted);
    decision.decrease_to.retain(|(cid, _)| {
        owned_entry(oms, cid) && reasons.admits(AdmissionAction::CancelEntry, facts)
    });
    if *decision.cancel_all {
        *decision.cancel_all = false;
        if reasons.admits(AdmissionAction::CancelEntry, facts) {
            for row in oms.by_client.values() {
                let Some(cid) = C::from_string(row.ctx.client_order_id.0.clone()) else {
                    continue;
                };
                if admitted_cancel_entry(&cid) {
                    decision.cancel_client_order_ids.push(cid);
                }
            }
        }
    }
    decision.cancel_client_order_ids.retain(|cid| {
        admitted_cancel_entry(cid) && reasons.admits(AdmissionAction::CancelEntry, facts)
    });
    let mut seen = BTreeSet::new();
    decision
        .cancel_client_order_ids
        .retain(|cid| seen.insert(cid.as_str().to_owned()));
    Ok(())
}

pub fn exit_cover_withheld_cancels<
    C: AdmissionClient<R>,
    I: AdmissionIntent<C>,
    A: AdmissionAmend<C>,
    R,
    Stage,
>(
    decision: &ReadDecision<'_, C, I, A>,
    oms: &LiveOmsBookState<R, Stage>,
    net: i64,
) -> Vec<C> {
    let net_abs = net.unsigned_abs();
    if net == 0 || decision.cancel_client_order_ids.is_empty() {
        return Vec::new();
    }
    let wires = WireCover::<C>::new(oms, net, true);
    let mut coverage: i128 = i128::from(wires.total());
    coverage += decision
        .intents
        .iter()
        .filter(|i| i.reduce_only() && reducing_side(i.side(), net))
        .map(|i| i128::from(i.qty_centi()))
        .sum::<i128>();
    for a in decision.amends.iter() {
        if a.reduce_only() && reducing_side(a.side(), net) {
            let current = oms
                .by_client
                .get(a.old_client_order_id().as_str())
                .and_then(|row| live_reducing_row_qty::<C, _, _>(oms, row, net))
                .unwrap_or(0);
            coverage += i128::from(a.qty_centi()) - i128::from(current);
        }
    }
    let mut withheld = Vec::new();
    let mut removed = BTreeSet::<String>::new();
    for cid in decision.cancel_client_order_ids.iter() {
        let is_cover = oms
            .by_client
            .get(cid.as_str())
            .filter(|row| !row_cancelling(row))
            .and_then(|row| live_reducing_row_qty::<C, _, _>(oms, row, net))
            .is_some();
        let Some(key) = wires.key(cid).filter(|_| is_cover) else {
            continue;
        };
        let q = wires.contrib[key];
        if removed.contains(key) {
            continue;
        }
        if coverage - i128::from(q) < i128::from(net_abs) {
            withheld.push(cid.clone());
        } else {
            coverage -= i128::from(q);
            removed.insert(key.clone());
        }
    }
    withheld
}

pub fn cancel_increasing_exit_rows_while_unready<
    C: AdmissionClient<R>,
    I: AdmissionIntent<C>,
    A: AdmissionAmend<C>,
    R,
    Stage,
>(
    decision: &mut Decision<'_, C, I, A>,
    oms: &LiveOmsBookState<R, Stage>,
    net: i64,
) -> Vec<C> {
    if net == 0 {
        return Vec::new();
    }
    let mut added = Vec::new();
    for row in oms.by_client.values() {
        if !matches!(
            row.state,
            crate::lifecycle::OrderState::Accepted { .. }
                | crate::lifecycle::OrderState::Partial { .. }
                | crate::lifecycle::OrderState::ImmediateFillUnattributed { .. }
        ) {
            continue;
        }
        let side = match row.ctx.side {
            crate::lifecycle::Side::BuyYes => Side::BuyYes,
            crate::lifecycle::Side::SellYes => Side::SellYes,
        };
        let increasing = !reducing_side(side, net);
        if !increasing || !C::is_exit_tag(&row.ctx.client_order_id.0) {
            continue;
        }
        let Some(cid) = C::from_string(row.ctx.client_order_id.0.clone()) else {
            continue;
        };
        if !decision.cancel_client_order_ids.contains(&cid) {
            decision.cancel_client_order_ids.push(cid.clone());
            added.push(cid);
        }
    }
    added
}

pub fn admit_decision_exit_aware<
    C: AdmissionClient<R>,
    I: AdmissionIntent<C>,
    A: AdmissionAmend<C>,
    R,
    Stage,
>(
    decision: &mut Decision<'_, C, I, A>,
    oms: &LiveOmsBookState<R, Stage>,
    reasons: &AdmissionReasons,
    facts: AdmissionFacts,
    b4_proven_cancels: &[C],
) -> Result<ExitAdmitNote> {
    let mut note = ExitAdmitNote::default();
    let retries: Vec<C> = decision
        .cancel_client_order_ids
        .iter()
        .filter(|cid| oms.by_client.get(cid.as_str()).is_some_and(row_cancelling))
        .cloned()
        .collect();
    let net = decision.net_centi;
    let net_abs = net.unsigned_abs();
    let working: u64 = oms
        .by_client
        .values()
        .filter_map(|row| live_reducing_row_qty::<C, _, _>(oms, row, net))
        .sum();
    let upper_unresolvable = oms
        .by_client
        .values()
        .any(|row| live_reducing_row_unresolvable::<C, _, _>(oms, row, net));
    let cancelled: u64 = decision
        .cancel_client_order_ids
        .iter()
        .filter_map(|cid| {
            let row = oms.by_client.get(cid.as_str())?;
            live_reducing_row_qty::<C, _, _>(oms, row, net)
        })
        .sum();
    let is_exit_create = |i: &I| i.reduce_only() && reducing_side(i.side(), net);
    let mut created: u64 = decision
        .intents
        .iter()
        .filter(|i| is_exit_create(i))
        .map(|i| i.qty_centi())
        .sum();
    let amend_delta = |a: &A| -> i128 {
        if !(a.reduce_only() && reducing_side(a.side(), net)) {
            return 0;
        }
        let current = oms
            .by_client
            .get(a.old_client_order_id().as_str())
            .and_then(|row| live_reducing_row_qty::<C, _, _>(oms, row, net))
            .unwrap_or(0);
        i128::from(a.qty_centi()) - i128::from(current)
    };
    let amend_net_delta = |amends: &[A]| -> i128 { amends.iter().map(|a| amend_delta(a)).sum() };
    let decrease_reduction = |d: &Decision<'_, C, I, A>| -> u64 {
        d.decrease_to
            .iter()
            .filter_map(|(cid, to)| {
                let row = oms.by_client.get(cid.as_str())?;
                let qty = live_reducing_row_qty::<C, _, _>(oms, row, net)?;
                Some(qty.saturating_sub(*to))
            })
            .sum()
    };
    let resulting = |created: u64, d: &Decision<'_, C, I, A>| -> i128 {
        i128::from(working) - i128::from(cancelled)
            + i128::from(created)
            + amend_net_delta(&d.amends)
            - i128::from(decrease_reduction(d))
    };
    let wires_lower = WireCover::<C>::new(oms, net, true);
    let working_lower: u64 = wires_lower.total();
    let cancelled_lower: u64 = wires_lower.cancelled(decision.cancel_client_order_ids.iter());
    let resulting_lower = |created: u64, d: &Decision<'_, C, I, A>| -> i128 {
        i128::from(working_lower) - i128::from(cancelled_lower)
            + i128::from(created)
            + amend_net_delta(&d.amends)
            - i128::from(decrease_reduction(d))
    };
    let growing = |d: &Decision<'_, C, I, A>| d.amends.iter().any(|a| amend_delta(a) > 0);
    if (created > 0 || growing(decision))
        && (upper_unresolvable || resulting(created, decision) > i128::from(net_abs))
    {
        if created > 0 {
            decision.intents.retain(|i| !is_exit_create(i));
            created = 0;
            note.invariant_refused = true;
        }
        if growing(decision)
            && (upper_unresolvable || resulting(created, decision) > i128::from(net_abs))
        {
            let amends = std::mem::take(decision.amends);
            *decision.amends = amends.into_iter().filter(|a| amend_delta(a) <= 0).collect();
            note.invariant_refused = true;
        }
    }
    let after = resulting_lower(created, decision);
    let create_cids: Vec<C> = decision
        .intents
        .iter()
        .filter(|i| is_exit_create(i))
        .map(|i| i.client_order_id().clone())
        .collect();
    let growing_amend_olds: Vec<C> = decision
        .amends
        .iter()
        .filter(|a| amend_delta(a) > 0)
        .map(|a| a.old_client_order_id().clone())
        .collect();
    let has_partner = !create_cids.is_empty() || !growing_amend_olds.is_empty();
    let reducing_cancels: Vec<C> = decision
        .cancel_client_order_ids
        .iter()
        .filter(|cid| {
            oms.by_client
                .get(cid.as_str())
                .is_some_and(|row| live_reducing_row_qty::<C, _, _>(oms, row, net).is_some())
        })
        .cloned()
        .collect();
    let mut pair = Vec::new();
    let mut surplus = Vec::new();
    if !reducing_cancels.is_empty() && (has_partner || !reasons.is_empty()) {
        decision
            .cancel_client_order_ids
            .retain(|cid| !reducing_cancels.contains(cid));
        if has_partner {
            pair = reducing_cancels;
        } else {
            surplus = reducing_cancels;
        }
    }
    let exit_decreases: Vec<(C, u64)> = if reasons.is_empty() {
        Vec::new()
    } else {
        let lifted: Vec<(C, u64)> = decision
            .decrease_to
            .iter()
            .filter(|(cid, _)| {
                oms.by_client
                    .get(cid.as_str())
                    .is_some_and(|row| live_reducing_row_qty::<C, _, _>(oms, row, net).is_some())
            })
            .cloned()
            .collect();
        decision
            .decrease_to
            .retain(|(cid, _)| !lifted.iter().any(|(c, _)| c == cid));
        lifted
    };
    admit_decision_with_b4_proven_cancels(decision, oms, reasons, facts, b4_proven_cancels)?;
    if !pair.is_empty() {
        let survived = decision
            .intents
            .iter()
            .any(|i| create_cids.contains(i.client_order_id()))
            || decision
                .amends
                .iter()
                .any(|a| growing_amend_olds.contains(a.old_client_order_id()));
        if survived && (reasons.is_empty() || after == i128::from(net_abs)) {
            note.pair_admitted_under_guard = !reasons.is_empty();
            for cid in pair {
                if !decision.cancel_client_order_ids.contains(&cid) {
                    decision.cancel_client_order_ids.push(cid);
                }
            }
        } else {
            decision
                .intents
                .retain(|i| !create_cids.contains(i.client_order_id()));
            decision
                .amends
                .retain(|a| !growing_amend_olds.contains(a.old_client_order_id()));
            note.pair_withheld = true;
        }
    } else if has_partner && !reasons.is_empty() {
    }
    if !surplus.is_empty() && reasons.admits(AdmissionAction::ExitReduce, facts) {
        let wires_surplus = WireCover::<C>::new(oms, net, true);
        let mut coverage: u64 = wires_surplus.total();
        let mut orders_left: usize = oms
            .by_client
            .values()
            .filter(|row| !row_cancelling(row))
            .filter(|row| live_reducing_row_qty::<C, _, _>(oms, row, net).is_some())
            .filter_map(|row| wires_surplus.key_of.get(&row.ctx.client_order_id.0))
            .collect::<BTreeSet<_>>()
            .len();
        let mut removed = BTreeSet::<String>::new();
        for cid in surplus {
            let Some(row) = oms.by_client.get(cid.as_str()) else {
                continue;
            };
            if live_reducing_row_qty::<C, _, _>(oms, row, net).is_none() {
                continue;
            }
            if row_cancelling(row) {
                continue;
            }
            let Some(key) = wires_surplus.key(&cid).cloned() else {
                continue;
            };
            if removed.contains(&key) {
                if !decision.cancel_client_order_ids.contains(&cid) {
                    decision.cancel_client_order_ids.push(cid);
                }
                note.surplus_cancel_admitted = true;
                continue;
            }
            let qty = wires_surplus.contrib[&key];
            if coverage.saturating_sub(qty) >= net_abs || orders_left >= 2 {
                coverage = coverage.saturating_sub(qty);
                orders_left = orders_left.saturating_sub(1);
                removed.insert(key);
                if !decision.cancel_client_order_ids.contains(&cid) {
                    decision.cancel_client_order_ids.push(cid);
                }
                note.surplus_cancel_admitted = true;
            }
        }
    }
    let decrease_ok = reasons.admits(AdmissionAction::ExitReduce, facts)
        || (reasons.guards.contains(&GuardReason::InvalidBook) && {
            let mut rest = reasons.clone();
            rest.guards.remove(&GuardReason::InvalidBook);
            rest.admits(AdmissionAction::ExitReduce, facts)
        });
    if !exit_decreases.is_empty() && decrease_ok {
        let mut cancelled_now: Vec<&C> = Vec::new();
        for cid in decision
            .cancel_client_order_ids
            .iter()
            .chain(retries.iter())
        {
            if !cancelled_now.contains(&cid) {
                cancelled_now.push(cid);
            }
        }
        let cancelled_now: u64 = wires_lower.cancelled(cancelled_now.into_iter());
        let created_now: u64 = decision
            .intents
            .iter()
            .filter(|i| is_exit_create(i))
            .map(|i| i.qty_centi())
            .sum();
        let mut coverage = i128::from(working_lower) - i128::from(cancelled_now)
            + i128::from(created_now)
            + amend_net_delta(&decision.amends)
            - i128::from(decrease_reduction(decision));
        for (cid, to) in exit_decreases {
            let Some(row) = oms.by_client.get(cid.as_str()) else {
                continue;
            };
            let Some(qty) = live_reducing_row_qty::<C, _, _>(oms, row, net) else {
                continue;
            };
            let reduction = qty.saturating_sub(to);
            if reduction > 0 && coverage - i128::from(reduction) >= i128::from(net_abs) {
                coverage -= i128::from(reduction);
                decision.decrease_to.push((cid, to));
                note.decrease_admitted = true;
            }
        }
    }
    for cid in retries {
        if !decision.cancel_client_order_ids.contains(&cid) {
            decision.cancel_client_order_ids.push(cid);
        }
    }
    Ok(note)
}
