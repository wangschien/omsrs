//! Self-test of the parity-gate runner (§4.1.5).
//!
//! 13-row smoke matrix covering every exit code 0–6 under the matched
//! condition. Uses a scripted fake manifest + pass/fail trial sets + inline
//! `excused.toml` sources, so changes to §4.1.2 exit semantics require a
//! matching change here (and vice versa).

use std::collections::HashSet;

use omsrs::parity_gate::{
    gate_arithmetic, validate_excused, ExcusedRow, GateExit,
};
use serde::Deserialize;

fn manifest() -> Vec<&'static str> {
    // Synthetic 13-id fake manifest — unrelated to R1's real parity list.
    vec![
        "t01", "t02", "t03", "t04", "t05", "t06", "t07", "t08", "t09", "t10", "t11", "t12", "t13",
    ]
}

fn manifest_set() -> HashSet<&'static str> {
    manifest().into_iter().collect()
}

fn all_pass() -> HashSet<&'static str> {
    manifest().into_iter().collect()
}

fn empty() -> HashSet<&'static str> {
    HashSet::new()
}

const R0_OFF: bool = false;
const R0_ON: bool = true;

/// TOML parse + validate + gate-arithmetic driver, mirroring the path the
/// real parity harness takes. Duplicating the 10-line glue (instead of
/// factoring into `src/parity_gate.rs`) keeps the library TOML-free.
fn drive(
    manifest: &[&str],
    manifest_set: &HashSet<&str>,
    excused_src: Option<&str>,
    passing: &HashSet<&str>,
    failing: &HashSet<&str>,
    r0: bool,
) -> GateExit {
    let rows = match parse_excused_toml(excused_src) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let excused = match validate_excused(&rows, manifest_set, r0) {
        Ok(s) => s,
        Err(e) => return e,
    };
    gate_arithmetic(manifest.len(), passing, failing, &excused)
}

fn parse_excused_toml(src: Option<&str>) -> Result<Vec<ExcusedRow>, GateExit> {
    let Some(body) = src else { return Ok(Vec::new()); };

    #[derive(Deserialize)]
    struct ExcusedFile {
        #[serde(default)]
        excused: Vec<ExcusedRow>,
    }
    toml::from_str::<ExcusedFile>(body)
        .map(|f| f.excused)
        .map_err(|_| GateExit::TomlInvalid)
}

// Row 1: absent excused.toml + all pass → exit 0
#[test]
fn row01_absent_all_pass_yields_zero() {
    let m = manifest();
    let gate = drive(&m, &manifest_set(), None, &all_pass(), &empty(), R0_OFF);
    assert_eq!(gate, GateExit::Pass);
    assert_eq!(gate.code(), 0);
}

// Row 2: present-empty (zero rows) + all pass → exit 0
#[test]
fn row02_present_empty_all_pass_yields_zero() {
    let m = manifest();
    let src = "# just comments, no rows\n";
    let gate = drive(&m, &manifest_set(), Some(src), &all_pass(), &empty(), R0_OFF);
    assert_eq!(gate, GateExit::Pass);
    assert_eq!(gate.code(), 0);
}

// Row 3: failing id ∈ excused, ≥ (|m|-7) pass, |excused| ≤ 7 → exit 0
#[test]
fn row03_excused_failure_within_budget_yields_zero() {
    let m = manifest();
    let mut p: HashSet<&str> = all_pass();
    p.remove("t05");
    let mut f: HashSet<&str> = HashSet::new();
    f.insert("t05");
    let src = r#"
[[excused]]
id = "t05"
rationale = "probabilistic flake"
approved_at = "R_smoke"
approved_by = "codex"
"#;
    let gate = drive(&m, &manifest_set(), Some(src), &p, &f, R0_OFF);
    assert_eq!(gate, GateExit::Pass);
    assert_eq!(gate.code(), 0);
}

// Row 4: failing id ∉ excused → exit 1
#[test]
fn row04_unexcused_failure_yields_one() {
    let m = manifest();
    let mut p: HashSet<&str> = all_pass();
    p.remove("t05");
    let mut f: HashSet<&str> = HashSet::new();
    f.insert("t05");
    let gate = drive(&m, &manifest_set(), None, &p, &f, R0_OFF);
    assert_eq!(gate, GateExit::RegressionOrShort);
    assert_eq!(gate.code(), 1);
}

// Row 5: excused id duplicated → exit 2
#[test]
fn row05_duplicate_excused_yields_two() {
    let m = manifest();
    let src = r#"
[[excused]]
id = "t05"
rationale = "first"
approved_at = "R_smoke"
approved_by = "codex"

[[excused]]
id = "t05"
rationale = "dup"
approved_at = "R_smoke"
approved_by = "codex"
"#;
    let gate = drive(&m, &manifest_set(), Some(src), &all_pass(), &empty(), R0_OFF);
    assert_eq!(gate, GateExit::DuplicateExcused);
    assert_eq!(gate.code(), 2);
}

