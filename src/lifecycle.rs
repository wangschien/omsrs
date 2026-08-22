//! Order lifecycle state machine + client↔venue id mapping (OMS core).
//!
//! Moved from kbot `oms_state` into **omsrs** so platform-agnostic order
//! authority lives here (GROK_NEXT_O1_O2_OMSRS_CORE O1). Pure: no I/O.
//!
//! OMS per-order lifecycle state machine — pure deterministic core (§6 B/B2/C/D).
//!
//! **Scope (this module only):**
//! - per-order lifecycle transitions (§6.B)
//! - `fill_id` idempotent dedup with payload conflict detection
//! - immediate-fill decision without authoritative `fill_id` (§6.B2)
//! - `SubmitUnknown` reconcile matching (§6.C)
//! - deterministic `client_order_id` derivation (§6.A/C)
//!
//! **Not in this module:** journal fsync / HTTP / WS I/O, portfolio-layer
//! cross-market reservation aggregation, fencing leases, startup authority
//! fetch (shell injects records; this module only matches / accounts).
//!
//! Pure functions only: no I/O, no wall clock, no network, no files. Every
//! timestamp / `attempt_id` / `venue_order_id` / `fill_id` arrives on events.
//! [`Effect`] values are pure descriptions for the I/O shell — never executed here.
//!
//! Mirror types (`CancelOutcome`, …) are defined locally so this module stays
//! isolatable and independently committable (no connector/omsrs I/O imports).
//!
//! **Structural invariants (true-money, centralized):**
//! - [`try_finalize_terminal`] is the **only** producer of `Effect::ReleaseReservation`
//!   and the only writer of true terminal states (Filled/Canceled/Terminal) with release.
//! - [`OrderCtx::fill_obligation`] is a monotonic high-water mark that must equal
//!   `attributed_fill_qty` before release.
//! - [`OrderCtx::venue_order_id`] is durable identity across all states.
//! - [`OrderCtx::authority_complete`] is a **generation-scoped** latch (F1/G1): the release
//!   gate reads **only** ctx (never a call-site bool). Fill-admitting activity (cancel,
//!   obligation raise with lag, new fill_id) invalidates it; `CancelOutcome` never latches it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// ─── Identity / value types ───────────────────────────────────────────────────

/// Deterministic client order id — **the** recovery key for `SubmitUnknown`
/// (may never have observed a venue `order_id`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClientOrderId(pub String);

impl ClientOrderId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ClientOrderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Venue order id (opaque string from create response / backfill).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VenueOrderId(pub String);

/// Authoritative fill id (WS or GET fills). Journal only accounts by this key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FillId(pub String);

/// Submit attempt id (shell-supplied; durable in `SubmitStarted` before HTTP).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttemptId(pub String);

/// Local side mirror (isolated from connector/strategy types).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    BuyYes,
    SellYes,
}

/// Deterministic client_order_id derivation (§6.A/C).
///
/// Same `(market, strategy, seq)` always yields the same id so restart /
/// backfill can recompute and match. This is the **only** recovery key when
/// venue `order_id` was never observed.
pub fn derive_client_order_id(market: &str, strategy: &str, seq: u64) -> ClientOrderId {
    ClientOrderId(format!("{market}|{strategy}|{seq}"))
}

// ─── Order state (§6.B + B2 + C) ──────────────────────────────────────────────

/// Per-order lifecycle state (typed; exhaustive for the pure core).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderState {
    /// Pre-prepare: no durable intent yet.
    New,
    /// `append+fsync SubmitPrepared` done; HTTP not yet authorized.
    SubmitPrepared,
    /// `append+fsync SubmitStarted` done **before** HTTP send.
    /// Crash here ⇒ "may have been sent" — never silently infer never-sent.
    SubmitStarted { attempt_id: AttemptId },
    /// Submit timeout / transport Unknown — persisted, **never** auto-resubmit.
    SubmitUnknown,
    /// Live on venue with zero attributed fills (or after full attribution of zero).
    Accepted { venue_order_id: VenueOrderId },
    /// Live with some attributed fills.
    Partial {
        venue_order_id: VenueOrderId,
        filled_qty: u64,
        remaining_qty: u64,
    },
    /// Fully filled **and** attributed by `fill_id` (true terminal).
    Filled,
    /// Cancel requested; awaiting typed outcome / reconcile.
    ///
    /// Carries optional **unattributed obligation** from create response (E1):
    /// when `response_fill_count` is set and `attributed < response_fill_count`,
    /// cancel outcomes must **not** release — route to reconcile first.
    CancelPending {
        venue_order_id: VenueOrderId,
        filled_qty: u64,
        remaining_qty: u64,
        /// Response fill_count obligation (from ImmediateFillUnattributed cancel).
        response_fill_count: Option<u64>,
        response_avg_price_cents: Option<u64>,
        response_fee_cents: Option<u64>,
        /// E5: set when cancel-reconcile already knows the venue terminal intent.
        reconcile_target: Option<ReconcileTarget>,
    },
    /// Non-terminal: venue terminal status known but fill attribution incomplete (E5).
    /// Accepts Fill / ReconcileResult until attributed == target.venue_filled_qty,
    /// then finalizes to Canceled or Filled (+ release) via [`try_finalize_terminal`].
    ReconcilePending {
        venue_order_id: VenueOrderId,
        filled_qty: u64,
        remaining_qty: u64,
        target: ReconcileTarget,
        response_fill_count: Option<u64>,
        response_avg_price_cents: Option<u64>,
        response_fee_cents: Option<u64>,
    },
    /// Cancel confirmed terminal.
    Canceled,
    /// Generic reconciled terminal (e.g. AlreadyTerminal on cancel).
    Terminal,
    /// §6.B2: create response `fill_count > 0` but no authoritative `fill_id` yet.
    ImmediateFillUnattributed {
        venue_order_id: VenueOrderId,
        /// Response fill_count (contracts) — cross-check only, never book from this.
        response_fill_count: u64,
        response_remaining_count: u64,
        response_avg_price_cents: Option<u64>,
        response_fee_cents: Option<u64>,
    },
    /// §6.B2: backfill deadline elapsed still without `fill_id` → HALT.
    ImmediateFillUnresolved,
    /// §6.C.4: exhaustive backfill found no matching client_order_id → HALT.
    UnknownNoMatch,
    /// Explicit halt with reason (cross-check mismatch, unproven resubmit, …).
    Halted { reason: HaltReason },
}

impl OrderState {
    pub fn is_terminal_or_halt(&self) -> bool {
        matches!(
            self,
            OrderState::Filled
                | OrderState::Canceled
                | OrderState::Terminal
                | OrderState::ImmediateFillUnresolved
                | OrderState::UnknownNoMatch
                | OrderState::Halted { .. }
        )
    }
}

// ─── Reconcile target (E5) ────────────────────────────────────────────────────

/// Known venue terminal intent while still reconciling fill attribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReconcileTerminal {
    Canceled,
    Filled,
}

/// Target final state after attribution catches up to venue_filled_qty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileTarget {
    pub terminal: ReconcileTerminal,
    pub venue_filled_qty: u64,
    /// G3: last venue-reported remaining for this terminal intent.
    /// `Some(r)` is durable evidence for the Filled remaining gate — never replace
    /// with a local synthetic 0 on enrichment. `None` = remaining not observed.
    pub venue_remaining_qty: Option<u64>,
}

// ─── Halt / reject reasons ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HaltReason {
    /// Response fill_count / remaining disagree with fill_id aggregate.
    ImmediateFillCrossCheckMismatch {
        response_fill_count: u64,
        attributed_fill_qty: u64,
        response_remaining_count: u64,
        implied_remaining: u64,
    },
    /// qty / avg_price / fee cross-check mismatch (structured detail).
    CrossCheckMismatch { detail: String },
    /// §6.B2 M4: backfill deadline, still no fill_id.
    ImmediateFillUnresolved,
    /// §6.C.4: exhaustive backfill, no client_order_id match.
    UnknownNoMatch,
    /// §6.C.5: resubmit requested but venue-idempotency not proven.
    UnprovenIdempotentResubmit,
    /// Late authoritative fill after we had already declared terminal/halt.
    PostTerminalFill,
    /// Fill would push attributed qty past order total (venue error).
    OverFill {
        attributed_fill_qty: u64,
        fill_qty: u64,
        order_qty: u64,
    },
    /// Create response claims zero fills but local WS fills already attributed.
    ResponseZeroButLocalFills {
        attributed_fill_qty: u64,
        remaining_count: u64,
    },
    /// ≥2 venue orders share the same client_order_id (ownership ambiguous).
    AmbiguousMatch { count: usize },
    /// Same fill_id observed with two different payloads (qty/price/fee/venue).
    ConflictingFillPayload { fill_id: FillId },
    /// Same fill_id first seen with fee=None, later with fee=Some — never silent 0.
    /// (F5: normal None→Some is fee upgrade; this remains for explicit refuse paths.)
    FeeArrivedAfterNone { fill_id: FillId },
    /// Fee accumulation overflowed u64.
    FeeOverflow { fill_id: FillId },
    /// Fill's venue_order_id does not belong to the matched parent order.
    FillOwnershipMismatch {
        fill_id: FillId,
        expected: VenueOrderId,
        got: VenueOrderId,
    },
    /// Known parent venue id replaced by a different id (parent substitution).
    OwnershipConflict {
        expected: VenueOrderId,
        got: VenueOrderId,
    },
    /// Venue / evidence claims fill obligation above order qty (F4).
    ObligationExceedsOrderQty {
        fill_obligation: u64,
        order_qty: u64,
    },
    /// G2: parent venue is known but an attributed fill still has None provenance.
    UnverifiedFillProvenance { fill_id: FillId },
    /// Operator / shell-injected halt.
    Operator(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectReason {
    /// Illegal transition for current state.
    IllegalTransition { state: String, event: String },
    /// StartSubmit / SubmitResponse out of order (skip Started, etc.).
    OrderInvariantViolated { detail: String },
    /// Duplicate PrepareSubmit / already past prepare.
    AlreadyPrepared,
    /// Event not applicable (e.g. Fill on New).
    NotApplicable { detail: String },
}

// ─── Cancel outcome mirror (omsrs semantics, local type) ──────────────────────

/// Typed cancel outcome (§6.B). Local mirror — do **not** import connector I/O types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CancelOutcome {
    /// Venue confirmed canceled → true terminal.
    Canceled,
    /// Cancel request accepted; not yet terminal confirmation.
    Accepted,
    /// Order not found at venue → reconcile, do **not** treat as canceled.
    NotFound,
    /// Already in a terminal state at venue → Terminal.
    AlreadyTerminal,
    /// Cancel rejected; order still live → restore Accepted/Partial.
    Rejected,
    /// Transport-layer unknown → reconcile, not canceled.
    TransportUnknown,
    /// Generic unknown → reconcile, not canceled.
    Unknown,
}

// ─── Events (all time / ids injected) ─────────────────────────────────────────

/// First-seen fill payload stored for conflict detection (D7 / R4).
/// Full-field equality required for NoOp; venue/fee None→Some is upgrade (F2/F5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FillPayload {
    pub qty: u64,
    pub price_cents: u64,
    pub fee: Option<u64>,
    pub venue_order_id: Option<VenueOrderId>,
    /// Fill timestamp (ns) for response-domain identity membership (F6).
    pub ts_ns: i64,
}

/// Snapshot boundary for create-response domain membership (F6).
/// Fills with `ts_ns <= TsNs` (or seq when wired) belong to the response domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SnapshotBoundary {
    TsNs(i64),
    Seq(u64),
}

/// One attributed fill (WS or GET). Always carries authoritative `fill_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FillRecord {
    pub fill_id: FillId,
    pub qty: u64,
    pub price_cents: u64,
    pub ts_ns: i64,
    /// Parent venue order this fill belongs to (when known from GET fills).
    /// Required once parent order's venue id is known (R3); WS may leave `None` only
    /// while the parent order's venue id is still unknown (pre-response).
    pub venue_order_id: Option<VenueOrderId>,
    /// Venue-reported fee for this fill (E3). OMS carries + cross-checks only.
    pub fee_cents: Option<u64>,
}

/// Matched order row from §6.C backfill (shell-fetched, pure-matched here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillOrderRecord {
    pub client_order_id: ClientOrderId,
    pub venue_order_id: VenueOrderId,
    pub status: BackfillOrderStatus,
    pub filled_qty: u64,
    pub remaining_qty: u64,
    pub fills: Vec<FillRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackfillOrderStatus {
    Open,
    Partial,
    Filled,
    Canceled,
}

/// Input event to the pure state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderEvent {
    /// Durable prepare (shell will append+fsync before HTTP).
    PrepareSubmit,
    /// Durable started marker **before** HTTP send.
    StartSubmit { attempt_id: AttemptId },
    /// Create response. `fill_count` / `remaining_count` are **cross-check only**.
    SubmitResponse {
        venue_order_id: VenueOrderId,
        fill_count: u64,
        remaining_count: u64,
        avg_price_cents: Option<u64>,
        fee_cents: Option<u64>,
        /// Response snapshot time/seq boundary for domain membership (F6).
        /// `None` → no reliable boundary; avg/fee cross-check degraded to obligation-only.
        snapshot_boundary: Option<SnapshotBoundary>,
    },
    /// Submit timeout / transport Unknown → `SubmitUnknown`, never resubmit.
    SubmitTimeout,
    /// Authoritative fill (WS or GET) keyed by `fill_id`.
    Fill {
        fill_id: FillId,
        qty: u64,
        price_cents: u64,
        ts_ns: i64,
        /// Kalshi WS fill carries order_id — required once parent venue id is known (R3).
        venue_order_id: Option<VenueOrderId>,
        /// Venue-reported fee cents for this fill (E3).
        fee_cents: Option<u64>,
    },
    /// Strategy/shell requests cancel.
    CancelRequested,
    /// Typed cancel outcome from venue / transport.
    CancelOutcome(CancelOutcome),
    /// §6.C backfill injection after `SubmitUnknown`.
    UnknownBackfillResult {
        /// Shell affirms pages exhausted under its cursor/time bounds.
        exhaustive: bool,
        /// All records matching this order's client_order_id.
        /// 0 → no match; 1 → normal; ≥2 → [`HaltReason::AmbiguousMatch`].
        matched: Vec<BackfillOrderRecord>,
    },
    /// §6.B2 fill-id backfill result while `ImmediateFillUnattributed`.
    ImmediateFillBackfillResult { fills: Vec<FillRecord> },
    /// §6.B2 backfill deadline elapsed.
    BackfillDeadlineElapsed,
    /// Shell attempts resubmit of same client_order_id (must be venue-idempotent).
    RequestResubmit {
        /// Whether venue idempotency for this client_order_id has been proven.
        venue_idempotent_proven: bool,
    },
    /// Reconcile result for cancel-unknown / open-order authority path.
    ReconcileResult {
        status: BackfillOrderStatus,
        venue_order_id: Option<VenueOrderId>,
        filled_qty: u64,
        remaining_qty: u64,
        fills: Vec<FillRecord>,
        /// Shell proves open orders + positions + fill authority fully fetched (E2).
        authority_complete: bool,
    },
}

// ─── Effects (pure descriptions — shell executes) ─────────────────────────────

/// Journal record the shell must append+fsync when listed in effects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JournalRecord {
    SubmitPrepared {
        client_order_id: ClientOrderId,
    },
    SubmitStarted {
        attempt_id: AttemptId,
    },
    SubmitResponse {
        venue_order_id: VenueOrderId,
        fill_count: u64,
        remaining_count: u64,
        avg_price_cents: Option<u64>,
        fee_cents: Option<u64>,
        snapshot_boundary: Option<SnapshotBoundary>,
    },
    SubmitUnknown,
    Fill {
        fill_id: FillId,
        qty: u64,
        price_cents: u64,
        ts_ns: i64,
        /// Durable fee for crash-rebuild after Fill fsync, before AccountFill.
        fee_cents: Option<u64>,
        /// Durable provenance for ownership rebuild.
        venue_order_id: Option<VenueOrderId>,
    },
    /// Fee enrichment after initial None→Some (F5); delta applied to attributed_fee.
    FeeCorrection {
        fill_id: FillId,
        delta_fee_cents: u64,
    },
    CancelRequested,
    CancelOutcome(CancelOutcome),
    /// ★ F9a(kbot spec F9A v3):带 cid 的撤单事件——旧裸变体**永久保留**(新读老);
    /// emit 全部切到 Cid 形;fold 语义与裸形逐字节同(仅 authority 失效)。
    CancelRequestedCid {
        client_order_id: ClientOrderId,
    },
    CancelOutcomeCid {
        client_order_id: ClientOrderId,
        outcome: CancelOutcome,
    },
    /// ★ F9a:cid↔wire 绑定(闭链必要件)——bind_venue_id 构造单点切换,每张有 wire
    /// 订单至少一条(create-ack 首绑必 emit),Terminal/Fill 经 wire join 归 cid。
    VenueBoundCid {
        client_order_id: ClientOrderId,
        venue_order_id: VenueOrderId,
    },
    ImmediateFillUnattributed {
        venue_order_id: VenueOrderId,
        fill_count: u64,
        remaining_count: u64,
        avg_price_cents: Option<u64>,
        fee_cents: Option<u64>,
    },
    ImmediateFillUnresolved,
    UnknownNoMatch,
    Halted {
        reason: HaltReason,
    },
    /// G4: durable parent venue bind (non-terminal reconcile / response).
    VenueBound {
        venue_order_id: VenueOrderId,
    },
    /// G4: durable obligation high-water raise.
    /// `authority_epoch` is the post-raise generation (fold restores; does not re-bump).
    ObligationRaised {
        fill_obligation: u64,
        authority_epoch: u64,
    },
    /// G4: authority latched at generation `epoch` (must match current epoch to release).
    AuthorityLatched {
        epoch: u64,
    },
    /// G4: authority invalidated (fill-admitting activity); new epoch after bump.
    AuthorityInvalidated {
        epoch: u64,
    },
    /// G4: non-terminal reconcile snapshot that mutates ctx evidence.
    ReconcileObserved {
        venue_order_id: VenueOrderId,
        venue_filled_qty: u64,
        venue_remaining_qty: u64,
        shell_authority_complete: bool,
        authority_epoch: u64,
    },
    /// True terminal with rebuild snapshot (F7/G4).
    OrderTerminal {
        kind: TerminalKind,
        venue_order_id: Option<VenueOrderId>,
        fill_obligation: u64,
        authority_complete: bool,
        /// Generation at terminal; fold restores epoch + latch.
        authority_epoch: u64,
        attributed_fill_qty: u64,
        attributed_fee_cents: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TerminalKind {
    Filled,
    Canceled,
    Terminal,
}

/// Pure effect enum. This module never performs these actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// append+fsync before the shell proceeds to the next step.
    AppendFsync(JournalRecord),
    /// GET fills backfill for immediate-fill attribution (deadline owned by shell).
    BackfillFills { venue_order_id: VenueOrderId },
    /// Paginated current/historical orders+fills for `SubmitUnknown` recovery.
    BackfillUnknown { client_order_id: ClientOrderId },
    /// Cancel path needs reconcile (NotFound / TransportUnknown / Unknown).
    ReconcileCancel {
        venue_order_id: Option<VenueOrderId>,
    },
    /// Authority incomplete while attribution already matches venue_filled (R6/F11).
    /// Shell must re-fetch open orders + fills authority — pure core cannot progress.
    RequestAuthorityReconcile {
        venue_order_id: Option<VenueOrderId>,
        client_order_id: ClientOrderId,
    },
    /// Book cash/position/fees for this fill_id (idempotent key).
    AccountFill {
        fill_id: FillId,
        qty: u64,
        price_cents: u64,
        ts_ns: i64,
        /// Venue-reported fee for this fill (E3); 0 when venue omitted.
        fee_cents: u64,
    },
    /// Correct attributed fee after None→Some enrichment (F5); does not re-book qty.
    AccountFeeCorrection {
        fill_id: FillId,
        delta_fee_cents: u64,
    },
    /// True terminal: release reservation.
    /// **Only** produced by [`try_finalize_terminal`].
    ReleaseReservation,
    /// Re-reserve full amount after a late post-terminal fill (D2).
    ReserveFull,
    /// Block new exposure at account portfolio owner (HALT paths).
    HaltNewExposure,
    // Intentionally NO Resubmit / RetrySubmit effect.
}

impl Effect {
    pub fn is_resubmit(&self) -> bool {
        false // no resubmit variant exists; helper for invariant tests
    }

    pub fn is_io_effect(&self) -> bool {
        matches!(
            self,
            Effect::AppendFsync(_)
                | Effect::BackfillFills { .. }
                | Effect::BackfillUnknown { .. }
                | Effect::ReconcileCancel { .. }
                | Effect::RequestAuthorityReconcile { .. }
                | Effect::HaltNewExposure
                | Effect::ReserveFull
        )
    }

    pub fn is_account_fill(&self) -> bool {
        matches!(self, Effect::AccountFill { .. })
    }

    pub fn is_fee_correction(&self) -> bool {
        matches!(self, Effect::AccountFeeCorrection { .. })
    }
}

// ─── Reservation predicate ────────────────────────────────────────────────────

/// Reservation hold for this order (portfolio shell aggregates across orders).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReservationHold {
    /// Full original reservation still held (conservative).
    Full,
    /// Released after true terminal with attributed accounting / cancel confirm.
    Released,
}

/// Reservation held predicate (§6.B invariant).
///
/// **Released only** on true terminals: `Canceled`, `Filled` (attributed),
/// `Terminal` (reconciled).  
/// **Full** for everything else — including `SubmitUnknown`,
/// `ImmediateFillUnattributed`, `ImmediateFillUnresolved`, `UnknownNoMatch`,
/// `Halted`, live states, and `Partial` / `Accepted`.
pub fn reservation_held(state: &OrderState) -> ReservationHold {
    match state {
        OrderState::Canceled | OrderState::Filled | OrderState::Terminal => {
            ReservationHold::Released
        }
        OrderState::New
        | OrderState::SubmitPrepared
        | OrderState::SubmitStarted { .. }
        | OrderState::SubmitUnknown
        | OrderState::Accepted { .. }
        | OrderState::Partial { .. }
        | OrderState::CancelPending { .. }
        | OrderState::ReconcilePending { .. }
        | OrderState::ImmediateFillUnattributed { .. }
        | OrderState::ImmediateFillUnresolved
        | OrderState::UnknownNoMatch
        | OrderState::Halted { .. } => ReservationHold::Full,
    }
}

// ─── Order context (immutable identity + applied fill set) ────────────────────

/// Per-order context: identity + applied fill payloads + running attributed qty.
///
/// `applied_fills` is the idempotency map: same `fill_id` + same payload → no-op;
/// same `fill_id` + different payload → halt (conflict) or upgrade (F2/F5).
///
/// **R2:** `fill_obligation` is a monotonic high-water mark in ctx (not in state
/// variants) so live-demotion / Rejected / AlreadyTerminal never drop it.
///
/// **R3:** `venue_order_id` is durable identity across all states including terminal.
///
/// **F1/G1:** `authority_complete` is a **generation-scoped** latch — the release gate
/// reads only ctx fields; callers cannot feed a one-shot boolean into finalize.
/// Fill-admitting activity bumps `authority_epoch` and clears the latch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderCtx {
    pub client_order_id: ClientOrderId,
    pub market: String,
    pub strategy: String,
    pub side: Side,
    pub price_cents: u64,
    pub qty: u64,
    /// Authoritative fill_ids already booked, with first-seen payload (D7/R4).
    pub applied_fills: BTreeMap<FillId, FillPayload>,
    /// Σ qty of applied fills (by fill_id).
    pub attributed_fill_qty: u64,
    /// Monotonic high-water mark of known fill qty from any source (R2).
    /// Release requires `attributed_fill_qty == fill_obligation`.
    pub fill_obligation: u64,
    /// Durable parent venue identity once established (R3). Survives all states.
    pub venue_order_id: Option<VenueOrderId>,
    /// Generation-scoped latch: authoritative fill-complete evidence for **current**
    /// `authority_epoch` (F1/G1). Cleared by cancel / obligation lag / new fill_id.
    /// Set only by exhaustive backfill / `ReconcileResult.authority_complete` /
    /// full fill-id attribution of the entire order qty. Never set by CancelOutcome.
    pub authority_complete: bool,
    /// G1: authority generation. Fill-admitting activity bumps; latch is valid only
    /// when `authority_complete && authority_latched_epoch == authority_epoch`.
    pub authority_epoch: u64,
    /// G1: epoch at which authority was last latched.
    pub authority_latched_epoch: u64,
    /// G3: last observed venue remaining qty (durable across enrichment).
    pub last_venue_remaining_qty: Option<u64>,
    /// Response snapshot for §6.B2 cross-check (set on SubmitResponse with fills).
    pub response_fill_count: Option<u64>,
    pub response_remaining_count: Option<u64>,
    /// Preserved create-response avg/fee — never rebuilt to None after backfill (D4).
    pub response_avg_price_cents: Option<u64>,
    pub response_fee_cents: Option<u64>,
    /// F6: response snapshot boundary for domain membership by fill identity.
    pub response_snapshot_boundary: Option<SnapshotBoundary>,
    /// Σ (qty * price) over applied fills for weighted-avg / notional cross-check (E8).
    pub attributed_notional_cents: u128,
    /// Σ venue-reported fee_cents over applied fills (E3). None fees contribute 0.
    pub attributed_fee_cents: u64,
    /// Response-snapshot-domain accumulators (R6/F6): only in-boundary fills
    /// participate in avg/fee cross-check — later/out-of-domain fills are new evidence.
    pub response_domain_qty: u64,
    pub response_domain_notional_cents: u128,
    pub response_domain_fee_cents: u64,
}

impl OrderCtx {
    pub fn new(
        client_order_id: ClientOrderId,
        market: impl Into<String>,
        strategy: impl Into<String>,
        side: Side,
        price_cents: u64,
        qty: u64,
    ) -> Self {
        Self {
            client_order_id,
            market: market.into(),
            strategy: strategy.into(),
            side,
            price_cents,
            qty,
            applied_fills: BTreeMap::new(),
            attributed_fill_qty: 0,
            fill_obligation: 0,
            venue_order_id: None,
            authority_complete: false,
            authority_epoch: 0,
            authority_latched_epoch: 0,
            last_venue_remaining_qty: None,
            response_fill_count: None,
            response_remaining_count: None,
            response_avg_price_cents: None,
            response_fee_cents: None,
            response_snapshot_boundary: None,
            attributed_notional_cents: 0,
            attributed_fee_cents: 0,
            response_domain_qty: 0,
            response_domain_notional_cents: 0,
            response_domain_fee_cents: 0,
        }
    }

    /// Raise fill_obligation high-water mark (R2). Never decreases.
    /// F4: evidence above order qty → Halt (do not clamp into a permanent Full trap).
    /// G1: when raise creates attributed < obligation, invalidate authority.
    pub fn raise_fill_obligation(&mut self, evidence: u64) -> Result<(), HaltReason> {
        if evidence > self.qty {
            return Err(HaltReason::ObligationExceedsOrderQty {
                fill_obligation: evidence,
                order_qty: self.qty,
            });
        }
        if evidence > self.fill_obligation {
            self.fill_obligation = evidence;
            if self.attributed_fill_qty < self.fill_obligation {
                self.invalidate_authority();
            }
        }
        Ok(())
    }

    /// G1: true iff latch is set **and** still on the current generation.
    pub fn authority_is_fresh(&self) -> bool {
        self.authority_complete && self.authority_latched_epoch == self.authority_epoch
    }

    /// Generation-scoped authority latch (F1/G1). Sets complete for **current** epoch.
    /// Incomplete shell reports do not re-open a proven latch on the same epoch.
    pub fn latch_authority_complete(&mut self) {
        self.authority_complete = true;
        self.authority_latched_epoch = self.authority_epoch;
    }

    /// G1: bump generation and clear latch — any prior live authority is stale.
    /// Call on fill-admitting activity: CancelRequested/CancelOutcome, obligation lag,
    /// new fill_id (non-enrichment).
    pub fn invalidate_authority(&mut self) {
        self.authority_epoch = self.authority_epoch.saturating_add(1);
        self.authority_complete = false;
    }

    /// G3: record venue remaining evidence (monotonic in the sense of "seen"; last write wins).
    pub fn note_venue_remaining(&mut self, remaining: u64) {
        self.last_venue_remaining_qty = Some(remaining);
    }

    /// Remaining open qty: `order_qty − attributed` via checked arithmetic (D3).
    /// Returns `None` if attributed already exceeds order qty (should have halted).
    pub fn remaining_qty_checked(&self) -> Option<u64> {
        self.qty.checked_sub(self.attributed_fill_qty)
    }

    pub fn remaining_qty(&self) -> u64 {
        self.remaining_qty_checked().unwrap_or(0)
    }

    /// Qty-weighted average price of applied fills (integer floor cents).
    pub fn attributed_avg_price_cents(&self) -> Option<u64> {
        if self.attributed_fill_qty == 0 {
            return None;
        }
        Some((self.attributed_notional_cents / self.attributed_fill_qty as u128) as u64)
    }
}

// ─── Transition outcome ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionOutcome {
    Accept {
        new_state: OrderState,
        effects: Vec<Effect>,
    },
    Reject {
        reason: RejectReason,
    },
    Halt {
        new_state: OrderState,
        reason: HaltReason,
        effects: Vec<Effect>,
    },
}

impl TransitionOutcome {
    pub fn effects(&self) -> &[Effect] {
        match self {
            TransitionOutcome::Accept { effects, .. } | TransitionOutcome::Halt { effects, .. } => {
                effects
            }
            TransitionOutcome::Reject { .. } => &[],
        }
    }

    pub fn new_state(&self) -> Option<&OrderState> {
        match self {
            TransitionOutcome::Accept { new_state, .. }
            | TransitionOutcome::Halt { new_state, .. } => Some(new_state),
            TransitionOutcome::Reject { .. } => None,
        }
    }

    pub fn is_reject(&self) -> bool {
        matches!(self, TransitionOutcome::Reject { .. })
    }

    pub fn is_halt(&self) -> bool {
        matches!(self, TransitionOutcome::Halt { .. })
    }

    pub fn has_resubmit_effect(&self) -> bool {
        self.effects().iter().any(|e| e.is_resubmit())
    }

    pub fn account_fill_count(&self) -> usize {
        self.effects()
            .iter()
            .filter(|e| e.is_account_fill())
            .count()
    }
}

// ─── apply_event (core pure transition) ───────────────────────────────────────

