//! Disposable open-order projection and ordered reconciliation.
use super::journal_projection::resting_wire_for_open_projection;
use super::live_cover::CoverageClient;
use super::live_state::LiveOmsBookState;
use crate::lifecycle::{ClientOrderId as OmsClientOrderId, OrderState};
use crate::observation::{MemberRole, WorkingView};
use serde::{Deserialize, Serialize};

pub trait ProjectionCancelInput<C> {
    fn client_order_id(&self) -> &C;
}
pub trait ProjectionCreateInput<C>: ProjectionCancelInput<C> {
    fn resting(&self) -> bool;
    fn wire_order_id(&self) -> &String;
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveBotOrderRef<C> {
    pub client_order_id: C,
    pub order_id: String,
    #[serde(default)]
    pub order_age_anchor_ns: Option<u64>,
    #[serde(default)]
    pub venue_known_filled_centi: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueOpenOrderRef<C> {
    pub wire_order_id: String,
    pub client_order_id: Option<C>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenSetDrift<C> {
    HostOnly {
        client_order_id: C,
        wire_order_id: String,
    },
    VenueOnly {
        wire_order_id: String,
        client_order_id: Option<C>,
    },
    CancelPendingStillOpen {
        client_order_id: C,
        wire_order_id: String,
    },
    ClientWireMismatch {
        client_order_id: C,
        host_wire: String,
        venue_wire: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSetReconcileReport<C> {
    pub matched: u64,
    pub drifts: Vec<OpenSetDrift<C>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestOrderSnapshot {
    pub order_id: String,
    pub client_order_id: String,
    pub status: String,
    pub ticker: Option<String>,
}

impl<C> Default for OpenSetReconcileReport<C> {
    fn default() -> Self {
        Self {
            matched: 0,
            drifts: Vec::new(),
        }
    }
}
#[derive(Clone)]
pub struct OpenProjection<C> {
    orders: Vec<LiveBotOrderRef<C>>,
    cancel_pending: std::collections::HashSet<C>,
}
impl<C> Default for OpenProjection<C> {
    fn default() -> Self {
        Self {
            orders: Vec::new(),
            cancel_pending: std::collections::HashSet::new(),
        }
    }
}

impl<C: CoverageClient + Clone + Eq + std::hash::Hash> OpenProjection<C> {
    pub const IS_PROJECTION_ONLY: bool = true;
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn is_projection_only(&self) -> bool {
        Self::IS_PROJECTION_ONLY
    }

    pub fn as_slice(&self) -> &[LiveBotOrderRef<C>] {
        &self.orders
    }

    pub fn len(&self) -> usize {
        self.orders.len()
    }

    pub fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &LiveBotOrderRef<C>> {
        self.orders.iter()
    }

    pub fn upsert(&mut self, client_order_id: C, wire_order_id: impl Into<String>) {
        self.upsert_age(client_order_id, wire_order_id, None);
    }

    pub fn upsert_age(
        &mut self,
        client_order_id: C,
        wire_order_id: impl Into<String>,
        order_age_anchor_ns: Option<u64>,
    ) {
        self.upsert_filled(client_order_id, wire_order_id, order_age_anchor_ns, 0);
    }

    pub fn upsert_filled(
        &mut self,
        client_order_id: C,
        wire_order_id: impl Into<String>,
        order_age_anchor_ns: Option<u64>,
        venue_known_filled_centi: u64,
    ) {
        let wire = wire_order_id.into();
        self.orders
            .retain(|o| o.client_order_id != client_order_id && o.order_id != wire);
        self.orders.push(LiveBotOrderRef {
            client_order_id,
            order_id: wire,
            order_age_anchor_ns,
            venue_known_filled_centi,
        });
    }

    pub fn mark_cancel_pending(&mut self, client_order_id: C) {
        self.cancel_pending.insert(client_order_id);
    }

    pub fn cancel_pending_sorted(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .cancel_pending
            .iter()
            .map(|c| c.as_str().to_string())
            .collect();
        v.sort();
        v
    }

    pub fn remove_client(&mut self, client_order_id: &C) {
        self.orders
            .retain(|o| &o.client_order_id != client_order_id);
    }

    pub fn apply_report<'a, A: ProjectionCancelInput<C> + 'a, B: ProjectionCreateInput<C> + 'a>(
        &mut self,
        cancels: impl IntoIterator<Item = &'a A>,
        creates: impl IntoIterator<Item = &'a B>,
    ) {
        for c in cancels {
            self.orders
                .retain(|o| o.client_order_id != *c.client_order_id());
        }
        for c in creates {
            self.orders
                .retain(|o| o.client_order_id != *c.client_order_id());
            if c.resting() {
                self.orders.push(bot_ref_from_create(c));
            }
        }
    }

    pub fn with_report_applied<
        'a,
        A: ProjectionCancelInput<C> + 'a,
        B: ProjectionCreateInput<C> + 'a,
    >(
        &self,
        cancels: impl IntoIterator<Item = &'a A>,
        creates: impl IntoIterator<Item = &'a B>,
    ) -> Self {
        let mut next = self.clone();
        next.apply_report(cancels, creates);
        next
    }

    pub fn reconcile_with_venue(
        &self,
        venue: &[VenueOpenOrderRef<C>],
    ) -> OpenSetReconcileReport<C> {
        let mut report = reconcile_open_sets(self.as_slice(), venue);
        for h in self.as_slice() {
            if !self.cancel_pending.contains(&h.client_order_id) {
                continue;
            }
            let still = venue.iter().any(|v| {
                v.client_order_id.as_ref() == Some(&h.client_order_id)
                    || v.wire_order_id == h.order_id
            });
            if still {
                report.drifts.push(OpenSetDrift::CancelPendingStillOpen {
                    client_order_id: h.client_order_id.clone(),
                    wire_order_id: h.order_id.clone(),
                });
            }
        }
        report
    }
}
impl<C> OpenSetReconcileReport<C> {
    pub fn is_clean(&self) -> bool {
        self.drifts.is_empty()
    }

    pub fn needs_attention(&self) -> bool {
        !self.drifts.is_empty()
    }

    pub fn host_only_count(&self) -> usize {
        self.drifts
            .iter()
            .filter(|d| matches!(d, OpenSetDrift::HostOnly { .. }))
            .count()
    }

    pub fn venue_only_count(&self) -> usize {
        self.drifts
            .iter()
            .filter(|d| matches!(d, OpenSetDrift::VenueOnly { .. }))
            .count()
    }
}
pub fn bot_ref_from_create<C: Clone, A: ProjectionCreateInput<C>>(ack: &A) -> LiveBotOrderRef<C> {
    LiveBotOrderRef {
        client_order_id: ack.client_order_id().clone(),
        order_id: ack.wire_order_id().clone(),
        order_age_anchor_ns: None,
        venue_known_filled_centi: 0,
    }
}

pub fn is_open_resting_status(status: &str) -> bool {
    matches!(
        status.to_ascii_lowercase().as_str(),
        "resting" | "open" | "pending" | "active"
    )
}

pub fn venue_open_ref_from_rest_fields<C: CoverageClient>(
    order_id: &str,
    client_order_id: &str,
    status: &str,
) -> Option<VenueOpenOrderRef<C>> {
    let wire = order_id.trim();
    if wire.is_empty() || !is_open_resting_status(status) {
        return None;
    }
    let client = {
        let c = client_order_id.trim();
        if c.is_empty() {
            None
        } else {
            C::from_string(c.to_string())
        }
    };
    Some(VenueOpenOrderRef {
        wire_order_id: wire.to_string(),
        client_order_id: client,
    })
}

pub fn venue_open_refs_from_rest_orders<C: CoverageClient>(
    orders: impl IntoIterator<Item = RestOrderSnapshot>,
    market_ticker: Option<&str>,
) -> Vec<VenueOpenOrderRef<C>> {
    use std::collections::HashSet;
    let mut out = Vec::new();
    let mut seen_wire = HashSet::new();
    for o in orders {
        if let Some(want) = market_ticker {
            if let Some(got) = o.ticker.as_deref() {
                if got != want {
                    continue;
                }
            }
        }
        if let Some(r) = venue_open_ref_from_rest_fields(&o.order_id, &o.client_order_id, &o.status)
        {
            if seen_wire.insert(r.wire_order_id.clone()) {
                out.push(r);
            }
        }
    }
    out
}

pub fn venue_open_refs_from_rest_pages<C: CoverageClient>(
    pages: impl IntoIterator<Item = Vec<RestOrderSnapshot>>,
    market_ticker: Option<&str>,
) -> Vec<VenueOpenOrderRef<C>> {
    venue_open_refs_from_rest_orders::<C>(pages.into_iter().flatten(), market_ticker)
}

pub fn reconcile_open_sets<C: CoverageClient + Clone>(
    host: &[LiveBotOrderRef<C>],
    venue: &[VenueOpenOrderRef<C>],
) -> OpenSetReconcileReport<C> {
    use std::collections::{HashMap, HashSet};

    let mut by_client: HashMap<String, &LiveBotOrderRef<C>> = HashMap::new();
    let mut by_wire: HashMap<String, &LiveBotOrderRef<C>> = HashMap::new();
    for h in host {
        by_client.insert(h.client_order_id.as_str().to_string(), h);
        by_wire.insert(h.order_id.clone(), h);
    }

    let mut matched_host: HashSet<String> = HashSet::new();
    let mut matched_venue: HashSet<usize> = HashSet::new();
    let mut drifts = Vec::new();
    let mut matched = 0u64;

    for (vi, v) in venue.iter().enumerate() {
        if let Some(cid) = &v.client_order_id {
            if let Some(h) = by_client.get(cid.as_str()) {
                if h.order_id == v.wire_order_id {
                    matched = matched.saturating_add(1);
                } else {
                    drifts.push(OpenSetDrift::ClientWireMismatch {
                        client_order_id: cid.clone(),
                        host_wire: h.order_id.clone(),
                        venue_wire: v.wire_order_id.clone(),
                    });
                }
                matched_host.insert(h.client_order_id.as_str().to_string());
                matched_venue.insert(vi);
                continue;
            }
        }
        if let Some(h) = by_wire.get(&v.wire_order_id) {
            matched = matched.saturating_add(1);
            matched_host.insert(h.client_order_id.as_str().to_string());
            matched_venue.insert(vi);
        }
    }

    for h in host {
        if !matched_host.contains(h.client_order_id.as_str()) {
            drifts.push(OpenSetDrift::HostOnly {
                client_order_id: h.client_order_id.clone(),
                wire_order_id: h.order_id.clone(),
            });
        }
    }
    for (vi, v) in venue.iter().enumerate() {
        if !matched_venue.contains(&vi) {
            drifts.push(OpenSetDrift::VenueOnly {
                wire_order_id: v.wire_order_id.clone(),
                client_order_id: v.client_order_id.clone(),
            });
        }
    }

    OpenSetReconcileReport { matched, drifts }
}
impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn to_open_projection_filtered<C: CoverageClient + Clone + Eq + std::hash::Hash>(
        &self,
    ) -> OpenProjection<C> {
        let mut open = OpenProjection::<C>::new();
        for o in self.by_client.values() {
            if o.venue_working_centi == Some(0) || o.ctx.last_venue_remaining_qty == Some(0) {
                continue;
            }
            let Some(wire) = resting_wire_for_open_projection(&o.state) else {
                continue;
            };
            let cid = OmsClientOrderId(o.ctx.client_order_id.as_str().to_string());
            if matches!(
                self.public_fields
                    .registry
                    .member_working_view(&self.ledger, &cid),
                WorkingView::Known(0)
            ) {
                continue;
            }
            if let Some(ws) = self.public_fields.registry.get(&self.ledger, &wire) {
                if ws.members.iter().any(|m| {
                    m.cid.0 == o.ctx.client_order_id.as_str()
                        && matches!(m.role, MemberRole::Superseded | MemberRole::Terminal)
                }) {
                    continue;
                }
            }
            let Ok(cid) = C::try_from_string(o.ctx.client_order_id.as_str().to_string()) else {
                continue;
            };
            if matches!(o.state, OrderState::CancelPending { .. }) {
                open.mark_cancel_pending(cid.clone());
            }
            open.upsert_filled(
                cid,
                wire.0.clone(),
                o.order_age_anchor_ns,
                o.ctx.attributed_fill_qty,
            );
        }
        open
    }
}