// Row 6: excused id not in manifest → exit 3
#[test]
fn row06_unknown_excused_id_yields_three() {
    let m = manifest();
    let src = r#"
[[excused]]
id = "t99"
rationale = "not in manifest"
approved_at = "R_smoke"
approved_by = "codex"
"#;
    let gate = drive(&m, &manifest_set(), Some(src), &all_pass(), &empty(), R0_OFF);
    assert_eq!(gate, GateExit::UnknownExcusedId);
    assert_eq!(gate.code(), 3);
}

// Row 7: OMSRS_R0_GATE=1 + non-empty excused → exit 4
#[test]
fn row07_r0_gate_rejects_nonempty_excused_yields_four() {
    let m = manifest();
    let src = r#"
[[excused]]
id = "t05"
rationale = "any"
approved_at = "R_smoke"
approved_by = "codex"
"#;
    let mut p = all_pass();
    p.remove("t05");
    let mut f = HashSet::new();
    f.insert("t05");
    let gate = drive(&m, &manifest_set(), Some(src), &p, &f, R0_ON);
    assert_eq!(gate, GateExit::R0GateViolation);
    assert_eq!(gate.code(), 4);
}

// Row 8: |excused| > 7 → exit 5
#[test]
fn row08_excused_over_cap_yields_five() {
    let m = manifest();
    let src = r#"
[[excused]]
id = "t01"
rationale = "1"
approved_at = "R_smoke"
approved_by = "codex"
[[excused]]
id = "t02"
rationale = "2"
approved_at = "R_smoke"
approved_by = "codex"
[[excused]]
id = "t03"
rationale = "3"
approved_at = "R_smoke"
approved_by = "codex"
[[excused]]
id = "t04"
rationale = "4"
approved_at = "R_smoke"
approved_by = "codex"
[[excused]]
id = "t05"
rationale = "5"
approved_at = "R_smoke"
approved_by = "codex"
[[excused]]
id = "t06"
rationale = "6"
approved_at = "R_smoke"
approved_by = "codex"
[[excused]]
id = "t07"
rationale = "7"
approved_at = "R_smoke"
approved_by = "codex"
[[excused]]
id = "t08"
rationale = "8"
approved_at = "R_smoke"
approved_by = "codex"
"#;
    let gate = drive(&m, &manifest_set(), Some(src), &all_pass(), &empty(), R0_OFF);
    assert_eq!(gate, GateExit::ExcusedOverCap);
    assert_eq!(gate.code(), 5);
}

// Row 9: malformed TOML (syntactically invalid) → exit 6
#[test]
fn row09_malformed_toml_yields_six() {
    let m = manifest();
    let src = "[[excused\nid = \"t01\""; // unterminated array-of-tables header
    let gate = drive(&m, &manifest_set(), Some(src), &all_pass(), &empty(), R0_OFF);
    assert_eq!(gate, GateExit::TomlInvalid);
    assert_eq!(gate.code(), 6);
}

// Row 10: well-formed TOML, wrong shape (`excused` is a string) → exit 6
#[test]
fn row10_wrong_shape_yields_six() {
    let m = manifest();
    let src = r#"excused = "should be array""#;
    let gate = drive(&m, &manifest_set(), Some(src), &all_pass(), &empty(), R0_OFF);
    assert_eq!(gate, GateExit::TomlInvalid);
    assert_eq!(gate.code(), 6);
}

// Row 11: row missing `rationale` → exit 6
#[test]
fn row11_missing_rationale_yields_six() {
    let m = manifest();
    let src = r#"
[[excused]]
id = "t05"
approved_at = "R_smoke"
approved_by = "codex"
"#;
    let gate = drive(&m, &manifest_set(), Some(src), &all_pass(), &empty(), R0_OFF);
    assert_eq!(gate, GateExit::TomlInvalid);
    assert_eq!(gate.code(), 6);
}

// Row 12: row missing `approved_at` → exit 6
#[test]
fn row12_missing_approved_at_yields_six() {
    let m = manifest();
    let src = r#"
[[excused]]
id = "t05"
rationale = "x"
approved_by = "codex"
"#;
    let gate = drive(&m, &manifest_set(), Some(src), &all_pass(), &empty(), R0_OFF);
    assert_eq!(gate, GateExit::TomlInvalid);
    assert_eq!(gate.code(), 6);
}

// Row 13: row missing `approved_by` → exit 6
#[test]
fn row13_missing_approved_by_yields_six() {
    let m = manifest();
    let src = r#"
[[excused]]
id = "t05"
rationale = "x"
approved_at = "R_smoke"
"#;
    let gate = drive(&m, &manifest_set(), Some(src), &all_pass(), &empty(), R0_OFF);
    assert_eq!(gate, GateExit::TomlInvalid);
    assert_eq!(gate.code(), 6);
}
