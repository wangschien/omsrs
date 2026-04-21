# Verdict: ACK

R2 may start. R1 lands the requested 20 parity trials, the parity gate exits 0 with no excused rows, and the Cargo/clippy/build matrix is clean in this sandbox.

## P0 findings

None.

## P1 findings

None.

## P2 findings

- P2.1 `tests/parity_runner_smoke` does not directly assert the numeric exit codes it claims to cover. Rows 1-13 currently assert the returned `GateExit` variants (`GateExit::Pass`, `GateExit::RegressionOrShort`, etc.) rather than `gate.code() == 0..6` (`tests/parity_runner_smoke/main.rs:74`, `tests/parity_runner_smoke/main.rs:110`, `tests/parity_runner_smoke/main.rs:122`, `tests/parity_runner_smoke/main.rs:143`, `tests/parity_runner_smoke/main.rs:158`, `tests/parity_runner_smoke/main.rs:177`, `tests/parity_runner_smoke/main.rs:227`). The current `GateExit::code()` mapping is correct by inspection (`src/parity_gate.rs:63`), so this is not an R2 blocker, but the smoke matrix should add numeric assertions to catch future exit-code drift.

## Non-blocking findings

- The phase-scaled floor in `gate_arithmetic` (`manifest_len.saturating_sub(EXCUSED_CAP)`) is acceptable for R1-R9 and preserves the R10 value of 230 with the frozen 237-item manifest. It is a literal deviation from PORT-PLAN §4.1.3's `|passing_set| >= 230` wording, but the hardcoded interpretation would make per-phase gates impossible before the full manifest exists. Consider updating the plan text to explicitly distinguish phase manifests from the R10 final floor.
- `BasicPosition.buy_quantity` / `sell_quantity` as `Decimal` is acceptable. It follows the plan's "price/quantity/PnL uses rust_decimal" direction and all R1 parity assertions pass.
- Keeping `src/parity_gate.rs` TOML-free is acceptable. `toml` remains dev-only and the shared library owns validation/arithmetic, while each test target owns its parse glue.
- The explicit two-order `BHARATFORG` construction for `test_create_basic_positions_from_orders_dict_qty_non_match` is acceptable. It preserves the arithmetic/value coverage without depending on pandas' non-stable sort ordering.
- `test_update_quantity_case0..case5` naming is acceptable. The manifest ids mirror those names, and `tests/parity/main.rs` cross-checks manifest ids against registered trial names at startup.

## Verified closures

- R1 scope is present: `rust-tests/parity-item-manifest.txt` has 20 active ids, split as 17 `test_utils` items and 3 `test_models::test_basic_position*` items.
- The parity harness registers the same 20 trials and checks both directions: every registered trial is in the manifest, and every manifest id has a registered trial.
- `cargo test --test parity` passed: 20 passed, 0 failed, 0 ignored; gate report showed manifest size 20, passed 20, failed 0, `Pass (exit 0)`.
- `scripts/parity_gate.sh` passed in release/all-features mode and returned exit 0 with the same 20/20 gate report.
- `tests/parity/excused.toml` is present-empty in the §4.1.2 sense: it contains comments only, has no `[[excused]]` rows, and the parity target deserializes it cleanly to an empty excused set.
- `OMSRS_R0_GATE=1 cargo test --test parity` also passed, confirming the present-empty excused file does not trip the R0 empty-set guard.
- The 13-row smoke matrix covers all semantic cases listed in PORT-PLAN §4.1.5, and the current `GateExit::code()` mapping is 0 through 6 as specified. See P2.1 for the missing numeric assertions in the smoke tests themselves.
- No `#[ignore]` appears in `src`, `tests`, `Cargo.toml`, `rust-tests`, or `scripts`.
- `Cargo.toml` matches the §7 dependency set in substance: production deps are in `[dependencies]`, `toml` and parity harness deps are dev-only, and `persistence` remains feature-gated.
- `cargo clippy --all-features --all-targets -- -D warnings` is clean.
- `cargo build` and `cargo build --no-default-features` are both warning-free.

## Non-regression checks

- `cargo test`
- `cargo test --test parity`
- `scripts/parity_gate.sh`
- `cargo test --test parity_runner_smoke`
- `OMSRS_R0_GATE=1 cargo test --test parity`
- `cargo clippy --all-features --all-targets -- -D warnings`
- `cargo build`
- `cargo build --no-default-features`
- `rg -n "#\\[ignore\\]|ignore\\]" src tests Cargo.toml rust-tests scripts` returned no hits.
- `rg -v "^\\s*(#|$)" rust-tests/parity-item-manifest.txt | wc -l` returned `20`.
- `rg -n "^\\s*\\[\\[excused\\]\\]" tests/parity/excused.toml` returned no hits.
