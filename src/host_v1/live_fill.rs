//! Private execution booking and idempotent unowned-fill records.
use super::close_proof::OmsPriceE4;
use super::live_admission::AdmissionClient;
use super::live_ingest::ExecutionTrace;
use super::live_state::LiveOmsBookState;
use super::live_transaction::TransactionIo;
use crate::execution::{ExecutionEvidence, ExecutionSource, OrderDisposition};
use crate::lifecycle::{
    ClientOrderId as OmsClientOrderId, Effect, FillId, Side, VenueOrderId, fill_journal_record,
};
use anyhow::{Result, bail};
use rust_decimal::Decimal;

/// Borrowed scalar access and the original quantity/timestamp codecs.
pub trait PrivateFillInput<C> {
    fn client_order_id(&self) -> &C;
    fn qty_centi_for_live(&self) -> Result<u64>;
    fn qty_centi_for_unowned(&self) -> Result<u64>;
    fn yes_price(&self) -> Decimal;
    fn order_id(&self) -> &String;
    fn trade_id(&self) -> &String;
    fn market_ticker(&self) -> &String;
    fn purchased_side(&self) -> &String;
    fn venue_ts_ns(&self) -> Result<i64>;
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateFillBook {
    Booked,
    NoRow,
    ZeroQty,
    BookedLatePostTerminal,
}
impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn on_private_fill<C: AdmissionClient<R>, I: PrivateFillInput<C>>(
        &mut self,
        sig: &I,
        fee_cents: Option<u64>,
        journal: &mut dyn TransactionIo,
    ) -> Result<PrivateFillBook>
    where
        Stage: ExecutionTrace<R>,
    {
        let key = sig.client_order_id().as_str();
        let qty = sig.qty_centi_for_live()?;
        if qty == 0 {
            return Ok(PrivateFillBook::ZeroQty);
        }
        let price_e4 = OmsPriceE4::from_dollars(sig.yes_price()).ok_or_else(|| {
            anyhow::anyhow!(
                "private fill price {} 不能表示为 OMS 1e-4 price units（离格/解析坏）—— 拒绝按 0 入账",
                sig.yes_price()
            )
        })?;
        let ts_ns = sig.venue_ts_ns()?;
        let venue_order_id = if sig.order_id().trim().is_empty() {
            None
        } else {
            Some(VenueOrderId(sig.order_id().clone()))
        };
        let evidence = ExecutionEvidence {
            scope: self.ledger.scope.clone(),
            market: sig.market_ticker().clone(),
            execution_id: FillId(sig.trade_id().clone()),
            side: match sig.purchased_side().as_str() {
                "yes" => Side::BuyYes,
                "no" => Side::SellYes,
                _ => {
                    bail!(
                        "private fill unknown purchased_side={}",
                        sig.purchased_side()
                    )
                }
            },
            qty,
            price_cents: price_e4.into_wire_price_cents(),
            ts_ns,
            venue_order_id,
            claimed_client_order_id: Some(OmsClientOrderId(
                sig.client_order_id().as_str().to_string(),
            )),
            fee_cents,
            source: ExecutionSource::WsFill,
        };
        let apply_res = self.ingest_execution::<C>(evidence, journal);
        let row_late_post_terminal = self
            .by_client
            .get(key)
            .is_some_and(|order| order.is_post_terminal_booked());
        if let Err(e) = apply_res {
            let journaled = journal.has_fill_id(sig.trade_id());
            if row_late_post_terminal && journaled {
                self.seen_fill_ids.insert(sig.trade_id().clone());
                if fee_cents.is_none() {
                    self.pending_fee_fill_ids.insert(sig.trade_id().clone());
                }
                return Ok(PrivateFillBook::BookedLatePostTerminal);
            }
            return Err(e);
        }
        let outcomes = apply_res.expect("checked err");
        if outcomes
            .iter()
            .any(|o| matches!(o.order, OrderDisposition::LateBooked))
        {
            self.seen_fill_ids.insert(sig.trade_id().clone());
            if fee_cents.is_none() {
                self.pending_fee_fill_ids.insert(sig.trade_id().clone());
            }
            return Ok(PrivateFillBook::BookedLatePostTerminal);
        }
        if outcomes
            .iter()
            .any(|o| matches!(o.order, OrderDisposition::NoRow))
        {
            self.seen_fill_ids.insert(sig.trade_id().clone());
            return Ok(PrivateFillBook::NoRow);
        }
        if fee_cents.is_none() {
            self.pending_fee_fill_ids.insert(sig.trade_id().clone());
        } else {
            self.pending_fee_fill_ids.remove(sig.trade_id());
        }
        self.seen_fill_ids.insert(sig.trade_id().clone());
        Ok(PrivateFillBook::Booked)
    }

    pub fn book_off_book_canceled_fill<C: AdmissionClient<R>, I: PrivateFillInput<C>>(
        &mut self,
        sig: &I,
        journal: &mut dyn TransactionIo,
    ) -> Result<bool> {
        if self.seen_fill_ids.contains(sig.trade_id()) {
            return Ok(false);
        }
        let qty = sig.qty_centi_for_unowned()?;
        if qty == 0 {
            return Ok(false);
        }
        let venue_order_id = if sig.order_id().trim().is_empty() {
            None
        } else {
            Some(VenueOrderId(sig.order_id().clone()))
        };
        journal.apply_effects(&[Effect::AppendFsync(fill_journal_record(
            OmsClientOrderId(sig.client_order_id().as_str().to_string()),
            FillId(sig.trade_id().clone()),
            qty,
            OmsPriceE4::from_dollars(sig.yes_price())
                .map(OmsPriceE4::into_wire_price_cents)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "off-book fill price {} 不能表示为 OMS 1e-4 price units —— 拒绝按 0 入账",
                        sig.yes_price()
                    )
                })?,
            sig.venue_ts_ns()?,
            None,
            venue_order_id,
        ))])?;
        self.seen_fill_ids.insert(sig.trade_id().clone());
        Ok(true)
    }
}
