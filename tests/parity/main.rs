//! Parity-target entry point. Custom `libtest-mimic` harness so we can enforce
//! the §4.1.3 gate arithmetic on top of pass/fail outcomes (stable Rust,
//! `harness = false` in `Cargo.toml`).
//!
//! Manifest is embedded at compile time via `include_str!(...)`; excused rows
//! are read from `tests/parity/excused.toml` at startup. Gate logic lives in
//! `omsrs::parity_gate` so `tests/parity_runner_smoke` can drive it with
//! injected fixtures without duplicating this file.

use std::collections::{BTreeSet, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Mutex;

use libtest_mimic::{Arguments, Failed, Trial};
use omsrs::parity_gate::{
    gate_arithmetic, validate_excused, ExcusedRow, GateExit,
};
use serde::Deserialize;

mod fixtures;
mod mock_broker;
mod test_models;
mod test_order;
mod test_utils;

use test_models::*;
use test_order::*;
use test_utils::*;

const MANIFEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/rust-tests/parity-item-manifest.txt"
));

/// Register a flat list of `fn()` parity tests by their *leaf* identifier
/// (must match the pytest-id mapped into `rust-tests/parity-item-manifest.txt`).
/// The macro emits:
///  - `PARITY_TEST_NAMES: &[&str]` — for manifest cross-check
///  - `build_parity_trials() -> Vec<Trial>` — wraps each fn with pass/fail
///    tracking via the static registries in this module.
macro_rules! register_parity_tests {
    ($($name:ident),* $(,)?) => {
        const PARITY_TEST_NAMES: &[&str] = &[ $( stringify!($name) ),* ];

        fn build_parity_trials() -> Vec<Trial> {
            vec![ $( wrap_trial(stringify!($name), $name) ),* ]
        }
    };
}

register_parity_tests!(
    test_create_basic_positions_from_orders_dict_keys,
    test_create_basic_positions_from_orders_dict_quantity,
    test_create_basic_positions_from_orders_dict_value,
    test_create_basic_positions_from_orders_dict_qty_non_match,
    test_empty_dict,
    test_identity_dict,
    test_simple_dict,
    test_no_matching_dict,
    test_filter_one,
    test_filter_two,
    test_multi_filter,
    test_update_quantity_case0,
    test_update_quantity_case1,
    test_update_quantity_case2,
    test_update_quantity_case3,
    test_update_quantity_case4,
    test_update_quantity_case5,
    test_basic_position,
    test_basic_position_calculations,
    test_basic_position_zero_quantity,
    // R2
    test_order_book,
    test_orderbook_is_bid_ask,
    test_orderbook_spread,
    test_orderbook_total_bid_ask_quantity,
    test_order_lock_defaults,
    test_order_lock_methods,
    test_order_lock_methods_max_duration,
    test_order_lock_can_methods_can_create,
    test_order_lock_can_methods_can_modify,
    test_order_lock_can_methods_can_cancel,
    // R3.a
    test_order_simple,
    test_order_id_custom,
    test_order_is_complete,
    test_order_is_complete_other_cases,
    test_order_is_pending,
    test_order_is_pending_canceled,
    test_order_is_pending_rejected,
    test_order_is_done,
    test_order_is_done_not_complete,
    test_order_has_parent,
    test_order_update_simple,
    test_order_update_timestamp,
    test_order_update_non_attribute,
    test_order_update_do_not_update_when_complete,
    test_order_update_do_not_update_rejected_order,
    test_order_update_do_not_update_cancelled_order,
    test_order_update_do_not_update_timestamp_for_completed_orders,
    test_order_update_pending_quantity,
    test_order_update_pending_quantity_in_data,
    test_order_expires,
    test_order_expiry_times,
    test_order_has_expired,
    test_simple_order_execute,
    test_simple_order_execute_kwargs,
    test_simple_order_execute_do_not_update_existing_kwargs,
    test_simple_order_do_not_execute_more_than_once,
    test_simple_order_do_not_execute_completed_order,
    test_simple_order_modify,
    test_simple_order_cancel,
    test_simple_order_cancel_none,
    test_order_modify_quantity,
    test_order_modify_by_attribute,
    test_order_modify_extra_attributes,
    test_order_modify_frozen,
    test_order_max_modifications,
    test_order_max_modifications_change_default,
    test_order_clone,
    test_order_clone_new_timestamp,
    test_order_timezone,
    test_order_lock_no_lock,
    test_order_lock_modify_and_cancel,
    test_order_lock_cancel,
    test_order_modify_args_to_add,
    test_order_modify_args_to_add_no_args,
    test_order_modify_args_to_add_override,
    test_order_modify_args_dont_modify_frozen,
    test_order_execute_attribs_to_copy,
    test_order_execute_attribs_to_copy_broker,
    test_order_execute_attribs_to_copy_broker2,
    test_order_execute_attribs_to_copy_override,
    test_get_other_args_from_attribs,
    test_order_modify_attribs_to_copy_broker,
    test_order_cancel_attribs_to_copy_broker,
    test_order_do_not_save_to_db_if_no_connection,
    test_order_save_to_db_dont_update_order_no_connection,
);