/// Apply one event. Mutates `ctx` only on Accept/Halt (Reject leaves ctx intact
/// except we clone-check: actually Reject must not mutate — we apply fill set
/// only on success paths).
///
/// Invariants enforced here (see tests):
/// 1. Order: Prepare → Started → Response (no skip Started).
/// 2. Timeout → SubmitUnknown, **no** resubmit effect.
/// 3. Cancel typed seven-way routing.
/// 4. `reservation_held` released only on true terminals.
/// 5. Restart-safe: replaying into terminal/halt yields no new I/O on re-apply
///    of terminal-confirming events (idempotent no-op / reject).
pub fn apply_event(
    state: &OrderState,
    ctx: &mut OrderCtx,
    event: &OrderEvent,
) -> TransitionOutcome {
    // Terminal / halt states: restart-safe — no new I/O effects (except D2 late fill).
    if is_restart_frozen(state) {
        return restart_safe_handle(state, ctx, event);
    }

    match (state, event) {
        // ── §6.B prepare / start / response / timeout ─────────────────────
        (OrderState::New, OrderEvent::PrepareSubmit) => {
            let effects = vec![Effect::AppendFsync(JournalRecord::SubmitPrepared {
                client_order_id: ctx.client_order_id.clone(),
            })];
            accept(OrderState::SubmitPrepared, effects)
        }
        (OrderState::New, _) => reject_illegal(state, event),

        (OrderState::SubmitPrepared, OrderEvent::StartSubmit { attempt_id }) => {
            let effects = vec![Effect::AppendFsync(JournalRecord::SubmitStarted {
                attempt_id: attempt_id.clone(),
            })];
            accept(
                OrderState::SubmitStarted {
                    attempt_id: attempt_id.clone(),
                },
                effects,
            )
        }
        // ★ Invariant: cannot skip Started — SubmitResponse from Prepared is Reject.
        (OrderState::SubmitPrepared, OrderEvent::SubmitResponse { .. }) => {
            TransitionOutcome::Reject {
                reason: RejectReason::OrderInvariantViolated {
                    detail: "SubmitResponse requires SubmitStarted (Started must be fsynced before HTTP/response)"
                        .into(),
                },
            }
        }
        (OrderState::SubmitPrepared, OrderEvent::SubmitTimeout) => {
            TransitionOutcome::Reject {
                reason: RejectReason::OrderInvariantViolated {
                    detail: "SubmitTimeout only valid from SubmitStarted".into(),
                },
            }
        }
        (OrderState::SubmitPrepared, OrderEvent::PrepareSubmit) => TransitionOutcome::Reject {
            reason: RejectReason::AlreadyPrepared,
        },
        (OrderState::SubmitPrepared, _) => reject_illegal(state, event),

        (OrderState::SubmitStarted { .. }, OrderEvent::SubmitResponse {
            venue_order_id,
            fill_count,
            remaining_count,
            avg_price_cents,
            fee_cents,
            snapshot_boundary,
        }) => apply_submit_response(
            ctx,
            venue_order_id.clone(),
            *fill_count,
            *remaining_count,
            *avg_price_cents,
            *fee_cents,
            *snapshot_boundary,
        ),
        (OrderState::SubmitStarted { .. }, OrderEvent::SubmitTimeout) => {
            // ★ Timeout = Unknown persist, NEVER resubmit.
            let effects = vec![
                Effect::AppendFsync(JournalRecord::SubmitUnknown),
                Effect::BackfillUnknown {
                    client_order_id: ctx.client_order_id.clone(),
                },
            ];
            accept(OrderState::SubmitUnknown, effects)
        }
        // WS fill may race ahead of create response (§6.B2 "WS before response").
        (OrderState::SubmitStarted { .. }, OrderEvent::Fill {
            fill_id,
            qty,
            price_cents,
            ts_ns,
            venue_order_id,
            fee_cents,
        }) => apply_fill_while_open(
            state,
            ctx,
            fill_id,
            *qty,
            *price_cents,
            *ts_ns,
            venue_order_id.clone(),
            *fee_cents,
        ),
        (OrderState::SubmitStarted { .. }, OrderEvent::StartSubmit { .. }) => {
            TransitionOutcome::Reject {
                reason: RejectReason::OrderInvariantViolated {
                    detail: "already SubmitStarted".into(),
                },
            }
        }
        (OrderState::SubmitStarted { .. }, _) => reject_illegal(state, event),

        // ── §6.C SubmitUnknown recovery ───────────────────────────────────
        (OrderState::SubmitUnknown, OrderEvent::UnknownBackfillResult { exhaustive, matched }) => {
            apply_unknown_backfill(ctx, *exhaustive, matched)
        }
        (OrderState::SubmitUnknown, OrderEvent::RequestResubmit {
            venue_idempotent_proven,
        }) => {
            if *venue_idempotent_proven {
                TransitionOutcome::Reject {
                    reason: RejectReason::NotApplicable {
                        detail: "core never emits resubmit; shell must open a new \
                                 prepared attempt under proven venue-idempotency policy"
                            .into(),
                    },
                }
            } else {
                // ★ Unproven venue-idempotency → HALT, no resubmit effect.
                halt_with_reason(HaltReason::UnprovenIdempotentResubmit, vec![])
            }
        }
        (OrderState::SubmitUnknown, OrderEvent::Fill {
            fill_id,
            qty,
            price_cents,
            ts_ns,
            venue_order_id,
            fee_cents,
        }) => {
            // Fills may surface via WS while unknown; attribute by fill_id, stay Unknown
            // until backfill routes a terminal/open state.
            apply_fill_dedup_only(
                ctx,
                fill_id,
                *qty,
                *price_cents,
                *ts_ns,
                venue_order_id.as_ref(),
                *fee_cents,
                state.clone(),
            )
        }
        (OrderState::SubmitUnknown, _) => reject_illegal(state, event),

        // ── Live: Accepted / Partial ──────────────────────────────────────
        (OrderState::Accepted { .. }, OrderEvent::Fill {
            fill_id,
            qty,
            price_cents,
            ts_ns,
            venue_order_id: fill_vid,
            fee_cents,
        }) => apply_fill_while_open(
            state,
            ctx,
            fill_id,
            *qty,
            *price_cents,
            *ts_ns,
            fill_vid.clone(),
            *fee_cents,
        ),
        (OrderState::Accepted { venue_order_id }, OrderEvent::CancelRequested) => {
            // G1: cancel is fill-admitting (fills may race between live authority and cancel).
            let mut effects = authority_invalidate_effects(ctx);
            effects.push(Effect::AppendFsync(JournalRecord::CancelRequestedCid {
                client_order_id: ctx.client_order_id.clone(),
            }));
            accept(
                OrderState::CancelPending {
                    venue_order_id: venue_order_id.clone(),
                    filled_qty: ctx.attributed_fill_qty,
                    remaining_qty: ctx.remaining_qty(),
                    response_fill_count: ctx.response_fill_count,
                    response_avg_price_cents: ctx.response_avg_price_cents,
                    response_fee_cents: ctx.response_fee_cents,
                    reconcile_target: None,
                },
                effects,
            )
        }
        (OrderState::Partial { .. }, OrderEvent::Fill {
            fill_id,
            qty,
            price_cents,
            ts_ns,
            venue_order_id: fill_vid,
            fee_cents,
        }) => apply_fill_while_open(
            state,
            ctx,
            fill_id,
            *qty,
            *price_cents,
            *ts_ns,
            fill_vid.clone(),
            *fee_cents,
        ),
        (OrderState::Partial {
            venue_order_id, ..
        }, OrderEvent::CancelRequested) => {
            // G1: cancel invalidates any prior live authority snapshot.
            let mut effects = authority_invalidate_effects(ctx);
            effects.push(Effect::AppendFsync(JournalRecord::CancelRequestedCid {
                client_order_id: ctx.client_order_id.clone(),
            }));
            accept(
                OrderState::CancelPending {
                    venue_order_id: venue_order_id.clone(),
                    filled_qty: ctx.attributed_fill_qty,
                    remaining_qty: ctx.remaining_qty(),
                    response_fill_count: ctx.response_fill_count,
                    response_avg_price_cents: ctx.response_avg_price_cents,
                    response_fee_cents: ctx.response_fee_cents,
                    reconcile_target: None,
                },
                effects,
            )
        }
        (OrderState::Accepted { .. } | OrderState::Partial { .. }, _) => reject_illegal(state, event),

        // ── CancelPending typed outcomes ──────────────────────────────────
        (OrderState::CancelPending {
            venue_order_id,
            filled_qty,
            remaining_qty,
            response_fill_count,
            response_avg_price_cents,
            response_fee_cents,
            reconcile_target,
        }, OrderEvent::CancelOutcome(outcome)) => {
            apply_cancel_outcome(
                ctx,
                venue_order_id,
                *filled_qty,
                *remaining_qty,
                *response_fill_count,
                *response_avg_price_cents,
                *response_fee_cents,
                reconcile_target.clone(),
                *outcome,
            )
        }
        (OrderState::CancelPending { .. }, OrderEvent::Fill {
            fill_id,
            qty,
            price_cents,
            ts_ns,
            venue_order_id: fill_vid,
            fee_cents,
        }) => {
            apply_fill_while_open(
                state,
                ctx,
                fill_id,
                *qty,
                *price_cents,
                *ts_ns,
                fill_vid.clone(),
                *fee_cents,
            )
        }
        (OrderState::CancelPending {
            venue_order_id,
            response_fill_count,
            response_avg_price_cents,
            response_fee_cents,
            ..
        }, OrderEvent::ReconcileResult {
            status,
            venue_order_id: vid,
            filled_qty,
            remaining_qty,
            fills,
            authority_complete,
        }) => {
            // B1: bind conflict after mutate must Halt with VenueBound already emitted.
            let (v, bind_jr) = match resolve_reconcile_venue(ctx, venue_order_id, vid.as_ref()) {
                Ok(x) => x,
                Err((reason, bind_jr)) => {
                    let mut effects = Vec::new();
                    push_journal(&mut effects, bind_jr);
                    return halt_with_reason(reason, effects);
                }
            };
            let mut prefix = Vec::new();
            push_journal(&mut prefix, bind_jr);
            route_from_backfill_status(
                ctx,
                v,
                *status,
                *filled_qty,
                *remaining_qty,
                fills,
                *authority_complete,
                /* from_submit_unknown */ false,
                *response_fill_count,
                *response_avg_price_cents,
                *response_fee_cents,
            )
            .prepend_effects(prefix)
        }
        // ★ 2026-08-09（真钱前双审 C2 死亡链 A）：cancel 重试必须幂等。
        //   此前 CancelPending + CancelRequested = reject ⇒ 宿主一次 cancel
        //   HTTP 失败后重试 ⇒ reject ⇒ bail ⇒ 进程死、场上单无人管。
        //   重试同一意图不是非法转移，是网络现实。no-op 保持 CancelPending。
        (OrderState::CancelPending { .. }, OrderEvent::CancelRequested) => {
            // no-op：状态不变、零 effect（重试不重复记 journal）。
            accept(state.clone(), vec![])
        }
        (OrderState::CancelPending { .. }, _) => reject_illegal(state, event),

        // ── E5 ReconcilePending ───────────────────────────────────────────
        (OrderState::ReconcilePending { .. }, OrderEvent::Fill {
            fill_id,
            qty,
            price_cents,
            ts_ns,
            venue_order_id: fill_vid,
            fee_cents,
        }) => apply_fill_while_open(
            state,
            ctx,
            fill_id,
            *qty,
            *price_cents,
            *ts_ns,
            fill_vid.clone(),
            *fee_cents,
        ),
        (OrderState::ReconcilePending {
            venue_order_id,
            target,
            response_fill_count,
            response_avg_price_cents,
            response_fee_cents,
            ..
        }, OrderEvent::ReconcileResult {
            status,
            venue_order_id: vid,
            filled_qty,
            remaining_qty,
            fills,
            authority_complete,
        }) => {
            // B1: bind conflict after mutate must Halt with VenueBound already emitted.
            let (v, bind_jr) = match resolve_reconcile_venue(ctx, venue_order_id, vid.as_ref()) {
                Ok(x) => x,
                Err((reason, bind_jr)) => {
                    let mut effects = Vec::new();
                    push_journal(&mut effects, bind_jr);
                    return halt_with_reason(reason, effects);
                }
            };
            let _ = target;
            let mut prefix = Vec::new();
            push_journal(&mut prefix, bind_jr);
            route_from_backfill_status(
                ctx,
                v,
                *status,
                *filled_qty,
                *remaining_qty,
                fills,
                *authority_complete,
                false,
                *response_fill_count,
                *response_avg_price_cents,
                *response_fee_cents,
            )
            .prepend_effects(prefix)
        }
        // ★ A3（2026-08-15 四仓扫雷 HIGH，C2 死亡链同型）：ReconcilePending 是
        //   cancel 意图的**直接延续态** —— 它仍在壳的 open 投影里，策略对不想要
        //   的 open 单重发 cancel 不是非法转移。此前落 catch-all reject ⇒ 壳
        //   `?` bail 杀 live loop。no-op 幂等放行后，壳照常发 cancel HTTP →
        //   404/success 分类 → 重发 `ReconcileResult(authority_complete=true)`
        //   →（上方已有的 ReconcileResult 臂）重供 authority ⇒ 卡死行自愈。
        (OrderState::ReconcilePending { .. }, OrderEvent::CancelRequested) => {
            accept(state.clone(), vec![])
        }
        // ★ A3b（复审 HIGH：自愈链第二跳）：壳的 on_cancel_http_success 第一步是
        //   apply CancelOutcome，然后才发 ReconcileResult —— ReconcilePending 对
        //   CancelOutcome 无臂时死点只是从 CancelRequested 挪后一个 HTTP。
        //   CancelOutcome 本就不供 authority（F1），容忍 no-op，让紧随的
        //   ReconcileResult(authority_complete=true)（上方既有臂）完成自愈。
        (OrderState::ReconcilePending { .. }, OrderEvent::CancelOutcome(_)) => {
            accept(state.clone(), vec![])
        }
        (OrderState::ReconcilePending { .. }, _) => reject_illegal(state, event),

        // ── §6.B2 ImmediateFillUnattributed ───────────────────────────────
        (OrderState::ImmediateFillUnattributed { .. }, OrderEvent::ImmediateFillBackfillResult {
            fills,
        }) => apply_immediate_fill_backfill(ctx, state, fills),
        (OrderState::ImmediateFillUnattributed { .. }, OrderEvent::Fill {
            fill_id,
            qty,
            price_cents,
            ts_ns,
            venue_order_id: fill_vid,
            fee_cents,
        }) => {
            let fill = FillRecord {
                fill_id: fill_id.clone(),
                qty: *qty,
                price_cents: *price_cents,
                ts_ns: *ts_ns,
                venue_order_id: fill_vid.clone(),
                fee_cents: *fee_cents,
            };
            apply_immediate_fill_backfill(ctx, state, &[fill])
        }
        // D5/E1: cancel live remainder; carry unattributed response obligation.
        (OrderState::ImmediateFillUnattributed {
            venue_order_id,
            response_fill_count,
            response_avg_price_cents,
            response_fee_cents,
            ..
        }, OrderEvent::CancelRequested) => {
            // Obligation already raised on response; keep on ctx (R2).
            // G1: cancel invalidates prior authority.
            let mut effects = authority_invalidate_effects(ctx);
            effects.push(Effect::AppendFsync(JournalRecord::CancelRequestedCid {
                client_order_id: ctx.client_order_id.clone(),
            }));
            accept(
                OrderState::CancelPending {
                    venue_order_id: venue_order_id.clone(),
                    filled_qty: ctx.attributed_fill_qty,
                    remaining_qty: ctx.remaining_qty(),
                    response_fill_count: Some(*response_fill_count),
                    response_avg_price_cents: *response_avg_price_cents,
                    response_fee_cents: *response_fee_cents,
                    reconcile_target: None,
                },
                effects,
            )
        }
        (OrderState::ImmediateFillUnattributed { .. }, OrderEvent::BackfillDeadlineElapsed) => {
            halt_with_reason_state(
                OrderState::ImmediateFillUnresolved,
                HaltReason::ImmediateFillUnresolved,
                vec![Effect::AppendFsync(JournalRecord::ImmediateFillUnresolved)],
            )
        }
        (OrderState::ImmediateFillUnattributed { .. }, _) => reject_illegal(state, event),

        // Frozen terminals already handled at top; defensive reject.
        _ => reject_illegal(state, event),
    }
}

fn is_restart_frozen(state: &OrderState) -> bool {
    matches!(
        state,
        OrderState::Filled
            | OrderState::Canceled
            | OrderState::Terminal
            | OrderState::ImmediateFillUnresolved
            | OrderState::UnknownNoMatch
            | OrderState::Halted { .. }
    )
}

/// Restart-in-terminal safety: replaying further events produces no new I/O
/// effects and reservation stays as prescribed — **except** D2 late new fills,
/// which must be accounted before HALT (with ownership check, R3).
/// G5: same-fill_id fee/provenance enrichment is **not** a late fill — apply
/// metadata only, no Halt / ReserveFull.
fn restart_safe_handle(
    state: &OrderState,
    ctx: &mut OrderCtx,
    event: &OrderEvent,
) -> TransitionOutcome {
    if let OrderEvent::Fill {
        fill_id,
        qty,
        price_cents,
        ts_ns,
        venue_order_id,
        fee_cents,
    } = event
    {
        // R3: terminal late fill still ownership-gated via ctx.venue_order_id.
        match try_account_fill(
            ctx,
            fill_id,
            *qty,
            *price_cents,
            *ts_ns,
            venue_order_id.as_ref(),
            *fee_cents,
        ) {
            AccountAttempt::NoOp => {
                return accept(state.clone(), vec![]);
            }
            AccountAttempt::Halt {
                reason,
                mut effects,
            } => {
                if reservation_held(state) == ReservationHold::Released {
                    effects.push(Effect::ReserveFull);
                }
                return halt_with_reason(reason, effects);
            }
            // G5: pure enrichment on frozen state — no PostTerminalFill / ReserveFull.
            AccountAttempt::Enriched { effects } => {
                return accept(state.clone(), effects);
            }
            AccountAttempt::Applied { mut effects } => {
                // D2: late true money after terminal/halt — account, re-reserve if needed, HALT.
                if reservation_held(state) == ReservationHold::Released {
                    effects.push(Effect::ReserveFull);
                }
                return halt_with_reason(HaltReason::PostTerminalFill, effects);
            }
        }
    }
    // Any other event on frozen state: reject (no I/O effects).
    TransitionOutcome::Reject {
        reason: RejectReason::NotApplicable {
            detail: format!(
                "restart-safe: state {:?} ignores event {}",
                state_name(state),
                event_name(event)
            ),
        },
    }
}

// ─── R3 venue identity ────────────────────────────────────────────────────────

/// Bind or verify parent venue id on ctx. Different known id → OwnershipConflict.
/// Returns `Some(VenueBound journal)` when newly bound (G4).
fn bind_venue_id(
    ctx: &mut OrderCtx,
    new_id: &VenueOrderId,
) -> Result<Option<JournalRecord>, HaltReason> {
    match &ctx.venue_order_id {
        None => {
            // Rebind-check fills booked in the unknown period.
            for (fid, payload) in &ctx.applied_fills {
                if let Some(got) = &payload.venue_order_id {
                    if got != new_id {
                        return Err(HaltReason::FillOwnershipMismatch {
                            fill_id: fid.clone(),
                            expected: new_id.clone(),
                            got: got.clone(),
                        });
                    }
                }
            }
            ctx.venue_order_id = Some(new_id.clone());
            // ★ F9a:构造单点切 Cid 形(七调用方零改动)。
            Ok(Some(JournalRecord::VenueBoundCid {
                client_order_id: ctx.client_order_id.clone(),
                venue_order_id: new_id.clone(),
            }))
        }
        Some(existing) if existing == new_id => Ok(None),
        Some(existing) => Err(HaltReason::OwnershipConflict {
            expected: existing.clone(),
            got: new_id.clone(),
        }),
    }
}

/// Resolve venue id for ReconcileResult: event id or existing; forbid substitution.
///
/// Returns `(venue_id, optional VenueBound journal)` on success.
///
/// **B1 (emit-before-Halt / same root as A1):** binding `state_vid` may mutate
/// `ctx.venue_order_id` and produce `VenueBound(A)`. If the event then carries a
/// conflicting `B`, that Halt **must** still journal `VenueBound(A)` so fold rebuild
/// keeps `ctx.venue_order_id == A == memory`. Err therefore carries any already-mutated
/// bind journal: `(HaltReason, Option<VenueBound>)`.
fn resolve_reconcile_venue(
    ctx: &mut OrderCtx,
    state_vid: &VenueOrderId,
    event_vid: Option<&VenueOrderId>,
) -> Result<(VenueOrderId, Option<JournalRecord>), (HaltReason, Option<JournalRecord>)> {
    // Bind state-known id first (may newly emit VenueBound + mutate ctx).
    let mut bind_jr = match bind_venue_id(ctx, state_vid) {
        Ok(jr) => jr,
        Err(reason) => return Err((reason, None)), // no mutate yet
    };
    if let Some(ev) = event_vid {
        match bind_venue_id(ctx, ev) {
            Ok(Some(jr)) => bind_jr = Some(jr),
            Ok(None) => {}
            // Conflict after A was bound: surface VenueBound(A) with the Halt reason.
            Err(reason) => return Err((reason, bind_jr)),
        }
        Ok((ev.clone(), bind_jr))
    } else {
        Ok((state_vid.clone(), bind_jr))
    }
}

// ─── R2 obligation evidence ───────────────────────────────────────────────────

/// Raise obligation; returns ObligationRaised journal when high-water moved (G4).
/// Carries post-raise `authority_epoch` so fold restores generation without re-bumping.
fn note_fill_evidence(
    ctx: &mut OrderCtx,
    venue_filled: u64,
) -> Result<Option<JournalRecord>, HaltReason> {
    let before = ctx.fill_obligation;
    ctx.raise_fill_obligation(venue_filled)?;
    let mut jr = None;
    if ctx.fill_obligation > before {
        jr = Some(JournalRecord::ObligationRaised {
            fill_obligation: ctx.fill_obligation,
            authority_epoch: ctx.authority_epoch,
        });
    }
    Ok(jr)
}

/// Set obligation high-water only (no authority epoch side effects). Used by journal fold.
fn restore_fill_obligation(ctx: &mut OrderCtx, evidence: u64) -> Result<(), HaltReason> {
    if evidence > ctx.qty {
        return Err(HaltReason::ObligationExceedsOrderQty {
            fill_obligation: evidence,
            order_qty: ctx.qty,
        });
    }
    if evidence > ctx.fill_obligation {
        ctx.fill_obligation = evidence;
    }
    Ok(())
}

/// Latch authority from shell-proven fill-complete evidence (F1/G1).
/// CancelOutcome never calls this. Returns AuthorityLatched journal (G4).
fn note_authority_complete(ctx: &mut OrderCtx, complete: bool) -> Option<JournalRecord> {
    if complete {
        ctx.latch_authority_complete();
        Some(JournalRecord::AuthorityLatched {
            epoch: ctx.authority_epoch,
        })
    } else {
        None
    }
}

/// G1/G4: invalidate authority generation and produce durable journal effect(s).
fn authority_invalidate_effects(ctx: &mut OrderCtx) -> Vec<Effect> {
    ctx.invalidate_authority();
    vec![Effect::AppendFsync(JournalRecord::AuthorityInvalidated {
        epoch: ctx.authority_epoch,
    })]
}

fn push_journal(effects: &mut Vec<Effect>, jr: Option<JournalRecord>) {
    if let Some(jr) = jr {
        effects.push(Effect::AppendFsync(jr));
    }
}

// ─── Fill accounting (D3 / D6 / D7 / R3 / R4 / G5) ────────────────────────────

enum AccountAttempt {
    NoOp,
    /// New fill_id booked (qty attributed).
    Applied {
        effects: Vec<Effect>,
    },
    /// G5: same fill_id fee/provenance enrichment only — no new qty.
    Enriched {
        effects: Vec<Effect>,
    },
    Halt {
        reason: HaltReason,
        effects: Vec<Effect>,
    },
}

/// Core fill booking: ownership check → payload conflict → overfill checked add.
/// All fill accounting **must** go through [`try_account_fill_owned`].
fn try_account_fill(
    ctx: &mut OrderCtx,
    fill_id: &FillId,
    qty: u64,
    price_cents: u64,
    ts_ns: i64,
    fill_venue: Option<&VenueOrderId>,
    fee_cents: Option<u64>,
) -> AccountAttempt {
    try_account_fill_owned(ctx, fill_id, qty, price_cents, ts_ns, fill_venue, fee_cents)
}

fn try_account_fill_record(ctx: &mut OrderCtx, f: &FillRecord) -> AccountAttempt {
    try_account_fill_owned(
        ctx,
        &f.fill_id,
        f.qty,
        f.price_cents,
        f.ts_ns,
        f.venue_order_id.as_ref(),
        f.fee_cents,
    )
}

/// Journal fold fill restore: book qty/fee/domain without authority epoch side effects.
/// Epoch is restored from explicit AuthorityInvalidated / ObligationRaised / latch records (M2).
fn try_account_fill_for_fold(
    ctx: &mut OrderCtx,
    fill_id: &FillId,
    qty: u64,
    price_cents: u64,
    ts_ns: i64,
    fill_venue: Option<&VenueOrderId>,
    fee_cents: Option<u64>,
) -> AccountAttempt {
    account_fill_core(
        ctx,
        fill_id,
        qty,
        price_cents,
        ts_ns,
        fill_venue,
        fee_cents,
        /* restore_mode */ true,
    )
}

fn try_account_fill_owned(
    ctx: &mut OrderCtx,
    fill_id: &FillId,
    qty: u64,
    price_cents: u64,
    ts_ns: i64,
    fill_venue: Option<&VenueOrderId>,
    fee_cents: Option<u64>,
) -> AccountAttempt {
    account_fill_core(
        ctx,
        fill_id,
        qty,
        price_cents,
        ts_ns,
        fill_venue,
        fee_cents,
        /* restore_mode */ false,
    )
}

fn account_fill_core(
    ctx: &mut OrderCtx,
    fill_id: &FillId,
    qty: u64,
    price_cents: u64,
    ts_ns: i64,
    fill_venue: Option<&VenueOrderId>,
    fee_cents: Option<u64>,
    restore_mode: bool,
) -> AccountAttempt {
    // R3: once parent venue id is known on ctx, fill **must** carry matching venue_order_id.
    // F2 exception: re-delivery of an already-applied fill with venue upgrade is handled
    // in the same-fill_id branch below (first sighting still requires ownership).
    if let Some(expected) = ctx.venue_order_id.clone() {
        if !ctx.applied_fills.contains_key(fill_id) {
            match fill_venue {
                Some(got) if got == &expected => {}
                Some(got) => {
                    return AccountAttempt::Halt {
                        reason: HaltReason::FillOwnershipMismatch {
                            fill_id: fill_id.clone(),
                            expected,
                            got: got.clone(),
                        },
                        effects: vec![],
                    };
                }
                None => {
                    return AccountAttempt::Halt {
                        reason: HaltReason::FillOwnershipMismatch {
                            fill_id: fill_id.clone(),
                            expected,
                            got: VenueOrderId("<missing>".into()),
                        },
                        effects: vec![],
                    };
                }
            }
        }
    }

    let new_payload = FillPayload {
        qty,
        price_cents,
        fee: fee_cents,
        venue_order_id: fill_venue.cloned(),
        ts_ns,
    };

    // R4/F2/F5: same fill_id — full payload equal → NoOp; upgrades vs conflicts.
    if let Some(prev) = ctx.applied_fills.get(fill_id).cloned() {
        if prev == new_payload {
            return AccountAttempt::NoOp;
        }
        // qty/price must be stable; any change is conflict.
        if prev.qty != qty || prev.price_cents != price_cents {
            return AccountAttempt::Halt {
                reason: HaltReason::ConflictingFillPayload {
                    fill_id: fill_id.clone(),
                },
                effects: vec![],
            };
        }

        let mut upgraded = prev.clone();
        let mut effects = Vec::new();
        let mut changed = false;

        // F2: venue provenance upgrade None → Some(v).
        match (&prev.venue_order_id, fill_venue) {
            (None, Some(v)) => {
                if let Some(bound) = &ctx.venue_order_id {
                    if v != bound {
                        return AccountAttempt::Halt {
                            reason: HaltReason::FillOwnershipMismatch {
                                fill_id: fill_id.clone(),
                                expected: bound.clone(),
                                got: v.clone(),
                            },
                            effects: vec![],
                        };
                    }
                }
                upgraded.venue_order_id = Some(v.clone());
                changed = true;
            }
            (Some(old), Some(new_v)) if old != new_v => {
                return AccountAttempt::Halt {
                    reason: HaltReason::ConflictingFillPayload {
                        fill_id: fill_id.clone(),
                    },
                    effects: vec![],
                };
            }
            (Some(_), None) => {
                // Reverse Some→None is conflict (not a silent downgrade).
                return AccountAttempt::Halt {
                    reason: HaltReason::ConflictingFillPayload {
                        fill_id: fill_id.clone(),
                    },
                    effects: vec![],
                };
            }
            _ => {}
        }

        // F5: fee None → Some(f) is enrichment (correct ledger), not Halt.
        match (prev.fee, fee_cents) {
            (None, Some(f)) => {
                let new_fee_total = match ctx.attributed_fee_cents.checked_add(f) {
                    Some(v) => v,
                    None => {
                        return AccountAttempt::Halt {
                            reason: HaltReason::FeeOverflow {
                                fill_id: fill_id.clone(),
                            },
                            effects: vec![],
                        };
                    }
                };
                // Domain fee correction if this fill was in the response domain.
                if fill_in_response_domain(ctx, prev.ts_ns) {
                    let room_ok = ctx.response_fill_count.is_some();
                    if room_ok {
                        ctx.response_domain_fee_cents =
                            ctx.response_domain_fee_cents.saturating_add(f);
                    }
                }
                ctx.attributed_fee_cents = new_fee_total;
                upgraded.fee = Some(f);
                changed = true;
                effects.push(Effect::AppendFsync(JournalRecord::FeeCorrection {
                    fill_id: fill_id.clone(),
                    delta_fee_cents: f,
                }));
                effects.push(Effect::AccountFeeCorrection {
                    fill_id: fill_id.clone(),
                    delta_fee_cents: f,
                });
            }
            (Some(a), Some(b)) if a != b => {
                return AccountAttempt::Halt {
                    reason: HaltReason::ConflictingFillPayload {
                        fill_id: fill_id.clone(),
                    },
                    effects: vec![],
                };
            }
            (Some(_), None) => {
                return AccountAttempt::Halt {
                    reason: HaltReason::ConflictingFillPayload {
                        fill_id: fill_id.clone(),
                    },
                    effects: vec![],
                };
            }
            _ => {}
        }

        // ts_ns: keep first-seen for domain identity; ignore later ts if only other fields upgrade.
        if !changed {
            // e.g. only ts differs → treat as conflict if materially different payload already handled;
            // remaining case: identical economic fields with different ts → NoOp (stable fill_id).
            if prev.ts_ns != ts_ns
                && prev.fee == fee_cents
                && prev.venue_order_id.as_ref() == fill_venue
            {
                return AccountAttempt::NoOp;
            }
            return AccountAttempt::Halt {
                reason: HaltReason::ConflictingFillPayload {
                    fill_id: fill_id.clone(),
                },
                effects: vec![],
            };
        }

        ctx.applied_fills.insert(fill_id.clone(), upgraded.clone());
        // Durable provenance/fee upgrade journal (re-state fill with upgraded fields).
        effects.insert(
            0,
            Effect::AppendFsync(JournalRecord::Fill {
                fill_id: fill_id.clone(),
                qty,
                price_cents,
                ts_ns: upgraded.ts_ns,
                fee_cents: upgraded.fee,
                venue_order_id: upgraded.venue_order_id.clone(),
            }),
        );
        // G5: enrichment only — no new qty, do not invalidate authority.
        return AccountAttempt::Enriched { effects };
    }

    // D3: checked overfill — never saturating-mask into Filled.
    let Some(new_attr) = ctx.attributed_fill_qty.checked_add(qty) else {
        return AccountAttempt::Halt {
            reason: HaltReason::OverFill {
                attributed_fill_qty: ctx.attributed_fill_qty,
                fill_qty: qty,
                order_qty: ctx.qty,
            },
            effects: vec![],
        };
    };
    if new_attr > ctx.qty {
        return AccountAttempt::Halt {
            reason: HaltReason::OverFill {
                attributed_fill_qty: ctx.attributed_fill_qty,
                fill_qty: qty,
                order_qty: ctx.qty,
            },
            effects: vec![],
        };
    }

    // R4: fee checked_add (overflow → Halt), not saturating.
    let fee_for_account = fee_cents.unwrap_or(0);
    let new_fee_total = match ctx.attributed_fee_cents.checked_add(fee_for_account) {
        Some(v) => v,
        None => {
            return AccountAttempt::Halt {
                reason: HaltReason::FeeOverflow {
                    fill_id: fill_id.clone(),
                },
                effects: vec![],
            };
        }
    };

    ctx.applied_fills.insert(fill_id.clone(), new_payload);
    ctx.attributed_fill_qty = new_attr;
    ctx.attributed_notional_cents = ctx
        .attributed_notional_cents
        .saturating_add(qty as u128 * price_cents as u128);
    ctx.attributed_fee_cents = new_fee_total;

    let mut effects = Vec::new();
    if restore_mode {
        // M2 fold: rebuild fill set + obligation high-water only; no epoch bump.
        if let Err(reason) = restore_fill_obligation(ctx, new_attr) {
            return AccountAttempt::Halt {
                reason,
                effects: vec![],
            };
        }
    } else {
        // G1: new fill_id is fill-admitting — prior authority is stale until re-proven.
        // Emit AuthorityInvalidated so fold restores epoch (Fill replay does not re-bump).
        ctx.invalidate_authority();
        effects.push(Effect::AppendFsync(JournalRecord::AuthorityInvalidated {
            epoch: ctx.authority_epoch,
        }));
        // R2: every attributed fill raises obligation high-water.
        let obl_before = ctx.fill_obligation;
        if let Err(reason) = ctx.raise_fill_obligation(new_attr) {
            // Ctx already holds attributed fill + epoch bump — durable records must
            // travel with Halt (never discard mutation without journal).
            effects.push(Effect::AppendFsync(JournalRecord::Fill {
                fill_id: fill_id.clone(),
                qty,
                price_cents,
                ts_ns,
                fee_cents,
                venue_order_id: fill_venue.cloned(),
            }));
            effects.push(Effect::AccountFill {
                fill_id: fill_id.clone(),
                qty,
                price_cents,
                ts_ns,
                fee_cents: fee_for_account,
            });
            return AccountAttempt::Halt { reason, effects };
        }
        if ctx.fill_obligation > obl_before {
            effects.push(Effect::AppendFsync(JournalRecord::ObligationRaised {
                fill_obligation: ctx.fill_obligation,
                authority_epoch: ctx.authority_epoch,
            }));
        }
        // A1 emit-before-Halt: all durable fill records before domain-overfill check
        // so contradiction Halt still carries Fill/AccountFill/Authority/Obligation.
        effects.push(Effect::AppendFsync(JournalRecord::Fill {
            fill_id: fill_id.clone(),
            qty,
            price_cents,
            ts_ns,
            fee_cents,
            venue_order_id: fill_venue.cloned(),
        }));
        effects.push(Effect::AccountFill {
            fill_id: fill_id.clone(),
            qty,
            price_cents,
            ts_ns,
            fee_cents: fee_for_account,
        });
    }
    // R6/F6: accumulate into response snapshot domain by identity (no min-truncate).
    accumulate_response_domain(ctx, qty, price_cents, fee_cents, ts_ns);
    // M1: domain qty > declared response_fill_count is a snapshot contradiction.
    // Real fill already booked in memory + effects — Halt with those records, not vec![].
    // Fold (restore_mode): keep the booked fill so rebuild ≡ post-mutation memory;
    // the separate Halted journal does not un-book durable Fill records.
    if !restore_mode {
        if let Some(detail) = response_domain_overfill_detail(ctx) {
            return AccountAttempt::Halt {
                reason: HaltReason::CrossCheckMismatch { detail },
                effects,
            };
        }
    }

    AccountAttempt::Applied { effects }
}

/// L1/F6: true only when boundary membership is implemented and trustworthy.
/// `Seq` is not wired to fill identity yet → treat as **no** reliable boundary
/// (obligation-only), never "claimed reliable but always out-of-domain".
fn has_reliable_boundary(ctx: &OrderCtx) -> bool {
    matches!(
        ctx.response_snapshot_boundary,
        Some(SnapshotBoundary::TsNs(_))
    )
}

/// F6: whether a fill timestamp belongs to the response snapshot domain.
fn fill_in_response_domain(ctx: &OrderCtx, ts_ns: i64) -> bool {
    match ctx.response_snapshot_boundary {
        Some(SnapshotBoundary::TsNs(boundary)) => ts_ns <= boundary,
        // Seq membership requires fill seq (not on FillRecord yet).
        Some(SnapshotBoundary::Seq(_)) => false,
        None => false,
    }
}

/// M1: domain overfill detail when in-domain qty exceeds declared response_fill_count.
fn response_domain_overfill_detail(ctx: &OrderCtx) -> Option<String> {
    let rc = ctx.response_fill_count?;
    if has_reliable_boundary(ctx) && ctx.response_domain_qty > rc {
        Some(format!(
            "response_domain_qty={} > response_fill_count={}",
            ctx.response_domain_qty, rc
        ))
    } else {
        None
    }
}

/// Accumulate fill into response-snapshot domain by **identity** (F6), not arrival order.
/// Without a reliable boundary, skip (obligation-only; no false avg/fee sample).
/// M1: **no min-truncate** — accumulate real in-domain qty; excess is Halt elsewhere.
fn accumulate_response_domain(
    ctx: &mut OrderCtx,
    qty: u64,
    price_cents: u64,
    fee_cents: Option<u64>,
    ts_ns: i64,
) {
    if ctx.response_fill_count.is_none() {
        return;
    }
    // F6/L1: no reliable boundary → do not build domain from arrival order.
    if !has_reliable_boundary(ctx) {
        return;
    }
    if !fill_in_response_domain(ctx, ts_ns) {
        return;
    }
    // Full in-domain qty (no room min-truncate).
    ctx.response_domain_qty = ctx.response_domain_qty.saturating_add(qty);
    ctx.response_domain_notional_cents = ctx
        .response_domain_notional_cents
        .saturating_add(qty as u128 * price_cents as u128);
    if let Some(f) = fee_cents {
        ctx.response_domain_fee_cents = ctx.response_domain_fee_cents.saturating_add(f);
    }
}

