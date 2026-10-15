//! Exact decimal close proofs and cleanup result values.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketPositionRestSnapshot {
    pub net_yes: Decimal, // position_fp（signed YES 张）——**必填**，缺则调用方 Err
    pub realized_pnl: Option<Decimal>, // realized_pnl_dollars（含 flatten）；缺⇒None（不折 0）
    pub fees_paid: Option<Decimal>, // fees_paid_dollars；缺⇒None
    pub market_exposure_dollars: Option<Decimal>, // final residual position cost basis
    pub total_traded_dollars: Decimal, // total_traded_dollars（成交额，$）
    pub last_updated_ts: Option<String>, // venue position timestamp; None is not fabricated
}

/// Convert the post-cleanup venue snapshot into the closed-window amount used
/// by S1. A residual position is charged its complete venue-reported cost
/// basis as a loss, since it may settle worthless. Missing final financials or
/// missing residual cost basis is unprovable rather than a cached-value fallback.
pub fn post_cleanup_window_pnl(snapshot: Option<&MarketPositionRestSnapshot>) -> Option<Decimal> {
    let snapshot = snapshot?;
    let realized_minus_fees = snapshot.realized_pnl?.checked_sub(snapshot.fees_paid?)?;
    if snapshot.net_yes.is_zero() {
        return Some(realized_minus_fees);
    }
    let cost_basis = snapshot.market_exposure_dollars?;
    if cost_basis <= Decimal::ZERO {
        return None;
    }
    realized_minus_fees.checked_sub(cost_basis)
}

/// Settlement row for one market.
/// `revenue_cents` is integer cents; all other money is fixed-point dollars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementRow {
    pub yes_count: Decimal,
    pub yes_total_cost: Decimal,
    pub no_count: Decimal,
    pub no_total_cost: Decimal,
    pub revenue_cents: i64,
    pub fee_cost: Decimal,
}

/// Window P&L implied by a settlement row:
/// revenue + min(yes, no) × $1 − yes cost − no cost − fee.
pub fn settlement_pnl(row: &SettlementRow) -> Option<Decimal> {
    let paired = row.yes_count.min(row.no_count);
    Decimal::new(row.revenue_cents, 2)
        .checked_add(paired)?
        .checked_sub(row.yes_total_cost)?
        .checked_sub(row.no_total_cost)?
        .checked_sub(row.fee_cost)
}

