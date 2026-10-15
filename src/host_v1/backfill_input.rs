//! Typed REST order details and exact unknown-submission matching.
use crate::lifecycle::{
    BackfillOrderRecord, BackfillOrderStatus, ClientOrderId as OmsClientOrderId, VenueOrderId,
};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestOrderDetail {
    pub order_id: String,
    pub client_order_id: String,
    pub status: String,
    pub filled_qty: u64,
    pub remaining_qty: u64,
    pub ticker: Option<String>,
}

pub fn rest_details_contain_client(client_order_id: &str, details: &[RestOrderDetail]) -> bool {
    let want = client_order_id.trim();
    !want.is_empty() && details.iter().any(|d| d.client_order_id.trim() == want)
}

pub fn backfill_status_from_rest(status: &str) -> Option<BackfillOrderStatus> {
    match status.to_ascii_lowercase().as_str() {
        "resting" | "open" | "pending" | "active" => Some(BackfillOrderStatus::Open),
        "executed" | "filled" | "complete" | "completed" => Some(BackfillOrderStatus::Filled),
        "canceled" | "cancelled" => Some(BackfillOrderStatus::Canceled),
        _ => None,
    }
}

pub fn backfill_records_matching_client(
    client_order_id: &str,
    details: &[RestOrderDetail],
) -> Vec<BackfillOrderRecord> {
    let want = client_order_id.trim();
    if want.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for d in details {
        if d.client_order_id.trim() != want {
            continue;
        }
        let wire = d.order_id.trim();
        if wire.is_empty() {
            continue;
        }
        let Some(status) = backfill_status_from_rest(&d.status) else {
            continue;
        };
        let status = match status {
            BackfillOrderStatus::Open if d.filled_qty > 0 && d.remaining_qty > 0 => {
                BackfillOrderStatus::Partial
            }
            other => other,
        };
        if out
            .iter()
            .any(|r: &BackfillOrderRecord| r.venue_order_id.0 == wire)
        {
            continue;
        }
        out.push(BackfillOrderRecord {
            other_members_filled: 0,
            client_order_id: OmsClientOrderId(want.to_string()),
            venue_order_id: VenueOrderId(wire.to_string()),
            status,
            filled_qty: d.filled_qty,
            remaining_qty: d.remaining_qty,
            fills: vec![],
        });
    }
    out
}

use super::journal_projection::backfill_claim_stamps_age;
use super::live_admission::AdmissionClient;
use super::live_ingest::ExecutionTrace;
use super::live_state::LiveOmsBookState;
use super::live_transaction::TransactionIo;
use crate::lifecycle::{OrderState, SubmissionCloseReason};
use anyhow::Result;
use std::collections::BTreeSet;