/// Rebuild response-domain from applied fills after response snapshot is set
/// (WS-before-response path). Membership by snapshot identity (F6), not map order.
fn recompute_response_domain(ctx: &mut OrderCtx) {
    ctx.response_domain_qty = 0;
    ctx.response_domain_notional_cents = 0;
    ctx.response_domain_fee_cents = 0;
    if ctx.response_fill_count.is_none() || !has_reliable_boundary(ctx) {
        return;
    }
    // Deterministic order: by fill_id (BTreeMap), but only in-boundary fills count.
    for (_id, p) in ctx.applied_fills.clone() {
        accumulate_response_domain(ctx, p.qty, p.price_cents, p.fee, p.ts_ns);
    }
}

/// E8: notional cross-check vs response avg·fill_count with ±(fill_count×1¢) tolerance.
fn notional_avg_mismatch(
    attributed_notional: u128,
    response_avg: u64,
    fill_count: u64,
) -> Option<String> {
    if fill_count == 0 {
        return None;
    }
    let expected = response_avg as u128 * fill_count as u128;
    let tol = fill_count as u128; // ±1 cent per contract (rounding bound)
    let diff = if attributed_notional >= expected {
        attributed_notional - expected
    } else {
        expected - attributed_notional
    };
    if diff > tol {
        Some(format!(
            "avg/notional mismatch: response_avg={response_avg} fill_count={fill_count} \
             expected_notional={expected} attributed_notional={attributed_notional} tol=±{tol}"
        ))
    } else {
        None
    }
}

/// E3: response fee vs Σ venue-reported fill fees (domain-scoped when used with domain fee).
fn fee_cross_check_mismatch(response_fee: Option<u64>, attributed_fee: u64) -> Option<String> {
    let Some(resp) = response_fee else {
        return None;
    };
    if resp != attributed_fee {
        Some(format!(
            "fee mismatch: response_fee_cents={resp} attributed_fee_cents={attributed_fee}"
        ))
    } else {
        None
    }
}

// ─── R6 unified halt helper ───────────────────────────────────────────────────

/// **All** Halt paths append durable `JournalRecord::Halted` + HaltNewExposure.
fn halt_with_reason(reason: HaltReason, mut prior: Vec<Effect>) -> TransitionOutcome {
    prior.push(Effect::AppendFsync(JournalRecord::Halted {
        reason: reason.clone(),
    }));
    prior.push(Effect::HaltNewExposure);
    TransitionOutcome::Halt {
        new_state: OrderState::Halted {
            reason: reason.clone(),
        },
        reason,
        effects: prior,
    }
}

/// Halt into a specific non-Halted halt state (ImmediateFillUnresolved / UnknownNoMatch)
/// while still durable-journaling the reason.
fn halt_with_reason_state(
    new_state: OrderState,
    reason: HaltReason,
    mut prior: Vec<Effect>,
) -> TransitionOutcome {
    // Avoid double Halted journal if caller already pushed a specialized record.
    let already_halted_journal = prior.iter().any(|e| {
        matches!(
            e,
            Effect::AppendFsync(JournalRecord::Halted { .. })
                | Effect::AppendFsync(JournalRecord::ImmediateFillUnresolved)
                | Effect::AppendFsync(JournalRecord::UnknownNoMatch)
        )
    });
    if !already_halted_journal {
        prior.push(Effect::AppendFsync(JournalRecord::Halted {
            reason: reason.clone(),
        }));
    }
    if !prior.iter().any(|e| matches!(e, Effect::HaltNewExposure)) {
        prior.push(Effect::HaltNewExposure);
    }
    TransitionOutcome::Halt {
        new_state,
        reason,
        effects: prior,
    }
}

// ─── R1: single terminal + ReleaseReservation choke point ─────────────────────

/// Proposed true-terminal kind for [`try_finalize_terminal`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposedTerminal {
    Filled {
        /// Venue-authoritative filled qty (must equal order_qty for Filled).
        venue_authoritative_filled: u64,
        /// Venue-reported remaining (F3: must be 0 for Filled release).
        venue_remaining_qty: u64,
    },
    Canceled,
    Terminal,
}

/// **Unique** producer of `Effect::ReleaseReservation` and true terminal states.
///
/// Enforces all five release invariants atomically:
/// 1. `ctx.authority_is_fresh()` (F1/G1: generation-scoped latch; never a call-site bool)
/// 2. `attributed_fill_qty == fill_obligation`
/// 3. avg/fee cross-check on **response snapshot domain** only (when boundary reliable)
/// 4. Filled: `venue_authoritative_filled == order_qty && venue_remaining == 0 && local remaining == 0`
/// 5. ownership: every attributed fill with known parent venue has Some(venue)==bound (G2)
///
/// Any failure → ReconcilePending (recoverable) or Halt (contradiction) — **no release**.
fn try_finalize_terminal(
    ctx: &OrderCtx,
    proposed: ProposedTerminal,
    venue_order_id: VenueOrderId,
    mut effects: Vec<Effect>,
    // Terminal intent for ReconcilePending fallback when gates fail.
    fallback_terminal: ReconcileTerminal,
) -> TransitionOutcome {
    // Venue remaining from proposed Filled (G3: persist into fallback target).
    let proposed_venue_remaining = match &proposed {
        ProposedTerminal::Filled {
            venue_remaining_qty,
            ..
        } => Some(*venue_remaining_qty),
        _ => ctx.last_venue_remaining_qty,
    };

    // ── invariant 5: ownership re-check + G2 None-provenance block ──
    if let Some(expected) = &ctx.venue_order_id {
        for (fid, p) in &ctx.applied_fills {
            match &p.venue_order_id {
                Some(got) if got == expected => {}
                Some(got) => {
                    return halt_with_reason(
                        HaltReason::FillOwnershipMismatch {
                            fill_id: fid.clone(),
                            expected: expected.clone(),
                            got: got.clone(),
                        },
                        effects,
                    );
                }
                None => {
                    // G2: bound parent requires verified provenance on every fill.
                    // Recoverable: backfill authoritative fills with venue id.
                    effects.push(Effect::BackfillFills {
                        venue_order_id: venue_order_id.clone(),
                    });
                    return not_ready_for_terminal(
                        ctx,
                        venue_order_id,
                        effects,
                        fallback_terminal,
                        /* request_authority */ true,
                        proposed_venue_remaining,
                    );
                }
            }
        }
    }

    // ── invariant 1: authority complete AND fresh generation (F1/G1) ──
    if !ctx.authority_is_fresh() {
        return not_ready_for_terminal(
            ctx,
            venue_order_id,
            effects,
            fallback_terminal,
            /* request_authority */ true,
            proposed_venue_remaining,
        );
    }

    // ── invariant 2: attributed == obligation ──
    if ctx.attributed_fill_qty != ctx.fill_obligation {
        return not_ready_for_terminal(
            ctx,
            venue_order_id,
            effects,
            fallback_terminal,
            /* request_authority */ false,
            proposed_venue_remaining,
        );
    }

    // ── invariant 3: response-domain integrity (H1/M1) then avg/fee ──
    // H1: reliable boundary + known response_fill_count ⇒ domain must be closed
    // before release. Out-of-domain fills covering global obligation do NOT substitute.
    // M1: domain_qty > response_fill_count is a snapshot contradiction → Halt.
    if let Some(rc) = ctx.response_fill_count {
        if has_reliable_boundary(ctx) {
            if ctx.response_domain_qty < rc {
                // Domain unclosed: wait for in-domain fills / authority — never release.
                effects.push(Effect::BackfillFills {
                    venue_order_id: venue_order_id.clone(),
                });
                return not_ready_for_terminal(
                    ctx,
                    venue_order_id,
                    effects,
                    fallback_terminal,
                    /* request_authority */ true,
                    proposed_venue_remaining,
                );
            }
            if ctx.response_domain_qty > rc {
                return halt_with_reason(
                    HaltReason::CrossCheckMismatch {
                        detail: format!(
                            "response_domain_qty={} > response_fill_count={rc}",
                            ctx.response_domain_qty
                        ),
                    },
                    effects,
                );
            }
            // Domain closed (==): avg/fee cross-check on response snapshot domain only.
            if let Some(avg) = ctx.response_avg_price_cents {
                if let Some(detail) =
                    notional_avg_mismatch(ctx.response_domain_notional_cents, avg, rc)
                {
                    return halt_with_reason(HaltReason::CrossCheckMismatch { detail }, effects);
                }
            }
            if let Some(detail) =
                fee_cross_check_mismatch(ctx.response_fee_cents, ctx.response_domain_fee_cents)
            {
                return halt_with_reason(HaltReason::CrossCheckMismatch { detail }, effects);
            }
        }
        // No reliable boundary → skip precise avg/fee (obligation-only).
    }

    // ── invariant 4: Filled-specific (F3 includes venue_remaining) ──
    if let ProposedTerminal::Filled {
        venue_authoritative_filled,
        venue_remaining_qty,
    } = &proposed
    {
        if *venue_authoritative_filled > ctx.qty {
            return halt_with_reason(
                HaltReason::OverFill {
                    attributed_fill_qty: ctx.attributed_fill_qty,
                    fill_qty: venue_authoritative_filled.saturating_sub(ctx.attributed_fill_qty),
                    order_qty: ctx.qty,
                },
                effects,
            );
        }
        let remaining = match ctx.remaining_qty_checked() {
            Some(r) => r,
            None => {
                return halt_with_reason(
                    HaltReason::OverFill {
                        attributed_fill_qty: ctx.attributed_fill_qty,
                        fill_qty: 0,
                        order_qty: ctx.qty,
                    },
                    effects,
                );
            }
        };
        if *venue_authoritative_filled != ctx.qty || *venue_remaining_qty != 0 || remaining != 0 {
            if ctx.attributed_fill_qty > ctx.qty {
                return halt_with_reason(
                    HaltReason::OverFill {
                        attributed_fill_qty: ctx.attributed_fill_qty,
                        fill_qty: 0,
                        order_qty: ctx.qty,
                    },
                    effects,
                );
            }
            return not_ready_for_terminal(
                ctx,
                venue_order_id,
                effects,
                ReconcileTerminal::Filled,
                false,
                Some(*venue_remaining_qty),
            );
        }
    }

    // ── all gates passed: unique ReleaseReservation production site ──
    let (kind, new_state) = match proposed {
        ProposedTerminal::Filled { .. } => (TerminalKind::Filled, OrderState::Filled),
        ProposedTerminal::Canceled => (TerminalKind::Canceled, OrderState::Canceled),
        ProposedTerminal::Terminal => (TerminalKind::Terminal, OrderState::Terminal),
    };
    effects.push(Effect::AppendFsync(JournalRecord::OrderTerminal {
        kind,
        venue_order_id: ctx.venue_order_id.clone(),
        fill_obligation: ctx.fill_obligation,
        authority_complete: ctx.authority_is_fresh(),
        authority_epoch: ctx.authority_epoch,
        attributed_fill_qty: ctx.attributed_fill_qty,
        attributed_fee_cents: ctx.attributed_fee_cents,
    }));
    effects.push(Effect::ReleaseReservation);
    accept(new_state, effects)
}

/// Non-terminal path when finalize gates fail: ReconcilePending + backfill/authority.
fn not_ready_for_terminal(
    ctx: &OrderCtx,
    venue_order_id: VenueOrderId,
    mut effects: Vec<Effect>,
    fallback_terminal: ReconcileTerminal,
    request_authority: bool,
    venue_remaining_qty: Option<u64>,
) -> TransitionOutcome {
    let target_qty = ctx.fill_obligation.max(ctx.attributed_fill_qty);
    // G3: prefer explicit remaining, else last seen on ctx.
    let venue_remaining_qty = venue_remaining_qty.or(ctx.last_venue_remaining_qty);
    if ctx.attributed_fill_qty < ctx.fill_obligation {
        effects.push(Effect::BackfillFills {
            venue_order_id: venue_order_id.clone(),
        });
    }
    // R6/F11: authority incomplete, or attributed already matches obligation but
    // finalize still failed → drive shell to re-fetch authority (cannot progress
    // on empty BackfillFills alone).
    if request_authority || ctx.attributed_fill_qty >= ctx.fill_obligation {
        effects.push(Effect::RequestAuthorityReconcile {
            venue_order_id: Some(venue_order_id.clone()),
            client_order_id: ctx.client_order_id.clone(),
        });
        if request_authority
            && !effects
                .iter()
                .any(|e| matches!(e, Effect::ReconcileCancel { .. }))
        {
            effects.push(Effect::ReconcileCancel {
                venue_order_id: Some(venue_order_id.clone()),
            });
        }
    }

    accept(
        OrderState::ReconcilePending {
            venue_order_id,
            filled_qty: ctx.attributed_fill_qty,
            remaining_qty: ctx.remaining_qty(),
            target: ReconcileTarget {
                terminal: fallback_terminal,
                venue_filled_qty: target_qty,
                venue_remaining_qty,
            },
            response_fill_count: ctx.response_fill_count,
            response_avg_price_cents: ctx.response_avg_price_cents,
            response_fee_cents: ctx.response_fee_cents,
        },
        effects,
    )
}

// ─── Submit response / B2 ─────────────────────────────────────────────────────

fn apply_submit_response(
    ctx: &mut OrderCtx,
    venue_order_id: VenueOrderId,
    fill_count: u64,
    remaining_count: u64,
    avg_price_cents: Option<u64>,
    fee_cents: Option<u64>,
    snapshot_boundary: Option<SnapshotBoundary>,
) -> TransitionOutcome {
    // R3: bind venue identity.
    let bind_jr = match bind_venue_id(ctx, &venue_order_id) {
        Ok(jr) => jr,
        Err(reason) => {
            let effects = vec![Effect::AppendFsync(JournalRecord::SubmitResponse {
                venue_order_id: venue_order_id.clone(),
                fill_count,
                remaining_count,
                avg_price_cents,
                fee_cents,
                snapshot_boundary,
            })];
            return halt_with_reason(reason, effects);
        }
    };

    let mut effects = vec![Effect::AppendFsync(JournalRecord::SubmitResponse {
        venue_order_id: venue_order_id.clone(),
        fill_count,
        remaining_count,
        avg_price_cents,
        fee_cents,
        snapshot_boundary,
    })];
    push_journal(&mut effects, bind_jr);
    ctx.note_venue_remaining(remaining_count);

    if fill_count == 0 {
        // D4: must verify no prior WS fills before claiming Accepted zero-fill.
        if ctx.attributed_fill_qty != 0 {
            // ★ A5（2026-08-15 四仓扫雷 LOW/D4）：response 是**建单时刻快照** ——
            //   建单后 ε 秒抢跑到达的 WS fill 与 fill_count=0 不矛盾（域外新证据，
            //   与 (SubmitStarted, Fill) 被显式支持自洽）。remaining 与 qty 一致
            //   （venue 说全量驻留）时按 Partial 收（fill 已入账）；remaining 也
            //   对不上（如 IOC 0/0 却有本地 fill）才是真矛盾，保持 halt。
            if remaining_count == ctx.qty {
                // ★ A5b（复审 HIGH）：走 `route_live_or_filled` 而非手搓 Partial ——
                //   全量抢跑（attributed==qty，小 clip 单笔打满是常态）手搓会落
                //   Partial{qty, 0} 僵尸：非终态不放资金、且壳 open 投影要求
                //   remaining>0 ⇒ 行对策略/cancel_all 全不可见。route 的
                //   remaining==0 支走 F1 latch → Filled+Release；partial 支产物
                //   与手搓逐字段相同。
                return route_live_or_filled(ctx, venue_order_id, effects);
            }
            let reason = HaltReason::ResponseZeroButLocalFills {
                attributed_fill_qty: ctx.attributed_fill_qty,
                remaining_count,
            };
            return halt_with_reason(reason, effects);
        }
        if remaining_count != ctx.qty {
            // ★ A1（扫雷 HIGH）：`fill_count=0 && remaining_count=0` 是 IOC 全 miss
            //   的**合法结局**（零成交、非驻留 —— 追砸时对手价撤走，恰是触发
            //   flatten/止损的行情）。此前无此臂 ⇒ CrossCheckMismatch Halt ⇒ 壳
            //   `?` bail 杀 live loop，进程死时手里正骑着逆向仓。create response
            //   对 IOC 是 venue 权威原子结果 ⇒ 记 authority 证据走 Canceled 终局。
            //   （post-only cross 被壳侧 HTTP 400 typed 挡在 DefiniteReject，不经
            //   此路；attributed>0 已在上面分支处理。）
            if remaining_count == 0 {
                let mut effects = effects;
                push_journal(&mut effects, note_authority_complete(ctx, true));
                return try_finalize_terminal(
                    ctx,
                    ProposedTerminal::Canceled,
                    venue_order_id,
                    effects,
                    ReconcileTerminal::Canceled,
                );
            }
            let reason = HaltReason::CrossCheckMismatch {
                detail: format!(
                    "fill_count=0 remaining_count={remaining_count} != order_qty={}",
                    ctx.qty
                ),
            };
            return halt_with_reason(reason, effects);
        }
        return accept(OrderState::Accepted { venue_order_id }, effects);
    }

    // fill_count > 0 → ImmediateFillUnattributed. Conservative FULL reservation.
    // ★ Do NOT account cash/position/fees from response aggregates.
    ctx.response_fill_count = Some(fill_count);
    ctx.response_remaining_count = Some(remaining_count);
    ctx.response_avg_price_cents = avg_price_cents;
    ctx.response_fee_cents = fee_cents;
    ctx.response_snapshot_boundary = snapshot_boundary;
    // R2: response fill_count raises obligation (G1 may invalidate if lag).
    let obl_before = ctx.fill_obligation;
    let epoch_before = ctx.authority_epoch;
    if let Err(reason) = ctx.raise_fill_obligation(fill_count) {
        return halt_with_reason(reason, effects);
    }
    if ctx.fill_obligation > obl_before {
        effects.push(Effect::AppendFsync(JournalRecord::ObligationRaised {
            fill_obligation: ctx.fill_obligation,
            authority_epoch: ctx.authority_epoch,
        }));
    }
    if ctx.authority_epoch != epoch_before {
        effects.push(Effect::AppendFsync(JournalRecord::AuthorityInvalidated {
            epoch: ctx.authority_epoch,
        }));
    }
    // Rebuild domain from any WS fills that arrived before response (F6 identity).
    recompute_response_domain(ctx);
    if let Some(detail) = response_domain_overfill_detail(ctx) {
        return halt_with_reason(HaltReason::CrossCheckMismatch { detail }, effects);
    }

    effects.push(Effect::AppendFsync(
        JournalRecord::ImmediateFillUnattributed {
            venue_order_id: venue_order_id.clone(),
            fill_count,
            remaining_count,
            avg_price_cents,
            fee_cents,
        },
    ));
    effects.push(Effect::BackfillFills {
        venue_order_id: venue_order_id.clone(),
    });

    let unattributed = OrderState::ImmediateFillUnattributed {
        venue_order_id: venue_order_id.clone(),
        response_fill_count: fill_count,
        response_remaining_count: remaining_count,
        response_avg_price_cents: avg_price_cents,
        response_fee_cents: fee_cents,
    };

    // If WS fills already attributed enough to cover fill_count, resolve now.
    // G6: domain-aware — resolve when domain (or obligation-only) is ready.
    if ctx.attributed_fill_qty > 0 {
        return resolve_unattributed(ctx, &unattributed, vec![], effects);
    }

    accept(unattributed, effects)
}

/// Apply fills under ImmediateFillUnattributed; cross-check vs response.
fn apply_immediate_fill_backfill(
    ctx: &mut OrderCtx,
    state: &OrderState,
    fills: &[FillRecord],
) -> TransitionOutcome {
    let OrderState::ImmediateFillUnattributed {
        venue_order_id,
        response_fill_count,
        response_remaining_count,
        response_avg_price_cents,
        response_fee_cents,
    } = state
    else {
        return reject_illegal(
            state,
            &OrderEvent::ImmediateFillBackfillResult {
                fills: fills.to_vec(),
            },
        );
    };

    // Ensure venue bound.
    let mut effects = Vec::new();
    match bind_venue_id(ctx, venue_order_id) {
        Ok(jr) => push_journal(&mut effects, jr),
        Err(reason) => return halt_with_reason(reason, vec![]),
    }

    for f in fills {
        match try_account_fill_record(ctx, f) {
            AccountAttempt::NoOp => {}
            AccountAttempt::Applied { effects: e } | AccountAttempt::Enriched { effects: e } => {
                effects.extend(e)
            }
            AccountAttempt::Halt { reason, effects: e } => {
                effects.extend(e);
                return halt_with_reason(reason, effects);
            }
        }
    }

    let unattributed = OrderState::ImmediateFillUnattributed {
        venue_order_id: venue_order_id.clone(),
        response_fill_count: *response_fill_count,
        response_remaining_count: *response_remaining_count,
        response_avg_price_cents: *response_avg_price_cents,
        response_fee_cents: *response_fee_cents,
    };
    resolve_unattributed(ctx, &unattributed, effects, vec![])
}

fn resolve_unattributed(
    ctx: &mut OrderCtx,
    unattributed: &OrderState,
    mut prior_effects: Vec<Effect>,
    mut enter_effects: Vec<Effect>,
) -> TransitionOutcome {
    let OrderState::ImmediateFillUnattributed {
        venue_order_id,
        response_fill_count,
        response_remaining_count,
        response_avg_price_cents,
        response_fee_cents,
    } = unattributed
    else {
        return TransitionOutcome::Reject {
            reason: RejectReason::NotApplicable {
                detail: "resolve_unattributed requires ImmediateFillUnattributed".into(),
            },
        };
    };

    // G6/L1: qty/remaining cross-check is **domain-scoped** only when boundary is
    // reliable (TsNs). Seq is unimplemented → obligation-only (never false Halt).
    let has_boundary = has_reliable_boundary(ctx);

    if has_boundary {
        // Not yet fully attributed in response domain → stay unattributed.
        if ctx.response_domain_qty < *response_fill_count {
            prior_effects.append(&mut enter_effects);
            return accept(unattributed.clone(), prior_effects);
        }
        // Domain qty must equal response_fill_count exactly.
        if ctx.response_domain_qty != *response_fill_count {
            let implied_remaining = ctx.qty.saturating_sub(ctx.response_domain_qty);
            let reason = HaltReason::ImmediateFillCrossCheckMismatch {
                response_fill_count: *response_fill_count,
                attributed_fill_qty: ctx.response_domain_qty,
                response_remaining_count: *response_remaining_count,
                implied_remaining,
            };
            prior_effects.append(&mut enter_effects);
            return halt_with_reason(reason, prior_effects);
        }
        // Response remaining is vs order_qty − response_fill_count (domain closed).
        let response_implied_remaining = ctx.qty.saturating_sub(*response_fill_count);
        if response_implied_remaining != *response_remaining_count {
            let reason = HaltReason::ImmediateFillCrossCheckMismatch {
                response_fill_count: *response_fill_count,
                attributed_fill_qty: ctx.response_domain_qty,
                response_remaining_count: *response_remaining_count,
                implied_remaining: response_implied_remaining,
            };
            prior_effects.append(&mut enter_effects);
            return halt_with_reason(reason, prior_effects);
        }
    } else {
        // No reliable boundary: wait until obligation covered; skip exact global==count.
        if ctx.attributed_fill_qty < *response_fill_count {
            prior_effects.append(&mut enter_effects);
            return accept(unattributed.clone(), prior_effects);
        }
        // Overfill of order still Halt (D3).
        if ctx.remaining_qty_checked().is_none() {
            prior_effects.append(&mut enter_effects);
            return halt_with_reason(
                HaltReason::OverFill {
                    attributed_fill_qty: ctx.attributed_fill_qty,
                    fill_qty: 0,
                    order_qty: ctx.qty,
                },
                prior_effects,
            );
        }
        // Obligation-only: do not Halt when global attributed ≠ response_fill_count
        // (out-of-domain fills may have arrived). Proceed to live routing.
    }

    // E8/E3: cross-check on response domain (R6/F6) — only with reliable boundary.
    if has_boundary && ctx.response_domain_qty == *response_fill_count {
        let resp_avg = response_avg_price_cents.or(ctx.response_avg_price_cents);
        if let Some(avg) = resp_avg {
            if let Some(detail) = notional_avg_mismatch(
                ctx.response_domain_notional_cents,
                avg,
                *response_fill_count,
            ) {
                prior_effects.append(&mut enter_effects);
                return halt_with_reason(HaltReason::CrossCheckMismatch { detail }, prior_effects);
            }
        }
        let resp_fee = response_fee_cents.or(ctx.response_fee_cents);
        if let Some(detail) = fee_cross_check_mismatch(resp_fee, ctx.response_domain_fee_cents) {
            prior_effects.append(&mut enter_effects);
            return halt_with_reason(HaltReason::CrossCheckMismatch { detail }, prior_effects);
        }
    }
    if ctx.response_avg_price_cents.is_none() {
        ctx.response_avg_price_cents = *response_avg_price_cents;
    }
    if ctx.response_fee_cents.is_none() {
        ctx.response_fee_cents = *response_fee_cents;
    }

    prior_effects.append(&mut enter_effects);
    route_live_or_filled(ctx, venue_order_id.clone(), prior_effects)
}

fn route_live_or_filled(
    ctx: &mut OrderCtx,
    venue_order_id: VenueOrderId,
    effects: Vec<Effect>,
) -> TransitionOutcome {
    let Some(remaining) = ctx.remaining_qty_checked() else {
        return halt_with_reason(
            HaltReason::OverFill {
                attributed_fill_qty: ctx.attributed_fill_qty,
                fill_qty: 0,
                order_qty: ctx.qty,
            },
            effects,
        );
    };
    if remaining == 0 {
        // Full order attributed by fill_ids: fill-complete evidence for Filled (F1 latch).
        // Not cancel authority — only full qty fill-id coverage.
        // G1: re-latch after new-fill invalidation on this path.
        let mut effects = effects;
        push_journal(&mut effects, note_authority_complete(ctx, true));
        return try_finalize_terminal(
            ctx,
            ProposedTerminal::Filled {
                venue_authoritative_filled: ctx.attributed_fill_qty,
                venue_remaining_qty: 0,
            },
            venue_order_id,
            effects,
            ReconcileTerminal::Filled,
        );
    } else if ctx.attributed_fill_qty == 0 {
        accept(OrderState::Accepted { venue_order_id }, effects)
    } else {
        accept(
            OrderState::Partial {
                venue_order_id,
                filled_qty: ctx.attributed_fill_qty,
                remaining_qty: remaining,
            },
            effects,
        )
    }
}

fn apply_fill_while_open(
    state: &OrderState,
    ctx: &mut OrderCtx,
    fill_id: &FillId,
    qty: u64,
    price_cents: u64,
    ts_ns: i64,
    fill_venue: Option<VenueOrderId>,
    fee_cents: Option<u64>,
) -> TransitionOutcome {
    match try_account_fill(
        ctx,
        fill_id,
        qty,
        price_cents,
        ts_ns,
        fill_venue.as_ref(),
        fee_cents,
    ) {
        AccountAttempt::NoOp => accept(state.clone(), vec![]),
        AccountAttempt::Halt { reason, effects } => halt_with_reason(reason, effects),
        // G5: enrichment — no new qty. On cancel/reconcile, re-check terminal
        // (provenance upgrade may unlock G2); never synthesize venue_remaining=0.
        AccountAttempt::Enriched { effects } => {
            if matches!(state, OrderState::SubmitStarted { .. }) {
                return accept(state.clone(), effects);
            }
            if let OrderState::CancelPending {
                venue_order_id: v,
                reconcile_target,
                ..
            } = state
            {
                return after_fill_cancel_or_reconcile(
                    ctx,
                    v.clone(),
                    reconcile_target.clone(),
                    effects,
                    /* last_fill_qty */ 0,
                    /* is_enrichment */ true,
                );
            }
            if let OrderState::ReconcilePending {
                venue_order_id: v,
                target,
                ..
            } = state
            {
                return after_fill_cancel_or_reconcile(
                    ctx,
                    v.clone(),
                    Some(target.clone()),
                    effects,
                    0,
                    true,
                );
            }
            accept(state.clone(), effects)
        }
        AccountAttempt::Applied { effects } => {
            // Under SubmitStarted (WS before response): stay Started, only book fills.
            if matches!(state, OrderState::SubmitStarted { .. }) {
                return accept(state.clone(), effects);
            }

            // CancelPending / ReconcilePending: may finalize via E1/E5 target.
            if let OrderState::CancelPending {
                venue_order_id: v,
                reconcile_target,
                ..
            } = state
            {
                return after_fill_cancel_or_reconcile(
                    ctx,
                    v.clone(),
                    reconcile_target.clone(),
                    effects,
                    qty,
                    false,
                );
            }
            if let OrderState::ReconcilePending {
                venue_order_id: v,
                target,
                ..
            } = state
            {
                return after_fill_cancel_or_reconcile(
                    ctx,
                    v.clone(),
                    Some(target.clone()),
                    effects,
                    qty,
                    false,
                );
            }

            let vid = ctx
                .venue_order_id
                .clone()
                .unwrap_or_else(|| VenueOrderId("unknown".into()));
            route_live_or_filled(ctx, vid, effects)
        }
    }
}

/// After a new fill while cancel/reconcile-pending: E1 never release with lag;
/// E5 finalize when attributed catches target.venue_filled_qty — via choke point.
/// G3/G5: enrichment must **not** synthesize venue_remaining=0 to re-drive Filled.
fn after_fill_cancel_or_reconcile(
    ctx: &mut OrderCtx,
    venue_order_id: VenueOrderId,
    reconcile_target: Option<ReconcileTarget>,
    mut effects: Vec<Effect>,
    last_fill_qty: u64,
    is_enrichment: bool,
) -> TransitionOutcome {
    let Some(remaining) = ctx.remaining_qty_checked() else {
        return halt_with_reason(
            HaltReason::OverFill {
                attributed_fill_qty: ctx.attributed_fill_qty,
                fill_qty: last_fill_qty,
                order_qty: ctx.qty,
            },
            effects,
        );
    };

    // E5 first: use persisted target venue_remaining (G3) — never local synthetic 0.
    if let Some(target) = &reconcile_target {
        // Raise obligation from target (R2).
        match note_fill_evidence(ctx, target.venue_filled_qty) {
            Ok(jr) => push_journal(&mut effects, jr),
            Err(reason) => return halt_with_reason(reason, effects),
        }
        if ctx.attributed_fill_qty > target.venue_filled_qty {
            // ★ A4（2026-08-15 四仓扫雷 MED）：target.venue_filled_qty 的唯一产地
            //   是 `not_ready_for_terminal` 的**本地合成地板**（max(obligation,
            //   attributed)），不是 venue 上限 —— 撤单前已撮合、WS 晚到的合法
            //   fill 越过地板不是 venue 矛盾。≤qty ⇒ 地板抬升重合成（保留
            //   fallback terminal 与已观测 remaining）；>qty 的真 OverFill 已由
            //   apply_fill 的 checked 路径在前面拦，此处兜底保持 halt。
            if ctx.attributed_fill_qty <= ctx.qty {
                let fallback = target.terminal;
                let vr = target.venue_remaining_qty;
                return not_ready_for_terminal(
                    ctx,
                    venue_order_id,
                    effects,
                    fallback,
                    /* request_authority */ true,
                    vr,
                );
            }
            return halt_with_reason(
                HaltReason::CrossCheckMismatch {
                    detail: format!(
                        "attributed_fill_qty={} > target.venue_filled_qty={}",
                        ctx.attributed_fill_qty, target.venue_filled_qty
                    ),
                },
                effects,
            );
        }
        if ctx.attributed_fill_qty == target.venue_filled_qty
            && ctx.attributed_fill_qty == ctx.fill_obligation
        {
            // F1/Med2: do NOT hardcode authority *without evidence* — but full
            // fill-id coverage IS evidence（route_live_or_filled 的 remaining==0
            // 同规则，F1 latch）。
            // ★ A2（扫雷 HIGH）：追平的最后一笔 fill 刚 invalidate 了 authority
            //   （新 fill_id 必然 invalidate）——不补 latch 则 gate1 必败 ⇒ 行永
            //   卡 ReconcilePending（资金锁死，且下一次撤单前 reject 杀 loop）。
            //   全量覆盖（attributed==qty）时按自家规则补 latch；部分成交
            //   canceled 情形不适用此证据，靠撤单重发重供 authority（A3 通链）。
            if ctx.attributed_fill_qty == ctx.qty {
                push_journal(&mut effects, note_authority_complete(ctx, true));
            }
            // G5 enrichment may re-check (provenance unlock) but uses target remaining.
            return finalize_reconcile_target(ctx, target, effects, venue_order_id);
        }
        // Still lagging: stay ReconcilePending with target (preserve venue_remaining).
        return accept(
            OrderState::ReconcilePending {
                venue_order_id,
                filled_qty: ctx.attributed_fill_qty,
                remaining_qty: remaining,
                target: target.clone(),
                response_fill_count: ctx.response_fill_count,
                response_avg_price_cents: ctx.response_avg_price_cents,
                response_fee_cents: ctx.response_fee_cents,
            },
            effects,
        );
    }

    // Fully filled order without target → Filled only on true new qty, not enrichment.
    // G3: enrichment must not invent venue_remaining=0.
    if remaining == 0 && !is_enrichment {
        push_journal(&mut effects, note_authority_complete(ctx, true));
        return try_finalize_terminal(
            ctx,
            ProposedTerminal::Filled {
                venue_authoritative_filled: ctx.attributed_fill_qty,
                venue_remaining_qty: 0,
            },
            venue_order_id,
            effects,
            ReconcileTerminal::Filled,
        );
    }
    if remaining == 0 && is_enrichment {
        // Metadata only: stay in cancel/reconcile with no synthetic remaining.
        return accept(
            OrderState::CancelPending {
                venue_order_id,
                filled_qty: ctx.attributed_fill_qty,
                remaining_qty: remaining,
                response_fill_count: ctx.response_fill_count,
                response_avg_price_cents: ctx.response_avg_price_cents,
                response_fee_cents: ctx.response_fee_cents,
                reconcile_target: None,
            },
            effects,
        );
    }

    // E1: if response fill obligation still unfilled, never release — stay CancelPending.
    if ctx.attributed_fill_qty < ctx.fill_obligation {
        return accept(
            OrderState::CancelPending {
                venue_order_id,
                filled_qty: ctx.attributed_fill_qty,
                remaining_qty: remaining,
                response_fill_count: ctx.response_fill_count,
                response_avg_price_cents: ctx.response_avg_price_cents,
                response_fee_cents: ctx.response_fee_cents,
                reconcile_target: None,
            },
            effects,
        );
    }

    accept(
        OrderState::CancelPending {
            venue_order_id,
            filled_qty: ctx.attributed_fill_qty,
            remaining_qty: remaining,
            response_fill_count: ctx.response_fill_count,
            response_avg_price_cents: ctx.response_avg_price_cents,
            response_fee_cents: ctx.response_fee_cents,
            reconcile_target: None,
        },
        effects,
    )
}

