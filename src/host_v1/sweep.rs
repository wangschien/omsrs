//! Deterministic scoped cleanup selection and report reduction.

use super::close_proof::{RestingOrderSnapshot, SweepOrder, SweepReport};
use anyhow::{Result, ensure};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy)]
pub struct SweepScopeRef<'a> {
    pub series: &'a str,
    pub subaccount: i64,
    pub ticker: Option<&'a str>,
}

impl SweepScopeRef<'_> {
    pub fn includes_ticker(&self, ticker: &str) -> bool {
        ticker.starts_with(&format!("{}-", self.series))
            && self
                .ticker
                .as_deref()
                .map_or(true, |selected| selected == ticker)
    }
}

pub fn prepare_sweep(
    initial: RestingOrderSnapshot,
    scope: SweepScopeRef<'_>,
) -> Result<(SweepReport, BTreeMap<String, SweepOrder>)> {
    ensure!(
        initial.exhaustive,
        "initial subaccount order list is not exhaustive; no cancels sent"
    );

    let mut report = SweepReport {
        initial_seen: initial.orders.len(),
        ..SweepReport::default()
    };
    let mut candidates = BTreeMap::<String, SweepOrder>::new();
    for order in initial.orders {
        ensure!(
            order.subaccount_number == Some(scope.subaccount),
            "resting order list contains an unknown or different subaccount; no cancels sent"
        );
        if !scope.includes_ticker(&order.ticker) {
            continue;
        }
        ensure!(
            !order.order_id.trim().is_empty(),
            "in-scope order has no order_id; no cancels sent"
        );
        candidates.entry(order.order_id.clone()).or_insert(order);
    }
    report.scope_candidates = candidates.len();

    Ok((report, candidates))
}

pub fn verify_sweep(
    report: &mut SweepReport,
    verification: RestingOrderSnapshot,
    scope: SweepScopeRef<'_>,
) -> Result<()> {
    ensure!(
        verification
            .orders
            .iter()
            .all(|order| order.subaccount_number == Some(scope.subaccount)),
        "verification list contains an unknown or different subaccount"
    );
    report.remaining = verification
        .orders
        .iter()
        .filter(|order| scope.includes_ticker(&order.ticker))
        .count();
    report.verification_exhaustive = verification.exhaustive;
    Ok(())
}

impl SweepReport {
    pub fn record_cancel_attempt(&mut self) {
        self.cancel_attempted = self.cancel_attempted.saturating_add(1);
    }
    pub fn record_cancel_accepted(&mut self) {
        self.cancel_accepted = self.cancel_accepted.saturating_add(1);
    }
    pub fn record_cancel_error(&mut self) {
        self.cancel_errors = self.cancel_errors.saturating_add(1);
    }
}
