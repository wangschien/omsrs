use std::collections::BTreeMap;

use crate::lifecycle::VenueOrderId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectiveCancelOutcome {
    Sent,
    Gone,
    NotFoundUnproven,
    StillThere,
    Failed,
}

#[derive(Debug, Clone, Copy, Default)]
struct CancelState {
    sends: u32,
    hold_until_ns: u64,
    terminal: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct ReadState {
    failed: u32,
    hold_until_ns: u64,
}

/// Process-local recovery cadence. It is separate from durable lifecycle
/// truth. Reset behavior alone does not prove a safe restart policy; combined
/// journal, latch and cadence recovery must be established by the host contract.
#[derive(Debug, Default)]
pub struct RecoveryCadence {
    cancels: BTreeMap<VenueOrderId, CancelState>,
    reads: BTreeMap<VenueOrderId, ReadState>,
    cancel_suppressed: u64,
    read_suppressed: u64,
}

impl RecoveryCadence {
    pub fn protective_cancel_due(&mut self, wire: &VenueOrderId, now_ns: u64) -> bool {
        let due = self.cancels.get(wire).map_or(true, |state| {
            !state.terminal && state.sends < 6 && now_ns >= state.hold_until_ns
        });
        if !due {
            self.cancel_suppressed = self.cancel_suppressed.saturating_add(1);
        }
        due
    }

    pub fn note_protective_cancel(
        &mut self,
        wire: &VenueOrderId,
        outcome: ProtectiveCancelOutcome,
        now_ns: u64,
    ) {
        let state = self.cancels.entry(wire.clone()).or_default();
        state.sends = state.sends.saturating_add(1);
        match outcome {
            ProtectiveCancelOutcome::Sent => {}
            ProtectiveCancelOutcome::Gone => state.terminal = true,
            ProtectiveCancelOutcome::NotFoundUnproven | ProtectiveCancelOutcome::StillThere => {
                state.hold_until_ns = now_ns.saturating_add(10_000_000_000);
            }
            ProtectiveCancelOutcome::Failed => {
                state.hold_until_ns = now_ns
                    .saturating_add((250_000_000_u64 << state.sends.min(4)).min(4_000_000_000));
            }
        }
    }

    pub fn authority_read_due(&mut self, wire: &VenueOrderId, now_ns: u64) -> bool {
        let due = self
            .reads
            .get(wire)
            .map_or(true, |state| now_ns >= state.hold_until_ns);
        if !due {
            self.read_suppressed = self.read_suppressed.saturating_add(1);
        }
        due
    }

    pub fn note_authority_read(&mut self, wire: &VenueOrderId, discharged: bool, now_ns: u64) {
        if discharged {
            self.reads.remove(wire);
            return;
        }
        let state = self.reads.entry(wire.clone()).or_default();
        let backoff = (500_000_000_u64 << state.failed.min(3)).min(4_000_000_000);
        state.failed = state.failed.saturating_add(1);
        state.hold_until_ns = now_ns.saturating_add(backoff);
    }

    pub fn cancel_suppressed(&self) -> u64 {
        self.cancel_suppressed
    }
    pub fn read_suppressed(&self) -> u64 {
        self.read_suppressed
    }
}
