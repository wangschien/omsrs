//! Original live OMS state moved without interpreting caller payloads.
//! The role and trace handle remain the caller's real types. Journal, transport,
//! clock and notification operations stay in the existing kbot caller.
use crate::execution::{
    ExecutionOutcome, ExecutionScope, ExecutionSource, InventoryDisposition, InventoryHealth,
    LedgerId, MarketInventory, OrderCursor, OrderRef,
};
use crate::lifecycle::{ClientOrderId as OmsClientOrderId, OrderCtx, OrderState, VenueOrderId};
use crate::observation::{Readiness, UserOrderEvidence, WireRegistry};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelGone {
    pub venue_filled_centi: u64,
    pub wire: VenueOrderId,
}

#[derive(Debug, Clone)]
pub struct LiveOmsOrder<R> {
    pub state: OrderState,
    pub ctx: OrderCtx,
    /// 撤/成竞态（2026-08-15 spec）：gc **首次**见到本行处于 `Canceled` 的时刻。
    /// `None` = 尚未被 gc 观察到。宽限期内保留行，让迟到私有 fill 走 omsrs D2
    /// 响亮路径（记账 + Halted + `PostTerminalFill`），而非 `NoRow` 静默丢。
    pub canceled_gc_seen_ns: Option<u64>,
    /// Venue decrease 后的剩余 centi。omsrs `qty` 不能缩，open 投影认这个。
    /// `Some(0)` = 场上没了，勿再当活单。
    pub venue_working_centi: Option<u64>,
    /// Venue `initial_count` last admitted for this row (in-memory). The venue
    /// keeps the ORIGINAL initial through Decrease and shrinking amends (demo
    /// probe 09-29), so an amend child inherits its parent's value.
    pub venue_initial_centi: Option<u64>,
    pub role: R,
    pub reduce_only: bool,
    /// v22: create ACK `Some(now_ns)`; REST seed `None`; amend reset `Some(now_ns)`.
    pub order_age_anchor_ns: Option<u64>,
    /// A's order cursor (seq). Required by prepare/apply; rebuilt from journal.
    pub cursor: OrderCursor,
    /// Set by a booked positive DELETE (#54); None after restart (conservative).
    pub cancel_gone: Option<CancelGone>,
}

