//! Create-response quantities and financial values from the original fills.
use super::close_proof::OmsPriceE4;
use super::live_admission::AdmissionIntent;
use super::live_cover::CoverageClient;
use crate::lifecycle::VenueOrderId;
use anyhow::{Result, bail};
use rust_decimal::Decimal;

/// Borrowed access to a fill; no second fill/order collection is built.
pub trait CreateFillInput {
    fn qty_centi(&self) -> u64;
    fn qty_contracts_decimal(&self) -> Decimal;
    fn price(&self) -> Decimal;
}
pub trait CreateAckInput {
    type Fill<'a>: CreateFillInput
    where
        Self: 'a;
    type Fills<'a>: Iterator<Item = Self::Fill<'a>>
    where
        Self: 'a;
    fn fills(&self) -> Self::Fills<'_>;
    fn resting(&self) -> bool;
    fn venue_order_id(&self) -> VenueOrderId;
    fn create_fee_known(&self) -> bool;
    fn create_fee_cents(&self) -> Option<u64>;
}
pub fn create_response_counts<C: CoverageClient, I: AdmissionIntent<C>, A: CreateAckInput>(
    intent: &I,
    ack: &A,
) -> Result<(u64, u64, Option<OmsPriceE4>, Option<u64>)> {
    let mut fill_count = 0u64;
    let mut notional = Decimal::ZERO;
    for f in ack.fills() {
        let fill_centi = f.qty_centi();
        fill_count = fill_count.saturating_add(fill_centi);
        notional += f.price() * f.qty_contracts_decimal();
    }
    let remaining = if ack.resting() {
        intent.qty_centi().saturating_sub(fill_count)
    } else {
        0
    };
    let avg_price_e4 = if fill_count > 0 {
        let avg = notional / (Decimal::from(fill_count) / Decimal::from(100u64));
        OmsPriceE4::from_dollars(avg)
    } else {
        None
    };
    let fee_cents = if fill_count == 0 {
        None
    } else if ack.create_fee_known() {
        match ack.create_fee_cents() {
            Some(c) => Some(c),
            None => {
                bail!(
                    "create_fee_known=true but create_fee_cents is None (client={})",
                    intent.client_order_id().as_str()
                );
            }
        }
    } else {
        None
    };
    Ok((fill_count, remaining, avg_price_e4, fee_cents))
}
