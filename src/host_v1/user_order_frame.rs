//! User-order admission and observation over the canonical order book.
use super::live_admission::AdmissionClient;
use super::live_observation::ObservationTrace;
use super::live_state::LiveOmsBookState;
use super::live_transaction::TransactionIo;
use crate::lifecycle::{ClientOrderId as OmsClientOrderId, OrderState, Side, VenueOrderId};
use crate::observation::UserOrderEvidence;
use anyhow::{Result, bail};
use serde_json::Value;

/// Existing input decoding and private diagnostics, without order-state effects.
pub trait UserOrderFrameCodec<Stage> {
    type UpdateReason;
    fn admit_ws_user_order(
        text: &str,
        expected_sid: Option<u64>,
        expected_market: &str,
        expected_exchange_index: i64,
        expected_subaccount: i64,
        connection_generation: u64,
    ) -> WsUserOrderAdmit<Self::UpdateReason>;
    fn describe_invalid_user_order_frame(reason: &InvalidUserOrderEvidence, raw: &Value) -> String;
    fn describe_user_order_frame(reason: &str, raw: &Value) -> String;
    fn wrong_exchange_trace(stage: Option<&Stage>, reason: &InvalidUserOrderEvidence, raw: &Value);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidUserOrderEvidence {
    Malformed,
    WrongSubscription,
    WrongMarket,
    WrongExchangeIndex,
    WrongSubaccount,
    MissingIdentity,
    MissingVersion,
    InvalidStatus,
    InvalidQuantity,
    InvalidPrice,
    InvalidSide,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedUserOrder<U> {
    pub sid: u64,
    pub wire: VenueOrderId,
    pub side: Side,
    pub yes_price_e4: u64,
    /// Diagnostic cause only. Venue `status`, not reason, determines terminality.
    pub update_reason: Option<U>,
    pub evidence: UserOrderEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsUserOrderAdmit<U> {
    NotUserOrder,
    Ours(AdmittedUserOrder<U>),
    Invalid(InvalidUserOrderEvidence),
}
impl<R, Stage: ObservationTrace> LiveOmsBookState<R, Stage> {
    pub fn consume_user_order_frame<C: AdmissionClient<R> + UserOrderFrameCodec<Stage>>(
        &mut self,
        journal: &mut dyn TransactionIo,
        text: &str,
        market: &str,
    ) -> Result<bool> {
        let Ok(raw) = serde_json::from_str::<Value>(text) else {
            return Ok(false);
        };
        if raw.get("type").and_then(Value::as_str) == Some("subscribed")
            && raw
                .get("msg")
                .and_then(|m| m.get("channel"))
                .and_then(Value::as_str)
                == Some("user_orders")
        {
            if self.user_orders_cmd_id.is_some()
                && raw
                    .get("id")
                    .and_then(Value::as_u64)
                    .map_or(true, |id| Some(id) == self.user_orders_cmd_id)
            {
                let sid = raw
                    .get("msg")
                    .and_then(|m| m.get("sid"))
                    .and_then(Value::as_u64)
                    .ok_or_else(|| anyhow::anyhow!("user_orders subscribed ACK missing sid"))?;
                self.user_orders_sid = Some(sid);
                self.user_orders_connection_generation = self
                    .user_orders_connection_generation
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("user_orders generation overflow"))?;
            }
            return Ok(false);
        }
        if raw.get("type").and_then(Value::as_str) == Some("user_order") {
            let local_cid = raw
                .get("msg")
                .and_then(|msg| msg.get("client_order_id"))
                .and_then(Value::as_str);
            if local_cid.map_or(true, |cid| self.by_client.get(cid).is_none()) {
                return Ok(false);
            }
        }
        let Some(exchange_index) = self.expected_exchange_index else {
            if raw.get("type").and_then(Value::as_str) == Some("user_order") {
                bail!("user_orders has no configured exchange index");
            }
            return Ok(false);
        };
        let order = match C::admit_ws_user_order(
            text,
            self.user_orders_sid,
            market,
            exchange_index,
            self.expected_subaccount,
            self.user_orders_connection_generation,
        ) {
            WsUserOrderAdmit::NotUserOrder => return Ok(false),
            WsUserOrderAdmit::Invalid(reason) => {
                if reason == InvalidUserOrderEvidence::WrongExchangeIndex {
                    C::wrong_exchange_trace(self.stage.as_ref(), &reason, &raw);
                    bail!("user_orders exchange index conflicts with configured index");
                }
                let cid = raw
                    .get("msg")
                    .and_then(|msg| msg.get("client_order_id"))
                    .and_then(Value::as_str)
                    .and_then(|value| C::try_from_string(value.to_owned()).ok());
                let wire = cid
                    .as_ref()
                    .and_then(|cid| self.by_client.get(cid.as_str()))
                    .and_then(|row| row.ctx.venue_order_id.clone());
                let local = cid
                    .as_ref()
                    .and_then(|cid| self.by_client.get(cid.as_str()))
                    .map_or_else(
                        || "local_row=none".to_owned(),
                        |row| {
                            format!(
                                "local_row=(cid={} qty={} price_e4={} state={:?})",
                                row.ctx.client_order_id.as_str(),
                                row.ctx.qty,
                                row.ctx.price_cents,
                                row.state
                            )
                        },
                    );
                let detail = C::describe_invalid_user_order_frame(&reason, &raw);
                self.quarantine_user_order(
                    cid.as_ref()
                        .map(|cid| OmsClientOrderId(cid.as_str().to_owned())),
                    wire,
                    &format!("invalid_order_frame {detail} {local}"),
                    journal,
                )?;
                return Ok(false);
            }
            WsUserOrderAdmit::Ours(order) => order,
        };
        let mut order = order;
        let cid = C::try_from_string(order.evidence.client_order_id.0.clone())?;
        let Some(row) = self.by_client.get(cid.as_str()) else {
            return Ok(false);
        };
        if row.ctx.side != order.side
            || row.ctx.price_cents != order.yes_price_e4
            || !self.user_order_initial_ok::<C>(row, &order.wire, order.evidence.initial)
            || (order.evidence.initial != row.ctx.qty
                && order
                    .evidence
                    .filled
                    .checked_add(order.evidence.remaining)
                    .map_or(true, |total| total > row.ctx.qty))
            || row
                .ctx
                .venue_order_id
                .as_ref()
                .is_some_and(|wire| wire != &order.wire)
        {
            let failed = format!(
                "failed=[{}{}{}{}{}]",
                if row.ctx.side != order.side {
                    "side "
                } else {
                    ""
                },
                if row.ctx.price_cents != order.yes_price_e4 {
                    "price "
                } else {
                    ""
                },
                if !self.user_order_initial_ok::<C>(row, &order.wire, order.evidence.initial) {
                    "initial "
                } else {
                    ""
                },
                if order.evidence.initial != row.ctx.qty
                    && order
                        .evidence
                        .filled
                        .checked_add(order.evidence.remaining)
                        .map_or(true, |total| total > row.ctx.qty)
                {
                    "total_over_known "
                } else {
                    ""
                },
                if row
                    .ctx
                    .venue_order_id
                    .as_ref()
                    .is_some_and(|wire| wire != &order.wire)
                {
                    "wire"
                } else {
                    ""
                }
            );
            let detail = C::describe_user_order_frame("order_identity_or_quantity_conflict", &raw);
            let local = format!(
                "local_row=(cid={} qty={} price_e4={} side={:?} venue_initial={:?} wire={:?} state={:?})",
                row.ctx.client_order_id.as_str(),
                row.ctx.qty,
                row.ctx.price_cents,
                row.ctx.side,
                row.venue_initial_centi,
                row.ctx.venue_order_id.as_ref().map(|w| w.0.as_str()),
                row.state
            );
            let wire = row.ctx.venue_order_id.clone().or(Some(order.wire));
            self.quarantine_user_order(
                Some(OmsClientOrderId(cid.as_str().to_owned())),
                wire,
                &format!("{detail} {failed} {local}"),
                journal,
            )?;
            return Ok(false);
        }
        let pre_submit = matches!(
            row.state,
            OrderState::SubmitStarted { .. } | OrderState::SubmitUnknown
        ) && row.ctx.venue_order_id.is_none();
        let venue_initial = order.evidence.initial;
        if let Some(row) = self.by_client.get_mut(cid.as_str()) {
            row.venue_initial_centi = Some(venue_initial);
        }
        if order.evidence.status == crate::observation::UserOrderStatus::Executed
            && order.evidence.filled < order.evidence.initial
        {
            order.evidence.initial = order.evidence.filled;
        }
        if matches!(
            order.evidence.status,
            crate::observation::UserOrderStatus::Resting
                | crate::observation::UserOrderStatus::Unknown
        ) {
            if let Some(total) = order.evidence.filled.checked_add(order.evidence.remaining) {
                if total < order.evidence.initial {
                    order.evidence.initial = total;
                }
            }
        }
        if pre_submit {
            let cid = cid.clone();
            let wire = order.wire.clone();
            self.stage_user_order(order.evidence, journal)?;
            if self
                .by_client
                .get(cid.as_str())
                .is_some_and(|o| matches!(o.state, OrderState::SubmitUnknown))
            {
                let _ = self.apply_staged_create_claim_and_prove::<C>(&cid, journal)?;
                let _ = self.recover_unknown_amend_from_user_order::<C>(
                    &wire,
                    journal.wall_now_ns(),
                    journal,
                )?;
                let _ = self.recover_unknown_amend_from_terminal_user_order::<C>(&wire, journal)?;
            }
            return Ok(true);
        }
        if self
            .public_fields
            .registry
            .prepare_user_order_received(&self.ledger, &order.wire, order.evidence.clone())
            .is_err()
        {
            self.quarantine_user_order(
                Some(OmsClientOrderId(cid.as_str().to_owned())),
                Some(order.wire),
                "stale_or_conflicting_order_version",
                journal,
            )?;
            return Ok(false);
        }
        self.record_user_order(&order.wire.0, order.evidence, journal)?;
        let _ = self.recover_unknown_amend_from_user_order::<C>(
            &order.wire,
            journal.wall_now_ns(),
            journal,
        )?;
        let _ = self.apply_user_order_terminal_if_complete::<C>(&order.wire, journal)?;
        if self
            .public_fields
            .registry
            .prepare_order_state_resting(&self.ledger, &order.wire, &self.public_fields.inventory)
            .is_ok()
        {
            self.prove_current_amend_resting::<C>(&order.wire.0, journal)?;
        }
        Ok(true)
    }
}