/// Signed YES-equivalent position and realized cash P&L (minus fees) from an
/// exhaustive venue fills list: `outcome_side=yes` is +count paid at the YES
/// price, `outcome_side=no` is −count received at the YES price. Any missing or
/// unparseable field, or a foreign ticker, makes the whole list unusable.
pub fn fills_position_and_pnl(
    fills: &[serde_json::Value],
    ticker: &str,
) -> Option<(Decimal, Decimal)> {
    let dec = |row: &serde_json::Value, key: &str| -> Option<Decimal> {
        Decimal::from_str(row.get(key)?.as_str()?.trim()).ok()
    };
    let mut net = Decimal::ZERO;
    let mut pnl = Decimal::ZERO;
    for row in fills {
        let row_ticker = row
            .get("ticker")
            .or_else(|| row.get("market_ticker"))?
            .as_str()?;
        if row_ticker != ticker {
            return None;
        }
        let count = dec(row, "count_fp")?;
        let yes_price = dec(row, "yes_price_dollars")?;
        let fee = dec(row, "fee_cost")?;
        let notional = count.checked_mul(yes_price)?;
        match row.get("outcome_side")?.as_str()? {
            "yes" => {
                net = net.checked_add(count)?;
                pnl = pnl.checked_sub(notional)?;
            }
            "no" => {
                net = net.checked_sub(count)?;
                pnl = pnl.checked_add(notional)?;
            }
            _ => return None,
        }
        pnl = pnl.checked_sub(fee)?;
    }
    Some((net, pnl))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbsentRowProof {
    Proven(Decimal),
    /// Non-flat by fills and no settlement row yet: retry, then Unprovable.
    AwaitSettlement,
    Unprovable(&'static str),
}

/// Close proof when the positions row is gone (it vanishes at settlement):
/// exhaustive fills prove the window; an existing settlement row must agree.
pub fn prove_absent_row(
    fills: &[serde_json::Value],
    exhaustive: bool,
    ticker: &str,
    window_flat: bool,
    settlement: Option<&SettlementRow>,
    local: &LocalFills,
) -> AbsentRowProof {
    if !exhaustive {
        return AbsentRowProof::Unprovable("fills not exhaustive");
    }
    if !local.covered_by(fills) {
        return AbsentRowProof::AwaitSettlement;
    }
    let Some((net, fills_pnl)) = fills_position_and_pnl(fills, ticker) else {
        return AbsentRowProof::Unprovable("fills unparseable or foreign");
    };
    let Some(row) = settlement else {
        // The fills-only zero proof is for a FLAT window only: a held window (or an unproven flat
        // bit) with lagged/empty fills must wait for its settlement row.
        return if net.is_zero() && window_flat {
            AbsentRowProof::Proven(fills_pnl)
        } else {
            AbsentRowProof::AwaitSettlement
        };
    };
    let Some(settled_pnl) = settlement_pnl(row) else {
        return AbsentRowProof::Unprovable("settlement arithmetic");
    };
    if row.yes_count.checked_sub(row.no_count) != Some(net) {
        return AbsentRowProof::Unprovable("settlement counts disagree with fills");
    }
    if net.is_zero() {
        if settled_pnl == fills_pnl {
            AbsentRowProof::Proven(fills_pnl)
        } else {
            AbsentRowProof::Unprovable("settlement P&L disagrees with fills")
        }
    } else {
        // Held into settlement: the settlement row values the whole window.
        AbsentRowProof::Proven(settled_pnl)
    }
}

/// What OUR OWN OMS already knows about this window's fills (S1-M2): the venue
/// view used for a proof must be at least as new as this, otherwise a jointly
/// lagged positions row + fills list could release a loss the OMS has seen.
#[derive(Debug, Clone, Default)]
pub struct LocalFills {
    pub ids: std::collections::BTreeSet<String>,
    pub latest: Option<DateTime<Utc>>,
    /// S1-M2 r3: the CANONICAL inventory (every execution this window, including VenueOnly
    /// ones that have no order row). Enumeration is not exposed, so coverage is a count check.
    pub inventory: Option<crate::execution::MarketInventory>,
}

impl LocalFills {
    /// Every locally recorded fill id must be present in the venue fills list.
    pub fn covered_by(&self, fills: &[serde_json::Value]) -> bool {
        let ids_ok = self.ids.iter().all(|id| {
            fills.iter().any(|f| {
                ["fill_id", "trade_id"]
                    .iter()
                    .any(|k| f.get(*k).and_then(|v| v.as_str()) == Some(id.as_str()))
            })
        });
        if !ids_ok {
            return false;
        }
        let Some(inventory) = self.inventory.as_ref() else {
            return true;
        };
        // Every execution the canonical inventory holds must be in the venue fills: the number
        // of DISTINCT venue fill ids the inventory knows must equal its execution count.
        let known: std::collections::BTreeSet<&str> = fills
            .iter()
            .filter_map(|f| {
                ["fill_id", "trade_id"]
                    .iter()
                    .find_map(|k| f.get(*k).and_then(|v| v.as_str()))
            })
            .filter(|id| {
                inventory
                    .get(&crate::lifecycle::FillId((*id).to_string()))
                    .is_some()
            })
            .collect();
        known.len() >= inventory.view().executions
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresentRowProof {
    Proven(Decimal),
    /// The positions row is older than the window's last fill: retry, then Unprovable.
    Stale,
    Unprovable(&'static str),
}

/// LI-M6: a FLAT positions row is used only when it is at least as new as the
/// window's last fill and agrees with the exhaustive venue fills; the fills are
/// the primary value (they are what the row must reproduce).
pub fn prove_present_flat_row(
    row: &MarketPositionRestSnapshot,
    fills: &[serde_json::Value],
    exhaustive: bool,
    ticker: &str,
    local: &LocalFills,
) -> PresentRowProof {
    if !exhaustive {
        return PresentRowProof::Unprovable("fills not exhaustive");
    }
    let Some((net, fills_pnl)) = fills_position_and_pnl(fills, ticker) else {
        return PresentRowProof::Unprovable("fills unparseable or foreign");
    };
    if !net.is_zero() || !row.net_yes.is_zero() {
        return PresentRowProof::Unprovable("not flat by fills and row");
    }
    // The venue fills must contain every fill our OMS already booked (S1-M2).
    if !local.covered_by(fills) {
        return PresentRowProof::Stale;
    }
    let mut last_fill = local.latest;
    for fill in fills {
        let parsed = fill
            .get("created_time")
            .and_then(|v| v.as_str())
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc));
        let Some(t) = parsed else {
            return PresentRowProof::Unprovable("fill without created_time");
        };
        last_fill = Some(last_fill.map_or(t, |prev: DateTime<Utc>| prev.max(t)));
    }
    if let Some(last_fill) = last_fill {
        let row_time = row
            .last_updated_ts
            .as_deref()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc));
        match row_time {
            Some(row_time) if row_time >= last_fill => {}
            _ => return PresentRowProof::Stale,
        }
    }
    let row_pnl = match (row.realized_pnl, row.fees_paid) {
        (Some(realized), Some(fees)) => realized.checked_sub(fees),
        _ => None,
    };
    if row_pnl != Some(fills_pnl) {
        return PresentRowProof::Unprovable("positions row disagrees with fills");
    }
    PresentRowProof::Proven(fills_pnl)
}

/// Signed net (contracts) -> (side, |net| in centi-contracts); None when flat.
pub fn flatten_target(net_yes: Decimal) -> Option<(crate::lifecycle::Side, u64)> {
    let centi = (net_yes * Decimal::from(100)).round();
    if centi.is_zero() {
        return None;
    }
    let side = if centi.is_sign_positive() {
        crate::lifecycle::Side::SellYes
    } else {
        crate::lifecycle::Side::BuyYes
    };
    let abs = u64::try_from(centi.abs()).ok()?;
    Some((side, abs))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepOrder {
    pub order_id: String,
    pub client_order_id: String,
    pub ticker: String,
    pub side: String,
    pub price: String,
    pub remaining: String,
    pub subaccount_number: Option<i64>,
    pub exchange_index: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestingOrderSnapshot {
    /// All resting orders returned for the explicitly requested child account.
    pub orders: Vec<SweepOrder>,
    pub exhaustive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SweepReport {
    pub initial_seen: usize,
    pub scope_candidates: usize,
    pub cancel_attempted: usize,
    pub cancel_accepted: usize,
    pub cancel_errors: usize,
    pub remaining: usize,
    pub verification_exhaustive: bool,
}

impl SweepReport {
    pub fn complete(&self) -> bool {
        self.verification_exhaustive && self.remaining == 0 && self.cancel_errors == 0
    }
}

/// A price in `1e-4 USD` units. `0.0850 USD` is represented as `850`.
///
/// omsrs retains the legacy wire field name `price_cents`; crossing that seam
/// must be explicit so price units cannot be confused with actual fee cents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OmsPriceE4(u64);

impl OmsPriceE4 {
    pub const SCALE: u32 = 10_000;

    /// Dollars → OMS price units. Non-exact grid ⇒ `None` (no rounding).
    pub fn from_dollars(d: Decimal) -> Option<Self> {
        if d.is_sign_negative() {
            return None;
        }
        let scaled = d * Decimal::from(Self::SCALE);
        if scaled != scaled.trunc() {
            return None;
        }
        scaled.to_u64().map(Self)
    }

    /// Read the legacy omsrs `price_cents` field, whose actual unit is 1e-4 USD.
    pub const fn from_wire_price_cents(units: u64) -> Self {
        Self(units)
    }

    /// Write the legacy omsrs `price_cents` field without changing its schema.
    pub const fn into_wire_price_cents(self) -> u64 {
        self.0
    }

    pub fn to_dollars(self) -> Decimal {
        Decimal::from(self.0) / Decimal::from(Self::SCALE)
    }
}

/// Spec Step C: venue fee **dollars** → OMS **integer cents** (not price units).
/// Fees stay on 1¢ grain; prices use [`OmsPriceE4`].
pub fn fee_dollars_to_cents(d: Decimal) -> Option<u64> {
    if d.is_sign_negative() {
        return None;
    }
    let cents = (d * Decimal::from(100)).round();
    if cents.is_sign_negative() {
        return None;
    }
    cents.to_u64()
}
