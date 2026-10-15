//! Typed cancellation acknowledgements and exact slot-release classification.
use super::live_lifecycle::LiveCancelTerminalKind;
#[derive(Debug, Clone)]
pub struct LiveCancelAck<C> {
    pub client_order_id: C,
    pub wire_order_id: String,
    pub terminal: LiveCancelTerminalKind,
    pub hold_slot: bool,
    pub cancel_not_found: bool,
    pub cancel_404_send_utc_ns: Option<u64>,
    pub delete_reduced_positive: bool,
    pub delete_reduced_by_centi: Option<u64>,
}
impl<C> LiveCancelAck<C> {
    pub fn releases_slot(&self) -> bool {
        if self.hold_slot {
            return false;
        }
        match self.terminal {
            LiveCancelTerminalKind::Canceled => true,
            LiveCancelTerminalKind::Filled {
                remaining_qty_centi,
                ..
            }
            | LiveCancelTerminalKind::CanceledWithFill {
                remaining_qty_centi,
                ..
            } => remaining_qty_centi == 0,
        }
    }

    pub fn canceled(client_order_id: C, wire_order_id: impl Into<String>) -> Self {
        Self {
            client_order_id,
            wire_order_id: wire_order_id.into(),
            terminal: LiveCancelTerminalKind::Canceled,
            hold_slot: false,
            cancel_not_found: false,
            cancel_404_send_utc_ns: None,
            delete_reduced_positive: false,
            delete_reduced_by_centi: None,
        }
    }

    pub fn canceled_with_fill(
        client_order_id: C,
        wire_order_id: impl Into<String>,
        filled_qty_centi: u64,
        remaining_qty_centi: u64,
    ) -> Self {
        Self {
            client_order_id,
            wire_order_id: wire_order_id.into(),
            terminal: LiveCancelTerminalKind::CanceledWithFill {
                filled_qty_centi,
                remaining_qty_centi,
            },
            hold_slot: false,
            cancel_not_found: false,
            cancel_404_send_utc_ns: None,
            delete_reduced_positive: false,
            delete_reduced_by_centi: None,
        }
    }

    pub fn filled_from_get(
        client_order_id: C,
        wire_order_id: impl Into<String>,
        filled_qty_centi: u64,
        remaining_qty_centi: u64,
    ) -> Self {
        Self {
            client_order_id,
            wire_order_id: wire_order_id.into(),
            terminal: LiveCancelTerminalKind::Filled {
                filled_qty_centi,
                remaining_qty_centi,
            },
            hold_slot: false,
            cancel_not_found: false,
            cancel_404_send_utc_ns: None,
            delete_reduced_positive: false,
            delete_reduced_by_centi: None,
        }
    }

    pub fn after_cancel_not_found(mut self) -> Self {
        self.cancel_not_found = true;
        self
    }

    pub fn with_cancel_404_send(mut self, send_ns: u64) -> Self {
        self.cancel_404_send_utc_ns = Some(send_ns);
        self
    }

    pub fn with_delete_reduced(mut self, reduced_by_centi: u64) -> Self {
        self.delete_reduced_positive = true;
        self.delete_reduced_by_centi = Some(reduced_by_centi);
        self
    }
}

impl<C> super::open_projection::ProjectionCancelInput<C> for LiveCancelAck<C> {
    fn client_order_id(&self) -> &C {
        &self.client_order_id
    }
}
impl<C> super::live_lifecycle::CancelAckInput<C> for LiveCancelAck<C> {
    fn client_order_id(&self) -> &C {
        &self.client_order_id
    }
    fn terminal_kind(&self) -> LiveCancelTerminalKind {
        self.terminal
    }
    fn set_hold_slot(&mut self, value: bool) {
        self.hold_slot = value;
    }
}

#[derive(Debug, Clone)]
pub struct CancelGoneEvidence<C> {
    pub client_order_id: C,
    pub wire: String,
    pub resolve: CancelGoneResolve,
    pub order_age_anchor_ns: Option<u64>,
    pub action_send_utc_ns: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelGoneResolve {
    StillOpen,
    Unconfirmed,
    AckCanceled,
    AckFilled {
        filled_qty_centi: u64,
        remaining_qty_centi: u64,
    },
    AckCanceledWithFill {
        filled_qty_centi: u64,
        remaining_qty_centi: u64,
    },
}

pub fn cancel_ack_from_get<C>(
    resolve: &CancelGoneResolve,
    client_order_id: C,
    wire_order_id: &str,
) -> Option<LiveCancelAck<C>> {
    match resolve {
        CancelGoneResolve::AckCanceled => {
            Some(LiveCancelAck::canceled(client_order_id, wire_order_id))
        }
        CancelGoneResolve::AckFilled {
            filled_qty_centi,
            remaining_qty_centi,
        } => Some(LiveCancelAck::filled_from_get(
            client_order_id,
            wire_order_id,
            *filled_qty_centi,
            *remaining_qty_centi,
        )),
        CancelGoneResolve::AckCanceledWithFill {
            filled_qty_centi,
            remaining_qty_centi,
        } => Some(LiveCancelAck::canceled_with_fill(
            client_order_id,
            wire_order_id,
            *filled_qty_centi,
            *remaining_qty_centi,
        )),
        CancelGoneResolve::StillOpen | CancelGoneResolve::Unconfirmed => None,
    }
}