impl<R> LiveOmsOrder<R> {
    /// The row's LATEST halt reason is `PostTerminalFill` — a LABEL only. omsrs `restart_safe_handle`
    /// relabels every restart-frozen state (any Halted incl. CrossCheckMismatch, ImmediateFillUnresolved,
    /// UnknownNoMatch, SubmissionClosed) to this on a newly accounted owned fill, so the label alone does NOT
    /// prove the row was terminal or that nothing rests on the venue. Use
    /// `LiveOmsBook::is_post_terminal_booked_after_terminal` for that proof.
    pub fn is_post_terminal_booked(&self) -> bool {
        matches!(
            self.state,
            OrderState::Halted {
                reason: crate::lifecycle::HaltReason::PostTerminalFill
            }
        )
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProtectiveCancelState {
    pub sends: u32,
    pub hold_until_ns: u64,
    /// The venue proved the id gone: never DELETE it again.
    pub terminal: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AuthorityReadState {
    pub failed: u32,
    pub hold_until_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageClear {
    pub new_cid: String,
    pub instant_ns: u64,
    pub source: &'static str,
}

pub struct LiveOmsBookState<R, Stage> {
    pub by_client: BTreeMap<String, LiveOmsOrder<R>>,
    /// Spec Step C: fill_ids booked with `fee_cents: None` awaiting F5 enrichment.
    pub pending_fee_fill_ids: std::collections::BTreeSet<String>,
    /// 撤/成竞态 spec B + 复审 grok HIGH-2：**已入账/已补记**的 fill trade_id 集合
    /// （Booked / D2 late / off-book 都登记；session 起动时从 journal 播种以跨
    /// crash）。NoRow 且已见 = 重投/redelivery ⇒ 幂等静默（不闩不双记）。
    pub seen_fill_ids: std::collections::BTreeSet<String>,
    pub staged_user_orders: BTreeMap<String, UserOrderEvidence>,
    pub amend_applied_mono: BTreeMap<String, std::time::Instant>,
    /// WO-UA R2-3: every lineage clear with its instant and writer (drained by `LiveSession::ua_sync`).
    pub lineage_clears: Vec<LineageClear>,
    pub replayed_http_pending: BTreeSet<VenueOrderId>,
    pub lagged_unknown_creates: BTreeSet<String>,
    pub ledger: LedgerId,
    pub public_fields: LiveOmsPublicFields,
    /// cids whose late fill was booked on a row that was already normal-terminal (ledger #33 r1).
    pub late_after_terminal: BTreeSet<String>,
    pub fold_pos: u64,
    /// Closure pending-wire index (§3.2 β): wires only, not persisted.
    pub pending_wires: BTreeSet<VenueOrderId>,
    /// #55 route (ii): wires whose positive DELETE found no live proof total (and no D3R1-1 deficit), so the Canceled
    /// arm refused the 0/0 synthesis; a D3 member while a row bound to the wire is open (not persisted).
    pub d3_cancel_none: BTreeSet<VenueOrderId>,
    /// Per-venue-id budget for the amended-hold protective cancel (not persisted).
    pub protective_cancel: BTreeMap<String, ProtectiveCancelState>,
    /// Per-venue-id cadence of the authority official-order GET (not persisted).
    pub authority_read: BTreeMap<String, AuthorityReadState>,
    /// #56: newest venue exec ts (ns, second-precision) among executions APPLIED to inventory (`Ingested`); duplicates,
    /// refusals and conflicts never move it. Rebuilt from the journal through the same predicate.
    pub last_applied_exec_ts_ns: u64,
    pub stage: Option<Stage>,
}

#[derive(Debug)]
pub struct LiveOmsPublicFields {
    pub inventory: MarketInventory,
    pub registry: WireRegistry,
    pub user_orders_cmd_id: Option<u64>,
    pub fill_cmd_id: Option<u64>,
    pub user_orders_sid: Option<u64>,
    pub user_orders_resubscribe_required: bool,
    pub user_orders_connection_generation: u64,
    pub expected_exchange_index: Option<i64>,
    pub expected_subaccount: i64,
    /// Tickers of the adjacent authorized WinX windows (start∓900); a same-subaccount
    /// WS fill on one of them is a neighbour's fill, not invalid evidence (ledger #35).
    pub adjacent_markets: Vec<String>,
    pub protective_cancel_suppressed: u64,
    pub authority_read_suppressed: u64,
    pub stopped: bool,
    pub g2_proven: bool,
}

impl<R, Stage> std::ops::Deref for LiveOmsBookState<R, Stage> {
    type Target = LiveOmsPublicFields;
    fn deref(&self) -> &Self::Target {
        &self.public_fields
    }
}
impl<R, Stage> std::ops::DerefMut for LiveOmsBookState<R, Stage> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.public_fields
    }
}

impl<R, Stage> Default for LiveOmsBookState<R, Stage> {
    fn default() -> Self {
        let ledger = LedgerId {
            scope: ExecutionScope("0".into()),
            market: String::new(),
        };
        Self {
            by_client: BTreeMap::new(),
            pending_fee_fill_ids: Default::default(),
            seen_fill_ids: Default::default(),
            staged_user_orders: BTreeMap::new(),
            amend_applied_mono: BTreeMap::new(),
            lineage_clears: Vec::new(),
            replayed_http_pending: BTreeSet::new(),
            lagged_unknown_creates: BTreeSet::new(),
            public_fields: LiveOmsPublicFields {
                inventory: MarketInventory::new(ledger.clone()),
                registry: WireRegistry::new(),
                user_orders_cmd_id: None,
                fill_cmd_id: None,
                user_orders_sid: None,
                user_orders_resubscribe_required: false,
                user_orders_connection_generation: 0,
                expected_exchange_index: None,
                expected_subaccount: 0,
                adjacent_markets: Vec::new(),
                protective_cancel_suppressed: 0,
                authority_read_suppressed: 0,
                stopped: false,
                g2_proven: false,
            },
            ledger,
            late_after_terminal: BTreeSet::new(),
            fold_pos: 0,
            pending_wires: BTreeSet::new(),
            d3_cancel_none: BTreeSet::new(),
            protective_cancel: BTreeMap::new(),
            authority_read: BTreeMap::new(),
            last_applied_exec_ts_ns: 0,
            stage: None,
        }
    }
}

impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn fill_id_seen(&self, trade_id: &str) -> bool {
        self.seen_fill_ids.contains(trade_id)
    }

    pub fn seed_seen_fill_ids<'a>(&mut self, ids: impl IntoIterator<Item = &'a str>) {
        for id in ids {
            self.seen_fill_ids.insert(id.to_string());
        }
    }

    pub fn pending_fee_fill_ids(&self) -> &std::collections::BTreeSet<String> {
        &self.pending_fee_fill_ids
    }

    pub fn has_pending_fee_enrichment(&self) -> bool {
        !self.pending_fee_fill_ids.is_empty()
    }

    pub fn net_yes_centi(&self) -> i64 {
        i64::try_from(self.inventory.net()).unwrap_or(i64::MAX)
    }

    pub fn last_applied_exec_ts_ns(&self) -> u64 {
        self.last_applied_exec_ts_ns
    }