fn finalize_reconcile_target(
    ctx: &OrderCtx,
    target: &ReconcileTarget,
    effects: Vec<Effect>,
    venue_order_id: VenueOrderId,
) -> TransitionOutcome {
    // Attribution caught target — finalize via choke point.
    // F1/Med2: never hardcode authority; gate reads ctx.authority_is_fresh() only.
    match target.terminal {
        ReconcileTerminal::Canceled => try_finalize_terminal(
            ctx,
            ProposedTerminal::Canceled,
            venue_order_id,
            effects,
            ReconcileTerminal::Canceled,
        ),
        ReconcileTerminal::Filled => {
            // G3: use persisted venue_remaining — never invent 0 when remaining was seen.
            let venue_remaining_qty = match target.venue_remaining_qty {
                Some(r) => r,
                None => ctx.last_venue_remaining_qty.unwrap_or_else(|| {
                    // Never observed remaining: only allow 0 when filled == order qty.
                    if target.venue_filled_qty == ctx.qty {
                        0
                    } else {
                        1 // fail-closed
                    }
                }),
            };
            try_finalize_terminal(
                ctx,
                ProposedTerminal::Filled {
                    venue_authoritative_filled: target.venue_filled_qty,
                    venue_remaining_qty,
                },
                venue_order_id,
                effects,
                ReconcileTerminal::Filled,
            )
        }
    }
}

fn apply_fill_dedup_only(
    ctx: &mut OrderCtx,
    fill_id: &FillId,
    qty: u64,
    price_cents: u64,
    ts_ns: i64,
    fill_venue: Option<&VenueOrderId>,
    fee_cents: Option<u64>,
    stay: OrderState,
) -> TransitionOutcome {
    match try_account_fill(ctx, fill_id, qty, price_cents, ts_ns, fill_venue, fee_cents) {
        AccountAttempt::NoOp => accept(stay, vec![]),
        AccountAttempt::Halt { reason, effects } => halt_with_reason(reason, effects),
        AccountAttempt::Applied { effects } | AccountAttempt::Enriched { effects } => {
            accept(stay, effects)
        }
    }
}

/// §6.C pure match on shell-injected backfill records (D1 / D6 / R5).
fn apply_unknown_backfill(
    ctx: &mut OrderCtx,
    exhaustive: bool,
    matched: &[BackfillOrderRecord],
) -> TransitionOutcome {
    // D6: count matches — 0 / 1 / ≥2.
    match matched.len() {
        0 if exhaustive => {
            // ★ §6.C.4: no match after exhaustive backfill → UnknownNoMatch + HALT + full reservation.
            halt_with_reason_state(
                OrderState::UnknownNoMatch,
                HaltReason::UnknownNoMatch,
                vec![Effect::AppendFsync(JournalRecord::UnknownNoMatch)],
            )
        }
        0 => {
            // Not exhaustive: stay SubmitUnknown, shell continues paging.
            accept(
                OrderState::SubmitUnknown,
                vec![Effect::BackfillUnknown {
                    client_order_id: ctx.client_order_id.clone(),
                }],
            )
        }
        n if n >= 2 => {
            // R5: ambiguous before any accounting — no pollution.
            halt_with_reason(HaltReason::AmbiguousMatch { count: n }, vec![])
        }
        _ => {
            let rec = &matched[0];
            // Must match our client_order_id (shell should filter; belt-and-suspenders).
            if rec.client_order_id != ctx.client_order_id {
                let reason = HaltReason::Operator(format!(
                    "backfill client_order_id mismatch: got {}, expected {}",
                    rec.client_order_id, ctx.client_order_id
                ));
                // R6/F13: durable Halted journal via helper.
                return halt_with_reason(reason, vec![]);
            }

            // R5: non-exhaustive single candidate — buffer only (no AccountFill).
            // Stay SubmitUnknown; do not bind venue or raise obligation from unconfirmed match.
            if !exhaustive {
                let mut effects = vec![Effect::BackfillUnknown {
                    client_order_id: ctx.client_order_id.clone(),
                }];
                // If candidate reports unattributed fills, also request fill pages —
                // but do not book them yet.
                if rec.filled_qty > 0 && rec.fills.is_empty() {
                    effects.push(Effect::BackfillFills {
                        venue_order_id: rec.venue_order_id.clone(),
                    });
                }
                return accept(OrderState::SubmitUnknown, effects);
            }

            // Exhaustive + unique: bind venue, account fills, route.
            let mut effects = Vec::new();
            match bind_venue_id(ctx, &rec.venue_order_id) {
                Ok(jr) => push_journal(&mut effects, jr),
                Err(reason) => return halt_with_reason(reason, vec![]),
            }
            ctx.note_venue_remaining(rec.remaining_qty);
            // A2/L2: durable ReconcileObserved **before** Halt-prone note_fill_evidence
            // so obligation-exceed Halt still carries last_venue_remaining_qty on fold.
            effects.push(Effect::AppendFsync(JournalRecord::ReconcileObserved {
                venue_order_id: rec.venue_order_id.clone(),
                venue_filled_qty: rec.filled_qty,
                venue_remaining_qty: rec.remaining_qty,
                // Latch is applied via AuthorityLatched after fills; remaining is durable now.
                shell_authority_complete: false,
                authority_epoch: ctx.authority_epoch,
            }));
            // R2: venue_filled raises obligation (before authority latch so lag clears it).
            match note_fill_evidence(ctx, rec.filled_qty) {
                Ok(jr) => push_journal(&mut effects, jr),
                Err(reason) => return halt_with_reason(reason, effects),
            }
            // F1: exhaustive unique backfill is authority-complete evidence.
            push_journal(&mut effects, note_authority_complete(ctx, true));

            for f in &rec.fills {
                // Exhaustive match context: fills should carry venue id.
                if f.venue_order_id.is_none() {
                    return halt_with_reason(
                        HaltReason::FillOwnershipMismatch {
                            fill_id: f.fill_id.clone(),
                            expected: rec.venue_order_id.clone(),
                            got: VenueOrderId("<missing>".into()),
                        },
                        effects,
                    );
                }
                match try_account_fill_record(ctx, f) {
                    AccountAttempt::NoOp => {}
                    AccountAttempt::Applied { effects: e }
                    | AccountAttempt::Enriched { effects: e } => effects.extend(e),
                    AccountAttempt::Halt { reason, effects: e } => {
                        effects.extend(e);
                        return halt_with_reason(reason, effects);
                    }
                }
            }
            // New fills invalidate authority (G1); re-latch after full account if shell exhaustive.
            if exhaustive {
                push_journal(&mut effects, note_authority_complete(ctx, true));
            }
            route_from_backfill_status(
                ctx,
                rec.venue_order_id.clone(),
                rec.status,
                rec.filled_qty,
                rec.remaining_qty,
                &[],
                exhaustive, // shell exhaustive flag for SubmitUnknown routing
                /* from_submit_unknown */ true,
                ctx.response_fill_count,
                ctx.response_avg_price_cents,
                ctx.response_fee_cents,
            )
            .prepend_effects(effects)
        }
    }
}

/// Route by venue status; terminal + ReleaseReservation **only** via
/// [`try_finalize_terminal`] when gates pass.
///
/// `shell_authority_complete` is shell evidence used to **latch** ctx (F1) and for
/// SubmitUnknown exhaustive routing — never passed into the release gate.
fn route_from_backfill_status(
    ctx: &mut OrderCtx,
    venue_order_id: VenueOrderId,
    status: BackfillOrderStatus,
    venue_filled_qty: u64,
    venue_remaining_qty: u64,
    extra_fills: &[FillRecord],
    shell_authority_complete: bool,
    from_submit_unknown: bool,
    _response_fill_count: Option<u64>,
    _response_avg: Option<u64>,
    _response_fee: Option<u64>,
) -> TransitionOutcome {
    let mut effects = Vec::new();
    match bind_venue_id(ctx, &venue_order_id) {
        Ok(jr) => push_journal(&mut effects, jr),
        Err(reason) => return halt_with_reason(reason, vec![]),
    }
    // G3: persist venue remaining evidence.
    ctx.note_venue_remaining(venue_remaining_qty);
    // A2/L2: durable ReconcileObserved **before** Halt-prone note_fill_evidence so
    // obligation-exceed / ownership Halt still carries last_venue_remaining_qty on fold.
    // shell_authority_complete=false here — real latch is AuthorityLatched after fills.
    effects.push(Effect::AppendFsync(JournalRecord::ReconcileObserved {
        venue_order_id: venue_order_id.clone(),
        venue_filled_qty,
        venue_remaining_qty,
        shell_authority_complete: false,
        authority_epoch: ctx.authority_epoch,
    }));
    // R2: venue filled raises obligation first (G1 may clear stale latch).
    match note_fill_evidence(ctx, venue_filled_qty) {
        Ok(jr) => push_journal(&mut effects, jr),
        Err(reason) => return halt_with_reason(reason, effects),
    }

    // Apply any extra fills first (dedup + ownership + overfill). New fills invalidate
    // authority (G1); shell latch happens **after** fills so post-cancel authority is fresh.
    for f in extra_fills {
        match try_account_fill_record(ctx, f) {
            AccountAttempt::NoOp => {}
            AccountAttempt::Applied { effects: e } | AccountAttempt::Enriched { effects: e } => {
                effects.extend(e)
            }
            AccountAttempt::Halt { reason, effects: e } => {
                effects.extend(e);
                return halt_with_reason(reason, effects);
            }
        }
    }

    // F1/G1: latch only when shell proves authority complete (never hardcode true).
    // After fill-admitting activity, this is a **new** generation latch.
    push_journal(
        &mut effects,
        note_authority_complete(ctx, shell_authority_complete),
    );
    // G4: final reconcile snapshot after fills (epoch/latch); remaining already durable.
    if shell_authority_complete {
        effects.push(Effect::AppendFsync(JournalRecord::ReconcileObserved {
            venue_order_id: venue_order_id.clone(),
            venue_filled_qty,
            venue_remaining_qty,
            shell_authority_complete,
            authority_epoch: ctx.authority_epoch,
        }));
    }

    // Over-attribution vs venue (local has more than venue reports) → halt.
    if ctx.attributed_fill_qty > venue_filled_qty {
        return halt_with_reason(
            HaltReason::CrossCheckMismatch {
                detail: format!(
                    "attributed_fill_qty={} > venue_filled_qty={venue_filled_qty}",
                    ctx.attributed_fill_qty
                ),
            },
            effects,
        );
    }

    match status {
        BackfillOrderStatus::Canceled => {
            // ★ 2026-08-15 审计 HIGH **撤销**（docs/OMSRS_AUDIT_2026_08_15.md §复核）：
            //   勿在此加 filled/remaining 守恒门。真 REST 实测（o2a 探针 1000 单）：
            //   Kalshi canceled 单 **987/1000 报 fill_count=0/remaining_count=0**（撤后
            //   清零常态），恒等 filled+remaining==qty 0/1000 成立——任何 0/0 或恒等门
            //   都会 false-halt 超时恢复 backfill 流（98.7% 命中，资金锁死）。journal
            //   的 ReconcileObserved 是壳回显（本地合成），对场端语义零鉴别力。
            //   「撤成空成交」的防线：fill 已投影 ⇒ note_fill_evidence 抬 obligation ⇒
            //   invariant 2 兜；未投影且行在 ⇒ PostTerminalFill；行不在 ⇒ 壳 off-book。
            //   REST 瞬时少计 + WS 同丢 = 本地不可见，无门可加。见正向钉
            //   inv_canceled_zero_zero_rest_is_normal_release。
            // Attempt terminal via choke point (reads ctx.authority_is_fresh()).
            let o = try_finalize_terminal(
                ctx,
                ProposedTerminal::Canceled,
                venue_order_id.clone(),
                effects,
                ReconcileTerminal::Canceled,
            );
            // If choke point returned ReconcilePending, good; if still SubmitUnknown needed:
            if from_submit_unknown && !shell_authority_complete {
                if let TransitionOutcome::Accept {
                    new_state: OrderState::ReconcilePending { .. },
                    effects: e,
                } = &o
                {
                    // Stay matching-pending so further pages can AmbiguousMatch (E7/E9).
                    let mut e = e.clone();
                    if !e
                        .iter()
                        .any(|x| matches!(x, Effect::BackfillUnknown { .. }))
                    {
                        e.push(Effect::BackfillUnknown {
                            client_order_id: ctx.client_order_id.clone(),
                        });
                    }
                    return accept(OrderState::SubmitUnknown, e);
                }
            }
            o
        }
        BackfillOrderStatus::Filled => {
            let o = try_finalize_terminal(
                ctx,
                ProposedTerminal::Filled {
                    venue_authoritative_filled: venue_filled_qty,
                    venue_remaining_qty,
                },
                venue_order_id.clone(),
                effects,
                ReconcileTerminal::Filled,
            );
            if from_submit_unknown && !shell_authority_complete {
                if let TransitionOutcome::Accept {
                    new_state: OrderState::ReconcilePending { .. },
                    effects: e,
                } = &o
                {
                    let mut e = e.clone();
                    if !e
                        .iter()
                        .any(|x| matches!(x, Effect::BackfillUnknown { .. }))
                    {
                        e.push(Effect::BackfillUnknown {
                            client_order_id: ctx.client_order_id.clone(),
                        });
                    }
                    return accept(OrderState::SubmitUnknown, e);
                }
            }
            o
        }
        BackfillOrderStatus::Open | BackfillOrderStatus::Partial => {
            // E7: SubmitUnknown live routes require exhaustive shell authority
            // before committing a single match to Accepted/Partial.
            if from_submit_unknown && !shell_authority_complete {
                let mut effects = effects;
                if ctx.attributed_fill_qty < venue_filled_qty {
                    effects.push(Effect::BackfillFills {
                        venue_order_id: venue_order_id.clone(),
                    });
                }
                effects.push(Effect::BackfillUnknown {
                    client_order_id: ctx.client_order_id.clone(),
                });
                return accept(OrderState::SubmitUnknown, effects);
            }
            let fills_fully_attributed = ctx.attributed_fill_qty == venue_filled_qty;
            if !fills_fully_attributed {
                return continue_reconcile_pending(
                    ctx,
                    venue_order_id,
                    venue_filled_qty,
                    effects,
                    from_submit_unknown,
                    shell_authority_complete,
                    None,
                );
            }
            let Some(remaining) = ctx.remaining_qty_checked() else {
                return halt_with_reason(
                    HaltReason::OverFill {
                        attributed_fill_qty: ctx.attributed_fill_qty,
                        fill_qty: 0,
                        order_qty: ctx.qty,
                    },
                    effects,
                );
            };
            let live_remaining = if venue_remaining_qty == remaining {
                remaining
            } else if fills_fully_attributed {
                return halt_with_reason(
                    HaltReason::CrossCheckMismatch {
                        detail: format!(
                            "venue_remaining={venue_remaining_qty} != local_remaining={remaining}"
                        ),
                    },
                    effects,
                );
            } else {
                remaining
            };
            if ctx.attributed_fill_qty == 0 {
                accept(OrderState::Accepted { venue_order_id }, effects)
            } else {
                accept(
                    OrderState::Partial {
                        venue_order_id,
                        filled_qty: ctx.attributed_fill_qty,
                        remaining_qty: live_remaining,
                    },
                    effects,
                )
            }
        }
    }
}

/// Non-terminal reconcile-pending: Full reservation + continue backfill (D1/E5).
fn continue_reconcile_pending(
    ctx: &OrderCtx,
    venue_order_id: VenueOrderId,
    venue_filled_qty: u64,
    mut effects: Vec<Effect>,
    from_submit_unknown: bool,
    authority_complete: bool,
    target: Option<ReconcileTarget>,
) -> TransitionOutcome {
    if ctx.attributed_fill_qty < venue_filled_qty {
        effects.push(Effect::BackfillFills {
            venue_order_id: venue_order_id.clone(),
        });
    } else if from_submit_unknown && !authority_complete {
        effects.push(Effect::BackfillUnknown {
            client_order_id: ctx.client_order_id.clone(),
        });
    } else if !authority_complete {
        // R6/F11: attributed == venue_filled but authority incomplete → request authority.
        effects.push(Effect::RequestAuthorityReconcile {
            venue_order_id: Some(venue_order_id.clone()),
            client_order_id: ctx.client_order_id.clone(),
        });
        effects.push(Effect::BackfillFills {
            venue_order_id: venue_order_id.clone(),
        });
    } else {
        effects.push(Effect::BackfillFills {
            venue_order_id: venue_order_id.clone(),
        });
    }

    // E7: non-exhaustive SubmitUnknown stays matching-pending.
    if from_submit_unknown && !authority_complete {
        return accept(OrderState::SubmitUnknown, effects);
    }

    let remaining = ctx.remaining_qty();

    // E5: carry target terminal intent — do NOT demote canceled/filled to live Partial.
    if let Some(t) = target {
        return accept(
            OrderState::ReconcilePending {
                venue_order_id,
                filled_qty: ctx.attributed_fill_qty,
                remaining_qty: remaining,
                target: t,
                response_fill_count: ctx.response_fill_count,
                response_avg_price_cents: ctx.response_avg_price_cents,
                response_fee_cents: ctx.response_fee_cents,
            },
            effects,
        );
    }

    if ctx.attributed_fill_qty == 0 {
        accept(OrderState::Accepted { venue_order_id }, effects)
    } else {
        accept(
            OrderState::Partial {
                venue_order_id,
                filled_qty: ctx.attributed_fill_qty,
                remaining_qty: remaining,
            },
            effects,
        )
    }
}

/// Seven-way cancel outcome routing (§6.B) + R1 choke point + R2 obligation.
fn apply_cancel_outcome(
    ctx: &mut OrderCtx,
    venue_order_id: &VenueOrderId,
    filled_qty: u64,
    remaining_qty: u64,
    response_fill_count: Option<u64>,
    response_avg: Option<u64>,
    response_fee: Option<u64>,
    reconcile_target: Option<ReconcileTarget>,
    outcome: CancelOutcome,
) -> TransitionOutcome {
    // G1: CancelOutcome is fill-admitting (fills may have occurred since last live latch).
    let mut effects = authority_invalidate_effects(ctx);
    effects.push(Effect::AppendFsync(JournalRecord::CancelOutcomeCid {
        client_order_id: ctx.client_order_id.clone(),
        outcome,
    }));

    // Preserve response snapshot fields on ctx if carried in state.
    if let Some(rc) = response_fill_count {
        ctx.response_fill_count = ctx.response_fill_count.or(Some(rc));
        let obl_before = ctx.fill_obligation;
        let epoch_before = ctx.authority_epoch;
        if let Err(reason) = ctx.raise_fill_obligation(rc) {
            return halt_with_reason(reason, effects);
        }
        if ctx.fill_obligation > obl_before {
            effects.push(Effect::AppendFsync(JournalRecord::ObligationRaised {
                fill_obligation: ctx.fill_obligation,
                authority_epoch: ctx.authority_epoch,
            }));
        }
        if ctx.authority_epoch != epoch_before {
            effects.push(Effect::AppendFsync(JournalRecord::AuthorityInvalidated {
                epoch: ctx.authority_epoch,
            }));
        }
    }
    if response_avg.is_some() {
        ctx.response_avg_price_cents = ctx.response_avg_price_cents.or(response_avg);
    }
    if response_fee.is_some() {
        ctx.response_fee_cents = ctx.response_fee_cents.or(response_fee);
    }
    if let Some(t) = &reconcile_target {
        match note_fill_evidence(ctx, t.venue_filled_qty) {
            Ok(jr) => push_journal(&mut effects, jr),
            Err(reason) => return halt_with_reason(reason, effects),
        }
        if let Some(r) = t.venue_remaining_qty {
            ctx.note_venue_remaining(r);
        }
    }
    // Ensure venue bound.
    match bind_venue_id(ctx, venue_order_id) {
        Ok(jr) => push_journal(&mut effects, jr),
        Err(reason) => return halt_with_reason(reason, effects),
    }

    match outcome {
        CancelOutcome::Canceled => {
            // F1/Med1: CancelOutcome NEVER latches authority_complete.
            // Gate reads ctx only — late venue fills cannot be forged away.
            // G1: prior live authority already invalidated above → ReconcilePending.
            try_finalize_terminal(
                ctx,
                ProposedTerminal::Canceled,
                venue_order_id.clone(),
                effects,
                ReconcileTerminal::Canceled,
            )
        }
        CancelOutcome::AlreadyTerminal => {
            // F1: no authority forge; AlreadyTerminal is not fill-complete evidence.
            try_finalize_terminal(
                ctx,
                ProposedTerminal::Terminal,
                venue_order_id.clone(),
                effects,
                // Prefer Filled target if obligation > 0 else Canceled-ish Terminal fallback.
                if ctx.fill_obligation > 0 {
                    ReconcileTerminal::Filled
                } else {
                    ReconcileTerminal::Canceled
                },
            )
        }
        CancelOutcome::Accepted => accept(
            OrderState::CancelPending {
                venue_order_id: venue_order_id.clone(),
                filled_qty,
                remaining_qty,
                response_fill_count: ctx.response_fill_count,
                response_avg_price_cents: ctx.response_avg_price_cents,
                response_fee_cents: ctx.response_fee_cents,
                reconcile_target,
            },
            effects,
        ),
        CancelOutcome::Rejected => {
            // R2: demotion must NOT drop fill_obligation (lives on ctx).
            if filled_qty == 0 && ctx.attributed_fill_qty == 0 {
                accept(
                    OrderState::Accepted {
                        venue_order_id: venue_order_id.clone(),
                    },
                    effects,
                )
            } else {
                accept(
                    OrderState::Partial {
                        venue_order_id: venue_order_id.clone(),
                        filled_qty: ctx.attributed_fill_qty.max(filled_qty),
                        remaining_qty: ctx.remaining_qty(),
                    },
                    effects,
                )
            }
        }
        CancelOutcome::NotFound | CancelOutcome::TransportUnknown | CancelOutcome::Unknown => {
            effects.push(Effect::ReconcileCancel {
                venue_order_id: Some(venue_order_id.clone()),
            });
            accept(
                OrderState::CancelPending {
                    venue_order_id: venue_order_id.clone(),
                    filled_qty,
                    remaining_qty,
                    response_fill_count: ctx.response_fill_count,
                    response_avg_price_cents: ctx.response_avg_price_cents,
                    response_fee_cents: ctx.response_fee_cents,
                    reconcile_target,
                },
                effects,
            )
        }
    }
}

// ─── TransitionOutcome helpers ────────────────────────────────────────────────

fn accept(new_state: OrderState, effects: Vec<Effect>) -> TransitionOutcome {
    TransitionOutcome::Accept { new_state, effects }
}

fn reject_illegal(state: &OrderState, event: &OrderEvent) -> TransitionOutcome {
    TransitionOutcome::Reject {
        reason: RejectReason::IllegalTransition {
            state: state_name(state).into(),
            event: event_name(event).into(),
        },
    }
}

impl TransitionOutcome {
    fn prepend_effects(self, mut prefix: Vec<Effect>) -> Self {
        match self {
            TransitionOutcome::Accept { new_state, effects } => {
                prefix.extend(effects);
                TransitionOutcome::Accept {
                    new_state,
                    effects: prefix,
                }
            }
            TransitionOutcome::Halt {
                new_state,
                reason,
                effects,
            } => {
                prefix.extend(effects);
                TransitionOutcome::Halt {
                    new_state,
                    reason,
                    effects: prefix,
                }
            }
            other => other,
        }
    }
}

fn state_name(s: &OrderState) -> &'static str {
    match s {
        OrderState::New => "New",
        OrderState::SubmitPrepared => "SubmitPrepared",
        OrderState::SubmitStarted { .. } => "SubmitStarted",
        OrderState::SubmitUnknown => "SubmitUnknown",
        OrderState::Accepted { .. } => "Accepted",
        OrderState::Partial { .. } => "Partial",
        OrderState::Filled => "Filled",
        OrderState::CancelPending { .. } => "CancelPending",
        OrderState::ReconcilePending { .. } => "ReconcilePending",
        OrderState::Canceled => "Canceled",
        OrderState::Terminal => "Terminal",
        OrderState::ImmediateFillUnattributed { .. } => "ImmediateFillUnattributed",
        OrderState::ImmediateFillUnresolved => "ImmediateFillUnresolved",
        OrderState::UnknownNoMatch => "UnknownNoMatch",
        OrderState::Halted { .. } => "Halted",
    }
}

fn event_name(e: &OrderEvent) -> &'static str {
    match e {
        OrderEvent::PrepareSubmit => "PrepareSubmit",
        OrderEvent::StartSubmit { .. } => "StartSubmit",
        OrderEvent::SubmitResponse { .. } => "SubmitResponse",
        OrderEvent::SubmitTimeout => "SubmitTimeout",
        OrderEvent::Fill { .. } => "Fill",
        OrderEvent::CancelRequested => "CancelRequested",
        OrderEvent::CancelOutcome(_) => "CancelOutcome",
        OrderEvent::UnknownBackfillResult { .. } => "UnknownBackfillResult",
        OrderEvent::ImmediateFillBackfillResult { .. } => "ImmediateFillBackfillResult",
        OrderEvent::BackfillDeadlineElapsed => "BackfillDeadlineElapsed",
        OrderEvent::RequestResubmit { .. } => "RequestResubmit",
        OrderEvent::ReconcileResult { .. } => "ReconcileResult",
    }
}

// ─── Journal rebuild (F7/G4) ──────────────────────────────────────────────────

/// Fold durable [`JournalRecord`]s into an [`OrderCtx`] for crash restart.
///
/// This is the production rebuild path (not hand-filled ctx fields). Identity
/// fields come from `base`; fill/obligation/authority/response snapshots are
/// reconstructed from journal records in order.
///
/// G4: errors from bind / obligation / fill are **not** silently dropped —
/// they surface as [`HaltReason`].
pub fn rebuild_ctx_from_journal(
    base: OrderCtx,
    records: &[JournalRecord],
) -> Result<OrderCtx, HaltReason> {
    let mut ctx = base;
    for rec in records {
        match rec {
            JournalRecord::SubmitPrepared { client_order_id } => {
                ctx.client_order_id = client_order_id.clone();
            }
            JournalRecord::SubmitStarted { .. } => {}
            JournalRecord::SubmitResponse {
                venue_order_id,
                fill_count,
                remaining_count,
                avg_price_cents,
                fee_cents,
                snapshot_boundary,
            } => {
                bind_venue_id(&mut ctx, venue_order_id)?;
                ctx.note_venue_remaining(*remaining_count);
                if *fill_count > 0 {
                    ctx.response_fill_count = Some(*fill_count);
                    ctx.response_remaining_count = Some(*remaining_count);
                    ctx.response_avg_price_cents = *avg_price_cents;
                    ctx.response_fee_cents = *fee_cents;
                    ctx.response_snapshot_boundary = *snapshot_boundary;
                    // M2: restore obligation only — epoch comes from ObligationRaised /
                    // AuthorityInvalidated records, not re-bumped raise side effects.
                    restore_fill_obligation(&mut ctx, *fill_count)?;
                    recompute_response_domain(&mut ctx);
                }
            }
            JournalRecord::SubmitUnknown => {}
            JournalRecord::Fill {
                fill_id,
                qty,
                price_cents,
                ts_ns,
                fee_cents,
                venue_order_id,
            } => {
                // M2: re-apply fill data only — do not re-run invalidate_authority bump.
                // Epoch is restored from AuthorityInvalidated / ObligationRaised / latch records.
                match try_account_fill_for_fold(
                    &mut ctx,
                    fill_id,
                    *qty,
                    *price_cents,
                    *ts_ns,
                    venue_order_id.as_ref(),
                    *fee_cents,
                ) {
                    AccountAttempt::NoOp
                    | AccountAttempt::Applied { .. }
                    | AccountAttempt::Enriched { .. } => {}
                    AccountAttempt::Halt { reason, .. } => return Err(reason),
                }
            }
            JournalRecord::FeeCorrection {
                fill_id,
                delta_fee_cents,
            } => {
                // If fill path already applied fee via Fill payload, skip double count
                // when payload already has Some(fee). Otherwise apply delta.
                if let Some(p) = ctx.applied_fills.get(fill_id) {
                    if p.fee.is_none() {
                        let total = ctx
                            .attributed_fee_cents
                            .checked_add(*delta_fee_cents)
                            .ok_or_else(|| HaltReason::FeeOverflow {
                                fill_id: fill_id.clone(),
                            })?;
                        ctx.attributed_fee_cents = total;
                        if let Some(p) = ctx.applied_fills.get_mut(fill_id) {
                            p.fee = Some(*delta_fee_cents);
                        }
                    }
                }
            }
            // G1/G4: cancel is fill-admitting. Prefer AuthorityInvalidated absolute epoch
            // when present; fallback invalidate when older journals lack it.
            JournalRecord::CancelRequested
            | JournalRecord::CancelOutcome(_)
            | JournalRecord::CancelRequestedCid { .. }
            | JournalRecord::CancelOutcomeCid { .. } => {
                if ctx.authority_complete {
                    ctx.invalidate_authority();
                }
            }
            JournalRecord::ImmediateFillUnattributed {
                venue_order_id,
                fill_count,
                remaining_count,
                avg_price_cents,
                fee_cents,
            } => {
                bind_venue_id(&mut ctx, venue_order_id)?;
                ctx.response_fill_count = Some(*fill_count);
                ctx.response_remaining_count = Some(*remaining_count);
                ctx.response_avg_price_cents = *avg_price_cents;
                ctx.response_fee_cents = *fee_cents;
                ctx.note_venue_remaining(*remaining_count);
                // M2: no epoch re-bump on fold.
                restore_fill_obligation(&mut ctx, *fill_count)?;
            }
            JournalRecord::ImmediateFillUnresolved
            | JournalRecord::UnknownNoMatch
            | JournalRecord::Halted { .. } => {}
            // G4: non-terminal ctx mutations.
            JournalRecord::VenueBound { venue_order_id } => {
                bind_venue_id(&mut ctx, venue_order_id)?;
            }
            // ★ F9a:Cid 形同语义(cid 载荷仅供离线,fold 只用 wire)。
            JournalRecord::VenueBoundCid { venue_order_id, .. } => {
                bind_venue_id(&mut ctx, venue_order_id)?;
            }
            JournalRecord::ObligationRaised {
                fill_obligation,
                authority_epoch,
            } => {
                // M2: restore obligation + epoch from record; do not re-run raise bump.
                restore_fill_obligation(&mut ctx, *fill_obligation)?;
                ctx.authority_epoch = (*authority_epoch).max(ctx.authority_epoch);
                // Raise with lag clears live authority; mirror when attributed still short.
                if ctx.attributed_fill_qty < ctx.fill_obligation {
                    ctx.authority_complete = false;
                }
            }
            JournalRecord::AuthorityLatched { epoch } => {
                ctx.authority_epoch = (*epoch).max(ctx.authority_epoch);
                ctx.authority_complete = true;
                ctx.authority_latched_epoch = *epoch;
            }
            JournalRecord::AuthorityInvalidated { epoch } => {
                ctx.authority_epoch = (*epoch).max(ctx.authority_epoch);
                ctx.authority_complete = false;
            }
            JournalRecord::ReconcileObserved {
                venue_order_id,
                venue_filled_qty,
                venue_remaining_qty,
                shell_authority_complete,
                authority_epoch,
            } => {
                bind_venue_id(&mut ctx, venue_order_id)?;
                // A2: remaining is always durable (noted in memory before Halt-prone raise).
                ctx.note_venue_remaining(*venue_remaining_qty);
                ctx.authority_epoch = (*authority_epoch).max(ctx.authority_epoch);
                // Venue filled > order qty: live path Halts without raising obligation —
                // still restore remaining/epoch so fold ≡ memory on obligation-exceed Halt.
                if *venue_filled_qty > ctx.qty {
                    // leave fill_obligation unchanged (mirrors note_fill_evidence Err)
                } else {
                    // M2: restore obligation without re-bump; epoch from explicit field.
                    restore_fill_obligation(&mut ctx, *venue_filled_qty)?;
                    if *shell_authority_complete {
                        ctx.latch_authority_complete();
                    } else if ctx.attributed_fill_qty < ctx.fill_obligation {
                        ctx.authority_complete = false;
                    }
                }
            }
            JournalRecord::OrderTerminal {
                kind: _,
                venue_order_id,
                fill_obligation,
                authority_complete,
                authority_epoch,
                attributed_fill_qty,
                attributed_fee_cents,
            } => {
                // G4: truly consume terminal snapshot (venue/obligation/authority/qty/fee).
                if let Some(v) = venue_order_id {
                    bind_venue_id(&mut ctx, v)?;
                }
                // M2: restore obligation without re-bump.
                restore_fill_obligation(&mut ctx, *fill_obligation)?;
                ctx.authority_epoch = (*authority_epoch).max(ctx.authority_epoch);
                if *authority_complete {
                    ctx.authority_complete = true;
                    ctx.authority_latched_epoch = *authority_epoch;
                }
                // Consume attributed qty/fee (high-water / recovery floor).
                if *attributed_fill_qty > ctx.attributed_fill_qty {
                    ctx.attributed_fill_qty = *attributed_fill_qty;
                }
                if *attributed_fee_cents > ctx.attributed_fee_cents {
                    ctx.attributed_fee_cents = *attributed_fee_cents;
                }
            }
        }
    }
    Ok(ctx)
}

// ─── replay helper (§6.D.1 prefix-causal rebuild) ─────────────────────────────

