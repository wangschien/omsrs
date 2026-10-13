//! Neutral monetary accumulation and journal contracts, version 1.
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct FeeError(pub String);
type Result<T> = std::result::Result<T, FeeError>;
macro_rules! fee_error { ($($arg:tt)*) => { FeeError(format!($($arg)*)) }; }
macro_rules! ensure { ($ok:expr, $($arg:tt)*) => { if !$ok { return Err(fee_error!($($arg)*)); } }; }

/// Cash is signed before fee; trade fee has already been rounded by the caller.
/// Quantity, venue price rules and fee schedules belong to the caller.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeeInput {
    pub revenue: Decimal,
    pub trade_fee: Decimal,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BalancePrecision {
    Cent,
    Centicent,
}

impl BalancePrecision {
    fn scale(self) -> Decimal {
        match self {
            Self::Cent => Decimal::new(1, 2),
            Self::Centicent => Decimal::new(1, 4),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OrderFillFeeBreakdown {
    pub trade_fee: Decimal,
    pub rounding_fee: Decimal,
    pub rebate: Decimal,
    pub net_fee: Decimal,
    pub balance_delta: Decimal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "OrderFeeAccumulatorRepr")]
pub struct OrderFeeAccumulator {
    pub precision: BalancePrecision,
    pub rounding_acc: Decimal,
    total_trade_fee: Decimal,
    total_rounding_fee: Decimal,
    total_rebate: Decimal,
    pub fill_count: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct OrderFeeAccumulatorRepr {
    precision: BalancePrecision,
    rounding_acc: Decimal,
    total_trade_fee: Decimal,
    total_rounding_fee: Decimal,
    total_rebate: Decimal,
    fill_count: u64,
}

impl TryFrom<OrderFeeAccumulatorRepr> for OrderFeeAccumulator {
    type Error = String;

    fn try_from(r: OrderFeeAccumulatorRepr) -> std::result::Result<Self, Self::Error> {
        let acc = Self {
            precision: r.precision,
            rounding_acc: r.rounding_acc,
            total_trade_fee: r.total_trade_fee,
            total_rounding_fee: r.total_rounding_fee,
            total_rebate: r.total_rebate,
            fill_count: r.fill_count,
        };
        acc.validate().map_err(|e| e.to_string())?;
        Ok(acc)
    }
}

impl OrderFeeAccumulator {
    pub fn new(precision: BalancePrecision) -> Self {
        Self {
            precision,
            rounding_acc: Decimal::ZERO,
            total_trade_fee: Decimal::ZERO,
            total_rounding_fee: Decimal::ZERO,
            total_rebate: Decimal::ZERO,
            fill_count: 0,
        }
    }

    #[inline]
    pub fn total_trade_fee(&self) -> Decimal {
        self.total_trade_fee
    }

    #[inline]
    pub fn total_rounding_fee(&self) -> Decimal {
        self.total_rounding_fee
    }

    #[inline]
    pub fn total_rebate(&self) -> Decimal {
        self.total_rebate
    }

    pub fn validate(&self) -> Result<()> {
        for (name, v) in [
            ("rounding_acc", self.rounding_acc),
            ("total_trade_fee", self.total_trade_fee),
            ("total_rounding_fee", self.total_rounding_fee),
            ("total_rebate", self.total_rebate),
        ] {
            ensure!(
                v != Decimal::MAX && v != Decimal::MIN,
                "OrderFeeAccumulator.{name} out of range (got {v})"
            );
            ensure!(
                v >= Decimal::ZERO,
                "OrderFeeAccumulator.{name} must be non-negative (got {v})"
            );
        }

        let cent = Decimal::new(1, 2);
        ensure!(
            self.rounding_acc < cent,
            "OrderFeeAccumulator.rounding_acc must be < {cent} residual (got {})",
            self.rounding_acc
        );

        self.total_trade_fee
            .checked_add(self.total_rounding_fee)
            .and_then(|s| s.checked_sub(self.total_rebate))
            .ok_or_else(|| {
                fee_error!("OrderFeeAccumulator economic net overflow (corrupted totals)")
            })?;
        Ok(())
    }

    pub fn economic_net_fee(&self) -> Decimal {
        self.total_trade_fee
            .checked_add(self.total_rounding_fee)
            .and_then(|s| s.checked_sub(self.total_rebate))
            .expect("OrderFeeAccumulator economic net overflow; state must pass validate()")
    }

    #[inline]
    pub fn total_net_fee(&self) -> Decimal {
        self.economic_net_fee()
    }

    pub fn apply_fill(&mut self, input: FeeInput) -> Result<OrderFillFeeBreakdown> {
        let FeeInput { revenue, trade_fee } = input;
        ensure!(trade_fee >= Decimal::ZERO, "trade_fee must be non-negative");
        let balance_change = revenue
            .checked_sub(trade_fee)
            .ok_or_else(|| fee_error!("balance_change overflow"))?;
        let balance_delta = floor_to_precision(balance_change, self.precision)?;

        let rounding_fee = balance_change
            .checked_sub(balance_delta)
            .ok_or_else(|| fee_error!("rounding_fee overflow"))?;
        ensure!(
            rounding_fee >= Decimal::ZERO,
            "rounding_fee must be non-negative (got {rounding_fee})"
        );

        let mut new_rounding_acc = self
            .rounding_acc
            .checked_add(rounding_fee)
            .ok_or_else(|| fee_error!("rounding_acc overflow"))?;
        let cent = Decimal::new(1, 2);

        let cents = new_rounding_acc
            .checked_div(cent)
            .ok_or_else(|| fee_error!("rounding drain div overflow"))?
            .floor();
        let rebate = if cents > Decimal::ZERO {
            cents
                .checked_mul(cent)
                .ok_or_else(|| fee_error!("rebate overflow"))?
        } else {
            Decimal::ZERO
        };
        new_rounding_acc = new_rounding_acc
            .checked_sub(rebate)
            .ok_or_else(|| fee_error!("rounding_acc drain underflow"))?;

        let economic_net = trade_fee
            .checked_add(rounding_fee)
            .and_then(|s| s.checked_sub(rebate))
            .ok_or_else(|| fee_error!("per-fill economic_net overflow"))?;

        let net_fee = economic_net.max(Decimal::ZERO);

        let new_total_trade_fee = self
            .total_trade_fee
            .checked_add(trade_fee)
            .ok_or_else(|| fee_error!("total_trade_fee overflow"))?;
        let new_total_rounding_fee = self
            .total_rounding_fee
            .checked_add(rounding_fee)
            .ok_or_else(|| fee_error!("total_rounding_fee overflow"))?;
        let new_total_rebate = self
            .total_rebate
            .checked_add(rebate)
            .ok_or_else(|| fee_error!("total_rebate overflow"))?;
        let new_fill_count = self
            .fill_count
            .checked_add(1)
            .ok_or_else(|| fee_error!("fill_count overflow"))?;

        new_total_trade_fee
            .checked_add(new_total_rounding_fee)
            .and_then(|s| s.checked_sub(new_total_rebate))
            .ok_or_else(|| fee_error!("economic net would overflow after fill"))?;

        self.rounding_acc = new_rounding_acc;
        self.total_trade_fee = new_total_trade_fee;
        self.total_rounding_fee = new_total_rounding_fee;
        self.total_rebate = new_total_rebate;
        self.fill_count = new_fill_count;

        Ok(OrderFillFeeBreakdown {
            trade_fee,
            rounding_fee,
            rebate,
            net_fee,
            balance_delta,
        })
    }
}
pub fn floor_to_precision(amount: Decimal, precision: BalancePrecision) -> Result<Decimal> {
    let unit = precision.scale();
    let scaled = amount
        .checked_div(unit)
        .ok_or_else(|| fee_error!("floor_to_precision overflow (amount/unit)"))?;
    scaled
        .floor()
        .checked_mul(unit)
        .ok_or_else(|| fee_error!("floor_to_precision overflow (floor*unit)"))
}
/// The foreground durability boundary is selected explicitly at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalDurability {
    SyncEveryCall,
    BackgroundFsync,
}

/// A background append never certifies completion on stable storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurabilityAck {
    Volatile {
        records_end: u64,
    },
    PageCache {
        records_end: u64,
    },
    StableStorage {
        records_end: u64,
    },
    /// An observed fence error makes this prefix uncertain even after retry.
    Uncertain {
        records_end: u64,
        fence_errors: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalOperation {
    PrepareResource,
    RecoveryRead,
    RepairTruncate,
    RepairFence,
    OpenWriter,
    Metadata,
    PaddingWrite,
    PaddingFence,
    AppendWrite,
    AppendFence,
    ExplicitFence,
    BackgroundFence,
    ShutdownFence,
    SpawnWorker,
}

#[derive(Debug)]
pub enum RecoveryReason<'a> {
    Hole,
    InvalidUtf8,
    Json {
        error: &'a serde_json::Error,
        line: &'a str,
    },
}

/// Emitted synchronously before any repair, including a repair that fails.
#[derive(Debug)]
pub struct RecoveryDiagnostic<'a> {
    pub line: usize,
    pub records_end: u64,
    pub discarded_bytes: u64,
    pub reason: &'a RecoveryReason<'a>,
    pub preview: Option<&'a str>,
}

/// A port formats/emits these facts; it cannot choose recovery or retry policy.
#[derive(Debug)]
pub enum JournalDiagnostic<'a> {
    Recovery(RecoveryDiagnostic<'a>),
    BackgroundFenceFailure {
        operation: JournalOperation,
        attempt: u32,
        error: &'a std::io::Error,
    },
    ShutdownFenceFailure {
        operation: JournalOperation,
        error: &'a std::io::Error,
    },
    PreallocationExhausted {
        count: u64,
        used_bytes: u64,
        startup_prealloc_bytes: u64,
        exceeded_count: u64,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("journal {operation:?}: {source}")]
    Io {
        operation: JournalOperation,
        #[source]
        source: std::io::Error,
    },
    #[error("journal serialization: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("journal line {line} has an unknown variant: {message}")]
    UnknownVariant { line: usize, message: String },
    #[error("prepared journal transaction belongs to a different prefix")]
    StalePreparation,
}

#[derive(Debug, Clone)]
pub struct RecoveredJournal {
    pub records: Vec<crate::lifecycle::JournalRecord>,
    pub dropped_torn: bool,
    pub records_end: u64,
    pub discarded_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn g1_fill_rejects_when_post_fill_economic_net_would_overflow() {
        let mut acc = OrderFeeAccumulator {
            precision: BalancePrecision::Cent,
            rounding_acc: Decimal::ZERO,
            total_trade_fee: Decimal::MAX - Decimal::ONE,
            total_rounding_fee: Decimal::ONE,
            total_rebate: Decimal::ZERO,
            fill_count: 0,
        };
        acc.validate().unwrap();
        let before = acc.clone();
        let err = acc
            .apply_fill(FeeInput {
                revenue: dec!(0.5),
                trade_fee: Decimal::ONE,
            })
            .unwrap_err();
        assert!(err
            .to_string()
            .contains("economic net would overflow after fill"));
        assert_eq!(acc, before);
        assert_eq!(acc.economic_net_fee(), Decimal::MAX);
    }

    #[test]
    fn extreme_inputs_and_corrupt_state_private_validation_slice() {
        let mut damaged = OrderFeeAccumulator::new(BalancePrecision::Cent);
        damaged.total_trade_fee = Decimal::MAX;
        damaged.total_rounding_fee = Decimal::ONE;
        assert!(damaged.validate().is_err());
        let mut damaged_neg = OrderFeeAccumulator::new(BalancePrecision::Cent);
        damaged_neg.total_rebate = dec!(-1);
        assert!(damaged_neg.validate().is_err());
    }
}
