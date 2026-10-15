//! Neutral protective-action values and deterministic action construction.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveFuseReason {
    UnreconciledPrivateFill,
    MissingVenueFeeCost,
    SpotSourceUnhealthy,
    WebsocketFrameTimeout,
    UnknownVenueOrder,
    UnknownLocalOrder,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveFuseAction<C> {
    pub reason: LiveFuseReason,
    pub detail: String,
    pub halt_new_opening_quotes: bool,
    pub cancel_client_order_ids: Vec<C>,
    pub cancel_venue_order_ids: Vec<String>,
    pub allow_flatten: bool,
}

impl<C> LiveFuseAction<C> {
    pub fn new(
        reason: LiveFuseReason,
        detail: impl Into<String>,
        cancel_client_order_ids: Vec<C>,
    ) -> Self {
        Self {
            reason,
            detail: detail.into(),
            halt_new_opening_quotes: true,
            cancel_client_order_ids,
            cancel_venue_order_ids: Vec::new(),
            allow_flatten: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveVenueOrderIdentity<C> {
    pub order_id: String,
    pub client_order_id: C,
}

pub fn fuse_for_missing_venue_fee<C: Clone>(
    fill_id: &str,
    bot_owned_resting_orders: &[C],
) -> LiveFuseAction<C> {
    LiveFuseAction::new(
        LiveFuseReason::MissingVenueFeeCost,
        format!("REST fill {fill_id} is missing venue fee_cost"),
        bot_owned_resting_orders.to_vec(),
    )
}

pub fn fuse_for_unknown_venue_orders<C: Clone>(
    unknown_venue_orders: &[LiveVenueOrderIdentity<C>],
) -> Option<LiveFuseAction<C>> {
    if unknown_venue_orders.is_empty() {
        return None;
    }
    let mut action = LiveFuseAction::new(
        LiveFuseReason::UnknownVenueOrder,
        format!(
            "{} bot-owned venue order(s) are not tracked locally",
            unknown_venue_orders.len()
        ),
        unknown_venue_orders
            .iter()
            .map(|order| order.client_order_id.clone())
            .collect(),
    );
    action.cancel_venue_order_ids = unknown_venue_orders
        .iter()
        .map(|order| order.order_id.clone())
        .collect();
    Some(action)
}

pub fn fuse_for_unknown_local_orders<C: Clone>(
    missing_local_order_ids: &[C],
    bot_owned_resting_orders: &[C],
) -> Option<LiveFuseAction<C>> {
    if missing_local_order_ids.is_empty() {
        return None;
    }
    Some(LiveFuseAction::new(
        LiveFuseReason::UnknownLocalOrder,
        format!(
            "{} local order(s) are missing from venue reconciliation",
            missing_local_order_ids.len()
        ),
        bot_owned_resting_orders.to_vec(),
    ))
}

pub fn fuse_for_websocket_frame_timeout<C: Clone>(
    timeout_ms: u64,
    bot_owned_resting_orders: &[C],
) -> LiveFuseAction<C> {
    LiveFuseAction::new(
        LiveFuseReason::WebsocketFrameTimeout,
        format!("websocket frame timeout after {timeout_ms} ms"),
        bot_owned_resting_orders.to_vec(),
    )
}

#[derive(Debug, Clone, Copy)]
pub struct PositionFeeFacts {
    pub fees_paid: Decimal,
    pub position_fee_cost: Decimal,
}
#[derive(Debug, Clone, Copy)]
pub struct SpotHealthFacts {
    pub healthy: bool,
    pub price_present: bool,
    pub samples: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveFeeCrossCheck {
    pub rest_fee_sum: Decimal,
    pub ws_fees_paid: Decimal,
    pub ws_position_fee_cost: Decimal,
    pub ws_total_fee_view: Decimal,
    pub covers_rest_fees: bool,
}

pub fn cross_check_position_fees(
    snapshot: PositionFeeFacts,
    rest_fee_sum: Decimal,
) -> LiveFeeCrossCheck {
    let ws_total_fee_view = snapshot.fees_paid + snapshot.position_fee_cost;
    LiveFeeCrossCheck {
        rest_fee_sum,
        ws_fees_paid: snapshot.fees_paid,
        ws_position_fee_cost: snapshot.position_fee_cost,
        ws_total_fee_view,
        covers_rest_fees: ws_total_fee_view >= rest_fee_sum,
    }
}

pub fn fuse_for_unreconciled_private_fill<C: Clone>(
    trade_id: &str,
    bot_owned_resting_orders: &[C],
) -> LiveFuseAction<C> {
    LiveFuseAction::new(
        LiveFuseReason::UnreconciledPrivateFill,
        format!("private fill {} lacks reconciled REST fee_cost", trade_id),
        bot_owned_resting_orders.to_vec(),
    )
}

pub fn fuse_for_spot_health<C: Clone>(
    spot: SpotHealthFacts,
    bot_owned_resting_orders: &[C],
) -> Option<LiveFuseAction<C>> {
    (!spot.healthy).then(|| {
        LiveFuseAction::new(
            LiveFuseReason::SpotSourceUnhealthy,
            format!(
                "spot source unhealthy: spot_price_present={} samples={}",
                spot.price_present, spot.samples
            ),
            bot_owned_resting_orders.to_vec(),
        )
    })
}