/// Fold events through [`apply_event`]. Reject stops the fold and is returned;
/// Halt continues with the halted state (further events hit restart-safe path).
pub fn replay(mut state: OrderState, mut ctx: OrderCtx, events: &[OrderEvent]) -> ReplayResult {
    let mut all_effects = Vec::new();
    let mut halted: Option<HaltReason> = None;
    for ev in events {
        match apply_event(&state, &mut ctx, ev) {
            TransitionOutcome::Accept { new_state, effects } => {
                all_effects.extend(effects);
                state = new_state;
            }
            TransitionOutcome::Halt {
                new_state,
                reason,
                effects,
            } => {
                all_effects.extend(effects);
                state = new_state;
                halted = Some(reason);
            }
            TransitionOutcome::Reject { reason } => {
                return ReplayResult {
                    state,
                    ctx,
                    effects: all_effects,
                    reject: Some(reason),
                    halt: halted,
                };
            }
        }
    }
    ReplayResult {
        state,
        ctx,
        effects: all_effects,
        reject: None,
        halt: halted,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayResult {
    pub state: OrderState,
    pub ctx: OrderCtx,
    pub effects: Vec<Effect>,
    pub reject: Option<RejectReason>,
    pub halt: Option<HaltReason>,
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_default() -> OrderCtx {
        OrderCtx::new(
            derive_client_order_id("KXBTC-M", "hoff-mm", 1),
            "KXBTC-M",
            "hoff-mm",
            Side::BuyYes,
            45,
            10,
        )
    }

    /// B2: fold `records` from a fresh identity-matched base and assert field-by-field
    /// equivalence with in-memory `mem` for durable fill/obligation/authority/venue state.
    /// Used by A1/M1 (and B3) so Halt paths prove rebuild ≡ live ctx, not a partial subset.
    fn assert_ctx_fold_equiv(mem: &OrderCtx, records: &[JournalRecord]) {
        let base = OrderCtx::new(
            mem.client_order_id.clone(),
            mem.market.clone(),
            mem.strategy.clone(),
            mem.side,
            mem.price_cents,
            mem.qty,
        );
        let rebuilt = rebuild_ctx_from_journal(base, records)
            .unwrap_or_else(|e| panic!("assert_ctx_fold_equiv: rebuild failed: {e:?}"));
        assert_eq!(
            rebuilt.attributed_fill_qty, mem.attributed_fill_qty,
            "fold attributed_fill_qty"
        );
        assert_eq!(
            rebuilt.fill_obligation, mem.fill_obligation,
            "fold fill_obligation"
        );
        assert_eq!(
            rebuilt.response_domain_qty, mem.response_domain_qty,
            "fold response_domain_qty"
        );
        assert_eq!(
            rebuilt.response_domain_notional_cents, mem.response_domain_notional_cents,
            "fold response_domain_notional_cents"
        );
        assert_eq!(
            rebuilt.response_domain_fee_cents, mem.response_domain_fee_cents,
            "fold response_domain_fee_cents"
        );
        assert_eq!(
            rebuilt.attributed_fee_cents, mem.attributed_fee_cents,
            "fold attributed_fee_cents"
        );
        assert_eq!(
            rebuilt.authority_epoch, mem.authority_epoch,
            "fold authority_epoch"
        );
        assert_eq!(
            rebuilt.authority_latched_epoch, mem.authority_latched_epoch,
            "fold authority_latched_epoch"
        );
        assert_eq!(
            rebuilt.authority_complete, mem.authority_complete,
            "fold authority_complete"
        );
        assert_eq!(
            rebuilt.venue_order_id, mem.venue_order_id,
            "fold venue_order_id"
        );
        assert_eq!(
            rebuilt.applied_fills, mem.applied_fills,
            "fold applied_fills (fill_id → payload full fields)"
        );
        // B3 / A2 remaining evidence also durable on fold.
        assert_eq!(
            rebuilt.last_venue_remaining_qty, mem.last_venue_remaining_qty,
            "fold last_venue_remaining_qty"
        );
    }

    fn prepare_started(ctx: &mut OrderCtx) -> OrderState {
        let mut s = OrderState::New;
        let o = apply_event(&s, ctx, &OrderEvent::PrepareSubmit);
        s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            ctx,
            &OrderEvent::StartSubmit {
                attempt_id: AttemptId("a1".into()),
            },
        );
        // Invariant: StartSubmit produces AppendFsync(SubmitStarted) — before any response.
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::AppendFsync(JournalRecord::SubmitStarted { .. }))),
            "StartSubmit must fsync Started before HTTP/response"
        );
        o.new_state().unwrap().clone()
    }

    fn vid(s: &str) -> VenueOrderId {
        VenueOrderId(s.into())
    }
    fn fid(s: &str) -> FillId {
        FillId(s.into())
    }
    fn fill_rec_v(id: &str, qty: u64, price: u64, ts: i64, v: &str) -> FillRecord {
        FillRecord {
            fill_id: fid(id),
            qty,
            price_cents: price,
            ts_ns: ts,
            venue_order_id: Some(vid(v)),
            fee_cents: None,
        }
    }
    fn fill_rec_vf(id: &str, qty: u64, price: u64, ts: i64, v: &str, fee: u64) -> FillRecord {
        FillRecord {
            fill_id: fid(id),
            qty,
            price_cents: price,
            ts_ns: ts,
            venue_order_id: Some(vid(v)),
            fee_cents: Some(fee),
        }
    }

    // ── client_order_id ───────────────────────────────────────────────────

    #[test]
    fn client_order_id_deterministic() {
        let a = derive_client_order_id("MKT", "strat", 7);
        let b = derive_client_order_id("MKT", "strat", 7);
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "MKT|strat|7");
    }

    #[test]
    fn client_order_id_differs_by_seq_market_strategy() {
        let base = derive_client_order_id("M", "S", 1);
        assert_ne!(base, derive_client_order_id("M", "S", 2));
        assert_ne!(base, derive_client_order_id("M2", "S", 1));
        assert_ne!(base, derive_client_order_id("M", "S2", 1));
    }

    // ── Invariant 1: order Prepare → Started → Response ───────────────────

    #[test]
    fn inv_order_prepare_started_response() {
        let mut ctx = ctx_default();
        let mut s = OrderState::New;
        let o = apply_event(&s, &mut ctx, &OrderEvent::PrepareSubmit);
        assert!(matches!(o.new_state(), Some(OrderState::SubmitPrepared)));
        s = o.new_state().unwrap().clone();
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::AppendFsync(JournalRecord::SubmitPrepared { .. })))
        );

        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::StartSubmit {
                attempt_id: AttemptId("att-1".into()),
            },
        );
        assert!(matches!(
            o.new_state(),
            Some(OrderState::SubmitStarted { .. })
        ));
        // Started fsync effect present (must precede any response in shell).
        let started_fsync = o
            .effects()
            .iter()
            .any(|e| matches!(e, Effect::AppendFsync(JournalRecord::SubmitStarted { .. })));
        assert!(started_fsync);

        s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V1"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        assert!(matches!(o.new_state(), Some(OrderState::Accepted { .. })));
    }

    #[test]
    fn inv_order_skip_started_rejects_response() {
        let mut ctx = ctx_default();
        let s = OrderState::New;
        let o = apply_event(&s, &mut ctx, &OrderEvent::PrepareSubmit);
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V1"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        assert!(o.is_reject(), "skip Started must Reject: {o:?}");
        assert!(matches!(
            o,
            TransitionOutcome::Reject {
                reason: RejectReason::OrderInvariantViolated { .. }
            }
        ));
    }

    // ── Invariant 2: timeout → Unknown, no resubmit ───────────────────────
    // D8: assert exact effects set (is_resubmit is always false structurally).

    #[test]
    fn inv_timeout_to_unknown_no_resubmit() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        assert!(matches!(o.new_state(), Some(OrderState::SubmitUnknown)));
        // Exact effects: AppendFsync(SubmitUnknown) + BackfillUnknown — nothing else
        // (any future resubmit/retry effect would fail this equality).
        assert_eq!(
            o.effects(),
            &[
                Effect::AppendFsync(JournalRecord::SubmitUnknown),
                Effect::BackfillUnknown {
                    client_order_id: ctx.client_order_id.clone(),
                },
            ]
        );
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
    }

    // ── Invariant 3: cancel typed seven-way ───────────────────────────────

    fn to_cancel_pending(ctx: &mut OrderCtx) -> OrderState {
        let s = prepare_started(ctx);
        let o = apply_event(
            &s,
            ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V1"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(&s, ctx, &OrderEvent::CancelRequested);
        o.new_state().unwrap().clone()
    }

    /// F1: shell authority reconcile after cancel (CancelOutcome never latches authority).
    fn authority_reconcile_canceled(filled: u64, remaining: u64) -> OrderEvent {
        OrderEvent::ReconcileResult {
            status: BackfillOrderStatus::Canceled,
            venue_order_id: Some(vid("V1")),
            filled_qty: filled,
            remaining_qty: remaining,
            fills: vec![],
            authority_complete: true,
        }
    }

    /// Drive CancelPending → Canceled via cancel + authority reconcile (F1).
    fn cancel_to_terminal_with_authority(ctx: &mut OrderCtx, s: OrderState) -> TransitionOutcome {
        let o = apply_event(&s, ctx, &OrderEvent::CancelOutcome(CancelOutcome::Canceled));
        // Without authority latch, must not release.
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        let s = o.new_state().unwrap().clone();
        apply_event(
            &s,
            ctx,
            &authority_reconcile_canceled(ctx.attributed_fill_qty, ctx.remaining_qty()),
        )
    }

    /// ★ 正向钉（2026-08-15 审计 HIGH **撤销**后的护栏）：Kalshi canceled 单 REST
    /// 实测（o2a 探针）**987/1000 报 filled=0/remaining=0**（撤后清零常态）——
    /// `0/0 + authority_complete` 的零成交撤必须正常 `Canceled + Release`。
    /// 任何人再加「零踪迹 halt」或「filled+remaining==qty 恒等」门 ⇒ 本钉红
    /// （那会 false-halt 98.7% 的超时恢复 backfill 流 = 资金锁死）。
    #[test]
    fn inv_canceled_zero_zero_rest_is_normal_release() {
        // 路径 1：cancel → ReconcileResult{0/0, complete}（HTTP 撤后 REST 复核形态）。
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::Canceled),
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V1")),
                filled_qty: 0,
                remaining_qty: 0,
                fills: vec![],
                authority_complete: true,
            },
        );
        assert_eq!(
            o.new_state(),
            Some(&OrderState::Canceled),
            "0/0 零成交撤是 Kalshi 常态，必须正常终态（加守恒门 = false-halt 恢复流）"
        );
        assert!(
            o.effects().iter().any(|e| matches!(e, Effect::ReleaseReservation)),
            "0/0 canceled 必须 Release（锁预留 = 资金锁死）"
        );
        // 路径 2：SubmitUnknown → exhaustive backfill 认领 Canceled 0/0（超时恢复真数据流，
        // 审计探针 987/1000 的形态）。
        let mut ctx2 = ctx_default();
        let cid2 = ctx2.client_order_id.clone();
        let s2 = OrderState::SubmitUnknown;
        let o2 = apply_event(
            &s2,
            &mut ctx2,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![BackfillOrderRecord {
                    client_order_id: cid2,
                    venue_order_id: vid("V2"),
                    status: BackfillOrderStatus::Canceled,
                    filled_qty: 0,
                    remaining_qty: 0,
                    fills: vec![],
                }],
            },
        );
        assert_eq!(
            o2.new_state(),
            Some(&OrderState::Canceled),
            "超时恢复 backfill 的 0/0 canceled 必须干净收口（98.7% 常态形态）"
        );
        assert!(
            o2.effects().iter().any(|e| matches!(e, Effect::ReleaseReservation)),
            "恢复流必须释放预留"
        );
    }

    #[test]
    fn inv_cancel_canceled_is_terminal_released() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        // F1: cancel alone does not forge authority / release.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::Canceled),
        );
        assert_ne!(o.new_state(), Some(&OrderState::Canceled));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(&s, &mut ctx, &authority_reconcile_canceled(0, 10));
        assert_eq!(o.new_state(), Some(&OrderState::Canceled));
        assert_eq!(
            reservation_held(&OrderState::Canceled),
            ReservationHold::Released
        );
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert!(ctx.authority_complete);
    }

    #[test]
    fn inv_cancel_already_terminal_released() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::AlreadyTerminal),
        );
        // F1: AlreadyTerminal does not latch authority.
        assert_ne!(o.new_state(), Some(&OrderState::Terminal));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V1")),
                filled_qty: 0,
                remaining_qty: 10,
                fills: vec![],
                authority_complete: true,
            },
        );
        assert_eq!(o.new_state(), Some(&OrderState::Canceled));
        assert_eq!(
            reservation_held(&OrderState::Canceled),
            ReservationHold::Released
        );
    }

    #[test]
    fn inv_cancel_accepted_stays_pending_not_canceled() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::Accepted),
        );
        assert!(matches!(
            o.new_state(),
            Some(OrderState::CancelPending { .. })
        ));
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
    }

    #[test]
    fn inv_cancel_rejected_restores_live() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::Rejected),
        );
        assert!(matches!(o.new_state(), Some(OrderState::Accepted { .. })));
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
    }

    #[test]
    fn inv_cancel_not_found_reconcile_not_canceled() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::NotFound),
        );
        assert!(matches!(
            o.new_state(),
            Some(OrderState::CancelPending { .. })
        ));
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReconcileCancel { .. }))
        );
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
    }

    #[test]
    fn inv_cancel_transport_unknown_reconcile_not_canceled() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::TransportUnknown),
        );
        assert!(matches!(
            o.new_state(),
            Some(OrderState::CancelPending { .. })
        ));
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReconcileCancel { .. }))
        );
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
    }

    #[test]
    fn inv_cancel_unknown_reconcile_not_canceled() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::Unknown),
        );
        assert!(matches!(
            o.new_state(),
            Some(OrderState::CancelPending { .. })
        ));
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReconcileCancel { .. }))
        );
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
    }

    // ── Invariant 4: reservation_held ─────────────────────────────────────

    #[test]
    fn inv_reservation_held_matrix() {
        let released = [
            OrderState::Canceled,
            OrderState::Filled,
            OrderState::Terminal,
        ];
        for s in &released {
            assert_eq!(reservation_held(s), ReservationHold::Released, "{s:?}");
        }
        let full = [
            OrderState::New,
            OrderState::SubmitPrepared,
            OrderState::SubmitStarted {
                attempt_id: AttemptId("a".into()),
            },
            OrderState::SubmitUnknown,
            OrderState::Accepted {
                venue_order_id: vid("V"),
            },
            OrderState::Partial {
                venue_order_id: vid("V"),
                filled_qty: 1,
                remaining_qty: 9,
            },
            OrderState::CancelPending {
                venue_order_id: vid("V"),
                filled_qty: 0,
                remaining_qty: 10,
                response_fill_count: None,
                response_avg_price_cents: None,
                response_fee_cents: None,
                reconcile_target: None,
            },
            OrderState::ReconcilePending {
                venue_order_id: vid("V"),
                filled_qty: 2,
                remaining_qty: 8,
                target: ReconcileTarget {
                    terminal: ReconcileTerminal::Canceled,
                    venue_filled_qty: 4,
                    venue_remaining_qty: Some(6),
                },
                response_fill_count: Some(4),
                response_avg_price_cents: None,
                response_fee_cents: None,
            },
            OrderState::ImmediateFillUnattributed {
                venue_order_id: vid("V"),
                response_fill_count: 5,
                response_remaining_count: 5,
                response_avg_price_cents: None,
                response_fee_cents: None,
            },
            OrderState::ImmediateFillUnresolved,
            OrderState::UnknownNoMatch,
            OrderState::Halted {
                reason: HaltReason::UnknownNoMatch,
            },
        ];
        for s in &full {
            assert_eq!(reservation_held(s), ReservationHold::Full, "{s:?}");
        }
    }

    // ── Invariant 5: restart-safe terminals ────────────────────────────────

    #[test]
    fn inv_restart_safe_terminal_no_new_io() {
        let ctx = ctx_default();
        let events = vec![
            OrderEvent::PrepareSubmit,
            OrderEvent::StartSubmit {
                attempt_id: AttemptId("a1".into()),
            },
            OrderEvent::SubmitResponse {
                venue_order_id: vid("V1"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
            OrderEvent::CancelRequested,
            OrderEvent::CancelOutcome(CancelOutcome::Canceled),
            // F1: authority reconcile required for release.
            OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V1")),
                filled_qty: 0,
                remaining_qty: 10,
                fills: vec![],
                authority_complete: true,
            },
        ];
        let r = replay(OrderState::New, ctx.clone(), &events);
        assert_eq!(r.state, OrderState::Canceled);
        assert_eq!(reservation_held(&r.state), ReservationHold::Released);

        // Re-apply cancel outcome on terminal → reject, no I/O effects.
        let mut ctx2 = r.ctx.clone();
        let o = apply_event(
            &r.state,
            &mut ctx2,
            &OrderEvent::CancelOutcome(CancelOutcome::Canceled),
        );
        assert!(o.is_reject());
        assert!(o.effects().is_empty());
        assert_eq!(reservation_held(&r.state), ReservationHold::Released);
    }

    /// D8: restart frozen state receiving a **new** fill_id must account + ReserveFull + Halt (D2).
    #[test]
    fn inv_restart_safe_frozen_late_fill_accounts_and_halts() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![],
            },
        );
        assert!(o.is_halt());
        assert_eq!(o.new_state(), Some(&OrderState::UnknownNoMatch));
        assert_eq!(
            reservation_held(&OrderState::UnknownNoMatch),
            ReservationHold::Full
        );

        // Late new fill on frozen halt state → must book, not swallow.
        let o2 = apply_event(
            &OrderState::UnknownNoMatch,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("late-F"),
                qty: 2,
                price_cents: 40,
                ts_ns: 99,
                venue_order_id: None,
                fee_cents: None,
            },
        );
        assert!(o2.is_halt());
        assert!(matches!(
            o2,
            TransitionOutcome::Halt {
                reason: HaltReason::PostTerminalFill,
                ..
            }
        ));
        assert_eq!(o2.account_fill_count(), 1);
        assert!(
            o2.effects()
                .iter()
                .any(|e| matches!(e, Effect::AppendFsync(JournalRecord::Fill { .. })))
        );
        assert!(
            o2.effects()
                .iter()
                .any(|e| matches!(e, Effect::AccountFill { .. }))
        );
        // Was Full already (UnknownNoMatch) — ReserveFull not required, but Halted is Full.
        assert_eq!(
            reservation_held(o2.new_state().unwrap()),
            ReservationHold::Full
        );
        assert_eq!(ctx.attributed_fill_qty, 2);

        // Same payload again → true no-op.
        let s = o2.new_state().unwrap().clone();
        let o3 = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("late-F"),
                qty: 2,
                price_cents: 40,
                ts_ns: 99,
                venue_order_id: None,
                fee_cents: None,
            },
        );
        assert_eq!(o3.account_fill_count(), 0);
        assert!(o3.effects().is_empty());
        assert_eq!(ctx.attributed_fill_qty, 2);
    }

    // ── §6.B2 six golden fixtures ─────────────────────────────────────────

    #[test]
    fn b2_golden_post_only_zero_fill() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-po"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        assert!(matches!(
            o.new_state(),
            Some(OrderState::Accepted {
                venue_order_id
            }) if venue_order_id == &vid("V-po")
        ));
        assert_eq!(o.account_fill_count(), 0, "zero fill → no accounting");
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
        assert!(!o.is_halt());
    }

    #[test]
    fn b2_golden_ioc_full_fill() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-ioc"),
                fill_count: 10,
                remaining_count: 0,
                avg_price_cents: Some(45),
                fee_cents: Some(1),
                snapshot_boundary: None,
            },
        );
        // Enter unattributed — full reservation, no account yet.
        let s = o.new_state().unwrap().clone();
        assert!(matches!(s, OrderState::ImmediateFillUnattributed { .. }));
        assert_eq!(o.account_fill_count(), 0);
        assert_eq!(reservation_held(&s), ReservationHold::Full);
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::BackfillFills { .. }))
        );

        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ImmediateFillBackfillResult {
                fills: vec![fill_rec_vf("F1", 10, 45, 1000, "V-ioc", 1)],
            },
        );
        assert_eq!(o.new_state(), Some(&OrderState::Filled));
        assert_eq!(o.account_fill_count(), 1);
        assert_eq!(
            reservation_held(&OrderState::Filled),
            ReservationHold::Released
        );
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert!(!o.is_halt());
        // D4: avg/fee preserved in ctx.
        assert_eq!(ctx.response_avg_price_cents, Some(45));
        assert_eq!(ctx.response_fee_cents, Some(1));
    }

    #[test]
    fn b2_golden_ioc_partial_fill() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-part"),
                fill_count: 4,
                remaining_count: 6,
                avg_price_cents: Some(45),
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        assert!(matches!(s, OrderState::ImmediateFillUnattributed { .. }));
        assert_eq!(reservation_held(&s), ReservationHold::Full);
        assert_eq!(o.account_fill_count(), 0);

        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ImmediateFillBackfillResult {
                fills: vec![fill_rec_v("Fp", 4, 45, 2000, "V-part")],
            },
        );
        match o.new_state() {
            Some(OrderState::Partial {
                filled_qty,
                remaining_qty,
                ..
            }) => {
                assert_eq!(*filled_qty, 4);
                assert_eq!(*remaining_qty, 6);
            }
            other => panic!("expected Partial, got {other:?}"),
        }
        assert_eq!(o.account_fill_count(), 1);
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
        assert!(!o.is_halt());
        assert_eq!(ctx.response_avg_price_cents, Some(45));
    }

    #[test]
    fn b2_golden_response_before_ws() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        // Response first → Unattributed.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-rb"),
                fill_count: 3,
                remaining_count: 7,
                avg_price_cents: Some(44),
                fee_cents: Some(2),
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        assert!(matches!(s, OrderState::ImmediateFillUnattributed { .. }));
        assert_eq!(o.account_fill_count(), 0);
        assert_eq!(reservation_held(&s), ReservationHold::Full);
        assert!(!o.is_halt());
        // avg/fee retained on state.
        match &s {
            OrderState::ImmediateFillUnattributed {
                response_avg_price_cents,
                response_fee_cents,
                ..
            } => {
                assert_eq!(*response_avg_price_cents, Some(44));
                assert_eq!(*response_fee_cents, Some(2));
            }
            _ => unreachable!(),
        }

        // Then WS fill with fill_id → account once, resolve Partial.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-ws"),
                qty: 3,
                price_cents: 44,
                ts_ns: 3000,
                venue_order_id: Some(vid("V-rb")),
                fee_cents: Some(2),
            },
        );
        assert!(matches!(
            o.new_state(),
            Some(OrderState::Partial {
                filled_qty: 3,
                remaining_qty: 7,
                ..
            })
        ));
        assert_eq!(o.account_fill_count(), 1);
        assert_eq!(ctx.attributed_fill_qty, 3);
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
        assert!(!o.is_halt());
        assert_eq!(ctx.response_avg_price_cents, Some(44));
        assert_eq!(ctx.response_fee_cents, Some(2));

        // Same fill_id again (WS redelivery) → no double book.
        let s = o.new_state().unwrap().clone();
        let o2 = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-ws"),
                qty: 3,
                price_cents: 44,
                ts_ns: 3000,
                venue_order_id: Some(vid("V-rb")),
                fee_cents: Some(2),
            },
        );
        assert_eq!(o2.account_fill_count(), 0);
        assert_eq!(ctx.attributed_fill_qty, 3);
        assert!(!o2.is_halt());
    }

    #[test]
    fn b2_golden_ws_before_response() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        // WS fill first while still Started.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-early"),
                qty: 5,
                price_cents: 45,
                ts_ns: 100,
                venue_order_id: None,
                fee_cents: None,
            },
        );
        assert!(matches!(
            o.new_state(),
            Some(OrderState::SubmitStarted { .. })
        ));
        assert_eq!(o.account_fill_count(), 1);
        assert_eq!(ctx.attributed_fill_qty, 5);
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
        assert!(!o.is_halt());

        // Response with fill_count=5 → Unattributed then immediate resolve via prior fills.
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-wsb"),
                fill_count: 5,
                remaining_count: 5,
                avg_price_cents: Some(45),
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        // Already attributed via WS; cross-check passes → Partial without second account.
        assert!(
            matches!(
                o.new_state(),
                Some(OrderState::Partial {
                    filled_qty: 5,
                    remaining_qty: 5,
                    ..
                })
            ),
            "got {:?}",
            o.new_state()
        );
        assert_eq!(
            o.account_fill_count(),
            0,
            "must not re-account already-applied fill_id"
        );
        assert_eq!(ctx.attributed_fill_qty, 5);
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
        assert!(!o.is_halt());
        assert_eq!(ctx.response_avg_price_cents, Some(45));

        // Redundant same fill_id → no double count.
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-early"),
                qty: 5,
                price_cents: 45,
                ts_ns: 100,
                venue_order_id: Some(vid("V-wsb")),
                fee_cents: None,
            },
        );
        assert_eq!(o.account_fill_count(), 0);
        assert_eq!(ctx.attributed_fill_qty, 5);
    }

    #[test]
    fn b2_golden_backfill_timeout() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-to"),
                fill_count: 2,
                remaining_count: 8,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        assert!(matches!(s, OrderState::ImmediateFillUnattributed { .. }));

        let o = apply_event(&s, &mut ctx, &OrderEvent::BackfillDeadlineElapsed);
        assert!(o.is_halt());
        assert_eq!(o.new_state(), Some(&OrderState::ImmediateFillUnresolved));
        assert_eq!(
            reservation_held(&OrderState::ImmediateFillUnresolved),
            ReservationHold::Full
        );
        assert_eq!(o.account_fill_count(), 0, "no fill_id → never account");
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::HaltNewExposure))
        );
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
    }

    // ── Cross-check mismatch HALT ─────────────────────────────────────────

    #[test]
    fn cross_check_mismatch_halts() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        // G6: qty/remaining check is domain-scoped; use reliable boundary + inconsistent
        // remaining (order_qty − fill_count = 5, but response claims remaining=3).
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-x"),
                fill_count: 5,
                remaining_count: 3,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: Some(SnapshotBoundary::TsNs(100)),
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ImmediateFillBackfillResult {
                fills: vec![fill_rec_vf("Fx", 5, 45, 1, "V-x", 0)],
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::ImmediateFillCrossCheckMismatch { .. },
                ..
            }
        ));
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
    }

    // ── fill_id dedup ─────────────────────────────────────────────────────

    #[test]
    fn fill_id_dedup_same_fill_twice() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-d"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let fill = OrderEvent::Fill {
            fill_id: fid("same"),
            qty: 2,
            price_cents: 40,
            ts_ns: 1,
            venue_order_id: Some(vid("V-d")),
            fee_cents: None,
        };
        let o1 = apply_event(&s, &mut ctx, &fill);
        assert_eq!(o1.account_fill_count(), 1);
        assert_eq!(ctx.attributed_fill_qty, 2);
        let s = o1.new_state().unwrap().clone();
        let o2 = apply_event(&s, &mut ctx, &fill);
        assert_eq!(o2.account_fill_count(), 0);
        assert_eq!(ctx.attributed_fill_qty, 2);
    }

    /// D8: true WS then ImmediateFillBackfillResult path (not a second Fill event).
    #[test]
    fn fill_id_dedup_ws_and_backfill_same_id() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-dd"),
                fill_count: 3,
                remaining_count: 7,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        assert!(matches!(s, OrderState::ImmediateFillUnattributed { .. }));
        // WS path while still unattributed.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("shared"),
                qty: 3,
                price_cents: 41,
                ts_ns: 5,
                venue_order_id: Some(vid("V-dd")),
                fee_cents: None,
            },
        );
        assert_eq!(o.account_fill_count(), 1);
        // Resolves to Partial (qty matches response).
        let s = o.new_state().unwrap().clone();
        assert!(matches!(s, OrderState::Partial { .. }));
        // Simulate backfill redelivery via ImmediateFillBackfillResult is only legal
        // from Unattributed — so re-enter path: book first via unattributed stay.
        // Alternate: stay unattributed with partial qty then backfill same id.
        let mut ctx2 = ctx_default();
        let s2 = prepare_started(&mut ctx2);
        let o = apply_event(
            &s2,
            &mut ctx2,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-dd2"),
                fill_count: 6,
                remaining_count: 4,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s2 = o.new_state().unwrap().clone();
        // First WS fill partial attribution (3 of 6).
        let o = apply_event(
            &s2,
            &mut ctx2,
            &OrderEvent::Fill {
                fill_id: fid("shared"),
                qty: 3,
                price_cents: 41,
                ts_ns: 5,
                venue_order_id: Some(vid("V-dd2")),
                fee_cents: None,
            },
        );
        assert_eq!(o.account_fill_count(), 1);
        let s2 = o.new_state().unwrap().clone();
        assert!(matches!(s2, OrderState::ImmediateFillUnattributed { .. }));
        // Backfill path returns same fill_id + new second fill → shared is no-op.
        let o = apply_event(
            &s2,
            &mut ctx2,
            &OrderEvent::ImmediateFillBackfillResult {
                fills: vec![
                    fill_rec_v("shared", 3, 41, 5, "V-dd2"),
                    fill_rec_v("F2", 3, 41, 6, "V-dd2"),
                ],
            },
        );
        assert_eq!(o.account_fill_count(), 1, "only F2 newly accounted");
        assert_eq!(ctx2.attributed_fill_qty, 6);
        assert!(matches!(o.new_state(), Some(OrderState::Partial { .. })));
    }

    // ── §6.C matching ─────────────────────────────────────────────────────

    #[test]
    fn c_backfill_match_to_filled() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        assert!(matches!(s, OrderState::SubmitUnknown));

        let coid = ctx.client_order_id.clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![BackfillOrderRecord {
                    client_order_id: coid,
                    venue_order_id: vid("V-found"),
                    status: BackfillOrderStatus::Filled,
                    filled_qty: 10,
                    remaining_qty: 0,
                    fills: vec![fill_rec_v("F-bf", 10, 45, 9, "V-found")],
                }],
            },
        );
        assert_eq!(o.new_state(), Some(&OrderState::Filled));
        assert_eq!(o.account_fill_count(), 1);
        assert_eq!(
            reservation_held(&OrderState::Filled),
            ReservationHold::Released
        );
        // Exact effects must not include any resubmit-like action.
        assert!(!o.effects().iter().any(|e| matches!(
            e,
            Effect::BackfillUnknown { .. } // no continue after terminal
        )));
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert!(!o.has_resubmit_effect());
    }

    #[test]
    fn c_backfill_no_match_unknown_no_match_halt() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![],
            },
        );
        assert!(o.is_halt());
        assert_eq!(o.new_state(), Some(&OrderState::UnknownNoMatch));
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::UnknownNoMatch,
                ..
            }
        ));
        assert_eq!(
            reservation_held(&OrderState::UnknownNoMatch),
            ReservationHold::Full
        );
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert!(!o.has_resubmit_effect());
    }

    #[test]
    fn c_unproven_resubmit_halts_no_resubmit_effect() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::RequestResubmit {
                venue_idempotent_proven: false,
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::UnprovenIdempotentResubmit,
                ..
            }
        ));
        // Exact effects: Halted journal + HaltNewExposure only.
        assert_eq!(
            o.effects(),
            &[
                Effect::AppendFsync(JournalRecord::Halted {
                    reason: HaltReason::UnprovenIdempotentResubmit,
                }),
                Effect::HaltNewExposure,
            ]
        );
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
    }

    #[test]
    fn c_non_exhaustive_no_match_stays_unknown() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: false,
                matched: vec![],
            },
        );
        assert_eq!(o.new_state(), Some(&OrderState::SubmitUnknown));
        assert!(!o.is_halt());
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::BackfillUnknown { .. }))
        );
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
    }

    // ── D1–D7 coverage ────────────────────────────────────────────────────

    /// D1: status=Filled but exhaustive=false → non-terminal, Full reservation, continue backfill.
    #[test]
    fn d1_non_exhaustive_filled_stays_reconcile_pending() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let coid = ctx.client_order_id.clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: false,
                matched: vec![BackfillOrderRecord {
                    client_order_id: coid,
                    venue_order_id: vid("V-ne"),
                    status: BackfillOrderStatus::Filled,
                    filled_qty: 10,
                    remaining_qty: 0,
                    fills: vec![], // no fill pages yet
                }],
            },
        );
        // Must NOT go Filled / release.
        assert_ne!(o.new_state(), Some(&OrderState::Filled));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
        assert!(matches!(o.new_state(), Some(OrderState::SubmitUnknown)));
        assert!(o.effects().iter().any(|e| matches!(
            e,
            Effect::BackfillFills { .. } | Effect::BackfillUnknown { .. }
        )));
        assert_eq!(ctx.attributed_fill_qty, 0);
    }

    /// D1: exhaustive + status=Filled but attributed < venue_filled → no release.
    #[test]
    fn d1_exhaustive_missing_fills_no_release() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let coid = ctx.client_order_id.clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![BackfillOrderRecord {
                    client_order_id: coid,
                    venue_order_id: vid("V-miss"),
                    status: BackfillOrderStatus::Filled,
                    filled_qty: 10,
                    remaining_qty: 0,
                    fills: vec![fill_rec_v("only-partial", 4, 45, 1, "V-miss")],
                }],
            },
        );
        assert_ne!(o.new_state(), Some(&OrderState::Filled));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
        assert_eq!(o.account_fill_count(), 1);
        assert_eq!(ctx.attributed_fill_qty, 4);
        assert!(matches!(
            o.new_state(),
            Some(OrderState::ReconcilePending {
                filled_qty: 4,
                target: ReconcileTarget {
                    terminal: ReconcileTerminal::Filled,
                    venue_filled_qty: 10,
                    ..
                },
                ..
            })
        ));
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::BackfillFills { .. }))
        );
    }

    #[test]
    fn d2_late_fill_on_canceled_accounts_reserve_full() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        // Partial fill while cancel pending then cancel confirms.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-pre"),
                qty: 3,
                price_cents: 41,
                ts_ns: 1,
                venue_order_id: Some(vid("V1")),
                fee_cents: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        assert!(matches!(s, OrderState::CancelPending { .. }));
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::Canceled),
        );
        assert_ne!(
            o.new_state(),
            Some(&OrderState::Canceled),
            "F1: cancel alone no release"
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V1")),
                filled_qty: 3,
                remaining_qty: 7,
                fills: vec![],
                authority_complete: true,
            },
        );
        assert_eq!(o.new_state(), Some(&OrderState::Canceled));
        assert_eq!(
            reservation_held(&OrderState::Canceled),
            ReservationHold::Released
        );
        assert_eq!(ctx.attributed_fill_qty, 3);

        // Late new fill_id after Canceled.
        let o = apply_event(
            &OrderState::Canceled,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-late"),
                qty: 2,
                price_cents: 42,
                ts_ns: 9,
                venue_order_id: Some(vid("V1")),
                fee_cents: None,
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::PostTerminalFill,
                ..
            }
        ));
        assert_eq!(o.account_fill_count(), 1);
        assert!(o.effects().iter().any(|e| matches!(e, Effect::ReserveFull)));
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::AppendFsync(JournalRecord::Fill { .. })))
        );
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::AccountFill { .. }))
        );
        assert_eq!(ctx.attributed_fill_qty, 5);
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
    }

    /// D3: overfill → Halt OverFill, not silent Filled.
    #[test]
    fn d3_overfill_halts() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-of"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-big"),
                qty: 11,
                price_cents: 40,
                ts_ns: 1,
                venue_order_id: Some(vid("V-of")),
                fee_cents: None,
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::OverFill { .. },
                ..
            }
        ));
        assert_eq!(ctx.attributed_fill_qty, 0, "must not book overfill");
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
    }

    /// D4（2026-08-15 A5 语义翻转）：fill_count=0 但本地 WS fill 已入账 ——
    /// response 是建单时刻快照，抢跑 fill 是域外新证据非矛盾：remaining==qty
    /// （venue 说全量驻留）⇒ **Partial** 而非 Halt（旧断言=接受 fill 又按矛盾
    /// halt 的自相矛盾行为）。真矛盾对照（response 0/0+本地 fill ⇒ 仍 halt）
    /// 见 `a5_ws_fill_before_zero_fill_response_is_partial_not_halt`。
    #[test]
    fn d4_zero_response_with_prior_ws_fills_halts() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-ws"),
                qty: 4,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: None,
                fee_cents: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        assert_eq!(ctx.attributed_fill_qty, 4);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-z"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        assert!(
            matches!(
                o.new_state(),
                Some(OrderState::Partial { filled_qty: 4, remaining_qty: 6, .. })
            ),
            "A5：抢跑 fill + 全量驻留 response = Partial（domain 外新证据）: {o:?}"
        );
        assert!(!matches!(o.new_state(), Some(OrderState::Accepted { .. })));
        assert_eq!(ctx.attributed_fill_qty, 4, "fill 已入账不回滚");
    }

    /// D4: avg_price mismatch after fill aggregation → CrossCheckMismatch.
    #[test]
    fn d4_avg_price_mismatch_halts() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-avg"),
                fill_count: 4,
                remaining_count: 6,
                avg_price_cents: Some(50), // claims 50
                fee_cents: Some(1),
                snapshot_boundary: Some(SnapshotBoundary::TsNs(100)),
            },
        );
        let s = o.new_state().unwrap().clone();
        // Preserve avg/fee on unattributed.
        match &s {
            OrderState::ImmediateFillUnattributed {
                response_avg_price_cents: Some(50),
                response_fee_cents: Some(1),
                ..
            } => {}
            other => panic!("avg/fee not preserved: {other:?}"),
        }
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ImmediateFillBackfillResult {
                fills: vec![fill_rec_vf("Fa", 4, 40, 1, "V-avg", 1)], // notional 160 vs 50*4=200
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::CrossCheckMismatch { .. },
                ..
            }
        ));
        // Still preserved in ctx.
        assert_eq!(ctx.response_avg_price_cents, Some(50));
        assert_eq!(ctx.response_fee_cents, Some(1));
    }

    /// D5: ImmediateFillUnattributed accepts CancelRequested → CancelPending.
    #[test]
    fn d5_unattributed_cancel_to_pending() {
        // fill_count=6 so after qty=4 we stay unattributed (not resolve yet).
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-c5b"),
                fill_count: 6,
                remaining_count: 4,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-part"),
                qty: 4,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V-c5b")),
                fee_cents: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        assert!(matches!(s, OrderState::ImmediateFillUnattributed { .. }));
        assert_eq!(ctx.attributed_fill_qty, 4);

        let o = apply_event(&s, &mut ctx, &OrderEvent::CancelRequested);
        assert!(matches!(
            o.new_state(),
            Some(OrderState::CancelPending {
                filled_qty: 4,
                remaining_qty: 6,
                response_fill_count: Some(6),
                ..
            })
        ));
        assert!(!o.is_reject());
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
        // Prior attributed fills retained.
        assert_eq!(ctx.attributed_fill_qty, 4);
    }

    /// D6: ≥2 matches → AmbiguousMatch Halt.
    #[test]
    fn d6_ambiguous_match_halts() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let coid = ctx.client_order_id.clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![
                    BackfillOrderRecord {
                        client_order_id: coid.clone(),
                        venue_order_id: vid("V-a"),
                        status: BackfillOrderStatus::Open,
                        filled_qty: 0,
                        remaining_qty: 10,
                        fills: vec![],
                    },
                    BackfillOrderRecord {
                        client_order_id: coid,
                        venue_order_id: vid("V-b"),
                        status: BackfillOrderStatus::Open,
                        filled_qty: 0,
                        remaining_qty: 10,
                        fills: vec![],
                    },
                ],
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::AmbiguousMatch { count: 2 },
                ..
            }
        ));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
    }

    /// D6: fill with wrong venue_order_id → ownership halt, not blind book.
    #[test]
    fn d6_fill_ownership_mismatch_halts() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let coid = ctx.client_order_id.clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![BackfillOrderRecord {
                    client_order_id: coid,
                    venue_order_id: vid("V-parent"),
                    status: BackfillOrderStatus::Partial,
                    filled_qty: 3,
                    remaining_qty: 7,
                    fills: vec![fill_rec_v("F-sib", 3, 45, 1, "V-SIBLING")],
                }],
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::FillOwnershipMismatch { .. },
                ..
            }
        ));
        assert_eq!(ctx.attributed_fill_qty, 0);
    }

    /// D7: same fill_id, different payload → ConflictingFillPayload.
    #[test]
    fn d7_conflicting_fill_payload_halts() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-cf"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 1,
                price_cents: 40,
                ts_ns: 1,
                venue_order_id: Some(vid("V-cf")),
                fee_cents: None,
            },
        );
        assert_eq!(o.account_fill_count(), 1);
        let s = o.new_state().unwrap().clone();
        // Same id, different qty (authoritative GET vs WS conflict).
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 4,
                price_cents: 40,
                ts_ns: 2,
                venue_order_id: Some(vid("V-cf")),
                fee_cents: None,
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::ConflictingFillPayload { .. },
                ..
            }
        ));
        assert_eq!(
            ctx.attributed_fill_qty, 1,
            "must not apply conflicting payload"
        );
    }

    // ── replay fold determinism ───────────────────────────────────────────

    #[test]
    fn replay_fold_equals_stepwise() {
        let events = vec![
            OrderEvent::PrepareSubmit,
            OrderEvent::StartSubmit {
                attempt_id: AttemptId("r1".into()),
            },
            OrderEvent::SubmitResponse {
                venue_order_id: vid("Vr"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
            OrderEvent::Fill {
                fill_id: fid("Fr1"),
                qty: 4,
                price_cents: 42,
                ts_ns: 10,
                venue_order_id: Some(vid("Vr")),
                fee_cents: None,
            },
            OrderEvent::Fill {
                fill_id: fid("Fr2"),
                qty: 6,
                price_cents: 43,
                ts_ns: 11,
                venue_order_id: Some(vid("Vr")),
                fee_cents: None,
            },
        ];
        let r = replay(OrderState::New, ctx_default(), &events);
        assert_eq!(r.state, OrderState::Filled);
        assert!(r.reject.is_none());
        assert!(r.halt.is_none());

        // Stepwise
        let mut ctx = ctx_default();
        let mut s = OrderState::New;
        for ev in &events {
            let o = apply_event(&s, &mut ctx, ev);
            s = o.new_state().expect("step ok").clone();
        }
        assert_eq!(s, r.state);
        assert_eq!(ctx.attributed_fill_qty, r.ctx.attributed_fill_qty);
        assert_eq!(ctx.applied_fills, r.ctx.applied_fills);
    }

    #[test]
    fn replay_started_fsync_before_response_in_effect_order() {
        let events = [
            OrderEvent::PrepareSubmit,
            OrderEvent::StartSubmit {
                attempt_id: AttemptId("ord".into()),
            },
            OrderEvent::SubmitResponse {
                venue_order_id: vid("Vo"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        ];
        let r = replay(OrderState::New, ctx_default(), &events);
        let mut seen_started = false;
        let mut seen_response = false;
        for e in &r.effects {
            match e {
                Effect::AppendFsync(JournalRecord::SubmitStarted { .. }) => {
                    assert!(!seen_response, "Started fsync must precede Response");
                    seen_started = true;
                }
                Effect::AppendFsync(JournalRecord::SubmitResponse { .. }) => {
                    assert!(seen_started, "Response requires prior Started fsync effect");
                    seen_response = true;
                }
                _ => {}
            }
        }
        assert!(seen_started && seen_response);
    }

    // ── E1–E9 coverage ────────────────────────────────────────────────────

    /// E1 + E9/D5: cancel from ImmediateFillUnattributed with lagging attribution
    /// must NOT release on CancelOutcome::Canceled.
    #[test]
    fn e1_cancel_outcome_with_unattributed_obligation_no_release() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-e1"),
                fill_count: 6,
                remaining_count: 4,
                avg_price_cents: Some(45),
                fee_cents: Some(3),
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-a"),
                qty: 4,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V-e1")),
                fee_cents: Some(2),
            },
        );
        let s = o.new_state().unwrap().clone();
        assert!(matches!(s, OrderState::ImmediateFillUnattributed { .. }));
        assert_eq!(ctx.attributed_fill_qty, 4);

        let o = apply_event(&s, &mut ctx, &OrderEvent::CancelRequested);
        let s = o.new_state().unwrap().clone();
        match &s {
            OrderState::CancelPending {
                response_fill_count: Some(6),
                response_avg_price_cents: Some(45),
                response_fee_cents: Some(3),
                filled_qty: 4,
                ..
            } => {}
            other => panic!("obligation not carried: {other:?}"),
        }
        assert_eq!(reservation_held(&s), ReservationHold::Full);

        // E1: Canceled must NOT release while attributed < response_fill_count.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::Canceled),
        );
        assert_ne!(o.new_state(), Some(&OrderState::Canceled));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
        assert!(matches!(
            o.new_state(),
            Some(OrderState::ReconcilePending {
                target: ReconcileTarget {
                    terminal: ReconcileTerminal::Canceled,
                    venue_filled_qty: 6,
                    ..
                },
                ..
            })
        ));
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::BackfillFills { .. }))
        );
        assert_eq!(ctx.attributed_fill_qty, 4);
        assert_eq!(
            ctx.fill_obligation, 6,
            "R2: response fill_count obligation survives cancel"
        );
    }

    /// E2: cancel-reconcile with authority_complete=false never terminals.
    #[test]
    fn e2_authority_complete_false_blocks_terminal() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        // Partial fill then NotFound → reconcile path.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 4,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V1")),
                fee_cents: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::NotFound),
        );
        let s = o.new_state().unwrap().clone();
        assert!(matches!(s, OrderState::CancelPending { .. }));

        // Authority incomplete even though fills fully match venue_filled.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V1")),
                filled_qty: 4,
                remaining_qty: 6,
                fills: vec![],
                authority_complete: false,
            },
        );
        assert_ne!(o.new_state(), Some(&OrderState::Canceled));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
        assert!(matches!(
            o.new_state(),
            Some(OrderState::ReconcilePending {
                target: ReconcileTarget {
                    terminal: ReconcileTerminal::Canceled,
                    venue_filled_qty: 4,
                    ..
                },
                ..
            })
        ));
    }

    /// E2 positive: authority_complete=true + full attribution → Canceled + release.
    #[test]
    fn e2_authority_complete_true_allows_terminal() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 4,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V1")),
                fee_cents: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::NotFound),
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V1")),
                filled_qty: 4,
                remaining_qty: 6,
                fills: vec![],
                authority_complete: true,
            },
        );
        assert_eq!(o.new_state(), Some(&OrderState::Canceled));
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert_eq!(
            reservation_held(&OrderState::Canceled),
            ReservationHold::Released
        );
        assert_eq!(ctx.attributed_fill_qty, 4);
    }

    /// E3: response fee vs Σ venue fill fees mismatch → Halt.
    #[test]
    fn e3_fee_mismatch_halts() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-fee"),
                fill_count: 4,
                remaining_count: 6,
                avg_price_cents: Some(45),
                fee_cents: Some(100),
                snapshot_boundary: Some(SnapshotBoundary::TsNs(100)),
            },
        );
        let s = o.new_state().unwrap().clone();
        // Journal persists fee (E4 companion).
        assert!(o.effects().iter().any(|e| matches!(
            e,
            Effect::AppendFsync(JournalRecord::SubmitResponse {
                fee_cents: Some(100),
                avg_price_cents: Some(45),
                ..
            })
        )));
        assert!(o.effects().iter().any(|e| matches!(
            e,
            Effect::AppendFsync(JournalRecord::ImmediateFillUnattributed {
                fee_cents: Some(100),
                avg_price_cents: Some(45),
                ..
            })
        )));

        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ImmediateFillBackfillResult {
                fills: vec![fill_rec_vf("Ff", 4, 45, 1, "V-fee", 1)], // fee 1 ≠ 100
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::CrossCheckMismatch { .. },
                ..
            }
        ));
        let detail = match &o {
            TransitionOutcome::Halt {
                reason: HaltReason::CrossCheckMismatch { detail },
                ..
            } => detail.clone(),
            _ => String::new(),
        };
        assert!(detail.contains("fee"), "detail={detail}");
        assert_eq!(ctx.attributed_fee_cents, 1);
        assert_eq!(ctx.response_fee_cents, Some(100));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        // AccountFill carried venue fee before halt cross-check.
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::AccountFill { fee_cents: 1, .. }))
        );
    }

    /// E4: after "restart" ctx rebuild of avg/fee, mismatch still fires.
    #[test]
    fn e4_restart_rebuild_avg_fee_still_cross_checks() {
        let mut ctx = ctx_default();
        // Simulate journal replay restoring response avg/fee (E4 durable fields).
        ctx.response_fill_count = Some(3);
        ctx.response_remaining_count = Some(7);
        ctx.response_avg_price_cents = Some(50);
        ctx.response_fee_cents = Some(9);
        ctx.response_snapshot_boundary = Some(SnapshotBoundary::TsNs(100));

        let s = OrderState::ImmediateFillUnattributed {
            venue_order_id: vid("V-e4"),
            response_fill_count: 3,
            response_remaining_count: 7,
            response_avg_price_cents: Some(50),
            response_fee_cents: Some(9),
        };
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ImmediateFillBackfillResult {
                // notional 3*40=120; expected 50*3=150; tol=3 → mismatch
                fills: vec![fill_rec_vf("Fe4", 3, 40, 1, "V-e4", 9)],
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::CrossCheckMismatch { .. },
                ..
            }
        ));
        // Durable fields preserved across halt.
        assert_eq!(ctx.response_avg_price_cents, Some(50));
        assert_eq!(ctx.response_fee_cents, Some(9));
    }

    /// E5: Canceled reconcile with lagging fills finalizes after attribution catches up.
    #[test]
    fn e5_canceled_target_finalizes_after_fills() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let coid = ctx.client_order_id.clone();
        // Exhaustive Canceled venue_filled=4 but only qty=2 fills first batch.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![BackfillOrderRecord {
                    client_order_id: coid,
                    venue_order_id: vid("V-e5"),
                    status: BackfillOrderStatus::Canceled,
                    filled_qty: 4,
                    remaining_qty: 6,
                    fills: vec![fill_rec_v("F1", 2, 45, 1, "V-e5")],
                }],
            },
        );
        assert!(matches!(
            o.new_state(),
            Some(OrderState::ReconcilePending {
                filled_qty: 2,
                target: ReconcileTarget {
                    terminal: ReconcileTerminal::Canceled,
                    venue_filled_qty: 4,
                    ..
                },
                ..
            })
        ));
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert_eq!(ctx.attributed_fill_qty, 2);

        let s = o.new_state().unwrap().clone();
        // G1: remaining fills arrive — new fill_id invalidates authority; attribution
        // catches target but must not release until post-fill authority re-proof.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F2"),
                qty: 2,
                price_cents: 45,
                ts_ns: 2,
                venue_order_id: Some(vid("V-e5")),
                fee_cents: None,
            },
        );
        assert_eq!(ctx.attributed_fill_qty, 4);
        assert_eq!(o.account_fill_count(), 1);
        assert!(!ctx.authority_is_fresh(), "G1: new fill clears authority");
        assert_ne!(o.new_state(), Some(&OrderState::Canceled));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert!(matches!(
            o.new_state(),
            Some(OrderState::ReconcilePending { .. })
        ));
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::RequestAuthorityReconcile { .. }))
        );
        // Post-fill authority re-proof → Canceled + release.
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V-e5")),
                filled_qty: 4,
                remaining_qty: 6,
                fills: vec![],
                authority_complete: true,
            },
        );
        assert_eq!(o.new_state(), Some(&OrderState::Canceled));
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert_eq!(
            reservation_held(&OrderState::Canceled),
            ReservationHold::Released
        );
    }

    /// E6: known venue order + fill missing venue_order_id → FillOwnershipMismatch.
    #[test]
    fn e6_fill_missing_venue_id_halts() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-own"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("Fx"),
                qty: 1,
                price_cents: 40,
                ts_ns: 1,
                venue_order_id: None, // missing while parent known
                fee_cents: None,
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::FillOwnershipMismatch { .. },
                ..
            }
        ));
        assert_eq!(ctx.attributed_fill_qty, 0);
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::AccountFill { .. }))
        );
    }

    /// E7: non-exhaustive Open match stays matching-pending; second match → AmbiguousMatch.
    #[test]
    fn e7_non_exhaustive_open_stays_pending_then_ambiguous() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let coid = ctx.client_order_id.clone();

        // Page 1: single Open match but not exhaustive → must NOT commit Accepted.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: false,
                matched: vec![BackfillOrderRecord {
                    client_order_id: coid.clone(),
                    venue_order_id: vid("V-a"),
                    status: BackfillOrderStatus::Open,
                    filled_qty: 0,
                    remaining_qty: 10,
                    fills: vec![],
                }],
            },
        );
        assert_eq!(o.new_state(), Some(&OrderState::SubmitUnknown));
        assert!(!matches!(o.new_state(), Some(OrderState::Accepted { .. })));
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::BackfillUnknown { .. }))
        );

        let s = o.new_state().unwrap().clone();
        // Page 2: second order same client_id → AmbiguousMatch.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: false,
                matched: vec![
                    BackfillOrderRecord {
                        client_order_id: coid.clone(),
                        venue_order_id: vid("V-a"),
                        status: BackfillOrderStatus::Open,
                        filled_qty: 0,
                        remaining_qty: 10,
                        fills: vec![],
                    },
                    BackfillOrderRecord {
                        client_order_id: coid,
                        venue_order_id: vid("V-b"),
                        status: BackfillOrderStatus::Open,
                        filled_qty: 0,
                        remaining_qty: 10,
                        fills: vec![],
                    },
                ],
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::AmbiguousMatch { count: 2 },
                ..
            }
        ));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
    }

    /// E7 positive: exhaustive Open with single match commits Accepted.
    #[test]
    fn e7_exhaustive_open_commits_accepted() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let coid = ctx.client_order_id.clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![BackfillOrderRecord {
                    client_order_id: coid,
                    venue_order_id: vid("V-ok"),
                    status: BackfillOrderStatus::Open,
                    filled_qty: 0,
                    remaining_qty: 10,
                    fills: vec![],
                }],
            },
        );
        assert!(matches!(
            o.new_state(),
            Some(OrderState::Accepted {
                venue_order_id
            }) if venue_order_id == &vid("V-ok")
        ));
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
    }

    /// E8: notional within ±(fill_count×1¢) of response avg·count passes (floored avg would fail).
    #[test]
    fn e8_notional_tolerance_passes() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        // fills 1@44 + 2@45 → notional 134; true avg 44.666; venue avg 45
        // expected_notional=135, tol=3 → pass
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-e8"),
                fill_count: 3,
                remaining_count: 7,
                avg_price_cents: Some(45),
                fee_cents: None,
                snapshot_boundary: Some(SnapshotBoundary::TsNs(100)),
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ImmediateFillBackfillResult {
                fills: vec![
                    fill_rec_v("Fa", 1, 44, 1, "V-e8"),
                    fill_rec_v("Fb", 2, 45, 2, "V-e8"),
                ],
            },
        );
        assert!(
            matches!(
                o.new_state(),
                Some(OrderState::Partial {
                    filled_qty: 3,
                    remaining_qty: 7,
                    ..
                })
            ),
            "got {:?}",
            o.new_state()
        );
        assert!(!o.is_halt());
        assert_eq!(ctx.attributed_notional_cents, 134);
        assert_eq!(ctx.attributed_fill_qty, 3);
        // Floored local avg would be 44 ≠ 45 — E8 must not use that comparison.
        assert_eq!(ctx.attributed_avg_price_cents(), Some(44));
    }

    /// E8: notional beyond tolerance → Halt.
    #[test]
    fn e8_notional_over_tolerance_halts() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-e8b"),
                fill_count: 3,
                remaining_count: 7,
                avg_price_cents: Some(50),
                fee_cents: None,
                snapshot_boundary: Some(SnapshotBoundary::TsNs(100)),
            },
        );
        let s = o.new_state().unwrap().clone();
        // notional 3*40=120; expected 150; tol=3 → halt
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ImmediateFillBackfillResult {
                fills: vec![fill_rec_v("Fx", 3, 40, 1, "V-e8b")],
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::CrossCheckMismatch { .. },
                ..
            }
        ));
    }

    /// E9/D1 isolate: attribution complete but exhaustive=false → still non-terminal.
    #[test]
    fn e9_d1_exhaustive_gate_isolated_from_attribution() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let coid = ctx.client_order_id.clone();
        // Full fills present (attribution complete) but exhaustive=false.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: false,
                matched: vec![BackfillOrderRecord {
                    client_order_id: coid,
                    venue_order_id: vid("V-iso"),
                    status: BackfillOrderStatus::Filled,
                    filled_qty: 10,
                    remaining_qty: 0,
                    fills: vec![fill_rec_v("Fall", 10, 45, 1, "V-iso")],
                }],
            },
        );
        assert_ne!(o.new_state(), Some(&OrderState::Filled));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        // R5: non-exhaustive single candidate must NOT AccountFill (no pollution).
        assert_eq!(
            ctx.attributed_fill_qty, 0,
            "R5: no accounting before exhaustive+unique"
        );
        assert_eq!(o.account_fill_count(), 0);
        assert_eq!(
            reservation_held(o.new_state().unwrap()),
            ReservationHold::Full
        );
        // Must stay matching-pending (SubmitUnknown) so further pages can AmbiguousMatch.
        assert_eq!(o.new_state(), Some(&OrderState::SubmitUnknown));
        assert!(o.effects().iter().any(|e| matches!(
            e,
            Effect::BackfillUnknown { .. } | Effect::BackfillFills { .. }
        )));
    }

    /// E9: golden IOC full fill exact effects (AccountFill carries fee).
    #[test]
    fn e9_golden_ioc_full_exact_effects() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-g"),
                fill_count: 10,
                remaining_count: 0,
                avg_price_cents: Some(45),
                fee_cents: Some(5),
                snapshot_boundary: None,
            },
        );
        // Exact enter effects (G4: VenueBound + ObligationRaised + AuthorityInvalidated).
        assert_eq!(
            o.effects(),
            &[
                Effect::AppendFsync(JournalRecord::SubmitResponse {
                    venue_order_id: vid("V-g"),
                    fill_count: 10,
                    remaining_count: 0,
                    avg_price_cents: Some(45),
                    fee_cents: Some(5),
                    snapshot_boundary: None,
                }),
                Effect::AppendFsync(JournalRecord::VenueBoundCid {
                    client_order_id: ClientOrderId("KXBTC-M|hoff-mm|1".into()),
                    venue_order_id: vid("V-g"),
                }),
                Effect::AppendFsync(JournalRecord::ObligationRaised {
                    fill_obligation: 10,
                    authority_epoch: 1,
                }),
                Effect::AppendFsync(JournalRecord::AuthorityInvalidated { epoch: 1 }),
                Effect::AppendFsync(JournalRecord::ImmediateFillUnattributed {
                    venue_order_id: vid("V-g"),
                    fill_count: 10,
                    remaining_count: 0,
                    avg_price_cents: Some(45),
                    fee_cents: Some(5),
                }),
                Effect::BackfillFills {
                    venue_order_id: vid("V-g"),
                },
            ]
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ImmediateFillBackfillResult {
                fills: vec![fill_rec_vf("Fg", 10, 45, 1, "V-g", 5)],
            },
        );
        assert_eq!(o.new_state(), Some(&OrderState::Filled));
        // epoch: 1 (response) → 2 (new fill + AuthorityInvalidated) → latch at 2.
        assert_eq!(
            o.effects(),
            &[
                Effect::AppendFsync(JournalRecord::AuthorityInvalidated { epoch: 2 }),
                Effect::AppendFsync(JournalRecord::Fill {
                    fill_id: fid("Fg"),
                    qty: 10,
                    price_cents: 45,
                    ts_ns: 1,
                    fee_cents: Some(5),
                    venue_order_id: Some(vid("V-g")),
                }),
                Effect::AccountFill {
                    fill_id: fid("Fg"),
                    qty: 10,
                    price_cents: 45,
                    ts_ns: 1,
                    fee_cents: 5,
                },
                Effect::AppendFsync(JournalRecord::AuthorityLatched { epoch: 2 }),
                Effect::AppendFsync(JournalRecord::OrderTerminal {
                    kind: TerminalKind::Filled,
                    venue_order_id: Some(vid("V-g")),
                    fill_obligation: 10,
                    authority_complete: true,
                    authority_epoch: 2,
                    attributed_fill_qty: 10,
                    attributed_fee_cents: 5,
                }),
                Effect::ReleaseReservation,
            ]
        );
        assert!(!o.has_resubmit_effect());
        assert_eq!(ctx.attributed_fee_cents, 5);
        assert_eq!(ctx.response_avg_price_cents, Some(45));
        assert_eq!(ctx.response_fee_cents, Some(5));
    }

    // ── R1–R7 structural coverage ─────────────────────────────────────────

    /// R1: sole production site for ReleaseReservation is try_finalize_terminal.
    #[test]
    fn r1_release_reservation_only_via_try_finalize() {
        // Runtime: successful release only after authority latch; cancel alone never releases.
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::Canceled),
        );
        assert_eq!(
            o.effects()
                .iter()
                .filter(|e| matches!(e, Effect::ReleaseReservation))
                .count(),
            0,
            "F1: cancel must not forge authority/release"
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(&s, &mut ctx, &authority_reconcile_canceled(0, 10));
        assert_eq!(o.new_state(), Some(&OrderState::Canceled));
        assert_eq!(
            o.effects()
                .iter()
                .filter(|e| matches!(e, Effect::ReleaseReservation))
                .count(),
            1
        );
    }

    /// R1/R2: response fill=6 → cancel Rejected → re-cancel Canceled with attributed=4 → no release.
    #[test]
    fn r2_obligation_survives_reject_then_blocks_cancel_release() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-r2"),
                fill_count: 6,
                remaining_count: 4,
                avg_price_cents: Some(45),
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        assert_eq!(ctx.fill_obligation, 6);
        // Attribute only 4.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("Fa"),
                qty: 4,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V-r2")),
                fee_cents: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        assert!(matches!(s, OrderState::ImmediateFillUnattributed { .. }));
        let o = apply_event(&s, &mut ctx, &OrderEvent::CancelRequested);
        let s = o.new_state().unwrap().clone();
        // Rejected demotes live — obligation must survive on ctx.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::Rejected),
        );
        let s = o.new_state().unwrap().clone();
        assert!(matches!(
            s,
            OrderState::Partial { .. } | OrderState::Accepted { .. }
        ));
        assert_eq!(
            ctx.fill_obligation, 6,
            "R2: Rejected must not drop obligation"
        );
        // Re-request cancel then Canceled — still lagging → no release.
        let o = apply_event(&s, &mut ctx, &OrderEvent::CancelRequested);
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::Canceled),
        );
        assert_ne!(o.new_state(), Some(&OrderState::Canceled));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert_eq!(ctx.fill_obligation, 6);
        assert_eq!(ctx.attributed_fill_qty, 4);
    }

    /// R5: non-exhaustive single candidate with fills → no AccountFill; page2 ambiguous → Halt clean.
    #[test]
    fn r5_non_exhaustive_candidate_buffered_no_account_then_ambiguous() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let coid = ctx.client_order_id.clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: false,
                matched: vec![BackfillOrderRecord {
                    client_order_id: coid.clone(),
                    venue_order_id: vid("V-a"),
                    status: BackfillOrderStatus::Filled,
                    filled_qty: 10,
                    remaining_qty: 0,
                    fills: vec![fill_rec_v("Fall", 10, 45, 1, "V-a")],
                }],
            },
        );
        assert_eq!(o.new_state(), Some(&OrderState::SubmitUnknown));
        assert_eq!(o.account_fill_count(), 0);
        assert_eq!(ctx.attributed_fill_qty, 0);
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: false,
                matched: vec![
                    BackfillOrderRecord {
                        client_order_id: coid.clone(),
                        venue_order_id: vid("V-a"),
                        status: BackfillOrderStatus::Filled,
                        filled_qty: 10,
                        remaining_qty: 0,
                        fills: vec![fill_rec_v("Fall", 10, 45, 1, "V-a")],
                    },
                    BackfillOrderRecord {
                        client_order_id: coid,
                        venue_order_id: vid("V-b"),
                        status: BackfillOrderStatus::Open,
                        filled_qty: 0,
                        remaining_qty: 10,
                        fills: vec![],
                    },
                ],
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::AmbiguousMatch { count: 2 },
                ..
            }
        ));
        assert_eq!(ctx.attributed_fill_qty, 0, "no pollution");
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::AccountFill { .. }))
        );
    }

    /// R3: ReconcileResult with different venue id → OwnershipConflict (no parent substitution).
    #[test]
    fn r3_reconcile_venue_id_substitution_halts() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        assert_eq!(ctx.venue_order_id, Some(vid("V1")));
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V-OTHER")),
                filled_qty: 0,
                remaining_qty: 10,
                fills: vec![],
                authority_complete: true,
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::OwnershipConflict { .. },
                ..
            }
        ));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
    }

    /// B1 / Low #1 (CancelPending call site, L1149): true mutate-before-Halt.
    /// `ctx.venue_order_id=None`, state holds venue=A, event carries B → bind mutates
    /// ctx to A + VenueBound(A), then OwnershipConflict. Halt effects **must** still
    /// include VenueBound(A) *before* Halted so fold rebuilds venue==Some(A)==memory.
    ///
    /// Catches a future regression that drops `push_journal(bind_jr)` on the Err path:
    /// memory would still show Some(A) but journals would lack VenueBound → fold stays
    /// None and `assert_ctx_fold_equiv` fails.
    #[test]
    fn b1_cancel_pending_mutate_before_halt_emits_venue_bound_fold_equiv() {
        let mut ctx = ctx_default();
        assert_eq!(ctx.venue_order_id, None, "fixture: unbound ctx");
        // State carries A while ctx is still None — forces resolve_reconcile_venue to
        // bind A (mutate + VenueBound) before comparing event B.
        let state = OrderState::CancelPending {
            venue_order_id: vid("A"),
            filled_qty: 0,
            remaining_qty: 10,
            response_fill_count: None,
            response_avg_price_cents: None,
            response_fee_cents: None,
            reconcile_target: None,
        };
        let o = apply_event(
            &state,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("B")),
                filled_qty: 0,
                remaining_qty: 10,
                fills: vec![],
                authority_complete: true,
            },
        );
        assert!(o.is_halt(), "B1 CancelPending: must Halt: {o:?}");
        assert!(
            matches!(
                o,
                TransitionOutcome::Halt {
                    reason: HaltReason::OwnershipConflict { .. },
                    ..
                }
            ),
            "B1 CancelPending: OwnershipConflict: {o:?}"
        );
        // ① memory reflects mutate-before-Halt bind of A.
        assert_eq!(
            ctx.venue_order_id,
            Some(vid("A")),
            "B1: memory venue must be A after mutate-then-conflict"
        );

        let eff = o.effects();
        // ② VenueBound(A) present and ordered before Halted.
        let vb_pos = eff.iter().position(|e| {
            matches!(
                e,
                Effect::AppendFsync(JournalRecord::VenueBoundCid { venue_order_id, .. })
                    if venue_order_id == &vid("A")
            )
        });
        let halt_pos = eff
            .iter()
            .position(|e| matches!(e, Effect::AppendFsync(JournalRecord::Halted { .. })));
        assert!(
            vb_pos.is_some(),
            "B1 CancelPending: Halt effects must include VenueBound(A) \
             (catches missing push_journal(bind_jr)): {eff:?}"
        );
        assert!(
            halt_pos.is_some() && vb_pos.unwrap() < halt_pos.unwrap(),
            "B1 CancelPending: VenueBound(A) must precede Halted: {eff:?}"
        );

        // ③ full field-by-field fold ≡ memory (venue_order_id==Some(A)).
        let mut journals = Vec::new();
        for e in eff {
            if let Effect::AppendFsync(jr) = e {
                journals.push(jr.clone());
            }
        }
        assert_ctx_fold_equiv(&ctx, &journals);
        assert_eq!(
            ctx.venue_order_id,
            Some(vid("A")),
            "B1 CancelPending: fold-equiv leaves memory venue Some(A)"
        );
    }

    /// B1 / Low #1 (ReconcilePending call site, L1210): same mutate-before-Halt
    /// fixture as CancelPending — ctx unbound, state venue=A, event B.
    /// Must emit VenueBound(A) before Halted; `assert_ctx_fold_equiv` fails if
    /// the Err path forgets `push_journal(bind_jr)`.
    #[test]
    fn b1_reconcile_pending_mutate_before_halt_emits_venue_bound_fold_equiv() {
        let mut ctx = ctx_default();
        assert_eq!(ctx.venue_order_id, None, "fixture: unbound ctx");
        let state = OrderState::ReconcilePending {
            venue_order_id: vid("A"),
            filled_qty: 0,
            remaining_qty: 10,
            target: ReconcileTarget {
                terminal: ReconcileTerminal::Canceled,
                venue_filled_qty: 0,
                venue_remaining_qty: Some(10),
            },
            response_fill_count: None,
            response_avg_price_cents: None,
            response_fee_cents: None,
        };
        let o = apply_event(
            &state,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("B")),
                filled_qty: 0,
                remaining_qty: 10,
                fills: vec![],
                authority_complete: true,
            },
        );
        assert!(o.is_halt(), "B1 ReconcilePending: must Halt: {o:?}");
        assert!(
            matches!(
                o,
                TransitionOutcome::Halt {
                    reason: HaltReason::OwnershipConflict { .. },
                    ..
                }
            ),
            "B1 ReconcilePending: OwnershipConflict: {o:?}"
        );
        assert_eq!(
            ctx.venue_order_id,
            Some(vid("A")),
            "B1: memory venue must be A after mutate-then-conflict"
        );

        let eff = o.effects();
        let vb_pos = eff.iter().position(|e| {
            matches!(
                e,
                Effect::AppendFsync(JournalRecord::VenueBoundCid { venue_order_id, .. })
                    if venue_order_id == &vid("A")
            )
        });
        let halt_pos = eff
            .iter()
            .position(|e| matches!(e, Effect::AppendFsync(JournalRecord::Halted { .. })));
        assert!(
            vb_pos.is_some(),
            "B1 ReconcilePending: Halt effects must include VenueBound(A) \
             (catches missing push_journal(bind_jr)): {eff:?}"
        );
        assert!(
            halt_pos.is_some() && vb_pos.unwrap() < halt_pos.unwrap(),
            "B1 ReconcilePending: VenueBound(A) must precede Halted: {eff:?}"
        );

        let mut journals = Vec::new();
        for e in eff {
            if let Effect::AppendFsync(jr) = e {
                journals.push(jr.clone());
            }
        }
        assert_ctx_fold_equiv(&ctx, &journals);
        assert_eq!(
            ctx.venue_order_id,
            Some(vid("A")),
            "B1 ReconcilePending: fold-equiv leaves memory venue Some(A)"
        );
    }

    /// R3: sibling fill with wrong venue while parent known → ownership halt.
    #[test]
    fn r3_sibling_fill_wrong_venue_halts() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-own"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("Fsib"),
                qty: 1,
                price_cents: 40,
                ts_ns: 1,
                venue_order_id: Some(vid("V-SIBLING")),
                fee_cents: None,
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::FillOwnershipMismatch { .. },
                ..
            }
        ));
        assert_eq!(ctx.attributed_fill_qty, 0);
    }

    /// R3: terminal late fill with wrong venue → ownership halt (no PostTerminal book).
    #[test]
    fn r3_terminal_late_fill_wrong_venue_halts() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        let o = cancel_to_terminal_with_authority(&mut ctx, s);
        assert_eq!(o.new_state(), Some(&OrderState::Canceled));
        let o = apply_event(
            &OrderState::Canceled,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-late-bad"),
                qty: 1,
                price_cents: 40,
                ts_ns: 9,
                venue_order_id: Some(vid("V-OTHER")),
                fee_cents: None,
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::FillOwnershipMismatch { .. },
                ..
            }
        ));
        assert_eq!(ctx.attributed_fill_qty, 0);
    }

    /// R4/F5: fee None→Some on same fill_id → fee correction upgrade, not Halt.
    #[test]
    fn r4_fee_none_then_some_upgrades() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-fee"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 2,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V-fee")),
                fee_cents: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        assert_eq!(ctx.attributed_fill_qty, 2);
        assert_eq!(ctx.attributed_fee_cents, 0);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 2,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V-fee")),
                fee_cents: Some(5),
            },
        );
        assert!(!o.is_halt(), "fee None→Some must upgrade: {o:?}");
        assert_eq!(ctx.attributed_fill_qty, 2, "must not re-apply qty");
        assert_eq!(ctx.attributed_fee_cents, 5, "fee ledger corrected");
        assert!(o.effects().iter().any(|e| matches!(
            e,
            Effect::AccountFeeCorrection {
                delta_fee_cents: 5,
                ..
            }
        )));
        assert_eq!(
            ctx.applied_fills.get(&fid("F1")).and_then(|p| p.fee),
            Some(5)
        );
    }

    /// R4: fee Some(1)→Some(2) → ConflictingFillPayload.
    #[test]
    fn r4_fee_some_conflict_halts() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-fee2"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 1,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V-fee2")),
                fee_cents: Some(1),
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 1,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V-fee2")),
                fee_cents: Some(2),
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::ConflictingFillPayload { .. },
                ..
            }
        ));
    }

    /// R4: JournalRecord::Fill carries fee + venue.
    #[test]
    fn r4_journal_fill_carries_fee_and_venue() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-j"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("Fj"),
                qty: 3,
                price_cents: 41,
                ts_ns: 7,
                venue_order_id: Some(vid("V-j")),
                fee_cents: Some(2),
            },
        );
        assert!(o.effects().iter().any(|e| matches!(
            e,
            Effect::AppendFsync(JournalRecord::Fill {
                fill_id,
                qty: 3,
                price_cents: 41,
                ts_ns: 7,
                fee_cents: Some(2),
                venue_order_id: Some(v),
            }) if fill_id == &fid("Fj") && v == &vid("V-j")
        )));
    }

    /// R6/F12: response snapshot 2 fills + later 1 more must not false-HALT on avg cross-check at cancel.
    #[test]
    fn r6_cross_check_domain_ignores_post_response_fills() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        // Response: 2 @ avg 45
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-dom"),
                fill_count: 2,
                remaining_count: 8,
                avg_price_cents: Some(45),
                fee_cents: Some(2),
                snapshot_boundary: Some(SnapshotBoundary::TsNs(2)),
            },
        );
        let s = o.new_state().unwrap().clone();
        // Two fills matching snapshot.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ImmediateFillBackfillResult {
                fills: vec![
                    fill_rec_vf("F1", 1, 45, 1, "V-dom", 1),
                    fill_rec_vf("F2", 1, 45, 2, "V-dom", 1),
                ],
            },
        );
        let s = o.new_state().unwrap().clone();
        assert!(matches!(s, OrderState::Partial { filled_qty: 2, .. }));
        // Later extra fill at different price (would break full-notional check vs response*2).
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F3"),
                qty: 1,
                price_cents: 10,
                ts_ns: 3,
                venue_order_id: Some(vid("V-dom")),
                fee_cents: Some(99),
            },
        );
        let s = o.new_state().unwrap().clone();
        assert!(
            !o.is_halt(),
            "post-response fill must not cross-check-halt: {o:?}"
        );
        // Cancel alone must not release without authority (F1).
        let o = apply_event(&s, &mut ctx, &OrderEvent::CancelRequested);
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::Canceled),
        );
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        let s = o.new_state().unwrap().clone();
        // Authority complete + domain cross-check uses only first 2 fills (F6 identity).
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V-dom")),
                filled_qty: 3,
                remaining_qty: 7,
                fills: vec![],
                authority_complete: true,
            },
        );
        assert_eq!(o.new_state(), Some(&OrderState::Canceled), "got {:?}", o);
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert!(!o.is_halt());
    }

    /// R6/F11: authority_complete=false && attributed==venue_filled → RequestAuthorityReconcile.
    #[test]
    fn r6_authority_incomplete_emits_request_authority() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 4,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V1")),
                fee_cents: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V1")),
                filled_qty: 4,
                remaining_qty: 6,
                fills: vec![],
                authority_complete: false,
            },
        );
        assert_ne!(o.new_state(), Some(&OrderState::Canceled));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::RequestAuthorityReconcile { .. })),
            "effects={:?}",
            o.effects()
        );
    }

    /// R6/F13: client_order_id mismatch Halt includes durable Halted journal.
    #[test]
    fn r6_client_id_mismatch_durable_halted() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![BackfillOrderRecord {
                    client_order_id: ClientOrderId("OTHER|x|1".into()),
                    venue_order_id: vid("V-x"),
                    status: BackfillOrderStatus::Open,
                    filled_qty: 0,
                    remaining_qty: 10,
                    fills: vec![],
                }],
            },
        );
        assert!(o.is_halt());
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::AppendFsync(JournalRecord::Halted { .. })))
        );
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::HaltNewExposure))
        );
    }

    /// E5: authority=false branch stays non-terminal with authority request.
    #[test]
    fn e5_authority_false_keeps_reconcile_pending() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let coid = ctx.client_order_id.clone();
        // Exhaustive unique with lagging fills → ReconcilePending.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![BackfillOrderRecord {
                    client_order_id: coid,
                    venue_order_id: vid("V-e5b"),
                    status: BackfillOrderStatus::Canceled,
                    filled_qty: 4,
                    remaining_qty: 6,
                    fills: vec![fill_rec_v("F1", 2, 45, 1, "V-e5b")],
                }],
            },
        );
        let s = o.new_state().unwrap().clone();
        assert!(matches!(s, OrderState::ReconcilePending { .. }));
        // Authority incomplete re-report with same attribution → still non-terminal.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V-e5b")),
                filled_qty: 4,
                remaining_qty: 6,
                fills: vec![],
                authority_complete: false,
            },
        );
        assert_ne!(o.new_state(), Some(&OrderState::Canceled));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert!(matches!(
            o.new_state(),
            Some(OrderState::ReconcilePending { .. })
        ));
    }

    /// E4/F7: true JournalRecord fold rebuilds response avg/fee + still cross-checks.
    #[test]
    fn e4_journal_record_rebuild_cross_checks() {
        let base = ctx_default();
        let records = vec![
            JournalRecord::SubmitPrepared {
                client_order_id: base.client_order_id.clone(),
            },
            JournalRecord::SubmitStarted {
                attempt_id: AttemptId("a1".into()),
            },
            JournalRecord::SubmitResponse {
                venue_order_id: vid("V-e4j"),
                fill_count: 3,
                remaining_count: 7,
                avg_price_cents: Some(50),
                fee_cents: Some(9),
                snapshot_boundary: Some(SnapshotBoundary::TsNs(100)),
            },
        ];
        let mut ctx = rebuild_ctx_from_journal(base, &records).expect("rebuild ok");
        assert_eq!(ctx.response_avg_price_cents, Some(50));
        assert_eq!(ctx.response_fee_cents, Some(9));
        assert_eq!(ctx.response_fill_count, Some(3));
        assert_eq!(ctx.venue_order_id, Some(vid("V-e4j")));
        assert_eq!(
            ctx.response_snapshot_boundary,
            Some(SnapshotBoundary::TsNs(100))
        );

        let s = OrderState::ImmediateFillUnattributed {
            venue_order_id: vid("V-e4j"),
            response_fill_count: 3,
            response_remaining_count: 7,
            response_avg_price_cents: Some(50),
            response_fee_cents: Some(9),
        };
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ImmediateFillBackfillResult {
                // notional 3*40=120; expected 50*3=150; tol=3 → mismatch
                fills: vec![fill_rec_vf("Fe4j", 3, 40, 1, "V-e4j", 9)],
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::CrossCheckMismatch { .. },
                ..
            }
        ));
    }

    /// Replay carries fee through fold and compares fee/notional.
    #[test]
    fn replay_fills_with_fee_and_notional() {
        let ctx0 = ctx_default();
        let events = vec![
            OrderEvent::PrepareSubmit,
            OrderEvent::StartSubmit {
                attempt_id: AttemptId("a1".into()),
            },
            OrderEvent::SubmitResponse {
                venue_order_id: vid("V-rp"),
                fill_count: 4,
                remaining_count: 6,
                avg_price_cents: Some(45),
                fee_cents: Some(4),
                snapshot_boundary: None,
            },
            OrderEvent::ImmediateFillBackfillResult {
                fills: vec![
                    fill_rec_vf("F1", 2, 45, 1, "V-rp", 2),
                    fill_rec_vf("F2", 2, 45, 2, "V-rp", 2),
                ],
            },
        ];
        let r = replay(OrderState::New, ctx0, &events);
        assert!(matches!(
            r.state,
            OrderState::Partial {
                filled_qty: 4,
                remaining_qty: 6,
                ..
            }
        ));
        assert_eq!(r.ctx.attributed_fill_qty, 4);
        assert_eq!(r.ctx.attributed_fee_cents, 4);
        assert_eq!(r.ctx.attributed_notional_cents, 180);
        assert_eq!(r.ctx.fill_obligation, 4);
        assert_eq!(r.halt, None);
        assert_eq!(r.reject, None);
    }

    // ── F1–F8 rework-4 coverage ───────────────────────────────────────────

    /// F1: authority=false then target fill catches up → still no release (Med2).
    #[test]
    fn f1_authority_false_target_fill_no_release() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        // Enter reconcile without authority; venue_filled=4, no fills yet.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V1")),
                filled_qty: 4,
                remaining_qty: 6,
                fills: vec![],
                authority_complete: false,
            },
        );
        assert!(!ctx.authority_complete);
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        let s = o.new_state().unwrap().clone();
        assert!(matches!(s, OrderState::ReconcilePending { .. }));
        // Target fills arrive — attributed catches obligation but authority still false.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-tgt"),
                qty: 4,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V1")),
                fee_cents: None,
            },
        );
        assert_eq!(ctx.attributed_fill_qty, 4);
        assert_eq!(ctx.fill_obligation, 4);
        assert!(!ctx.authority_complete);
        assert_ne!(o.new_state(), Some(&OrderState::Canceled));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert!(matches!(
            o.new_state(),
            Some(OrderState::ReconcilePending { .. })
        ));
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::RequestAuthorityReconcile { .. }))
        );
    }

    /// F1: cancel alone never latches authority even with attributed==obligation==0.
    #[test]
    fn f1_cancel_outcome_never_latches_authority() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::Canceled),
        );
        assert!(!ctx.authority_complete);
        assert_ne!(o.new_state(), Some(&OrderState::Canceled));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
    }

    /// F2: pre-bind None venue fill then Some(bound) → provenance upgrade, no Halt.
    #[test]
    fn f2_none_venue_provenance_upgrade_no_halt() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        // WS fill before response (venue unknown on fill).
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 2,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: None,
                fee_cents: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        assert_eq!(ctx.attributed_fill_qty, 2);
        assert!(
            ctx.applied_fills
                .get(&fid("F1"))
                .unwrap()
                .venue_order_id
                .is_none()
        );
        // Bind parent via response covering WS fills.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-A"),
                fill_count: 2,
                remaining_count: 8,
                avg_price_cents: Some(45),
                fee_cents: None,
                snapshot_boundary: Some(SnapshotBoundary::TsNs(10)),
            },
        );
        assert!(!o.is_halt(), "bind with prior None-venue fill: {o:?}");
        assert_eq!(ctx.venue_order_id, Some(vid("V-A")));
        let s = o.new_state().unwrap().clone();
        // Same fill re-delivered with bound venue → provenance upgrade.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 2,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V-A")),
                fee_cents: None,
            },
        );
        assert!(!o.is_halt(), "provenance upgrade must not Halt: {o:?}");
        assert_eq!(ctx.attributed_fill_qty, 2, "no double qty");
        assert_eq!(
            ctx.applied_fills.get(&fid("F1")).unwrap().venue_order_id,
            Some(vid("V-A"))
        );
    }

    /// F2: None→Some(wrong venue) → FillOwnershipMismatch.
    #[test]
    fn f2_none_venue_upgrade_wrong_venue_halts() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 2,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: None,
                fee_cents: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-A"),
                fill_count: 2,
                remaining_count: 8,
                avg_price_cents: Some(45),
                fee_cents: None,
                snapshot_boundary: Some(SnapshotBoundary::TsNs(10)),
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 2,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V-OTHER")),
                fee_cents: None,
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::FillOwnershipMismatch { .. },
                ..
            }
        ));
    }

    /// F3: Filled status with venue_remaining>0 must not release.
    #[test]
    fn f3_filled_with_venue_remaining_no_release() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let coid = ctx.client_order_id.clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![BackfillOrderRecord {
                    client_order_id: coid,
                    venue_order_id: vid("V-f3"),
                    status: BackfillOrderStatus::Filled,
                    filled_qty: 10,
                    remaining_qty: 1, // contradict Filled
                    fills: vec![fill_rec_v("Fall", 10, 45, 1, "V-f3")],
                }],
            },
        );
        assert_ne!(o.new_state(), Some(&OrderState::Filled));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert!(ctx.authority_complete); // exhaustive latched, but F3 gate blocks
    }

    /// F3: venue_filled > order qty → OverFill Halt.
    #[test]
    fn f3_venue_filled_over_qty_halts() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let coid = ctx.client_order_id.clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![BackfillOrderRecord {
                    client_order_id: coid,
                    venue_order_id: vid("V-over"),
                    status: BackfillOrderStatus::Filled,
                    filled_qty: 12, // > qty=10
                    remaining_qty: 0,
                    fills: vec![],
                }],
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::ObligationExceedsOrderQty { .. } | HaltReason::OverFill { .. },
                ..
            }
        ));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
    }

    /// F4: raise obligation > qty → ObligationExceedsOrderQty Halt.
    /// B3/A2 second site (`route_from_backfill_status`): Halt carries ReconcileObserved;
    /// fold last_venue_remaining_qty ≡ memory (assert_ctx_fold_equiv).
    #[test]
    fn f4_obligation_exceeds_qty_halts() {
        let base = ctx_default();
        let mut ctx = base.clone();
        let s = to_cancel_pending(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V1")),
                filled_qty: 99,
                remaining_qty: 0,
                fills: vec![],
                authority_complete: true,
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::ObligationExceedsOrderQty {
                    fill_obligation: 99,
                    order_qty: 10,
                },
                ..
            }
        ));
        // B3: route_from_backfill_status emits ReconcileObserved before Halt-prone raise.
        let eff = o.effects();
        assert!(
            eff.iter().any(|e| matches!(
                e,
                Effect::AppendFsync(JournalRecord::ReconcileObserved {
                    venue_remaining_qty: 0,
                    venue_filled_qty: 99,
                    ..
                })
            )),
            "B3/A2: Halt must carry ReconcileObserved: {eff:?}"
        );
        assert_eq!(
            ctx.last_venue_remaining_qty,
            Some(0),
            "B3 memory remaining after note_venue_remaining"
        );

        // Full journal prefix through CancelPending + Halt effects.
        let mut journals = vec![
            JournalRecord::SubmitPrepared {
                client_order_id: base.client_order_id.clone(),
            },
            JournalRecord::SubmitStarted {
                attempt_id: AttemptId("a1".into()),
            },
            JournalRecord::SubmitResponse {
                venue_order_id: vid("V1"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
            JournalRecord::CancelRequested,
        ];
        for e in eff {
            if let Effect::AppendFsync(jr) = e {
                journals.push(jr.clone());
            }
        }
        assert_ctx_fold_equiv(&ctx, &journals);
        assert_eq!(
            ctx.last_venue_remaining_qty,
            Some(0),
            "B3 fold remaining via helper ≡ memory Some(0)"
        );
    }

    /// F5: fee None→Some corrects attributed_fee (covered by r4_fee_none_then_some_upgrades).
    #[test]
    fn f5_fee_none_to_some_corrects_ledger() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-f5"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("Ff5"),
                qty: 3,
                price_cents: 40,
                ts_ns: 1,
                venue_order_id: Some(vid("V-f5")),
                fee_cents: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        assert_eq!(ctx.attributed_fee_cents, 0);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("Ff5"),
                qty: 3,
                price_cents: 40,
                ts_ns: 1,
                venue_order_id: Some(vid("V-f5")),
                fee_cents: Some(7),
            },
        );
        assert!(!o.is_halt());
        assert_eq!(ctx.attributed_fee_cents, 7);
        assert_eq!(ctx.attributed_fill_qty, 3);
    }

    /// F6: out-of-domain fill arrives first and must not steal response-domain slots.
    #[test]
    fn f6_response_domain_identity_not_arrival_order() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        // Response domain: fill_count=2, boundary ts<=10, avg/fee for snapshot only.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-f6"),
                fill_count: 2,
                remaining_count: 8,
                avg_price_cents: Some(45),
                fee_cents: Some(2),
                snapshot_boundary: Some(SnapshotBoundary::TsNs(10)),
            },
        );
        let s = o.new_state().unwrap().clone();
        // Out-of-domain fill arrives first (ts=20) — must not occupy domain.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-out"),
                qty: 1,
                price_cents: 10,
                ts_ns: 20,
                venue_order_id: Some(vid("V-f6")),
                fee_cents: Some(99),
            },
        );
        let s = o.new_state().unwrap().clone();
        assert_eq!(
            ctx.response_domain_qty, 0,
            "out-of-domain must not accumulate"
        );
        assert_eq!(ctx.attributed_fill_qty, 1);
        assert!(!o.is_halt());
        // Snapshot fills arrive later (ts within boundary).
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ImmediateFillBackfillResult {
                fills: vec![
                    fill_rec_vf("F1", 1, 45, 5, "V-f6", 1),
                    fill_rec_vf("F2", 1, 45, 6, "V-f6", 1),
                ],
            },
        );
        // Domain membership by identity: only F1+F2 (qty 2), not F-out.
        assert_eq!(ctx.response_domain_qty, 2);
        assert_eq!(ctx.response_domain_fee_cents, 2);
        assert_eq!(ctx.response_domain_notional_cents, 90);
        // attributed=3 total; B2 exact qty path may Halt on attributed!=response_fill_count —
        // the critical F6 assertion is domain isolation above, not terminal routing.
        assert_eq!(ctx.attributed_fill_qty, 3);
        let _ = o;
    }

    /// F7: rebuild_ctx_from_journal equals event-apply ctx (durable restart).
    #[test]
    fn f7_journal_rebuild_matches_event_apply_ctx() {
        let base = ctx_default();
        let events = vec![
            OrderEvent::PrepareSubmit,
            OrderEvent::StartSubmit {
                attempt_id: AttemptId("a1".into()),
            },
            OrderEvent::SubmitResponse {
                venue_order_id: vid("V-j7"),
                fill_count: 4,
                remaining_count: 6,
                avg_price_cents: Some(45),
                fee_cents: Some(4),
                snapshot_boundary: Some(SnapshotBoundary::TsNs(100)),
            },
            OrderEvent::ImmediateFillBackfillResult {
                fills: vec![
                    fill_rec_vf("F1", 2, 45, 1, "V-j7", 2),
                    fill_rec_vf("F2", 2, 45, 2, "V-j7", 2),
                ],
            },
        ];
        let applied = replay(OrderState::New, base.clone(), &events);
        assert_eq!(applied.halt, None);
        assert_eq!(applied.reject, None);

        // Collect durable journal records from effects.
        let mut journals = Vec::new();
        for e in &applied.effects {
            if let Effect::AppendFsync(jr) = e {
                journals.push(jr.clone());
            }
        }
        assert!(
            journals
                .iter()
                .any(|j| matches!(j, JournalRecord::Fill { .. })),
            "expected Fill journals"
        );
        let rebuilt = rebuild_ctx_from_journal(base, &journals).expect("rebuild ok");
        assert_eq!(rebuilt.venue_order_id, applied.ctx.venue_order_id);
        assert_eq!(rebuilt.attributed_fill_qty, applied.ctx.attributed_fill_qty);
        assert_eq!(rebuilt.fill_obligation, applied.ctx.fill_obligation);
        assert_eq!(
            rebuilt.attributed_fee_cents,
            applied.ctx.attributed_fee_cents
        );
        assert_eq!(
            rebuilt.attributed_notional_cents,
            applied.ctx.attributed_notional_cents
        );
        assert_eq!(rebuilt.applied_fills, applied.ctx.applied_fills);
        assert_eq!(rebuilt.response_fill_count, applied.ctx.response_fill_count);
        assert_eq!(
            rebuilt.response_avg_price_cents,
            applied.ctx.response_avg_price_cents
        );
        assert_eq!(rebuilt.response_fee_cents, applied.ctx.response_fee_cents);
        assert_eq!(
            rebuilt.response_snapshot_boundary,
            applied.ctx.response_snapshot_boundary
        );
        // Authority not latched for partial; both false.
        assert_eq!(rebuilt.authority_complete, applied.ctx.authority_complete);
        assert_eq!(rebuilt.authority_epoch, applied.ctx.authority_epoch);
    }

    /// F7: OrderTerminal journal carries authority/obligation/venue snapshot.
    #[test]
    fn f7_order_terminal_journal_enriched() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-term"),
                fill_count: 10,
                remaining_count: 0,
                avg_price_cents: Some(45),
                fee_cents: Some(5),
                snapshot_boundary: Some(SnapshotBoundary::TsNs(100)),
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ImmediateFillBackfillResult {
                fills: vec![fill_rec_vf("Fall", 10, 45, 1, "V-term", 5)],
            },
        );
        assert_eq!(o.new_state(), Some(&OrderState::Filled));
        assert!(o.effects().iter().any(|e| matches!(
            e,
            Effect::AppendFsync(JournalRecord::OrderTerminal {
                kind: TerminalKind::Filled,
                venue_order_id: Some(v),
                fill_obligation: 10,
                authority_complete: true,
                attributed_fill_qty: 10,
                attributed_fee_cents: 5,
                ..
            }) if v == &vid("V-term")
        )));
    }

    /// F8 structural: ReleaseReservation only appears once in a successful terminal path,
    /// and never without ctx.authority_complete.
    #[test]
    fn f8_single_release_requires_authority_latch() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::Canceled),
        );
        assert_eq!(
            o.effects()
                .iter()
                .filter(|e| matches!(e, Effect::ReleaseReservation))
                .count(),
            0
        );
        assert!(!ctx.authority_complete);
        let s = o.new_state().unwrap().clone();
        let o = apply_event(&s, &mut ctx, &authority_reconcile_canceled(0, 10));
        assert!(ctx.authority_complete);
        assert_eq!(
            o.effects()
                .iter()
                .filter(|e| matches!(e, Effect::ReleaseReservation))
                .count(),
            1
        );
        assert_eq!(o.new_state(), Some(&OrderState::Canceled));
    }

    // ── G1–G6 rework-5 coverage ───────────────────────────────────────────

    /// G1: live authority latch is cleared by CancelRequested (stale post-snapshot fills).
    #[test]
    fn g1_cancel_requested_invalidates_live_authority() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let coid = ctx.client_order_id.clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![BackfillOrderRecord {
                    client_order_id: coid,
                    venue_order_id: vid("V-g1a"),
                    status: BackfillOrderStatus::Open,
                    filled_qty: 0,
                    remaining_qty: 10,
                    fills: vec![],
                }],
            },
        );
        assert_eq!(
            o.new_state(),
            Some(&OrderState::Accepted {
                venue_order_id: vid("V-g1a"),
            })
        );
        assert!(
            ctx.authority_is_fresh(),
            "exhaustive open latches authority"
        );
        let epoch_before = ctx.authority_epoch;
        let s = o.new_state().unwrap().clone();
        let o = apply_event(&s, &mut ctx, &OrderEvent::CancelRequested);
        assert!(matches!(
            o.new_state(),
            Some(OrderState::CancelPending { .. })
        ));
        assert!(
            !ctx.authority_is_fresh(),
            "G1: CancelRequested clears latch"
        );
        assert!(ctx.authority_epoch > epoch_before);
        assert!(o.effects().iter().any(|e| matches!(
            e,
            Effect::AppendFsync(JournalRecord::AuthorityInvalidated { .. })
        )));
        // CancelOutcome alone must not release with stale/cleared authority.
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::Canceled),
        );
        assert_ne!(o.new_state(), Some(&OrderState::Canceled));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::RequestAuthorityReconcile { .. }))
        );
    }

    /// G1: new fill_id clears a fresh authority latch (must re-prove before release).
    #[test]
    fn g1_new_fill_and_obligation_raise_clear_authority() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-g1c"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("Fa2"),
                qty: 2,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V-g1c")),
                fee_cents: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(&s, &mut ctx, &OrderEvent::CancelRequested);
        let s = o.new_state().unwrap().clone();
        // Authority complete but venue_filled lag (4 > attributed 2) → ReconcilePending + latch.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V-g1c")),
                filled_qty: 4,
                remaining_qty: 6,
                fills: vec![],
                authority_complete: true,
            },
        );
        assert!(matches!(
            o.new_state(),
            Some(OrderState::ReconcilePending { .. })
        ));
        assert!(
            ctx.authority_is_fresh(),
            "shell latched post-cancel authority"
        );
        assert_eq!(ctx.fill_obligation, 4);
        let s = o.new_state().unwrap().clone();
        // New fill_id → G1 clear authority even though it helps attribution.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("Fb2"),
                qty: 2,
                price_cents: 45,
                ts_ns: 2,
                venue_order_id: Some(vid("V-g1c")),
                fee_cents: None,
            },
        );
        assert_eq!(ctx.attributed_fill_qty, 4);
        assert!(
            !ctx.authority_is_fresh(),
            "G1: new fill_id clears authority"
        );
        assert_ne!(o.new_state(), Some(&OrderState::Canceled));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        // Obligation raise with lag: re-latch then raise filled=6.
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V-g1c")),
                filled_qty: 6,
                remaining_qty: 4,
                fills: vec![],
                authority_complete: true,
            },
        );
        assert_eq!(ctx.fill_obligation, 6);
        assert_eq!(ctx.attributed_fill_qty, 4);
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert!(matches!(
            o.new_state(),
            Some(OrderState::ReconcilePending { .. })
        ));
    }

    /// G2: None-provenance fill cannot participate in terminal release after venue bind.
    #[test]
    fn g2_none_provenance_blocks_release() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        // Full qty fill before bind (None venue).
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("Fn"),
                qty: 10,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: None,
                fee_cents: Some(1),
            },
        );
        let s = o.new_state().unwrap().clone();
        assert_eq!(ctx.attributed_fill_qty, 10);
        assert!(
            ctx.applied_fills
                .get(&fid("Fn"))
                .unwrap()
                .venue_order_id
                .is_none()
        );
        // Response binds parent and claims full fill.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-g2"),
                fill_count: 10,
                remaining_count: 0,
                avg_price_cents: Some(45),
                fee_cents: Some(1),
                snapshot_boundary: Some(SnapshotBoundary::TsNs(100)),
            },
        );
        // G2: must not release with None provenance after bind.
        assert_ne!(o.new_state(), Some(&OrderState::Filled));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        // Domain may resolve unattributed → route_live_or_filled tries finalize → G2 blocks.
        if let Some(st) = o.new_state() {
            assert!(
                matches!(
                    st,
                    OrderState::ReconcilePending { .. }
                        | OrderState::ImmediateFillUnattributed { .. }
                        | OrderState::Partial { .. }
                        | OrderState::Accepted { .. }
                ),
                "unexpected state: {st:?}"
            );
        }
        let _ = o;
    }

    /// G3: venue_remaining persisted on target; enrichment must not synthesize 0 to release.
    #[test]
    fn g3_venue_remaining_persisted_blocks_enrichment_release() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let coid = ctx.client_order_id.clone();
        // Venue says Filled but remaining=1 (F3/G3 evidence).
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![BackfillOrderRecord {
                    client_order_id: coid,
                    venue_order_id: vid("V-g3"),
                    status: BackfillOrderStatus::Filled,
                    filled_qty: 10,
                    remaining_qty: 1,
                    fills: vec![fill_rec_v("Fg3", 10, 45, 1, "V-g3")],
                }],
            },
        );
        assert_ne!(o.new_state(), Some(&OrderState::Filled));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        let s = o.new_state().unwrap().clone();
        assert!(matches!(
            s,
            OrderState::ReconcilePending {
                target: ReconcileTarget {
                    venue_remaining_qty: Some(1),
                    ..
                },
                ..
            }
        ));
        // Fee enrichment None→Some on same fill_id — must not release via synthetic remaining=0.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("Fg3"),
                qty: 10,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V-g3")),
                fee_cents: Some(3),
            },
        );
        assert!(!o.is_halt(), "G5: enrichment not Halt: {o:?}");
        assert_ne!(o.new_state(), Some(&OrderState::Filled));
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert_eq!(ctx.attributed_fee_cents, 3);
        // Target remaining still blocked.
        if let Some(OrderState::ReconcilePending { target, .. }) = o.new_state() {
            assert_eq!(target.venue_remaining_qty, Some(1));
        }
    }

    /// G4: journal fold with non-terminal reconcile + fill + terminal equals event-apply ctx.
    #[test]
    fn g4_journal_fold_equiv_nonterminal_reconcile_and_terminal() {
        let base = ctx_default();
        let events = vec![
            OrderEvent::PrepareSubmit,
            OrderEvent::StartSubmit {
                attempt_id: AttemptId("a1".into()),
            },
            OrderEvent::SubmitTimeout,
            OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![BackfillOrderRecord {
                    client_order_id: base.client_order_id.clone(),
                    venue_order_id: vid("V-g4"),
                    status: BackfillOrderStatus::Canceled,
                    filled_qty: 0,
                    remaining_qty: 10,
                    fills: vec![],
                }],
            },
        ];
        let applied = replay(OrderState::New, base.clone(), &events);
        assert_eq!(applied.halt, None);
        assert_eq!(applied.state, OrderState::Canceled);
        let mut journals = Vec::new();
        for e in &applied.effects {
            if let Effect::AppendFsync(jr) = e {
                journals.push(jr.clone());
            }
        }
        assert!(
            journals
                .iter()
                .any(|j| matches!(j, JournalRecord::VenueBound { .. }))
                || journals
                    .iter()
                    .any(|j| matches!(j, JournalRecord::ReconcileObserved { .. })),
            "expected venue/reconcile journals: {journals:?}"
        );
        assert!(journals.iter().any(|j| matches!(
            j,
            JournalRecord::OrderTerminal {
                attributed_fill_qty: 0,
                ..
            }
        )));
        let rebuilt = rebuild_ctx_from_journal(base, &journals).expect("rebuild");
        assert_eq!(rebuilt.venue_order_id, applied.ctx.venue_order_id);
        assert_eq!(rebuilt.fill_obligation, applied.ctx.fill_obligation);
        assert_eq!(
            rebuilt.authority_is_fresh(),
            applied.ctx.authority_is_fresh()
        );
        assert_eq!(rebuilt.attributed_fill_qty, applied.ctx.attributed_fill_qty);
        assert_eq!(
            rebuilt.attributed_fee_cents,
            applied.ctx.attributed_fee_cents
        );
        assert_eq!(rebuilt.authority_epoch, applied.ctx.authority_epoch);
    }

    /// G5: frozen terminal fee enrichment is not PostTerminalFill / ReserveFull.
    #[test]
    fn g5_frozen_enrichment_no_halt_vs_new_fill_halt() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-g5b"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("Fe"),
                qty: 10,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V-g5b")),
                fee_cents: None,
            },
        );
        assert_eq!(o.new_state(), Some(&OrderState::Filled));
        let s = OrderState::Filled;
        // Fee enrichment on frozen Filled.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("Fe"),
                qty: 10,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V-g5b")),
                fee_cents: Some(7),
            },
        );
        assert!(!o.is_halt(), "G5 enrichment must not Halt: {o:?}");
        assert_eq!(o.new_state(), Some(&OrderState::Filled));
        assert!(!o.effects().iter().any(|e| matches!(e, Effect::ReserveFull)));
        assert_eq!(ctx.attributed_fee_cents, 7);
        // New fill_id on frozen → PostTerminalFill + ReserveFull + Halt.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-late"),
                qty: 1,
                price_cents: 45,
                ts_ns: 2,
                venue_order_id: Some(vid("V-g5b")),
                fee_cents: None,
            },
        );
        // Overfill of order qty=10 with +1 → OverFill Halt (still not silent).
        assert!(o.is_halt());
        assert!(o.effects().iter().any(|e| matches!(e, Effect::ReserveFull)));
    }

    /// G6: domain qty (not global attributed) drives B2 response snapshot check.
    #[test]
    fn g6_b2_domain_qty_not_global_attributed() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-g6"),
                fill_count: 2,
                remaining_count: 8,
                avg_price_cents: Some(45),
                fee_cents: Some(2),
                snapshot_boundary: Some(SnapshotBoundary::TsNs(10)),
            },
        );
        let s = o.new_state().unwrap().clone();
        // Out-of-domain fill first.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-out"),
                qty: 1,
                price_cents: 10,
                ts_ns: 20,
                venue_order_id: Some(vid("V-g6")),
                fee_cents: Some(99),
            },
        );
        let s = o.new_state().unwrap().clone();
        assert_eq!(ctx.attributed_fill_qty, 1);
        assert_eq!(ctx.response_domain_qty, 0);
        // Domain fills close the snapshot (qty 2) while global attributed becomes 3.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ImmediateFillBackfillResult {
                fills: vec![
                    fill_rec_vf("F1", 1, 45, 5, "V-g6", 1),
                    fill_rec_vf("F2", 1, 45, 6, "V-g6", 1),
                ],
            },
        );
        assert_eq!(ctx.response_domain_qty, 2);
        assert_eq!(ctx.attributed_fill_qty, 3);
        // G6: must NOT ImmediateFillCrossCheckMismatch on global 3≠2.
        assert!(
            !matches!(
                o,
                TransitionOutcome::Halt {
                    reason: HaltReason::ImmediateFillCrossCheckMismatch { .. },
                    ..
                }
            ),
            "G6 false Halt: {o:?}"
        );
        // Routes live Partial (remaining 7).
        assert!(matches!(
            o.new_state(),
            Some(OrderState::Partial {
                filled_qty: 3,
                remaining_qty: 7,
                ..
            })
        ));
    }

    // ── Rework-6: H1/M1/L1/M2/L2 + G2/G5 supplements ─────────────────────

    /// H1: reliable boundary + domain unclosed (domain 0, out-of-boundary fills cover
    /// global obligation) + cancel authority → not_ready, never ReleaseReservation.
    #[test]
    fn h1_domain_unclosed_blocks_release_despite_global_obligation() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        // Response claims domain fill_count=2 with TsNs boundary.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-h1"),
                fill_count: 2,
                remaining_count: 8,
                avg_price_cents: Some(45),
                fee_cents: Some(2),
                snapshot_boundary: Some(SnapshotBoundary::TsNs(10)),
            },
        );
        let s = o.new_state().unwrap().clone();
        // Only out-of-domain fill (ts > boundary) covering full obligation qty=2.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-out"),
                qty: 2,
                price_cents: 45,
                ts_ns: 20,
                venue_order_id: Some(vid("V-h1")),
                fee_cents: Some(2),
            },
        );
        let s = o.new_state().unwrap().clone();
        assert_eq!(ctx.attributed_fill_qty, 2);
        assert_eq!(ctx.fill_obligation, 2);
        assert_eq!(ctx.response_domain_qty, 0, "H1 setup: domain unclosed");
        // Cancel + authority-complete reconcile must NOT release (domain still open).
        let o = apply_event(&s, &mut ctx, &OrderEvent::CancelRequested);
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::Canceled),
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V-h1")),
                filled_qty: 2,
                remaining_qty: 8,
                fills: vec![],
                authority_complete: true,
            },
        );
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation)),
            "H1: must not release with unclosed domain: {o:?}"
        );
        assert_ne!(o.new_state(), Some(&OrderState::Canceled));
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::BackfillFills { .. }))
                || o.effects()
                    .iter()
                    .any(|e| matches!(e, Effect::RequestAuthorityReconcile { .. })),
            "H1: not_ready should drive backfill/authority: {o:?}"
        );
        assert!(matches!(
            o.new_state(),
            Some(OrderState::ReconcilePending { .. })
        ));
    }

    /// M1/A1: in-domain fill qty exceeding response_fill_count → Halt (no min-truncate).
    /// Real fill is durable first: Halt effects carry Fill/AccountFill/Authority/Obligation;
    /// fold rebuild equals memory field-by-field (emit-before-Halt).
    #[test]
    fn m1_domain_overfill_halts_no_truncate() {
        let base = ctx_default();
        let mut ctx = base.clone();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-m1"),
                fill_count: 2,
                remaining_count: 8,
                avg_price_cents: Some(45),
                fee_cents: Some(2),
                snapshot_boundary: Some(SnapshotBoundary::TsNs(100)),
            },
        );
        let mut journals: Vec<JournalRecord> = vec![
            JournalRecord::SubmitPrepared {
                client_order_id: base.client_order_id.clone(),
            },
            JournalRecord::SubmitStarted {
                attempt_id: AttemptId("a1".into()),
            },
        ];
        for e in o.effects() {
            if let Effect::AppendFsync(jr) = e {
                journals.push(jr.clone());
            }
        }
        let s = o.new_state().unwrap().clone();
        // Single in-domain fill qty=3 > response_fill_count=2.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-over"),
                qty: 3,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V-m1")),
                fee_cents: Some(3),
            },
        );
        assert!(o.is_halt(), "M1: domain overfill must Halt: {o:?}");
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::CrossCheckMismatch { .. },
                ..
            }
        ));
        // Domain accumulated real qty 3 (not truncated to 2).
        assert_eq!(ctx.response_domain_qty, 3);
        assert_eq!(ctx.attributed_fill_qty, 3);
        assert_eq!(ctx.fill_obligation, 3);

        // A1: Halt effects must carry the real fill's durable records (not empty / discarded).
        let eff = o.effects();
        assert!(
            eff.iter().any(|e| matches!(
                e,
                Effect::AppendFsync(JournalRecord::Fill {
                    fill_id,
                    qty: 3,
                    ..
                }) if fill_id == &fid("F-over")
            )),
            "A1: Halt must carry JournalRecord::Fill: {eff:?}"
        );
        assert!(
            eff.iter().any(|e| matches!(
                e,
                Effect::AccountFill {
                    fill_id,
                    qty: 3,
                    ..
                } if fill_id == &fid("F-over")
            )),
            "A1: Halt must carry AccountFill: {eff:?}"
        );
        assert!(
            eff.iter().any(|e| matches!(
                e,
                Effect::AppendFsync(JournalRecord::AuthorityInvalidated { .. })
            )),
            "A1: Halt must carry AuthorityInvalidated: {eff:?}"
        );
        assert!(
            eff.iter().any(|e| matches!(
                e,
                Effect::AppendFsync(JournalRecord::ObligationRaised {
                    fill_obligation: 3,
                    ..
                })
            )),
            "A1: Halt must carry ObligationRaised: {eff:?}"
        );

        for e in eff {
            if let Effect::AppendFsync(jr) = e {
                journals.push(jr.clone());
            }
        }
        // B2: full field-by-field fold ≡ memory (not a partial subset).
        assert_ctx_fold_equiv(&ctx, &journals);
    }

    /// A1 dedicated: response_fill_count=2 + TsNs boundary + in-domain qty=3 →
    /// Halt + durable fill records + fold ≡ memory.
    #[test]
    fn a1_domain_overfill_halt_carries_fill_records_fold_equiv() {
        // Covered by m1_domain_overfill_halts_no_truncate records+fold assertions.
        // Keep a thin alias-style path with the exact brief fixture labels.
        let base = ctx_default();
        let mut ctx = base.clone();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-a1"),
                fill_count: 2,
                remaining_count: 8,
                avg_price_cents: Some(45),
                fee_cents: Some(2),
                snapshot_boundary: Some(SnapshotBoundary::TsNs(100)),
            },
        );
        let mut journals: Vec<JournalRecord> = vec![
            JournalRecord::SubmitPrepared {
                client_order_id: base.client_order_id.clone(),
            },
            JournalRecord::SubmitStarted {
                attempt_id: AttemptId("a1".into()),
            },
        ];
        for e in o.effects() {
            if let Effect::AppendFsync(jr) = e {
                journals.push(jr.clone());
            }
        }
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-a1"),
                qty: 3,
                price_cents: 45,
                ts_ns: 50,
                venue_order_id: Some(vid("V-a1")),
                fee_cents: Some(3),
            },
        );
        assert!(o.is_halt());
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::CrossCheckMismatch { .. },
                ..
            }
        ));
        let eff = o.effects();
        assert!(
            eff.iter()
                .any(|e| matches!(e, Effect::AppendFsync(JournalRecord::Fill { .. })))
        );
        assert!(eff.iter().any(|e| matches!(e, Effect::AccountFill { .. })));
        assert!(eff.iter().any(|e| {
            matches!(
                e,
                Effect::AppendFsync(JournalRecord::AuthorityInvalidated { .. })
            )
        }));
        assert!(eff.iter().any(|e| {
            matches!(
                e,
                Effect::AppendFsync(JournalRecord::ObligationRaised { .. })
            )
        }));
        for e in eff {
            if let Effect::AppendFsync(jr) = e {
                journals.push(jr.clone());
            }
        }
        // B2: full field-by-field fold ≡ memory.
        assert_ctx_fold_equiv(&ctx, &journals);
    }

    /// L1: Seq boundary is not reliable → obligation-only path; full backfill reaches
    /// terminal without false domain Halt / permanent stuck.
    #[test]
    fn l1_seq_boundary_degrades_to_obligation_only() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-l1"),
                fill_count: 10,
                remaining_count: 0,
                avg_price_cents: Some(45),
                fee_cents: Some(5),
                snapshot_boundary: Some(SnapshotBoundary::Seq(99)),
            },
        );
        let s = o.new_state().unwrap().clone();
        assert!(
            !has_reliable_boundary(&ctx),
            "L1: Seq must not claim reliable boundary"
        );
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ImmediateFillBackfillResult {
                fills: vec![fill_rec_vf("Fall", 10, 45, 1, "V-l1", 5)],
            },
        );
        assert_eq!(
            o.new_state(),
            Some(&OrderState::Filled),
            "L1: obligation-only full fill should terminal: {o:?}"
        );
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        assert_eq!(ctx.response_domain_qty, 0, "Seq domain must not accumulate");
        assert!(!o.is_halt());
    }

    /// M2: zero-fill response then first live fill that also fills order completely —
    /// memory epoch/latched_epoch equals fold rebuild (no double-bump).
    /// Does **not** pre-raise obligation before the fill (that fixture hid the bug).
    #[test]
    fn m2_zero_fill_then_full_live_fill_epoch_fold_equiv() {
        let base = ctx_default();
        let mut ctx = base.clone();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-m2"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        assert_eq!(ctx.fill_obligation, 0);
        assert_eq!(ctx.authority_epoch, 0);
        // First live fill raises obligation and fills the order.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-m2"),
                qty: 10,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V-m2")),
                fee_cents: Some(1),
            },
        );
        assert_eq!(o.new_state(), Some(&OrderState::Filled));
        assert_eq!(ctx.authority_epoch, 1);
        assert_eq!(ctx.authority_latched_epoch, 1);
        assert!(ctx.authority_is_fresh());

        // Collect durable journals (including Prepare/Start from prepare_started path).
        let mut journals = vec![
            JournalRecord::SubmitPrepared {
                client_order_id: base.client_order_id.clone(),
            },
            JournalRecord::SubmitStarted {
                attempt_id: AttemptId("a1".into()),
            },
        ];
        // Replay events to collect all AppendFsync from response+fill terminal path.
        let applied = replay(
            OrderState::New,
            base.clone(),
            &[
                OrderEvent::PrepareSubmit,
                OrderEvent::StartSubmit {
                    attempt_id: AttemptId("a1".into()),
                },
                OrderEvent::SubmitResponse {
                    venue_order_id: vid("V-m2"),
                    fill_count: 0,
                    remaining_count: 10,
                    avg_price_cents: None,
                    fee_cents: None,
                    snapshot_boundary: None,
                },
                OrderEvent::Fill {
                    fill_id: fid("F-m2"),
                    qty: 10,
                    price_cents: 45,
                    ts_ns: 1,
                    venue_order_id: Some(vid("V-m2")),
                    fee_cents: Some(1),
                },
            ],
        );
        assert_eq!(applied.halt, None);
        assert_eq!(applied.state, OrderState::Filled);
        journals.clear();
        for e in &applied.effects {
            if let Effect::AppendFsync(jr) = e {
                journals.push(jr.clone());
            }
        }
        // Must have raised obligation on the fill (not pre-raised on response).
        assert!(
            journals.iter().any(|j| matches!(
                j,
                JournalRecord::ObligationRaised {
                    fill_obligation: 10,
                    ..
                }
            )),
            "M2: fill must emit ObligationRaised: {journals:?}"
        );
        assert!(
            journals
                .iter()
                .any(|j| matches!(j, JournalRecord::AuthorityInvalidated { epoch: 1 })),
            "M2: fill must emit AuthorityInvalidated for fold restore: {journals:?}"
        );

        let rebuilt = rebuild_ctx_from_journal(base, &journals).expect("fold ok");
        assert_eq!(
            rebuilt.authority_epoch, applied.ctx.authority_epoch,
            "M2 epoch memory vs fold"
        );
        assert_eq!(
            rebuilt.authority_latched_epoch, applied.ctx.authority_latched_epoch,
            "M2 latched_epoch memory vs fold"
        );
        assert_eq!(rebuilt.authority_complete, applied.ctx.authority_complete);
        assert_eq!(rebuilt.fill_obligation, applied.ctx.fill_obligation);
        assert_eq!(rebuilt.attributed_fill_qty, applied.ctx.attributed_fill_qty);
        assert_eq!(
            rebuilt.authority_is_fresh(),
            applied.ctx.authority_is_fresh(),
            "M2 authority_is_fresh must match after fold"
        );
    }

    /// L2: exhaustive unique candidate with remaining=6 but fill missing venue id → Halt;
    /// fold rebuilds last_venue_remaining_qty == Some(6) == memory (Halt path durable).
    #[test]
    fn l2_halt_path_venue_remaining_fold_equiv() {
        let base = ctx_default();
        let mut ctx = base.clone();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let coid = ctx.client_order_id.clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![BackfillOrderRecord {
                    client_order_id: coid,
                    venue_order_id: vid("V-l2"),
                    status: BackfillOrderStatus::Open,
                    filled_qty: 4,
                    remaining_qty: 6,
                    fills: vec![FillRecord {
                        fill_id: fid("F-nov"),
                        qty: 4,
                        price_cents: 45,
                        ts_ns: 1,
                        venue_order_id: None, // missing provenance → Halt
                        fee_cents: None,
                    }],
                }],
            },
        );
        assert!(o.is_halt(), "L2 setup must Halt on missing venue: {o:?}");
        assert_eq!(
            ctx.last_venue_remaining_qty,
            Some(6),
            "L2 memory must have remaining before Halt"
        );

        let mut journals = Vec::new();
        for e in o.effects() {
            if let Effect::AppendFsync(jr) = e {
                journals.push(jr.clone());
            }
        }
        // Also need prepare/start journals for identity (remaining is on ReconcileObserved).
        let mut full = vec![
            JournalRecord::SubmitPrepared {
                client_order_id: base.client_order_id.clone(),
            },
            JournalRecord::SubmitStarted {
                attempt_id: AttemptId("a1".into()),
            },
            JournalRecord::SubmitUnknown,
        ];
        full.extend(journals);
        assert!(
            full.iter().any(|j| matches!(
                j,
                JournalRecord::ReconcileObserved {
                    venue_remaining_qty: 6,
                    ..
                }
            )),
            "L2: ReconcileObserved must precede Halt-prone fills: {full:?}"
        );

        let rebuilt = rebuild_ctx_from_journal(base, &full).expect("fold ok");
        assert_eq!(
            rebuilt.last_venue_remaining_qty,
            Some(6),
            "L2 fold remaining must equal memory Some(6)"
        );
        assert_eq!(
            rebuilt.last_venue_remaining_qty,
            ctx.last_venue_remaining_qty
        );
        // L2 fold field-level parity for other remaining-adjacent state.
        assert_eq!(rebuilt.venue_order_id, ctx.venue_order_id);
        assert_eq!(rebuilt.fill_obligation, ctx.fill_obligation);
        assert_eq!(rebuilt.authority_epoch, ctx.authority_epoch);
    }

    /// A2: reconcile filled=12/remaining=0 (order qty=10) → ObligationExceedsOrderQty Halt
    /// carries ReconcileObserved; fold last_venue_remaining_qty == Some(0) == memory.
    #[test]
    fn a2_obligation_exceed_halt_carries_reconcile_observed_fold_equiv() {
        let base = ctx_default();
        let mut ctx = base.clone();
        let s = prepare_started(&mut ctx);
        let o = apply_event(&s, &mut ctx, &OrderEvent::SubmitTimeout);
        let s = o.new_state().unwrap().clone();
        let coid = ctx.client_order_id.clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::UnknownBackfillResult {
                exhaustive: true,
                matched: vec![BackfillOrderRecord {
                    client_order_id: coid,
                    venue_order_id: vid("V-a2"),
                    status: BackfillOrderStatus::Filled,
                    filled_qty: 12, // > qty=10
                    remaining_qty: 0,
                    fills: vec![],
                }],
            },
        );
        assert!(o.is_halt(), "A2: obligation exceed must Halt: {o:?}");
        assert!(matches!(
            o,
            TransitionOutcome::Halt {
                reason: HaltReason::ObligationExceedsOrderQty { .. },
                ..
            }
        ));
        assert_eq!(
            ctx.last_venue_remaining_qty,
            Some(0),
            "A2 memory remaining after note_venue_remaining"
        );

        let mut journals = Vec::new();
        for e in o.effects() {
            if let Effect::AppendFsync(jr) = e {
                journals.push(jr.clone());
            }
        }
        assert!(
            journals.iter().any(|j| matches!(
                j,
                JournalRecord::ReconcileObserved {
                    venue_remaining_qty: 0,
                    venue_filled_qty: 12,
                    ..
                }
            )),
            "A2: Halt effects must include ReconcileObserved: {journals:?}"
        );

        let mut full = vec![
            JournalRecord::SubmitPrepared {
                client_order_id: base.client_order_id.clone(),
            },
            JournalRecord::SubmitStarted {
                attempt_id: AttemptId("a1".into()),
            },
            JournalRecord::SubmitUnknown,
        ];
        full.extend(journals);
        // B2/A2: full field-by-field fold ≡ memory (venue/authority/domain/fee/payloads/
        // remaining/obligation) — same helper as F4/M1/A1, not a partial subset.
        assert_ctx_fold_equiv(&ctx, &full);
        assert_eq!(
            ctx.last_venue_remaining_qty,
            Some(0),
            "A2 fold-equiv remaining still Some(0)"
        );
    }

    /// G2 supplement: None→Some(bound) provenance upgrade eventually allows release.
    #[test]
    fn g2_provenance_upgrade_eventually_releases() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        // Full qty fill before bind (None venue).
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("Fn2"),
                qty: 10,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: None,
                fee_cents: Some(1),
            },
        );
        let s = o.new_state().unwrap().clone();
        // Response binds parent (no snapshot boundary → obligation-only finalize path).
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-g2b"),
                fill_count: 10,
                remaining_count: 0,
                avg_price_cents: Some(45),
                fee_cents: Some(1),
                snapshot_boundary: None,
            },
        );
        // G2 blocks release while provenance is None.
        assert!(
            !o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        let s = o.new_state().unwrap().clone();
        // Upgrade None→Some(bound) on same fill_id.
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("Fn2"),
                qty: 10,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V-g2b")),
                fee_cents: Some(1),
            },
        );
        assert!(!o.is_halt(), "G2 upgrade must not Halt: {o:?}");
        assert_eq!(
            ctx.applied_fills
                .get(&fid("Fn2"))
                .and_then(|p| p.venue_order_id.clone()),
            Some(vid("V-g2b"))
        );
        // After provenance unlock + authority, should release (may need reconcile if
        // intermediate state was ReconcilePending without latch).
        if o.effects()
            .iter()
            .any(|e| matches!(e, Effect::ReleaseReservation))
        {
            assert_eq!(o.new_state(), Some(&OrderState::Filled));
            return;
        }
        // Drive authority reconcile if still pending.
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Filled,
                venue_order_id: Some(vid("V-g2b")),
                filled_qty: 10,
                remaining_qty: 0,
                fills: vec![],
                authority_complete: true,
            },
        );
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation)),
            "G2: after provenance upgrade must eventually release: {o:?}"
        );
        assert_eq!(o.new_state(), Some(&OrderState::Filled));
    }

    /// G5 supplement: true new-qty fill on frozen terminal (not OverFill of order)
    /// goes Applied then PostTerminalFill Halt — fixture must not mask as OverFill.
    #[test]
    fn g5_new_qty_fill_on_partial_terminal_post_terminal_halt() {
        // Order qty=10; cancel after partial fill qty=4 so remaining room exists;
        // late new fill_id with qty=1 on frozen Canceled → PostTerminalFill (not OverFill).
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V-g5c"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-part"),
                qty: 4,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V-g5c")),
                fee_cents: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        assert_eq!(ctx.attributed_fill_qty, 4);
        // Cancel + authority → Canceled terminal (partial fill).
        let o = apply_event(&s, &mut ctx, &OrderEvent::CancelRequested);
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::Canceled),
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V-g5c")),
                filled_qty: 4,
                remaining_qty: 6,
                fills: vec![],
                authority_complete: true,
            },
        );
        assert_eq!(o.new_state(), Some(&OrderState::Canceled));
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation))
        );
        // New fill_id with room under order qty → Applied then PostTerminalFill.
        let s = OrderState::Canceled;
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F-late-new"),
                qty: 1,
                price_cents: 45,
                ts_ns: 2,
                venue_order_id: Some(vid("V-g5c")),
                fee_cents: None,
            },
        );
        assert!(o.is_halt());
        assert!(
            matches!(
                o,
                TransitionOutcome::Halt {
                    reason: HaltReason::PostTerminalFill,
                    ..
                }
            ),
            "G5: true new qty must be PostTerminalFill not OverFill: {o:?}"
        );
        assert!(o.effects().iter().any(|e| matches!(e, Effect::ReserveFull)));
        assert_eq!(
            ctx.attributed_fill_qty, 5,
            "late fill was applied before Halt"
        );
    }
    // ─── 2026-08-15 四仓扫雷修复夹具(A1-A5) ───────────────────────────────

    /// ★ A1(HIGH):IOC 全 miss —— response(fill=0, remaining=0) 是合法终局
    /// (零成交、非驻留),必须 Canceled+Release,不许 CrossCheckMismatch Halt
    /// 杀 live loop(修前行为)。
    #[test]
    fn a1_ioc_full_miss_zero_zero_is_canceled_release() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V1"),
                fill_count: 0,
                remaining_count: 0,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        assert_eq!(
            o.new_state(),
            Some(&OrderState::Canceled),
            "IOC 全 miss 必须落 Canceled 终态(修前 Halt 杀 loop): {o:?}"
        );
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation)),
            "零成交终局必须放资金"
        );
    }

    /// ★ A5(LOW/D4):WS fill 抢在零成交 response 前 —— response 是建单快照,
    /// remaining==qty 时按 Partial 收(修前 ResponseZeroButLocalFills 假 halt);
    /// 对照臂:response(0,0) 且有本地 fill = 真矛盾,保持 halt。
    #[test]
    fn a5_ws_fill_before_zero_fill_response_is_partial_not_halt() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 4,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V1")),
                fee_cents: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V1"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        assert!(
            matches!(
                o.new_state(),
                Some(OrderState::Partial { filled_qty: 4, remaining_qty: 6, .. })
            ),
            "抢跑 fill + 全量驻留 response = Partial 非矛盾: {o:?}"
        );
        // 对照:response(0,0) 但本地有 fill = 真矛盾 ⇒ halt 保持。
        let mut ctx2 = ctx_default();
        let s2 = prepare_started(&mut ctx2);
        let o2 = apply_event(
            &s2,
            &mut ctx2,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 4,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V1")),
                fee_cents: None,
            },
        );
        let s2 = o2.new_state().unwrap().clone();
        let o2 = apply_event(
            &s2,
            &mut ctx2,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V1"),
                fill_count: 0,
                remaining_count: 0,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        assert!(
            matches!(
                o2.new_state(),
                Some(OrderState::Halted { reason: HaltReason::ResponseZeroButLocalFills { .. } })
            ),
            "response(0,0)+本地 fill = 真矛盾必须保持 halt: {o2:?}"
        );
    }

    /// ★ A3(HIGH,C2 同型):ReconcilePending + CancelRequested = 幂等 no-op
    /// (修前 reject ⇒ 壳 bail 杀 loop)。
    #[test]
    fn a3_reconcile_pending_cancel_requested_is_idempotent() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 4,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V1")),
                fee_cents: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::NotFound),
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V1")),
                filled_qty: 4,
                remaining_qty: 6,
                fills: vec![],
                authority_complete: false,
            },
        );
        let s = o.new_state().unwrap().clone();
        assert!(matches!(s, OrderState::ReconcilePending { .. }));
        let o = apply_event(&s, &mut ctx, &OrderEvent::CancelRequested);
        assert!(!o.is_reject(), "ReconcilePending 重撤必须幂等放行(修前 reject 杀 loop): {o:?}");
        assert_eq!(o.new_state(), Some(&s), "no-op 保持原态");
        assert!(o.effects().is_empty(), "幂等重试零 effect");
    }

    /// ★ A2(HIGH):ReconcilePending 靠 WS fill 追平 —— 全量 fill_id 覆盖
    /// (attributed==qty)= 权威证据,必须补 latch 并 finalize(修前:最后一笔
    /// fill invalidate authority ⇒ gate1 必败 ⇒ 行永久卡死锁资金)。
    #[test]
    fn a2_late_fill_full_coverage_finalizes_from_reconcile_pending() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        // 壳 cancel HTTP 回包:venue 权威说 filled=10(全成)但 WS fill 尚未到。
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V1")),
                filled_qty: 10,
                remaining_qty: 0,
                fills: vec![],
                authority_complete: true,
            },
        );
        let s = o.new_state().unwrap().clone();
        assert!(
            matches!(s, OrderState::ReconcilePending { .. }),
            "attributed(0)<obligation(10) ⇒ 先挂 ReconcilePending: {s:?}"
        );
        // WS fill 迟到追平(一笔 10 张)——它会 invalidate authority(新 fill_id)。
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 10,
                price_cents: 45,
                ts_ns: 2,
                venue_order_id: Some(vid("V1")),
                fee_cents: None,
            },
        );
        let ns = o.new_state().unwrap().clone();
        assert!(
            !matches!(ns, OrderState::ReconcilePending { .. }),
            "追平后必须离开 ReconcilePending(修前永卡=资金锁死): {ns:?}"
        );
        assert!(
            ns.is_terminal_or_halt() && !matches!(ns, OrderState::Halted { .. }),
            "全量覆盖 ⇒ 干净终态非 halt: {ns:?}"
        );
        assert!(
            o.effects()
                .iter()
                .any(|e| matches!(e, Effect::ReleaseReservation)),
            "终态必须放资金"
        );
    }

    /// ★ A3b(复审 HIGH):壳的真实撤单序列必须全程走通到终态 ——
    /// CancelRequested(no-op)→ CancelOutcome(no-op)→ ReconcileResult(authority)
    /// ⇒ 终态+Release。修前 CancelOutcome 落 catch-all reject = 死点只挪后一个 HTTP。
    #[test]
    fn a3b_shell_cancel_sequence_reaches_terminal_from_reconcile_pending() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 4,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V1")),
                fee_cents: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(&s, &mut ctx, &OrderEvent::CancelOutcome(CancelOutcome::NotFound));
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V1")),
                filled_qty: 4,
                remaining_qty: 6,
                fills: vec![],
                authority_complete: false,
            },
        );
        let s = o.new_state().unwrap().clone();
        assert!(matches!(s, OrderState::ReconcilePending { .. }));
        // 壳重发撤单:CancelRequested → HTTP → CancelOutcome → ReconcileResult。
        let o = apply_event(&s, &mut ctx, &OrderEvent::CancelRequested);
        assert!(!o.is_reject());
        let s = o.new_state().unwrap().clone();
        let o = apply_event(&s, &mut ctx, &OrderEvent::CancelOutcome(CancelOutcome::Canceled));
        assert!(!o.is_reject(), "★ CancelOutcome 必须容忍(修前 reject=死点挪后一跳): {o:?}");
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V1")),
                filled_qty: 4,
                remaining_qty: 0,
                fills: vec![],
                authority_complete: true,
            },
        );
        let ns = o.new_state().unwrap().clone();
        assert!(
            ns.is_terminal_or_halt() && !matches!(ns, OrderState::Halted { .. }),
            "authority 重供后必须到干净终态: {ns:?}"
        );
        assert!(
            o.effects().iter().any(|e| matches!(e, Effect::ReleaseReservation)),
            "终态放资金"
        );
    }

    /// ★ A5b(复审 HIGH):全量抢跑 fill + 零成交全量驻留 response ⇒ **Filled+
    /// Release**(修前手搓 Partial{qty,0} 僵尸:不放资金+open 投影不可见)。
    /// 第三臂(复审 LOW):response(0, 6)+attributed=4 ⇒ 仍 halt(中段矛盾面保钉)。
    #[test]
    fn a5b_full_preempt_fill_finalizes_filled_and_mid_mismatch_still_halts() {
        let mut ctx = ctx_default();
        let s = prepare_started(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 10,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V1")),
                fee_cents: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V1"),
                fill_count: 0,
                remaining_count: 10,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        assert!(
            matches!(o.new_state(), Some(OrderState::Filled { .. })),
            "全量抢跑必须 Filled 非 Partial{{10,0}} 僵尸: {o:?}"
        );
        assert!(
            o.effects().iter().any(|e| matches!(e, Effect::ReleaseReservation)),
            "Filled 必须放资金"
        );
        // 第三臂:remaining 与 qty 不一致的中段矛盾仍 halt。
        let mut ctx3 = ctx_default();
        let s3 = prepare_started(&mut ctx3);
        let o3 = apply_event(
            &s3,
            &mut ctx3,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 4,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V1")),
                fee_cents: None,
            },
        );
        let s3 = o3.new_state().unwrap().clone();
        let o3 = apply_event(
            &s3,
            &mut ctx3,
            &OrderEvent::SubmitResponse {
                venue_order_id: vid("V1"),
                fill_count: 0,
                remaining_count: 6,
                avg_price_cents: None,
                fee_cents: None,
                snapshot_boundary: None,
            },
        );
        assert!(
            matches!(
                o3.new_state(),
                Some(OrderState::Halted { reason: HaltReason::ResponseZeroButLocalFills { .. } })
            ),
            "response(0,6)+attributed=4 中段矛盾必须保钉 halt: {o3:?}"
        );
    }

    /// ★ A4(MED):迟到 fill 越过本地合成 target 地板(≤qty)= 域外新证据,
    /// 地板抬升重合成,不许按 venue 矛盾 Halt(修前 CrossCheckMismatch)。
    #[test]
    fn a4_late_fill_beyond_target_floor_reraises_not_halt() {
        let mut ctx = ctx_default();
        let s = to_cancel_pending(&mut ctx);
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F1"),
                qty: 3,
                price_cents: 45,
                ts_ns: 1,
                venue_order_id: Some(vid("V1")),
                fee_cents: None,
            },
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::CancelOutcome(CancelOutcome::NotFound),
        );
        let s = o.new_state().unwrap().clone();
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::ReconcileResult {
                status: BackfillOrderStatus::Canceled,
                venue_order_id: Some(vid("V1")),
                filled_qty: 3,
                remaining_qty: 7,
                fills: vec![],
                authority_complete: false,
            },
        );
        let s = o.new_state().unwrap().clone();
        assert!(matches!(
            s,
            OrderState::ReconcilePending {
                target: ReconcileTarget { venue_filled_qty: 3, .. },
                ..
            }
        ));
        // 撤前已撮合、WS 晚到的第 4 张(域外合法 fill)。
        let o = apply_event(
            &s,
            &mut ctx,
            &OrderEvent::Fill {
                fill_id: fid("F2"),
                qty: 1,
                price_cents: 45,
                ts_ns: 2,
                venue_order_id: Some(vid("V1")),
                fee_cents: None,
            },
        );
        let ns = o.new_state().unwrap().clone();
        assert!(
            !matches!(ns, OrderState::Halted { .. }),
            "越过本地地板 ≤qty 不是 venue 矛盾(修前假阳性 Halt): {ns:?}"
        );
        assert!(
            matches!(
                &ns,
                OrderState::ReconcilePending {
                    target: ReconcileTarget { venue_filled_qty: 4, .. },
                    ..
                }
            ),
            "地板抬升重合成 target=4: {ns:?}"
        );
        assert_eq!(ctx.attributed_fill_qty, 4);
    }
}