pub trait BackfillJournalIo: TransactionIo {
    fn retain_backfill_unknown(&mut self, keep: impl FnMut(&OmsClientOrderId) -> bool);
    fn peek_backfill_unknown(&self) -> Vec<OmsClientOrderId>;
    fn drain_backfill_unknown(&mut self) -> Vec<OmsClientOrderId>;
}
pub trait BackfillTrace<R>: ExecutionTrace<R> {
    fn exhaustive_absent(cid: &str);
}
#[derive(Debug, Clone)]
pub struct UnknownBackfillRequest {
    pub cids: BTreeSet<String>,
    pub limit: u32,
    pub min_ts: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackfillShellReport<Clients> {
    pub clients_processed: u64,
    pub matches_found: u64,
    pub applied: u64,
    pub exhaustive_page: bool,
    pub errors: Vec<String>,
    pub claimed_resting: Clients,
}
impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn apply_unknown_backfill_details<C: AdmissionClient<R>, J: BackfillJournalIo>(
        &mut self,
        details: Vec<RestOrderDetail>,
        walked_to_end: bool,
        limit: u32,
        journal: &mut J,
    ) -> Result<BackfillShellReport<Vec<C>>>
    where
        Stage: BackfillTrace<R>,
    {
        const BACKFILL_MAX_PAGES: u32 = 20;
        let exhaustive = walked_to_end;
        if !exhaustive {
            anyhow::bail!(
                "BACKFILL NOT EXHAUSTIVE: fetched {} rows without reaching the last page \
                 (limit {BACKFILL_MAX_PAGES} pages x {limit}); request queue retained",
                details.len()
            );
        }
        let clients = journal.drain_backfill_unknown();
        let mut report: BackfillShellReport<Vec<C>> = BackfillShellReport {
            clients_processed: clients.len() as u64,
            exhaustive_page: exhaustive,
            ..Default::default()
        };
        for c in clients {
            let mut matched = backfill_records_matching_client(&c.0, &details);
            report.matches_found = report.matches_found.saturating_add(matched.len() as u64);
            let host_cid = C::try_from_string(c.0.as_str().to_owned())
                .map_err(|e| anyhow::anyhow!("invalid client_order_id for backfill: {e}"))?;
            if exhaustive
                && matched.is_empty()
                && !rest_details_contain_client(host_cid.as_str(), &details)
                && self
                    .by_client
                    .get(host_cid.as_str())
                    .is_some_and(|o| o.ctx.attributed_fill_qty == 0)
            {
                match self.close_exhaustive_unknown_no_match::<C>(&host_cid, journal) {
                    Ok(()) => {
                        report.applied = report.applied.saturating_add(1);
                    }
                    Err(e) => {
                        report.errors.push(format!("{}: {e}", host_cid.as_str()));
                    }
                }
                continue;
            }
            if matched.len() == 1 {
                let record = &mut matched[0];
                match self.backfill_member_filled_qty::<C>(
                    &record.venue_order_id,
                    &host_cid,
                    record.filled_qty,
                ) {
                    Some(filled_qty) => {
                        if self
                            .by_client
                            .get(host_cid.as_str())
                            .is_some_and(|row| filled_qty < row.ctx.attributed_fill_qty)
                        {
                            self.note_lagged_unknown_create(host_cid.as_str());
                            if let Err(e) =
                                self.apply_unknown_backfill::<C>(&host_cid, false, matched, journal)
                            {
                                report.errors.push(format!("{}: {e}", host_cid.as_str()));
                            }
                            continue;
                        }
                        record.other_members_filled = record.filled_qty - filled_qty;
                    }
                    None => {
                        if let Err(e) =
                            self.apply_unknown_backfill::<C>(&host_cid, false, matched, journal)
                        {
                            report.errors.push(format!("{}: {e}", host_cid.as_str()));
                        }
                        continue;
                    }
                }
            }
            match self.apply_unknown_backfill::<C>(&host_cid, exhaustive, matched, journal) {
                Ok(()) => {
                    report.applied = report.applied.saturating_add(1);
                    if self.stamp_claimed_resting_age::<C>(&host_cid, journal) {
                        report.claimed_resting.push(host_cid.clone());
                    }
                }
                Err(e) => {
                    report.errors.push(format!("{}: {e}", host_cid.as_str()));
                }
            }
        }

        Ok(report)
    }

    pub fn prepare_unknown_backfill<C: AdmissionClient<R>, J: BackfillJournalIo>(
        &mut self,
        journal: &mut J,
        limit: u32,
        min_ts: Option<u64>,
    ) -> Option<UnknownBackfillRequest> {
        let oms = &*self;
        journal.retain_backfill_unknown(|cid| {
            let Ok(host) = C::try_from_string(cid.0.trim().to_owned()) else {
                return true;
            };
            !oms.by_client.get(host.as_str()).is_some_and(|row| {
                matches!(
                    row.state,
                    OrderState::Filled | OrderState::Canceled | OrderState::Terminal
                ) || row.state.is_submission_closed()
            })
        });
        let peek = journal.peek_backfill_unknown();
        if peek.is_empty() {
            return None;
        }
        let limit = limit.max(1);
        let cids: BTreeSet<String> = peek.iter().map(|cid| cid.0.trim().to_owned()).collect();
        Some(UnknownBackfillRequest {
            cids,
            limit,
            min_ts,
        })
    }

    pub fn stamp_claimed_resting_age<C: AdmissionClient<R>>(
        &mut self,
        host_cid: &C,
        journal: &mut dyn TransactionIo,
    ) -> bool {
        if !self.claimed_resting(host_cid.as_str()) {
            return false;
        }
        if let Some(o) = self.by_client.get_mut(host_cid.as_str()) {
            if backfill_claim_stamps_age(&o.state) {
                o.order_age_anchor_ns = Some(journal.wall_now_ns());
            }
        }
        true
    }

    pub fn close_exhaustive_unknown_no_match<C: AdmissionClient<R>>(
        &mut self,
        cid: &C,
        journal: &mut dyn TransactionIo,
    ) -> Result<()>
    where
        Stage: BackfillTrace<R>,
    {
        self.close_submission(cid, SubmissionCloseReason::ExhaustiveAbsent, journal)?;
        Stage::exhaustive_absent(cid.as_str());
        Ok(())
    }
}
