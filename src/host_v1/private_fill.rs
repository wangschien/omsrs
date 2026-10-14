//! Post-ingestion private-fill bookkeeping, independent of journal choreography.

use std::collections::BTreeSet;

use crate::execution::{ExecutionOutcome, OrderDisposition};
use crate::lifecycle::{HaltReason, OrderState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateFillBook {
    Booked,
    NoRow,
    /// The caller returns this before ingesting a zero-quantity signal.
    ZeroQty,
    /// A late execution was booked. This does not prove prior normal-terminal
    /// provenance, resting authority, or permission to send an order.
    BookedLatePostTerminal,
}

/// Owns only the pending-fee and seen-fill ID sets. Canonical orders, inventory,
/// execution preparation, storage and transport remain with their existing owner.
#[derive(Debug, Default)]
pub struct PrivateFillBookkeeping {
    pending_fee_fill_ids: BTreeSet<String>,
    seen_fill_ids: BTreeSet<String>,
}

impl PrivateFillBookkeeping {
    pub fn fill_id_seen(&self, trade_id: &str) -> bool {
        self.seen_fill_ids.contains(trade_id)
    }

    /// The caller explicitly supplies journaled IDs during recovery.
    pub fn seed_seen_fill_ids<'a>(&mut self, ids: impl IntoIterator<Item = &'a str>) {
        for id in ids {
            self.seen_fill_ids.insert(id.to_string());
        }
    }

    pub fn pending_fee_fill_ids(&self) -> &BTreeSet<String> {
        &self.pending_fee_fill_ids
    }

    pub fn has_pending_fee_enrichment(&self) -> bool {
        !self.pending_fee_fill_ids.is_empty()
    }
}

/// Named caller adapter for the result of the caller's real ingestion operation.
///
/// Pass the original fill ID/fee and that client's actual post-ingestion state.
/// On an error, the caller supplies its actual `has_fill_id` result, queried even
/// when the row lacks the post-terminal label. On success that fact is unused.
/// The adapter performs no ingestion, journal read/write/fence, or notification.
/// Raw-field validation and the zero-quantity return precede ingestion and remain
/// caller-owned. An ordinary ingestion error is returned unchanged.
pub fn finish_private_fill_ingestion<E>(
    bookkeeping: &mut PrivateFillBookkeeping,
    fill_id: &str,
    fee_cents: Option<u64>,
    apply_res: Result<Vec<ExecutionOutcome>, E>,
    post_ingestion_state: Option<&OrderState>,
    journaled_after_error: bool,
) -> Result<PrivateFillBook, E> {
    let row_late_post_terminal = post_ingestion_state.is_some_and(|state| {
        matches!(
            state,
            OrderState::Halted {
                reason: HaltReason::PostTerminalFill
            }
        )
    });
    let outcomes = match apply_res {
        Ok(outcomes) => outcomes,
        Err(error) => {
            if row_late_post_terminal && journaled_after_error {
                bookkeeping.seen_fill_ids.insert(fill_id.to_string());
                if fee_cents.is_none() {
                    bookkeeping.pending_fee_fill_ids.insert(fill_id.to_string());
                }
                return Ok(PrivateFillBook::BookedLatePostTerminal);
            }
            return Err(error);
        }
    };
    if outcomes
        .iter()
        .any(|outcome| matches!(outcome.order, OrderDisposition::LateBooked))
    {
        bookkeeping.seen_fill_ids.insert(fill_id.to_string());
        if fee_cents.is_none() {
            bookkeeping.pending_fee_fill_ids.insert(fill_id.to_string());
        }
        return Ok(PrivateFillBook::BookedLatePostTerminal);
    }
    if outcomes
        .iter()
        .any(|outcome| matches!(outcome.order, OrderDisposition::NoRow))
    {
        bookkeeping.seen_fill_ids.insert(fill_id.to_string());
        return Ok(PrivateFillBook::NoRow);
    }
    if fee_cents.is_none() {
        bookkeeping.pending_fee_fill_ids.insert(fill_id.to_string());
    } else {
        bookkeeping.pending_fee_fill_ids.remove(fill_id);
    }
    bookkeeping.seen_fill_ids.insert(fill_id.to_string());
    Ok(PrivateFillBook::Booked)
}