#[cfg(test)]
mod cancel_retry_idempotency_pins {
    use super::*;

    /// ★ C2b 活性钉（复审 R2 MEDIUM：删幂等 arm 两仓全绿 = 零钉）：
    /// CancelPending + CancelRequested = **Accept、state 不变、effects 空**。
    /// 此前是 reject_illegal ⇒ 宿主一次 cancel HTTP 失败后重试 ⇒ reject ⇒
    /// bail ⇒ 进程死、场上单无人管（真钱前双审 C2 死亡链 A）。
    /// 变异：删掉该 arm（回落 reject_illegal）⇒ 本测试红。
    #[test]
    fn cancel_requested_on_cancel_pending_is_idempotent_noop() {
        let state = OrderState::CancelPending {
            venue_order_id: VenueOrderId("w1".into()),
            filled_qty: 0,
            remaining_qty: 500,
            response_fill_count: None,
            response_avg_price_cents: None,
            response_fee_cents: None,
            reconcile_target: None,
        };
        let mut ctx = OrderCtx::new(
            derive_client_order_id("KXBTC-M", "hoff-mm", 1),
            "KXBTC-M",
            "hoff-mm",
            Side::BuyYes,
            50,
            500,
        );
        let out = apply_event(&state, &mut ctx, &OrderEvent::CancelRequested);
        match out {
            TransitionOutcome::Accept { new_state, effects } => {
                assert!(
                    matches!(new_state, OrderState::CancelPending { ref venue_order_id, .. }
                        if venue_order_id.0 == "w1"),
                    "state 必须原样保持 CancelPending"
                );
                assert!(effects.is_empty(), "重试不许重复记 journal（零 effect）");
            }
            other => panic!("cancel 重试必须 Accept(no-op)，实际 {other:?} —— \
                reject 会让宿主 bail 进程死"),
        }
    }
}
