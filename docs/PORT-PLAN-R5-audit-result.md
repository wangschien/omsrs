R5 is ACKed. R6 may start: the R5 parity surface lands the requested 54
`tests/simulation/test_models.py` items, keeps the only excused failure as the
R3.a `test_order_timezone` row, and passes the requested Cargo, clippy,
statistical, and release gate checks.

## Findings

No blocking findings.

Non-blocking notes:

- `OrderType::parse("STOP")` currently succeeds, while upstream's pydantic
  string validator accepts only `"LIMIT"` / `"MARKET"` and accepts STOP only
  through the enum path (`OrderType.STOP`). The current R5 parity tests use the
  enum path for STOP orders, so this does not affect the gate. If `order_type_str`
  is meant to mirror the upstream validator exactly, tighten this edge before it
  becomes public surface.
- The literal `pytest --collect-only -q tests/simulation/test_models.py | wc -l`
  shape is pytest-output dependent. In this sandbox the `pytest` executable is
  not on PATH, and `python3 -m pytest` emits 55 nodeid lines plus a blank line
  and the summary line. Filtering nodeid lines gives 55 collected tests.
- The R5 manifest order differs from pytest collection for four entries in the
  duplicate `test_vorder_modify_by_status_partial_fill` cluster, but set coverage
  is exact and the parity harness cross-checks by id, not order.

## Scope Check

- `rust-tests/parity-item-manifest.txt` has 158 active rows, with exactly 54 in
  the R5 section. `test_ticker_ltp` is not in the manifest and is replaced by
  `tests/statistical/main.rs`.
- Upstream collection has 55 nodeids from
  `tests/simulation/test_models.py`; after excluding `test_ticker_ltp` and
  normalizing the six `test_vorder_is_done[...]` parametrized cases to
  `test_vorder_is_done_case0..5`, the Rust R5 manifest has no missing or extra
  ids.
- `tests/parity/main.rs` registers the 54 R5 trials, and
  `tests/parity/test_simulation_models.rs` implements the simulation-model
  parity surface.
- `Cargo.toml` declares `rand = "=0.8"` with `small_rng`, `rand_distr = "=0.4"`,
  and the `statistical` test target with `harness = false` plus
  `required-features = ["statistical-tests"]`.
- `tests/parity/excused.toml` still has exactly one `[[excused]]` row:
  `test_order_timezone`.
- R5 landed as a single local commit: `752b73e R5: simulation models + 54 parity
  items + statistical target`.

## Design Review

1. Using `f64` for virtual simulation price/quantity arithmetic is acceptable
   for R5. It is a deliberate deviation from the broad Section 7 Decimal rule,
   but it matches upstream simulation floats and is confined to the virtual
   simulation path; the real order lifecycle remains Decimal-backed.
2. `test_ticker_ticker_mode` does not need a Section 14B excusal. The fixed
   seed makes the random-mode read deterministic in Rust, and the parity test
   passes without the 95/100 fallback.
3. The duplicate upstream `test_vorder_modify_by_status_partial_fill` is handled
   correctly for the gate: pytest collects one function, and Rust registers one
   matching trial. The uncollected direct helper behavior is still exercised
   through the full-flow partial-fill path.
4. The `VOrderInit` enum/string split is a reasonable Rust representation of
   pydantic's mixed input shapes. The only caveat is the non-blocking STOP string
   validator edge noted above.
5. `ResponseStatus::parse` instead of an inherent `from_str` is fine. An
   `impl FromStr` would be idiomatic API polish, not a parity blocker.
6. `GenericResponseData::VOrder(Box<VOrder>)` is semantically fine and avoids
   the large enum variant warning without changing the tested behavior.
7. The statistical target is an acceptable Section 14A replacement for
   seed-exact Python RNG parity. The bounds are intentionally loose but still
   check that the Ticker path is producing a Normal-shaped perturbation.

## Verification

- `cargo test --test parity --all-features` exited 0 via the parity gate:
  manifest 158, passed 157, failed 1, failing id `test_order_timezone`, gate
  `Pass`.
- `cargo test --test statistical --features statistical-tests --release` exited
  0: 1 passed, 0 failed.
- `cargo test --no-default-features` exited 0. The effective no-default parity
  manifest is 149 because the 9 R3.b persistence trials are feature-gated out:
  passed 148, failed 1, failing id `test_order_timezone`, gate `Pass`.
- `cargo clippy --all-targets --all-features` passed.
- `cargo clippy --all-targets --no-default-features` passed.
- `scripts/parity_gate.sh` exited 0 in release mode with the same 158 / 157 / 1
  gate shape.
- `rg -n "^\s*#\[ignore\]" src tests Cargo.toml rust-tests scripts` returned no
  matches.
- `rg -n "^\s*\[\[excused\]\]" tests/parity/excused.toml` returned exactly one
  row.
- Upstream collection was verified from `/home/ubuntu/refs/omspy` with
  `python3 -m pytest --collect-only -q tests/simulation/test_models.py`: 55
  nodeid lines collected, 54 after excluding `test_ticker_ltp`.
