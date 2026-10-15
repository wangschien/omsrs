use crate::lifecycle::Side;
use rust_decimal::Decimal;
use std::collections::BTreeSet;

pub fn invalid_book(bid: Option<Decimal>, ask: Option<Decimal>) -> Option<InvalidBook> {
    match (bid, ask) {
        (Some(bid), Some(ask)) if bid > ask => Some(InvalidBook::Crossed),
        (Some(bid), Some(ask)) if bid == ask => Some(InvalidBook::Locked),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AuthorityReason {
    AuthorityHold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GuardReason {
    DailyLoss,
    DailyLossUnprovable,
    WindowStop,
    MtmInvalid,
    StatusUnproven,
    StatusUnknown,
    TradingPaused,
    ExchangePaused,
    ScheduleClosed,
    ScheduleUnknown,
    FeeUnverified,
    InvalidBook,
    LimitsUnverified,
    IdentityUnverified,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdmissionReasons {
    pub authority: BTreeSet<AuthorityReason>,
    pub guards: BTreeSet<GuardReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionAction {
    New,
    CancelEntry,
    CancelExit,
    ExitKeep,
    ExitReduce,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetTrust {
    Untrusted,
    Trusted { reconciled_at_ns: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionFacts {
    pub net_trusted: NetTrust,
    pub before_pause: bool,
}

impl AdmissionReasons {
    pub fn is_empty(&self) -> bool {
        self.authority.is_empty() && self.guards.is_empty()
    }

    /// Intersect every independent reason. Clearing one reason never clears
    /// another reason or implicitly grants exit-cancellation authority.
    pub fn admits(&self, action: AdmissionAction, facts: AdmissionFacts) -> bool {
        admission_permits(&self.authority, &self.guards, action, facts)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidBook {
    Locked,
    Crossed,
}

/// Admission history fed by the canonical market-data owner's classification.
/// The outer None means incomplete data; the inner None means a complete,
/// healthy book. Incomplete data never clears an existing invalid-book hold.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BookGuard {
    pub current: Option<InvalidBook>,
    pub healthy_seen: bool,
    pub episodes: u64,
    pub invalid_updates: u64,
}

impl BookGuard {
    /// A one-sided book does not establish recovery from a prior invalid
    /// two-sided book.  A later valid two-sided update does.
    pub fn observe(&mut self, bid: Option<Decimal>, ask: Option<Decimal>) {
        if bid.is_none() || ask.is_none() {
            return;
        }
        let next = invalid_book(bid, ask);
        self.healthy_seen = next.is_none();
        if next.is_some() {
            self.invalid_updates = self.invalid_updates.saturating_add(1);
            if self.current.is_none() {
                self.episodes = self.episodes.saturating_add(1);
            }
        }
        self.current = next;
    }

    pub fn observe_class(&mut self, observation: Option<Option<InvalidBook>>) {
        let Some(next) = observation else { return };
        self.healthy_seen = next.is_none();
        if next.is_some() {
            self.invalid_updates = self.invalid_updates.saturating_add(1);
            if self.current.is_none() {
                self.episodes = self.episodes.saturating_add(1);
            }
        }
        self.current = next;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusObservation {
    Active,
    TradingPaused,
    ExchangePaused,
    Failed,
    WatchClosed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusGuard {
    consecutive_failures: u8,
    current: Option<GuardReason>,
}

impl Default for StatusGuard {
    fn default() -> Self {
        Self {
            consecutive_failures: 0,
            current: Some(GuardReason::StatusUnproven),
        }
    }
}

impl StatusGuard {
    pub fn current(self) -> Option<GuardReason> {
        self.current
    }

    pub fn completed_probe(&mut self, observed: StatusObservation) {
        match observed {
            StatusObservation::Active => {
                self.consecutive_failures = 0;
                self.current = None;
            }
            StatusObservation::TradingPaused => {
                self.consecutive_failures = 0;
                self.current = Some(GuardReason::TradingPaused);
            }
            StatusObservation::ExchangePaused => {
                self.consecutive_failures = 0;
                self.current = Some(GuardReason::ExchangePaused);
            }
            StatusObservation::Failed => {
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                if self.consecutive_failures >= 2 {
                    self.current = Some(GuardReason::StatusUnknown);
                }
            }
            StatusObservation::WatchClosed => {
                self.consecutive_failures = 2;
                self.current = Some(GuardReason::StatusUnknown);
            }
        }
    }
}

/// Two independent observation streams whose acknowledgements establish
/// transport liveness, without proving order or position authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ObservationChannel {
    Fill,
    OrderState,
}

/// The adapter classifies protocol error codes before passing neutral facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelFault {
    Rejected,
    Gap,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum HoldCause {
    FillUnsubscribed,
    OrderStateUnsubscribed,
    ChannelGap,
    ConnectionDead,
    NetMismatch,
    ReconcileFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChannelState {
    Absent,
    Subscribing { command_id: u64, since_ns: u64 },
    Subscribed { sid: u64 },
    Lost { since_ns: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelError {
    SubscribeRejected(ObservationChannel),
    ChannelLost(ObservationChannel),
    Unrelated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorityReleaseProof {
    pub book_healthy: bool,
    pub position_matches: bool,
    pub orders_match: bool,
    pub lineage_resolved: bool,
    pub reconciled_at_ns: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionObservation {
    Failed,
    Absent,
    Row {
        net: i64,
        updated_ts_ns: Option<u64>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionFreshness {
    Match,
    Mismatch,
    Stale,
    Failed,
}

impl PositionFreshness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Mismatch => "mismatch",
            Self::Stale => "stale",
            Self::Failed => "failed",
        }
    }
}

pub fn classify_position(
    read: PositionObservation,
    oms_net: i64,
    last_exec_ts_ns: u64,
) -> PositionFreshness {
    let floor_l = last_exec_ts_ns / 1_000_000_000 * 1_000_000_000;
    match read {
        PositionObservation::Failed => PositionFreshness::Failed,
        PositionObservation::Absent if last_exec_ts_ns > 0 => PositionFreshness::Stale,
        PositionObservation::Absent => {
            if oms_net == 0 {
                PositionFreshness::Match
            } else {
                PositionFreshness::Mismatch
            }
        }
        PositionObservation::Row { net, updated_ts_ns } => match updated_ts_ns {
            Some(t) if last_exec_ts_ns > 0 && t < floor_l => PositionFreshness::Stale,
            _ => {
                if net == oms_net {
                    PositionFreshness::Match
                } else {
                    PositionFreshness::Mismatch
                }
            }
        },
    }
}

#[derive(Clone, Debug)]
pub struct AuthorityHold {
    reasons: BTreeSet<HoldCause>,
    generation: u64,
    socket_live: bool,
    fill: ChannelState,
    order_state: ChannelState,
    last_reconciled: Option<(u64, u64)>,
    last_rest_net: Option<(u64, i64)>,
    since_ns: Option<u64>,
    entered_effects: Vec<HoldCause>,
}

impl Default for AuthorityHold {
    fn default() -> Self {
        Self {
            reasons: BTreeSet::new(),
            generation: 0,
            socket_live: false,
            fill: ChannelState::Absent,
            order_state: ChannelState::Absent,
            last_reconciled: None,
            last_rest_net: None,
            since_ns: None,
            entered_effects: Vec::new(),
        }
    }
}

impl AuthorityHold {
    pub fn reasons(&self) -> &BTreeSet<HoldCause> {
        &self.reasons
    }
    pub fn is_held(&self) -> bool {
        !self.reasons.is_empty()
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn needs_reconnect(&self) -> bool {
        !self.socket_live
    }
    pub fn last_rest_net(&self) -> Option<i64> {
        self.last_rest_net
            .and_then(|(g, n)| (g == self.generation).then_some(n))
    }
    pub fn trusted_at(&self) -> Option<u64> {
        (!self.is_held())
            .then_some(self.last_reconciled?)
            .and_then(|(g, t)| (g == self.generation).then_some(t))
    }
    pub fn enter(&mut self, reason: HoldCause, now_ns: u64) {
        let entering_hold = self.reasons.is_empty();
        if reason == HoldCause::ConnectionDead {
            self.socket_live = false;
            self.fill = ChannelState::Lost { since_ns: now_ns };
            self.order_state = ChannelState::Lost { since_ns: now_ns };
        }
        if self.reasons.is_empty() {
            self.since_ns = Some(now_ns);
        }
        self.reasons.insert(reason);
        self.last_reconciled = None;
        if entering_hold {
            self.entered_effects.push(reason);
        }
    }
    pub fn begin_generation(&mut self, now_ns: u64) -> bool {
        let Some(next) = self.generation.checked_add(1) else {
            self.enter(HoldCause::ConnectionDead, now_ns);
            return false;
        };
        self.generation = next;
        self.socket_live = true;
        self.fill = ChannelState::Absent;
        self.order_state = ChannelState::Absent;
        self.last_rest_net = None;
        self.reasons.remove(&HoldCause::ConnectionDead);
        self.enter(HoldCause::FillUnsubscribed, now_ns);
        self.enter(HoldCause::OrderStateUnsubscribed, now_ns);
        true
    }
    fn state(&self, channel: ObservationChannel) -> ChannelState {
        match channel {
            ObservationChannel::Fill => self.fill,
            ObservationChannel::OrderState => self.order_state,
        }
    }
    fn state_mut(&mut self, channel: ObservationChannel) -> &mut ChannelState {
        match channel {
            ObservationChannel::Fill => &mut self.fill,
            ObservationChannel::OrderState => &mut self.order_state,
        }
    }
    fn unsubscribed_cause(channel: ObservationChannel) -> HoldCause {
        match channel {
            ObservationChannel::Fill => HoldCause::FillUnsubscribed,
            ObservationChannel::OrderState => HoldCause::OrderStateUnsubscribed,
        }
    }
    pub fn subscribe_started(&mut self, channel: ObservationChannel, command_id: u64, now_ns: u64) {
        *self.state_mut(channel) = ChannelState::Subscribing {
            command_id,
            since_ns: now_ns,
        };
        self.enter(Self::unsubscribed_cause(channel), now_ns);
    }
    pub fn subscribe_failed(&mut self, channel: ObservationChannel, now_ns: u64) {
        *self.state_mut(channel) = ChannelState::Lost { since_ns: now_ns };
        self.enter(Self::unsubscribed_cause(channel), now_ns);
    }
    pub fn observe_ack(
        &mut self,
        channel: Option<ObservationChannel>,
        command_id: Option<u64>,
        sid: u64,
    ) -> bool {
        let Some(channel) = channel else {
            return false;
        };
        let ChannelState::Subscribing {
            command_id: expected,
            ..
        } = self.state(channel)
        else {
            return false;
        };
        if command_id.is_some_and(|id| id != expected) {
            return false;
        }
        *self.state_mut(channel) = ChannelState::Subscribed { sid };
        true
    }
    pub fn on_error(
        &mut self,
        command_id: Option<u64>,
        sid: Option<u64>,
        fault: ChannelFault,
        now_ns: u64,
    ) -> ChannelError {
        for channel in [ObservationChannel::Fill, ObservationChannel::OrderState] {
            if let ChannelState::Subscribing {
                command_id: expected,
                ..
            } = self.state(channel)
            {
                if command_id == Some(expected) {
                    self.subscribe_failed(channel, now_ns);
                    if fault == ChannelFault::Gap {
                        self.enter(HoldCause::ChannelGap, now_ns);
                        return ChannelError::ChannelLost(channel);
                    }
                    return ChannelError::SubscribeRejected(channel);
                }
            }
        }
        if fault == ChannelFault::Gap {
            for channel in [ObservationChannel::Fill, ObservationChannel::OrderState] {
                if matches!(self.state(channel), ChannelState::Subscribed { sid: current } if Some(current) == sid)
                {
                    self.subscribe_failed(channel, now_ns);
                    self.enter(HoldCause::ChannelGap, now_ns);
                    return ChannelError::ChannelLost(channel);
                }
            }
        }
        ChannelError::Unrelated
    }
    pub fn ack_expired(&mut self, now_ns: u64, bound_ns: u64) -> bool {
        let mut expired = false;
        for channel in [ObservationChannel::Fill, ObservationChannel::OrderState] {
            if matches!(self.state(channel), ChannelState::Subscribing { since_ns, .. } if now_ns.saturating_sub(since_ns) >= bound_ns)
            {
                self.subscribe_failed(channel, now_ns);
                expired = true;
            }
        }
        expired
    }
    pub fn expiring_ack_channels(
        &self,
        now_ns: u64,
        bound_ns: u64,
    ) -> Vec<(ObservationChannel, u64)> {
        [ObservationChannel::Fill, ObservationChannel::OrderState]
            .into_iter()
            .filter_map(|channel| match self.state(channel) {
                ChannelState::Subscribing {
                    command_id,
                    since_ns,
                } if now_ns.saturating_sub(since_ns) >= bound_ns => Some((channel, command_id)),
                ChannelState::Subscribing { .. }
                | ChannelState::Absent
                | ChannelState::Subscribed { .. }
                | ChannelState::Lost { .. } => None,
            })
            .collect()
    }
    pub fn channels_healthy(&self) -> bool {
        matches!(self.fill, ChannelState::Subscribed { .. })
            && matches!(self.order_state, ChannelState::Subscribed { .. })
    }
    pub fn needs_resubscribe(&self, channel: ObservationChannel) -> bool {
        matches!(
            self.state(channel),
            ChannelState::Lost { .. } | ChannelState::Absent
        )
    }
    pub fn retry_due(&self, channel: ObservationChannel, now_ns: u64, backoff_ns: u64) -> bool {
        match self.state(channel) {
            ChannelState::Absent => true,
            ChannelState::Lost { since_ns } => now_ns.saturating_sub(since_ns) >= backoff_ns,
            _ => false,
        }
    }
    pub fn next_channel_due_in_ns(&self, now_ns: u64, bound_ns: u64) -> u64 {
        // Both channels Subscribed means no pending channel ACK/retry deadline.
        // MAX is the neutral sentinel for the consumer contract wait.min(due),
        // so other deadlines still wake the caller. This is an Option over the
        // enumerated timer states, not a discarded failure. Position/order/
        // lineage reconciliation and hold release remain separate checks;
        // MAX neither proves their completion nor clears an authority hold.
        [self.fill, self.order_state]
            .into_iter()
            .filter_map(|state| match state {
                ChannelState::Subscribing { since_ns, .. } | ChannelState::Lost { since_ns } => {
                    Some(since_ns.saturating_add(bound_ns).saturating_sub(now_ns))
                }
                ChannelState::Absent => Some(0),
                ChannelState::Subscribed { .. } => None,
            })
            .min()
            .unwrap_or(u64::MAX)
    }
    pub fn observe_position_result(&mut self, rest_net: Option<i64>, oms_net: i64, now_ns: u64) {
        match rest_net {
            Some(net) => {
                self.last_rest_net = Some((self.generation, net));
                if net != oms_net {
                    self.enter(HoldCause::NetMismatch, now_ns);
                }
            }
            None => {
                self.enter(HoldCause::ReconcileFailed, now_ns);
            }
        }
    }
    pub fn observe_position_class(
        &mut self,
        class: PositionFreshness,
        rest_net: Option<i64>,
        oms_net: i64,
        now_ns: u64,
    ) {
        match class {
            PositionFreshness::Stale => {}
            PositionFreshness::Failed => self.observe_position_result(None, oms_net, now_ns),
            PositionFreshness::Match | PositionFreshness::Mismatch => {
                self.observe_position_result(rest_net, oms_net, now_ns)
            }
        }
    }
    pub fn socket_live(&self) -> bool {
        self.socket_live
    }
    pub fn try_release(&mut self, proof: AuthorityReleaseProof) -> bool {
        if !self.socket_live
            || !proof.book_healthy
            || !self.channels_healthy()
            || !proof.position_matches
            || !proof.orders_match
            || !proof.lineage_resolved
            || self
                .last_rest_net
                .map_or(true, |(g, _)| g != self.generation)
        {
            return false;
        }
        self.reasons.clear();
        self.since_ns = None;
        self.last_reconciled = Some((self.generation, proof.reconciled_at_ns));
        true
    }
}
impl AuthorityHold {
    /// Ordered notification facts for an external IO adapter. No logging,
    /// callback, clock, transport or journal operation runs in this owner.
    pub fn take_entered_effects(&mut self) -> Vec<HoldCause> {
        std::mem::take(&mut self.entered_effects)
    }
}

// One reducer shared by the native and compatibility field names.
pub(crate) fn admission_permits(
    authority: &BTreeSet<AuthorityReason>,
    guards: &BTreeSet<GuardReason>,
    action: AdmissionAction,
    facts: AdmissionFacts,
) -> bool {
    if action == AdmissionAction::CancelExit {
        return false;
    }
    let trusted_net = authority.is_empty() && matches!(facts.net_trusted, NetTrust::Trusted { .. });
    authority.iter().all(|reason| match reason {
        AuthorityReason::AuthorityHold => match action {
            AdmissionAction::New | AdmissionAction::ExitReduce => false,
            AdmissionAction::CancelEntry | AdmissionAction::ExitKeep => true,
            AdmissionAction::CancelExit => false,
        },
    }) && guards.iter().all(|reason| {
        use AdmissionAction::{CancelEntry, CancelExit, ExitKeep, ExitReduce, New};
        match reason {
            GuardReason::IdentityUnverified => false,
            GuardReason::ExchangePaused => action == ExitKeep,
            GuardReason::TradingPaused
            | GuardReason::InvalidBook
            | GuardReason::ScheduleUnknown => {
                matches!(action, CancelEntry | ExitKeep)
            }
            GuardReason::ScheduleClosed => match action {
                New | CancelExit => false,
                CancelEntry | ExitKeep => true,
                ExitReduce => facts.before_pause,
            },
            GuardReason::StatusUnproven
            | GuardReason::StatusUnknown
            | GuardReason::DailyLoss
            | GuardReason::DailyLossUnprovable
            | GuardReason::WindowStop
            | GuardReason::MtmInvalid
            | GuardReason::FeeUnverified
            | GuardReason::LimitsUnverified => match action {
                New | CancelExit => false,
                CancelEntry | ExitKeep => true,
                ExitReduce => trusted_net,
            },
        }
    })
}

pub fn reducing_side(side: Side, net_centi: i64) -> bool {
    match net_centi {
        n if n > 0 => side == Side::SellYes,
        n if n < 0 => side == Side::BuyYes,
        _ => false,
    }
}

pub fn exit_reduce_action(
    side: Side,
    qty_centi: u64,
    net_centi: i64,
    reduce_only: bool,
) -> AdmissionAction {
    if reduce_only && reducing_side(side, net_centi) && net_centi.unsigned_abs() >= qty_centi {
        AdmissionAction::ExitReduce
    } else {
        AdmissionAction::New
    }
}

pub fn entry_identity_row<'a, R, Stage>(
    oms: &'a super::live_state::LiveOmsBookState<R, Stage>,
    cid: &str,
) -> Option<&'a super::live_state::LiveOmsOrder<R>> {
    let row = oms.by_client.get(cid)?;
    if row.ctx.client_order_id.0 != cid || row.state.is_submission_closed() {
        return None;
    }
    let wire = row.ctx.venue_order_id.as_ref()?;
    let known = oms.public_fields.registry.get(oms.ledger(), wire)?;
    if known.authority_hold
        || known.amend_lineage.is_some()
        || known.observed_exchange_index != oms.public_fields.expected_exchange_index
        || known.proved_current_cid.as_ref() != Some(&row.ctx.client_order_id)
        || !known.members.iter().any(|member| {
            member.role == crate::observation::MemberRole::Current
                && member.cid == row.ctx.client_order_id
        })
    {
        return None;
    }
    Some(row)
}

pub fn entry_matches_net(side: Side, net_centi: i64) -> bool {
    match side {
        Side::BuyYes => net_centi >= 0,
        Side::SellYes => net_centi <= 0,
    }
}
