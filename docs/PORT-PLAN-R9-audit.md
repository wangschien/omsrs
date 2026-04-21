# R9 audit prompt

R8 ACKed (after NACK→fix→ACK). R9 landed locally — this is the **last
implementation phase**; R10 is the pure parity-sweep stabilisation, no
new code. The frozen 237-item manifest now matches PORT-PLAN §4 exactly.

## Scope

R9 per PORT-PLAN §8: 7 items from `tests/test_order_strategy.py`.

## Deliverables

- `src/order_strategy.rs` — `OrderStrategy` with broker / id / ltp /
  orders: Vec<CompoundOrder>. Methods: positions, update_ltp,
  update_orders, mtm, total_mtm, run, add, save.
- `src/compound_order.rs` — new field `run_fn: Option<RunFn>` where
  `RunFn = Arc<dyn Fn(&mut CompoundOrder, &HashMap<String, f64>) + Send +
  Sync>`. Rust analogue of upstream `CompoundOrderRun(CompoundOrder)`
  subclassing.
- `tests/parity/test_order_strategy.rs` — 7 trials.

## Design choices worth scrutinising

1. **`run_fn` closure-field replaces Python subclass.** Upstream
   `test_order_strategy_run` defines `CompoundOrderRun(CompoundOrder)`
   with an override `def run(self, data)`. Rust doesn't do subclassing;
   the Rust trial sets `compound.run_fn = Some(Arc::new(|_, data| { ... }))`
   and the captured closure writes into a `Mutex<i64>` the test holds.
   `CompoundOrderNoRun` analogue is just a plain CompoundOrder with
   `run_fn = None` (strategy.run skips it, matching upstream's
   `callable(getattr(co, "run"))` → False path).

2. **strategy.mtm aggregates per-symbol Decimal across compound
   orders.** Upstream's `Counter.update` style → Rust's HashMap<Symbol,
   Decimal> with += per entry.

3. **strategy.add propagates broker** if the new compound doesn't have
   one, mirroring upstream's guard. Upstream also logs a warning on
   broker mismatch; Rust port skips the warning (logging layer is a
   future-work detail).

4. **`update_ltp` propagates to children first, then stores at strategy
   level.** Matches upstream line 1355.

5. **`test_order_strategy_mtm` assertions use the upstream-typo
   computation** (`29 * (110 - 110)` for amzn where filled_quantity is
   actually 39). Both evaluate to 0 so the test passes either way; the
   Rust trial matches the expected 0 without hardcoding 29 vs 39.

## What to verify

- `cargo test --test parity --all-features` → 237 / 236 / 1 excused,
  exit 0. **This is the R10 target shape** — frozen 237-item manifest
  with 7-slack passing cleanly.
- `cargo test --no-default-features` → all targets green.
- `cargo clippy` clean in both feature configs.
- `scripts/parity_gate.sh` → 237 / 236 / 1, exit 0.
- Manifest exactly 237 (count matches R10 `REQUIRED_FLOOR = 230`
  arithmetic: `237 - 7 = 230` required floor, `236 ≥ 230` passes).
- No `#[ignore]`, excused.toml still the single R3.a row.
- The 5 design choices — any red flags.
- R10 readiness: with R9 done, is there anything that would block the
  parity_gate sweep exit-0 invariant at a true R10 audit?

## Output format

Same as prior audits. Write to `docs/PORT-PLAN-R9-audit-result.md`. If
ACK, R10 may start (pure parity-sweep stabilisation / regression
hardening — no new parity items).
