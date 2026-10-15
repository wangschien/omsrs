//! Live submission, terminal settlement and protective-cancel bookkeeping.
use super::close_proof::OmsPriceE4;
use super::live_admission::{AdmissionClient, AdmissionIntent};
use super::live_state::{LiveOmsBookState, LiveOmsOrder};
use super::live_transaction::TransactionIo;
use crate::execution::{ExecutionSource, OrderCursor};
use crate::lifecycle::{
    AttemptId, BackfillOrderRecord, ClientOrderId as OmsClientOrderId, FillId, OrderCtx,
    OrderEvent, OrderState,
};
use anyhow::{Result, bail};
use rust_decimal::Decimal;

pub trait SubmissionIntent<C, R>: AdmissionIntent<C> {
    fn price(&self) -> Decimal;
    fn market_ticker(&self) -> &String;
    fn role(&self) -> R;
}
pub trait CancelAckInput<C> {
    fn client_order_id(&self) -> &C;
    fn terminal_kind(&self) -> LiveCancelTerminalKind;
    fn set_hold_slot(&mut self, value: bool);
}
pub const PROTECTIVE_CANCEL_UNPROVEN_HOLD_NS: u64 = 10_000_000_000;
const PROTECTIVE_CANCEL_FAIL_BACKOFF_NS: u64 = 250_000_000;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LiveCancelTerminalKind {
    #[default]
    Canceled,
    Filled {
        filled_qty_centi: u64,
        remaining_qty_centi: u64,
    },
    CanceledWithFill {
        filled_qty_centi: u64,
        remaining_qty_centi: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReapOutcome {
    GoneCanceled,
    GoneFilled {
        filled_qty_centi: u64,
        remaining_qty_centi: u64,
    },
    StillThere,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectiveCancelOutcome {
    Sent,
    Gone(ReapOutcome),
    NotFoundUnproven,
    StillThere,
    Failed,
}
impl ReapOutcome {
    pub fn gone(self) -> bool {
        !matches!(self, ReapOutcome::StillThere)
    }
}
impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn note_protective_cancel(
        &mut self,
        wire: &str,
        outcome: ProtectiveCancelOutcome,
        now_ns: u64,
    ) {
        use ProtectiveCancelOutcome as O;
        let st = self.protective_cancel.entry(wire.to_string()).or_default();
        st.sends = st.sends.saturating_add(1);
        match outcome {
            O::Sent => {}
            O::Gone(_) => st.terminal = true,
            O::NotFoundUnproven | O::StillThere => {
                st.hold_until_ns = now_ns.saturating_add(PROTECTIVE_CANCEL_UNPROVEN_HOLD_NS);
            }
            O::Failed => {
                st.hold_until_ns = now_ns.saturating_add(
                    (PROTECTIVE_CANCEL_FAIL_BACKOFF_NS << st.sends.min(4)).min(4_000_000_000),
                );
            }
        }
    }

    pub fn unique_live_wire<C: AdmissionClient<R>>(&self, client: &C) -> Option<String> {
        let wire = self
            .by_client
            .get((client).as_str())?
            .ctx
            .venue_order_id
            .as_ref()?
            .0
            .clone();
        let shared = self.by_client.values().any(|o| {
            o.ctx.client_order_id.as_str() != client.as_str()
                && !o.state.is_terminal_or_halt()
                && o.ctx.venue_order_id.as_ref().is_some_and(|v| v.0 == wire)
        });
        (!shared).then_some(wire)
    }

    pub fn begin_submit<C: AdmissionClient<R>, I: SubmissionIntent<C, R>>(
        &mut self,
        intent: &I,
        strategy: &str,
        attempt_id: AttemptId,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let key = intent.client_order_id().as_str().to_string();
        if self.by_client.contains_key(&key) {
            bail!("oms already tracks client_order_id {key}");
        }
        let price_e4 = OmsPriceE4::from_dollars(intent.price()).ok_or_else(|| {
            anyhow::anyhow!(
                "intent price {} not representable as OMS 万分位 (no round)",
                intent.price()
            )
        })?;
        let mut ctx = OrderCtx::new(
            OmsClientOrderId(intent.client_order_id().as_str().to_string()),
            intent.market_ticker().clone(),
            strategy,
            intent.side(),
            price_e4.into_wire_price_cents(),
            intent.qty_centi(),
        );
        let mut state = OrderState::New;
        let mut cursor = OrderCursor { seq: 0 };
        self.apply_and_journal(
            &mut state,
            &mut ctx,
            &mut cursor,
            &OrderEvent::PrepareSubmit,
            journal,
            ExecutionSource::CreateResponseBackfill,
        )?;
        self.apply_and_journal(
            &mut state,
            &mut ctx,
            &mut cursor,
            &OrderEvent::StartSubmit {
                attempt_id: attempt_id.clone(),
            },
            journal,
            ExecutionSource::CreateResponseBackfill,
        )?;
        self.by_client.insert(
            key,
            LiveOmsOrder {
                state,
                ctx,
                canceled_gc_seen_ns: None,
                venue_working_centi: None,
                venue_initial_centi: None,
                role: intent.role(),
                reduce_only: intent.reduce_only(),
                order_age_anchor_ns: None,
                cursor,
                cancel_gone: None,
            },
        );
        Ok(())
    }

    pub fn terminal_same_wire_siblings<C: AdmissionClient<R> + std::fmt::Display>(
        &mut self,
        claimed: &C,
        journal: &mut dyn TransactionIo,
    ) -> Result<Vec<C>> {
        let wire = match self
            .by_client
            .get((claimed).as_str())
            .and_then(|o| o.ctx.venue_order_id.as_ref().map(|v| v.0.clone()))
        {
            Some(w) => w,
            None => return Ok(Vec::new()),
        };
        let siblings: Vec<C> = self
            .by_client
            .values()
            .filter(|o| {
                o.ctx.client_order_id.as_str() != claimed.as_str()
                    && !o.state.is_terminal_or_halt()
                    && o.ctx.venue_order_id.as_ref().is_some_and(|v| v.0 == wire)
            })
            .filter_map(|o| C::from_string(o.ctx.client_order_id.as_str().to_string()))
            .collect();
        let mut all_http_ok = true;
        for cid in &siblings {
            if let Err(e) = self
                .on_cancel_requested::<C>(cid, journal)
                .and_then(|_| self.on_cancel_http_success::<C>(cid, journal))
            {
                all_http_ok = false;
                eprintln!("KIN-SWAP sibling 终局失败(留非终态,收窗第一门拦) cid={cid}: {e:#}");
            }
        }
        if all_http_ok && !siblings.is_empty() {
            let rem = self
                .by_client
                .get((claimed).as_str())
                .and_then(|o| o.ctx.last_venue_remaining_qty)
                .unwrap_or(0);
            let _ = self.observe_current_resting(&wire, rem, 0, journal);
        }
        Ok(siblings)
    }

    pub fn book_decrease_gone<C: AdmissionClient<R>, A: CancelAckInput<C>>(
        &mut self,
        ack: &A,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let cid = ack.client_order_id();
        if !self
            .by_client
            .get((cid).as_str())
            .is_some_and(|o| !o.state.is_terminal_or_halt() && !o.state.is_submission_closed())
        {
            return Ok(());
        }
        match &self.by_client.get((cid).as_str()).map(|o| o.state.clone()) {
            Some(
                OrderState::Accepted { .. }
                | OrderState::Partial { .. }
                | OrderState::ImmediateFillUnattributed { .. },
            ) => {
                self.on_cancel_requested::<C>(cid, journal)?;
            }
            _ => {}
        }
        if !self
            .by_client
            .get((cid).as_str())
            .is_some_and(|o| !o.state.is_terminal_or_halt() && !o.state.is_submission_closed())
        {
            return Ok(());
        }
        let attributed = self
            .by_client
            .get((cid).as_str())
            .map(|o| o.ctx.attributed_fill_qty)
            .unwrap_or(0);
        match ack.terminal_kind() {
            LiveCancelTerminalKind::Canceled => self.on_cancel_http_success::<C>(cid, journal),
            LiveCancelTerminalKind::Filled {
                filled_qty_centi,
                remaining_qty_centi,
            }
            | LiveCancelTerminalKind::CanceledWithFill {
                filled_qty_centi,
                remaining_qty_centi,
            } => self.on_cancel_canceled_with_fill::<C>(
                cid,
                filled_qty_centi.max(attributed),
                remaining_qty_centi,
                journal,
            ),
        }
    }

    pub fn mark_hold_slot_if_open<C: AdmissionClient<R>, A: CancelAckInput<C>>(&self, ack: &mut A) {
        if self
            .by_client
            .get((ack.client_order_id()).as_str())
            .is_some_and(|o| !o.state.is_terminal_or_halt() && !o.state.is_submission_closed())
        {
            ack.set_hold_slot(true);
        }
    }

    pub fn can_canceled_terminal_host_only(state: &OrderState) -> bool {
        matches!(
            state,
            OrderState::Accepted { .. }
                | OrderState::Partial { .. }
                | OrderState::ImmediateFillUnattributed { .. }
                | OrderState::CancelPending { .. }
        )
    }

    pub fn terminal_host_only_ghost<C: AdmissionClient<R> + std::fmt::Display>(
        &mut self,
        cid: &C,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let Some(state) = self.by_client.get((cid).as_str()).map(|o| o.state.clone()) else {
            return Ok(());
        };
        if state.is_terminal_or_halt() {
            return Ok(());
        }
        if !Self::can_canceled_terminal_host_only(&state) {
            return Ok(());
        }
        match &state {
            OrderState::Accepted { .. }
            | OrderState::Partial { .. }
            | OrderState::ImmediateFillUnattributed { .. } => {
                self.on_cancel_requested::<C>(cid, journal)?;
            }
            _ => {}
        }
        let Some(wire) = self.unique_live_wire::<C>(cid) else {
            bail!("host-only ghost {cid} still non-terminal after Canceled book —— 留 open 投影");
        };
        if !self.wire_held_empty(&wire) {
            bail!("host-only ghost {cid} still non-terminal after Canceled book —— 留 open 投影");
        }
        self.observe_canceled_empty(&wire, journal)?;
        if !self
            .by_client
            .get((cid).as_str())
            .is_some_and(|o| o.state.is_terminal_or_halt())
        {
            bail!("host-only ghost {cid} still non-terminal after Canceled book —— 留 open 投影");
        }
        Ok(())
    }

    pub fn enrich_fill_fee<C: AdmissionClient<R>>(
        &mut self,
        client: &C,
        fill_id: &str,
        fee_cents: u64,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let key = client.as_str();
        let fid = FillId(fill_id.to_string());
        let prev = self
            .by_client
            .get(key)
            .ok_or_else(|| anyhow::anyhow!("enrich_fill_fee: unknown client {key}"))?
            .ctx
            .applied_fills
            .get(&fid)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("enrich_fill_fee: unknown fill_id {fill_id}"))?;
        if let Some(old) = prev.fee {
            if old != fee_cents {
                bail!("enrich_fill_fee halt: fee conflict Some({old}) → Some({fee_cents})");
            }
            self.pending_fee_fill_ids.remove(fill_id);
            return Ok(());
        }
        self.apply_for_client(
            key,
            &OrderEvent::Fill {
                fill_id: fid,
                qty: prev.qty,
                price_cents: OmsPriceE4::from_wire_price_cents(prev.price_cents)
                    .into_wire_price_cents(),
                ts_ns: prev.ts_ns,
                venue_order_id: prev.venue_order_id.clone(),
                fee_cents: Some(fee_cents),
            },
            journal,
            ExecutionSource::RestFill,
        )?;
        self.pending_fee_fill_ids.remove(fill_id);
        Ok(())
    }

    pub fn venue_wire_for_client<C: AdmissionClient<R>>(&self, client: &C) -> Option<&str> {
        self.by_client
            .get(client.as_str())
            .and_then(|o| o.ctx.venue_order_id.as_ref())
            .map(|v| v.0.as_str())
    }

    pub fn is_fully_filled<C: AdmissionClient<R>>(&self, client: &C) -> bool {
        self.by_client.get(client.as_str()).is_some_and(|o| {
            o.ctx.remaining_qty() == 0
                || matches!(o.state, OrderState::Filled | OrderState::Terminal)
        })
    }

    pub fn apply_unknown_backfill<C: AdmissionClient<R>>(
        &mut self,
        client: &C,
        exhaustive: bool,
        matched: Vec<BackfillOrderRecord>,
        journal: &mut dyn TransactionIo,
    ) -> Result<()> {
        let key = client.as_str();
        self.apply_for_client(
            key,
            &OrderEvent::UnknownBackfillResult {
                exhaustive,
                matched,
            },
            journal,
            ExecutionSource::RestFill,
        )
        .map(|_| ())
    }
}

/// The existing quantity decoder used by the private input adapter.
pub trait DecreaseQuantityCodec {
    type Error;
    fn centi_from_contracts(value: Decimal) -> std::result::Result<u64, Self::Error>;
}
impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn on_decrease_ack<C: AdmissionClient<R> + DecreaseQuantityCodec>(
        &mut self,
        client: &C,
        remaining_qty: rust_decimal::Decimal,
        ts_ms: i64,
        journal: &mut dyn TransactionIo,
    ) where
        Stage: super::live_ingest::ExecutionTrace<R>,
    {
        let Ok(remaining) = C::centi_from_contracts(remaining_qty) else {
            return;
        };
        let outcome = self.apply_for_client(
            client.as_str(),
            &OrderEvent::DecreaseObserved { remaining, ts_ms },
            journal,
            ExecutionSource::RestFill,
        );
        Stage::action_admit_venue(
            self,
            "decrease",
            if outcome.is_ok() { "ok" } else { "error" },
            client.as_str(),
            None,
            None,
        );
    }
}
