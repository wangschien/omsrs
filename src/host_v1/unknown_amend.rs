//! Per-lineage direct-read tracks and deterministic attempt scheduling.
use super::backfill_input::{RestOrderDetail, backfill_records_matching_client};
use super::live_admission::AdmissionClient;
use super::live_authority::UaLineageRef;
use super::live_cover::CoverageClient;
use super::live_state::LiveOmsBookState;
use crate::lifecycle::OrderState;
use crate::lifecycle::{BackfillOrderStatus, ClientOrderId as OmsClientOrderId, VenueOrderId};
use anyhow::Result;
use std::collections::BTreeMap;

pub const UA_ATTEMPT_GAP_NS: u64 = 120_000_000;
#[derive(Debug, Clone)]
pub struct UaTrack {
    pub t_a_ns: u64,
    pub answered: bool,
    pub old_cid: String,
    pub wire: String,
    pub next_attempt_ns: u64,
    pub attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UaResolved {
    pub new_cid: String,
    pub t_a_ns: u64,
    pub resolved_ns: u64,
    pub source: &'static str,
}

#[derive(Debug, Default)]
pub struct UaState {
    pub tracks: BTreeMap<String, UaTrack>,
    pub read_resolved: BTreeMap<String, u64>,
    pub resolved: Vec<UaResolved>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UaAttempt {
    pub new_cid: String,
    pub old_cid: String,
    pub wire: String,
    pub t_a_ns: u64,
    pub answered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UaRow {
    pub client_order_id: String,
    pub order_id: String,
    pub ticker: String,
    pub status: String,
    pub filled_centi: u64,
    pub remaining_centi: u64,
    pub initial_centi: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UaVerdict {
    Applied,
    NotApplied,
    NoChange,
}
impl UaState {
    pub fn ua_note_unknown(
        &mut self,
        new_cid: &str,
        old_cid: &str,
        wire: &str,
        answered: bool,
        t_a_ns: u64,
    ) {
        self.tracks.insert(
            new_cid.to_owned(),
            UaTrack {
                t_a_ns,
                answered,
                old_cid: old_cid.to_owned(),
                wire: wire.to_owned(),
                next_attempt_ns: t_a_ns,
                attempts: 0,
            },
        );
    }

    pub fn ua_is_read_resolved(&self, new_cid: &str) -> bool {
        self.read_resolved.contains_key(new_cid)
    }

    pub fn ua_sync<C: AdmissionClient<R>, R, Stage>(
        &mut self,
        oms: &mut LiveOmsBookState<R, Stage>,
        now_ns: u64,
    ) -> Vec<UaLineageRef> {
        let unresolved = oms.unresolved_unknown_amend_lineages::<C>();
        for l in &unresolved {
            self.tracks
                .entry(l.new_cid.clone())
                .or_insert_with(|| UaTrack {
                    t_a_ns: now_ns,
                    answered: false,
                    old_cid: l.old_cid.clone(),
                    wire: l.wire.clone(),
                    next_attempt_ns: now_ns,
                    attempts: 0,
                });
        }
        let clears = std::mem::take(&mut oms.lineage_clears);
        let gone: Vec<String> = self
            .tracks
            .keys()
            .filter(|cid| !unresolved.iter().any(|l| &l.new_cid == *cid))
            .cloned()
            .collect();
        for cid in gone {
            let Some(t) = self.tracks.remove(&cid) else {
                continue;
            };
            let (source, resolved_ns) = if let Some(at) = self.read_resolved.remove(&cid) {
                ("read", at)
            } else if let Some(c) = clears.iter().rev().find(|c| c.new_cid == cid) {
                (c.source, c.instant_ns)
            } else {
                ("other", now_ns)
            };
            if C::from_string(cid.to_string())
                .and_then(|c| {
                    oms.by_client
                        .get(c.as_str())
                        .map(|r| matches!(r.state, OrderState::Halted { .. }))
                })
                .unwrap_or(false)
            {
                continue;
            }
            self.resolved.push(UaResolved {
                new_cid: cid,
                t_a_ns: t.t_a_ns,
                resolved_ns,
                source,
            });
        }
        unresolved
    }

    pub fn ua_next_wake_ns(&self, bound_ns: u64) -> Option<u64> {
        self.tracks
            .values()
            .map(|t| {
                t.next_attempt_ns
                    .min(t.t_a_ns.saturating_add(bound_ns).saturating_add(1))
            })
            .min()
    }

    pub fn ua_next_due_ns(&self) -> Option<u64> {
        self.tracks.values().map(|t| t.next_attempt_ns).min()
    }

    pub fn ua_take_due(&mut self, now_ns: u64) -> Option<UaAttempt> {
        let cid = self
            .tracks
            .iter()
            .filter(|(_, t)| t.next_attempt_ns <= now_ns)
            .min_by_key(|(_, t)| t.next_attempt_ns)
            .map(|(cid, _)| cid.clone())?;
        let t = self.tracks.get_mut(&cid)?;
        t.next_attempt_ns = now_ns.saturating_add(UA_ATTEMPT_GAP_NS);
        t.attempts = t.attempts.saturating_add(1);
        Some(UaAttempt {
            new_cid: cid,
            old_cid: t.old_cid.clone(),
            wire: t.wire.clone(),
            t_a_ns: t.t_a_ns,
            answered: t.answered,
        })
    }
}

/// Transparent borrows of the calling context's existing order owner and IO.
pub trait UaApplyHost {
    type Role;
    type Client: AdmissionClient<Self::Role>;
    type Stage: super::live_ingest::ExecutionTrace<Self::Role>;
    fn oms(&self) -> &LiveOmsBookState<Self::Role, Self::Stage>;
    fn ua_mut(&mut self) -> &mut UaState;
    fn with_book_journal<T>(
        &mut self,
        call: impl FnOnce(
            &mut LiveOmsBookState<Self::Role, Self::Stage>,
            &mut dyn super::live_transaction::TransactionIo,
        ) -> T,
    ) -> T;
    fn retain_backfill_unknown(&mut self, want: &str);
    fn stamp_claimed_resting_age(&mut self, client: &Self::Client) -> bool;
    fn sync_open_from_oms(&mut self);
    fn close_exhaustive_unknown_no_match(&mut self, client: &Self::Client) -> anyhow::Result<()>;
    fn amend_reject_tombstones_mut(&mut self) -> &mut std::collections::HashMap<String, u64>;
}

pub fn ua_apply<H: UaApplyHost>(
    host: &mut H,
    att: &UaAttempt,
    as_of_ns: Option<i64>,
    row: Option<&UaRow>,
    now_ns: u64,
) -> Result<UaVerdict> {
    let Some(row) = row else {
        return Ok(UaVerdict::NoChange);
    };
    let (Ok(child), Ok(old)) = (
        <H::Client as super::live_cover::CoverageClient>::try_from_string(att.new_cid.clone()),
        <H::Client as super::live_cover::CoverageClient>::try_from_string(att.old_cid.clone()),
    ) else {
        return Ok(UaVerdict::NoChange);
    };
    let wire = VenueOrderId(att.wire.clone());
    if row.order_id != att.wire {
        return Ok(UaVerdict::NoChange);
    }
    if !host
        .oms()
        .by_client
        .get(child.as_str())
        .is_some_and(|r| matches!(r.state, OrderState::SubmitUnknown))
    {
        return Ok(UaVerdict::NoChange);
    }
    if row.client_order_id == att.new_cid {
        let Some(filled_qty) =
            host.oms()
                .backfill_member_filled_qty::<H::Client>(&wire, &child, row.filled_centi)
        else {
            return Ok(UaVerdict::NoChange);
        };
        let detail = RestOrderDetail {
            order_id: row.order_id.clone(),
            client_order_id: row.client_order_id.clone(),
            status: row.status.clone(),
            filled_qty: row.filled_centi,
            remaining_qty: row.remaining_centi,
            ticker: Some(row.ticker.clone()),
        };
        let mut matched =
            backfill_records_matching_client(&att.new_cid, std::slice::from_ref(&detail));
        if matched.is_empty() {
            return Ok(UaVerdict::NoChange);
        }
        if host
            .oms()
            .by_client
            .get(child.as_str())
            .is_some_and(|r| filled_qty < r.ctx.attributed_fill_qty)
        {
            return Ok(UaVerdict::NoChange);
        }
        let declared = host
            .oms()
            .public_fields
            .registry
            .get(&host.oms().ledger, &wire)
            .and_then(|ws| ws.amend_lineage.as_ref().map(|l| l.declared_remaining));
        let resting_unfilled = matched.len() == 1
            && matched[0].status == BackfillOrderStatus::Open
            && row.filled_centi == 0
            && Some(row.remaining_centi) == declared;
        let qualified = as_of_ns.is_some_and(|a| a > 0 && (a as u64) > att.t_a_ns);
        if resting_unfilled && !qualified {
            return Ok(UaVerdict::NoChange);
        }
        let claim_clean = host
            .oms()
            .by_client
            .get(child.as_str())
            .is_some_and(|r| r.ctx.attributed_fill_qty == 0)
            && host.oms().wire_held_inventory_centi(&wire) == Some(0);
        let recovered = resting_unfilled
            && claim_clean
            && host.with_book_journal(|oms, journal| {
                oms.recover_unknown_amend_from_rest::<H::Client>(
                    &wire,
                    &OmsClientOrderId(child.as_str().to_string()),
                    0,
                    row.remaining_centi,
                    row.initial_centi,
                    journal,
                )
            })?;
        if !recovered {
            matched[0].other_members_filled = row.filled_centi - filled_qty;
            host.with_book_journal(|oms, journal| {
                oms.apply_unknown_backfill::<H::Client>(&child, true, matched, journal)
            })?;
            host.with_book_journal(|oms, journal| {
                oms.settle_lineage_after_unknown_backfill::<H::Client>(&wire, &child, &old, journal)
            })?;
        }
        let want = att.new_cid.clone();
        host.retain_backfill_unknown(&want);
        host.stamp_claimed_resting_age(&child);
        host.sync_open_from_oms();
        if !host
            .oms()
            .unresolved_unknown_amends::<H::Client>()
            .iter()
            .any(|(c, _)| c == &att.new_cid)
        {
            host.ua_mut()
                .read_resolved
                .insert(att.new_cid.clone(), now_ns);
        }
        return Ok(UaVerdict::Applied);
    }
    let qualified = as_of_ns.is_some_and(|a| a > 0 && (a as u64) > att.t_a_ns);
    if row.client_order_id == att.old_cid
        && qualified
        && att.answered
        && host
            .oms()
            .by_client
            .get(child.as_str())
            .is_some_and(|r| r.ctx.attributed_fill_qty == 0)
    {
        let newly = host
            .oms()
            .by_client
            .get(child.as_str())
            .is_some_and(|r| !r.state.is_submission_closed());
        host.close_exhaustive_unknown_no_match(&child)?;
        host.with_book_journal(|oms, journal| {
            oms.clear_amend_lineage_as(&att.wire, journal, "read")
        })?;
        if newly {
            host.amend_reject_tombstones_mut()
                .insert(att.new_cid.clone(), now_ns);
        }
        let want = att.new_cid.clone();
        host.retain_backfill_unknown(&want);
        host.sync_open_from_oms();
        host.ua_mut()
            .read_resolved
            .insert(att.new_cid.clone(), now_ns);
        return Ok(UaVerdict::NotApplied);
    }
    Ok(UaVerdict::NoChange)
}