static PASSED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
static FAILED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn wrap_trial(name: &'static str, f: fn()) -> Trial {
    Trial::test(name, move || {
        let result = catch_unwind(AssertUnwindSafe(f));
        match result {
            Ok(()) => {
                PASSED.lock().unwrap().insert(name.to_string());
                Ok(())
            }
            Err(panic) => {
                FAILED.lock().unwrap().insert(name.to_string());
                let msg = panic_msg(panic);
                Err(Failed::from(msg))
            }
        }
    })
}

fn panic_msg(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "test panicked".to_string()
    }
}

fn main() -> ExitCode {
    let manifest_ids: Vec<&str> = parse_manifest(MANIFEST);
    let manifest_set: HashSet<&str> = manifest_ids.iter().copied().collect();

    // Cross-check: every registered trial must appear in the manifest, and
    // every manifest id must map to a registered trial.
    for name in PARITY_TEST_NAMES {
        assert!(
            manifest_set.contains(*name),
            "trial `{name}` registered but missing from parity-item-manifest.txt"
        );
    }
    for id in &manifest_ids {
        assert!(
            PARITY_TEST_NAMES.contains(id),
            "manifest id `{id}` has no registered trial"
        );
    }

    let excused_src = read_excused();
    let r0_gate = std::env::var("OMSRS_R0_GATE").ok().as_deref() == Some("1");

    let args = Arguments::from_args();
    let trials = build_parity_trials();
    let conclusion = libtest_mimic::run(&args, trials);

    // `libtest-mimic` may short-circuit on `--list`; in that case skip the gate.
    if args.list {
        return ExitCode::from(0);
    }

    let passed = PASSED.lock().unwrap();
    let failed = FAILED.lock().unwrap();
    let passing: HashSet<&str> = passed.iter().map(String::as_str).collect();
    let failing: HashSet<&str> = failed.iter().map(String::as_str).collect();

    let gate = run_gate(
        &manifest_ids,
        &manifest_set,
        excused_src.as_deref(),
        &passing,
        &failing,
        r0_gate,
    );
    emit_report(&manifest_ids, &passing, &failing, gate, &conclusion);
    ExitCode::from(gate.code() as u8)
}

/// Parse excused-file source and run the gate. TOML parsing lives here
/// (dev-dep) rather than in `omsrs::parity_gate` — the library stays
/// TOML-free.
fn run_gate(
    manifest: &[&str],
    manifest_set: &HashSet<&str>,
    excused_src: Option<&str>,
    passing: &HashSet<&str>,
    failing: &HashSet<&str>,
    r0_gate_enabled: bool,
) -> GateExit {
    let rows = match parse_excused_toml(excused_src) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let excused = match validate_excused(&rows, manifest_set, r0_gate_enabled) {
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

fn parse_manifest(body: &str) -> Vec<&str> {
    body.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

/// Reads `tests/parity/excused.toml` if present. Returns `None` on absent,
/// `Some("")` on present-but-empty — gate parser treats both paths per
/// §4.1.2 step 1.
fn read_excused() -> Option<String> {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "parity",
        "excused.toml",
    ]
    .iter()
    .collect();
    match std::fs::read_to_string(&path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => panic!("excused.toml read error: {e}"),
    }
}

fn emit_report(
    manifest: &[&str],
    passing: &HashSet<&str>,
    failing: &HashSet<&str>,
    gate: GateExit,
    conclusion: &libtest_mimic::Conclusion,
) {
    println!();
    println!("═══ parity gate report ═══");
    println!("  manifest size : {}", manifest.len());
    println!("  passed        : {}", passing.len());
    println!("  failed        : {}", failing.len());
    println!("  libtest num_passed / num_failed : {} / {}",
        conclusion.num_passed, conclusion.num_failed);
    println!("  gate          : {:?} (exit {})", gate, gate.code());
    if !failing.is_empty() {
        let mut sorted: Vec<&&str> = failing.iter().collect();
        sorted.sort();
        println!("  failing ids   :");
        for id in sorted {
            println!("    - {id}");
        }
    }
    println!();
}
