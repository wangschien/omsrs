# R1 audit prompt

PORT-PLAN v11 was ACKed at R0 (commit f5f4108). R1 just landed as commit
716e587 and must pass your audit before R2 starts.

## Scope

R1 per PORT-PLAN §8:
- 17 portable `test_utils` items (34 total − 1 `tick` − 8 `stop_loss_step_decimal` − 8 `load_broker_*`)
- 3 `test_models::test_basic_position*` items
- Total: 20 parity trials

## Deliverables landed in 716e587

- `Cargo.toml` with the §7 dep set (prod + dev)
- `src/lib.rs`, `src/models.rs`, `src/utils.rs`, `src/parity_gate.rs`
- `tests/parity/` — libtest-mimic harness + `excused.toml` + 20 trials
- `tests/parity_runner_smoke/` — 13-row §4.1.5 smoke matrix
- `tests/data/real_orders.csv` — copied from upstream
- `rust-tests/parity-item-manifest.txt` — 20 ids
- `scripts/parity_gate.sh`

## Evidence captured in the commit message

- `cargo test --test parity` — 20 passed, 0 failed, gate exit 0
- `cargo test --test parity_runner_smoke` — 13 passed
- `cargo clippy --all-features --all-targets -- -D warnings` clean
- `cargo build` + `cargo build --no-default-features` both warning-free

## Design choices worth scrutinising

1. **Gate required-floor formula.** `src/parity_gate.rs::gate_arithmetic`
   uses `required = manifest_len.saturating_sub(EXCUSED_CAP)` rather than a
   hardcoded 230. At R10 with the frozen 237-item manifest this yields 230
   exactly (preserving plan §4 "≥ 230 of 237"); earlier phases with shorter
   manifests scale linearly. `R10_REQUIRED_FLOOR = 230` is kept as a `pub
   const` so the plan's numeric target is still surfaced.

   Rationale: the alternative (hardcode 230) makes `cargo test --test
   parity` exit non-zero for R1–R9 even when every implemented trial
   passes, which would block per-phase ACK cycles. Flag if this deviates
   from the plan's intent.

2. **BasicPosition.buy_quantity / sell_quantity type.** Upstream uses
   `int`; the Rust port uses `Decimal`, matching PORT-PLAN §7 + §12's
   "rust_decimal for every price/qty/PnL field". All R1 parity assertions
   pass under this choice.

3. **TOML-free library.** `src/parity_gate.rs` only does validation +
   arithmetic. Each test target (parity harness and smoke runner) calls
   `toml::from_str` itself and hands rows to the shared validator. Keeps
   `toml` as dev-dep per §7.

4. **`test_create_basic_positions_from_orders_dict_qty_non_match` is
   constructed explicitly** rather than threading through the
   `load_orders()` fixture and indexing into it. Upstream depends on
   `pandas.sort_values` (default quicksort = non-stable); Python's
   assertions only hold for a specific within-symbol row order that
   quicksort happens to produce. Our two-order hand-built BHARATFORG
   scenario preserves the actual value/arithmetic coverage without
   importing a sort-parity dependency.

5. **Parametrized-test naming.** Upstream `test_update_quantity` is a
   6-row pytest parametrize. The Rust port maps to
   `test_update_quantity_case0..case5`. Manifest ids mirror these names;
   the 1:1 trial-name ↔ manifest-id cross-check runs in `main.rs`.

## What to verify

- `cargo test` is green in your sandbox.
- Gate exits 0 on R1's manifest and trial set (no excused entries needed).
- The 13-row smoke matrix covers all exit codes 0–6 and stays in sync
  with `src/parity_gate.rs` exit-code definitions.
- `tests/parity/excused.toml` is present-empty (§4.1.2 silent-empty case
  #2) and deserializes cleanly.
- No `#[ignore]` anywhere.
- Clippy + both feature configurations compile warning-free.
- Anything about the five design choices above that would be worth
  fixing now versus deferring to a later phase.

## Output format

Follow the same structure as `PORT-PLAN-v11-audit-result.md`:

- Verdict line (ACK or NACK)
- P0 / P1 / P2 / non-blocking findings
- Verified closures section
- Non-regression checks you actually ran

Write the result to `PORT-PLAN-R1-audit-result.md`.