    pub fn is_post_terminal_booked_after_terminal(&self, row: &LiveOmsOrder<R>) -> bool {
        row.is_post_terminal_booked()
            && self
                .late_after_terminal
                .contains(&row.ctx.client_order_id.0)
    }
}

impl<R: std::fmt::Debug, Stage: std::fmt::Debug> std::fmt::Debug for LiveOmsBookState<R, Stage> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveOmsBook")
            .field("by_client", &self.by_client)
            .field("pending_fee_fill_ids", &self.pending_fee_fill_ids)
            .field("seen_fill_ids", &self.seen_fill_ids)
            .field("staged_user_orders", &self.staged_user_orders)
            .field("amend_applied_mono", &self.amend_applied_mono)
            .field("lineage_clears", &self.lineage_clears)
            .field("replayed_http_pending", &self.replayed_http_pending)
            .field("lagged_unknown_creates", &self.lagged_unknown_creates)
            .field("ledger", &self.ledger)
            .field("inventory", &self.inventory)
            .field("registry", &self.registry)
            .field("user_orders_cmd_id", &self.user_orders_cmd_id)
            .field("fill_cmd_id", &self.fill_cmd_id)
            .field("user_orders_sid", &self.user_orders_sid)
            .field(
                "user_orders_resubscribe_required",
                &self.user_orders_resubscribe_required,
            )
            .field(
                "user_orders_connection_generation",
                &self.user_orders_connection_generation,
            )
            .field("expected_exchange_index", &self.expected_exchange_index)
            .field("expected_subaccount", &self.expected_subaccount)
            .field("adjacent_markets", &self.adjacent_markets)
            .field("late_after_terminal", &self.late_after_terminal)
            .field("fold_pos", &self.fold_pos)
            .field("pending_wires", &self.pending_wires)
            .field("d3_cancel_none", &self.d3_cancel_none)
            .field("protective_cancel", &self.protective_cancel)
            .field(
                "protective_cancel_suppressed",
                &self.protective_cancel_suppressed,
            )
            .field("authority_read", &self.authority_read)
            .field("authority_read_suppressed", &self.authority_read_suppressed)
            .field("stopped", &self.stopped)
            .field("g2_proven", &self.g2_proven)
            .field("last_applied_exec_ts_ns", &self.last_applied_exec_ts_ns)
            .field("stage", &self.stage)
            .finish()
    }
}

pub const PROTECTIVE_CANCEL_MAX_SENDS: u32 = 6;
const AUTHORITY_READ_BACKOFF_BASE_NS: u64 = 500_000_000;
const AUTHORITY_READ_BACKOFF_CAP_NS: u64 = 4_000_000_000;

fn d3_deficit(venue_cum: u64, booked: Option<u64>) -> bool {
    venue_cum > booked.unwrap_or(0)
}

impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn ledger(&self) -> &LedgerId {
        &self.ledger
    }

    pub fn n_readiness(&self, fill_apply_failed: bool) -> Readiness {
        if fill_apply_failed
            || matches!(
                self.inventory.view().health,
                InventoryHealth::Conflicted { .. }
            )
        {
            return Readiness::Conflict;
        }
        if !self.g2_proven || self.stopped {
            return Readiness::Unresolved {
                member_debt: 0,
                wire_residual: 0,
                unresolved_venue_only: 0,
                tracked_scoped: 0,
            };
        }
        if self.by_client.values().any(|o| {
            matches!(
                o.state,
                OrderState::SubmitUnknown | OrderState::UnknownNoMatch
            )
        }) {
            return Readiness::Unresolved {
                member_debt: 0,
                wire_residual: 0,
                unresolved_venue_only: 0,
                tracked_scoped: 0,
            };
        }
        let ledger = &self.ledger;
        match self.registry.readiness(
            ledger,
            |cid| {
                self.by_client.get(&cid.0).map(|o| OrderRef {
                    ledger,
                    cursor: o.cursor,
                    state: &o.state,
                    ctx: &o.ctx,
                })
            },
            &self.inventory,
        ) {
            Ok(r) => r,
            Err(_) => Readiness::Unresolved {
                member_debt: 0,
                wire_residual: 0,
                unresolved_venue_only: 0,
                tracked_scoped: 0,
            },
        }
    }

    pub fn is_ready(&self, fill_apply_failed: bool) -> bool {
        matches!(self.n_readiness(fill_apply_failed), Readiness::Ready)
    }

    pub fn protective_cancel_due(&mut self, wire: &str, now_ns: u64) -> bool {
        let due = self.protective_cancel.get(wire).map_or(true, |st| {
            !st.terminal && st.sends < PROTECTIVE_CANCEL_MAX_SENDS && now_ns >= st.hold_until_ns
        });
        if !due {
            self.protective_cancel_suppressed = self.protective_cancel_suppressed.saturating_add(1);
        }
        due
    }

    pub fn wire_rows_terminal(&self, wire: &str, current_cid: Option<&OmsClientOrderId>) -> bool {
        let row_terminal = |cid: &str| {
            self.by_client.get(cid).map(|row| {
                matches!(
                    row.state,
                    OrderState::Filled
                        | OrderState::Canceled
                        | OrderState::Terminal
                        | OrderState::SubmissionClosed { .. }
                )
            })
        };
        current_cid.is_some_and(|cid| row_terminal(cid.0.as_str()) == Some(true))
            && self
                .registry
                .get(&self.ledger, &VenueOrderId(wire.to_owned()))
                .is_some_and(|state| {
                    state.amend_lineage.as_ref().map_or(true, |lineage| {
                        row_terminal(lineage.new_cid.0.as_str()) == Some(true)
                    }) && state
                        .members
                        .iter()
                        .all(|member| row_terminal(member.cid.0.as_str()) == Some(true))
                })
    }

    pub fn protective_cancel_due_for(
        &mut self,
        wire: &str,
        current_cid: Option<&OmsClientOrderId>,
        now_ns: u64,
    ) -> bool {
        let wire_done = self.wire_rows_terminal(wire, current_cid);
        if wire_done {
            self.protective_cancel
                .entry(wire.to_string())
                .or_default()
                .terminal = true;
            self.protective_cancel_suppressed = self.protective_cancel_suppressed.saturating_add(1);
            return false;
        }
        self.protective_cancel_due(wire, now_ns)
    }

    pub fn authority_read_due(&mut self, wire: &str, now_ns: u64) -> bool {
        let due = self
            .authority_read
            .get(wire)
            .map_or(true, |st| now_ns >= st.hold_until_ns);
        if !due {
            self.authority_read_suppressed = self.authority_read_suppressed.saturating_add(1);
        }
        due
    }

    pub fn note_authority_read(&mut self, wire: &str, discharged: bool, now_ns: u64) {
        if discharged {
            self.authority_read.remove(wire);
            return;
        }
        let st = self.authority_read.entry(wire.to_string()).or_default();
        let backoff =
            (AUTHORITY_READ_BACKOFF_BASE_NS << st.failed.min(3)).min(AUTHORITY_READ_BACKOFF_CAP_NS);
        st.failed = st.failed.saturating_add(1);
        st.hold_until_ns = now_ns.saturating_add(backoff);
    }

    pub fn wire_held_inventory_centi(&self, wire: &VenueOrderId) -> Option<u64> {
        let ws = self.registry.get(&self.ledger, wire)?;
        ws.held.iter().try_fold(0_u64, |acc, id| {
            acc.checked_add(self.inventory.get(id)?.qty)
        })
    }

    pub fn wire_total_at_proof_centi(&self, wire: &VenueOrderId) -> Option<u64> {
        let obs = self
            .registry
            .get(&self.ledger, wire)?
            .remaining_observation
            .as_ref()?;
        obs.covered.iter().try_fold(obs.remaining, |acc, id| {
            acc.checked_add(self.inventory.get(id)?.qty)
        })
    }

    pub fn wire_holds_no_inventory(&self, wire: &VenueOrderId) -> bool {
        self.wire_held_inventory_centi(wire).map_or_else(
            || self.registry.get(&self.ledger, wire).is_none(),
            |held| held == 0,
        )
    }

    pub fn wire_held_empty(&self, wire: &str) -> bool {
        let vid = VenueOrderId(wire.to_string());
        self.registry
            .get(&self.ledger, &vid)
            .map(|ws| ws.held.is_empty())
            .unwrap_or(true)
    }

    pub fn wire_known_deficit(&self, wire: &VenueOrderId) -> bool {
        let Some(state) = self.registry.get(&self.ledger, wire) else {
            return false;
        };
        let venue_cum = state
            .latest_user_order
            .as_ref()
            .map_or(0, |seen| seen.filled);
        d3_deficit(venue_cum, self.wire_held_inventory_centi(wire))
    }

    pub fn row_is_normal_terminal(&self, cid: &str) -> bool {
        self.by_client.get(cid).is_some_and(|order| {
            matches!(
                order.state,
                OrderState::Filled | OrderState::Canceled | OrderState::Terminal
            )
        })
    }

    pub fn note_applied_exec_ts(&mut self, outcomes: &[ExecutionOutcome]) {
        for o in outcomes {
            if matches!(o.inventory, InventoryDisposition::Ingested(_))
                && matches!(
                    o.evidence.source,
                    ExecutionSource::WsFill | ExecutionSource::RestFill
                )
            {
                if let Ok(ts) = u64::try_from(o.evidence.ts_ns) {
                    self.last_applied_exec_ts_ns = self.last_applied_exec_ts_ns.max(ts);
                }
            }
        }
    }
}
