//! Parity-gate arithmetic + `excused.toml` validation. Factored out so the
//! smoke runner (`tests/parity_runner_smoke`) can drive the same logic with
//! injected fixtures without duplicating the real parity harness (v11 audit
//! non-blocking note).
//!
//! TOML parsing is **not** done here — the `toml` crate is a dev-dependency
//! (PORT-PLAN §7), so the test harness does `toml::from_str` and then hands
//! parsed rows to [`validate_excused`]. This keeps the library TOML-free.
//!
//! Exit-code contract (PORT-PLAN §4.1.2 + §4.1.3):
//!
//! | code | meaning |
//! |---:|---|
//! | 0 | gate passes |
//! | 1 | failing ⊄ excused, or passing < required floor |
//! | 2 | duplicate id in `excused.toml` |
//! | 3 | excused id not present in manifest |
//! | 4 | `OMSRS_R0_GATE=1` set but `excused.toml` non-empty |
//! | 5 | `|excused| > 7` |
//! | 6 | `excused.toml` exists but fails TOML parse / schema / missing required fields |
//!
//! The "required floor" is `|manifest| - EXCUSED_CAP`, which evaluates to
//! `237 - 7 = 230` at R10 under the frozen manifest — matching §4 "≥ 230 of
//! 237". Earlier phases with shorter manifests scale proportionally; the
//! 7-item slack from §4 is preserved as the constant offset.

use std::collections::HashSet;

use serde::Deserialize;

/// Maximum excused-set size permitted at the final gate (PORT-PLAN §4).
pub const EXCUSED_CAP: usize = 7;

/// Required floor at R10 under the frozen 237-item manifest. At any phase the
/// effective floor is `manifest_len.saturating_sub(EXCUSED_CAP)`; this constant
/// documents the R10 value the plan's "≥ 230" text refers to.
pub const R10_REQUIRED_FLOOR: usize = 230;

/// One `[[excused]]` row. All four fields are required — **no** `#[serde(default)]`
/// at the field level — so a missing key fails deserialization and routes to
/// exit 6 per PORT-PLAN §4.1.2.
#[derive(Debug, Clone, Deserialize)]
pub struct ExcusedRow {
    pub id: String,
    pub rationale: String,
    pub approved_at: String,
    pub approved_by: String,
}

/// Exit-code classification for the gate's final verdict. Mapped to process
/// exit via [`GateExit::code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateExit {
    Pass,
    RegressionOrShort,
    DuplicateExcused,
    UnknownExcusedId,
    R0GateViolation,
    ExcusedOverCap,
    TomlInvalid,
}

impl GateExit {
    pub fn code(self) -> i32 {
        match self {
            GateExit::Pass => 0,
            GateExit::RegressionOrShort => 1,
            GateExit::DuplicateExcused => 2,
            GateExit::UnknownExcusedId => 3,
            GateExit::R0GateViolation => 4,
            GateExit::ExcusedOverCap => 5,
            GateExit::TomlInvalid => 6,
        }
    }
}

/// Validate a parsed excused-row set against the manifest + env knobs.
///
/// Returns `Ok(excused_id_set)` when all checks pass (the ids are returned so
/// the caller can feed them straight into [`gate_arithmetic`]). On failure,
/// the first tripped rule wins — duplicate before unknown, unknown before R0
/// gate, R0 gate before cap.
pub fn validate_excused(
    excused: &[ExcusedRow],
    manifest_ids: &HashSet<&str>,
    r0_gate_enabled: bool,
) -> Result<HashSet<String>, GateExit> {
    let mut seen: HashSet<String> = HashSet::new();
    for row in excused {
        if !seen.insert(row.id.clone()) {
            return Err(GateExit::DuplicateExcused);
        }
        if !manifest_ids.contains(row.id.as_str()) {
            return Err(GateExit::UnknownExcusedId);
        }
    }
    if r0_gate_enabled && !seen.is_empty() {
        return Err(GateExit::R0GateViolation);
    }
    if seen.len() > EXCUSED_CAP {
        return Err(GateExit::ExcusedOverCap);
    }
    Ok(seen)
}

/// Final gate arithmetic. Returns [`GateExit::Pass`] iff every failing trial
/// is in the excused set, at least `manifest_len - EXCUSED_CAP` trials
/// passed, and `|excused| ≤ EXCUSED_CAP`.
pub fn gate_arithmetic(
    manifest_len: usize,
    passing: &HashSet<&str>,
    failing: &HashSet<&str>,
    excused: &HashSet<String>,
) -> GateExit {
    let required = manifest_len.saturating_sub(EXCUSED_CAP);
    let all_failures_excused = failing.iter().all(|id| excused.contains(*id));
    if all_failures_excused && passing.len() >= required && excused.len() <= EXCUSED_CAP {
        GateExit::Pass
    } else {
        GateExit::RegressionOrShort
    }
}
