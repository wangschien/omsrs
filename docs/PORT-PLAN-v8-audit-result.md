# PORT-PLAN v8 audit result

Verdict: **NACK**. Do **not** start R1 yet.

v8 closes the two v7 P0s on the live denominator math and the R3/R8 `test_order.py` split. The phase table now sums mechanically to 237, the `64/40` split is auditable, the clock cascade rule is fixed, and parity/statistical test commands are separated.

The remaining blocker is new in the v8 fix for §14(B): the parity gate depends on unstable libtest JSON output but the plan never declares a nightly test toolchain or a stable alternative.

## P0 findings

### P0.1 Parity gate silently requires nightly Rust

`PORT-PLAN.md` §4.1 and §9.3 define the gate as:

```text
cargo test -p omsrs --test parity --all-features -- --format json -Z unstable-options
```

`--format json -Z unstable-options` is a libtest unstable option and requires nightly. The plan otherwise frames the crate as a normal stable/MSRV library (`cargo build`, `cargo build --no-default-features`, clippy, MSRV-minimum feature build), and never says CI or developers must run the parity gate under nightly.

This means the mechanism that closes v7 P1.1 is not actually runnable under the plan's implied stable toolchain. If the gate is the acceptance mechanism, its toolchain has to be explicit and reproducible.

Required fix: either replace the gate with a stable mechanism, or explicitly pin and document a nightly-only parity-gate toolchain, including the exact CI command. The script must also capture and parse `cargo test` output even when `cargo test` exits non-zero.

## P1 findings

### P1.1 `test_ticker_ticker_mode` threshold is stated but not justified

v8 now gives a criterion: `>= 95/100` successes over `SmallRng::seed_from_u64(seed)` for `seed in 0..100`.

That closes the "no criterion at all" gap, but the threshold is still arbitrary. Upstream `test_ticker_ticker_mode` only checks that manual mode leaves `ltp == 125` and the first random-mode read changes it to `ltp != 125`. Given the 0.05 rounding around an initial price of 125, a correct random implementation should collide with exactly 125 only rarely. The plan does not derive why 95/100 is the right lower bound, what false-reject rate it implies, or what broken implementations it is meant to catch.

Required fix: justify the threshold from the expected collision probability and desired false-reject rate, or replace it with a clearer deterministic/property test plus the separate `test_ticker_ltp_statistical` distribution check.

### P1.2 `excused.toml` gate validation is under-specified

The v8 runner asserts:

```text
failing_set subset excused_set
passing_set >= 230
|excused_set| <= 7
```

That catches unexpected failures, but it does not say the runner rejects duplicate excused IDs, unknown/stale test IDs, malformed rows, or typoed IDs that do not correspond to the frozen parity manifest. It also does not explicitly say `tests/parity/excused.toml` starts empty at R0; §4.1 gives a populated example row and source-notes still says "No v7 pre-authorized entries" rather than v8/R0.

Required fix: define the manifest cross-check. The runner should reject duplicate IDs, unknown IDs, missing rationale/approval fields, and any non-empty R0 `excused.toml` unless the entry was approved at a phase gate.

### P1.3 Source notes still contain a live `CompoundOrder.__init__` contradiction

`omspy-source-notes.md` §5 still says:

```text
create_db(...) ... Called by `CompoundOrder.__init__` as optional.
```

Later, §13 correctly says `CompoundOrder.__init__` does **not** call `create_db`, and upstream `order.py:741-760` confirms that. The later correction is right, but the earlier inventory line is still live source-note text and can mislead persistence implementation.

Required fix: update the §5 inventory line to say `create_db` is used by tests/callers and by persistence paths, but not called from `CompoundOrder.__init__`.

## P2 findings

### P2.1 Source-notes R1 label still mentions `QuantityMatch`

`PORT-PLAN.md` §8 correctly says R1 is `17 utils + 3 BasicPosition` and explicitly says there is no upstream `QuantityMatch` test in `tests/test_models.py`.

`omspy-source-notes.md` §11.1a still says:

```text
test_models.py (QuantityMatch, BasicPosition)
```

The count is correct, but the label is still inconsistent with the plan and with upstream `tests/test_models.py`, which has 3 `BasicPosition` tests and 0 `QuantityMatch` tests.

### P2.2 R5 table wording says "minus Ticker"

`PORT-PLAN.md` §8 table says:

```text
R5 | 54 (simulation/models minus Ticker)
```

The detailed row correctly says "all 55 `test_simulation_models.py` items minus `test_ticker_ltp`", and D10 says the other 5 `test_ticker_*` tests remain parity items. The table should say `minus test_ticker_ltp`, not `minus Ticker`.

### P2.3 Grep noise remains for stale LOC strings

The live source-notes Rust test LOC section is fixed, but `PORT-PLAN.md` §1 still contains the exact stale strings `238`, `4760`, and `~5760` while saying the stale block is gone. This is not an active budget contradiction, but it fails the requested mechanical grep hygiene.

Required fix: rephrase the v8 correction summary without embedding the old numbers, or move prior-version details to the audit history.

### P2.4 Version labels in source-notes are stale

Several current source-notes headings still say v7, including:

- `### §11.1a Phase allocation (v7, denominator = 237)`
- `### §14 Parity-denominator exclusions + exception register (v7)`
- `No v7 pre-authorized entries`
- `### Rust test LOC (v7, denominator = 237 pytest items...)`

The content is mostly v8-correct, but the labels should say v8/current so future audits do not have to infer whether the section is stale or authoritative.

### P2.5 R8 schedule risk is not called out

R3 has 64 non-compound order items in 3 weeks (~21/week). R8 has 40 `CompoundOrder` items in 1.5 weeks (~27/week), despite compound behavior being at least as stateful as the R3 lifecycle/persistence work. This is not an ACK blocker by itself, but it should be listed as a schedule risk or R8 should be widened.

### P2.6 `rand` fallback should mention tests explicitly

v8 documents the `rand = "=0.8"` maintenance cost and says the fallback is a vendored/local `ChaCha8Rng` for Ticker RNG. If that fallback is taken, the statistical tests must use the same RNG path to preserve reproducibility. The plan implies this by scoping it to Ticker RNG, but should state it explicitly at R5.

## Verified closures / notes

- Source-notes Rust test LOC section now uses `237 x 20 = 4740`, plus 500 fixtures, 300 proptest, 200 clock harness, and 50 ticker statistical replacement, totaling ~5790.
- No live source-notes `4760`, `5760`, or `~5760` budget remains.
- `test_order.py` split is now mechanically correct: 106 function bodies, 105 collected bodies after duplicate collapse, 107 pytest items after `test_get_option` parametrization, 104 portable after excluding `test_get_option[...]`, 40 unique collected `test_compound_order_*` items, 64 other portable items.
- `PORT-PLAN.md` §8 and `omspy-source-notes.md` §11.1a/§11.1b both use R3=64 and R8=40.
- Phase sum is correct: `20 + 10 + 64 + 10 + 54 + 22 + 10 + 40 + 7 = 237`.
- §14(B) no longer uses `#[ignore]`; tests remain plain `#[test]` functions and the gate is intended to be external to raw `cargo test`.
- R10 parity and statistical runs are separated: parity through `scripts/parity_gate.sh` / `--test parity`, statistical through `--test statistical --features statistical-tests`.
- `OrderStrategy::add(compound)` now explicitly cascades to every already-contained child order in the same call. No live deferral to a future mutation remains.
- Clock response rules are now correct: `VirtualBroker::{order_place, order_modify, order_cancel}` construct `OrderResponse` with `self.clock`; `ReplicaBroker::order_place` constructs `VOrder` only and does not construct `OrderResponse`.
- MVP summary no longer says `OHLCVI` is needed for `VQuote` inheritance; it says `OHLCVI` is kept only for `test_ohlcvi` parity.
- §7 dependency table is restored inline. The requested crates are present, and `rusqlite` is optional behind `persistence`.
- `test_ticker_ltp` is §14(A), outside the 237 denominator, and not counted as slack.
- Week math is honest enough: clean-path sum is 16.25 weeks and the plan rounds it to "~16 weeks"; expected-with-rework `16 + 1.5 x 10 = 31` is consistent.
- `scripts/parity_gate.sh` not existing yet is acceptable at plan ACK time; the issue is the unspecified nightly dependency and incomplete gate validation, not absence of implementation before R0.

## Minimum changes for ACK

1. Fix the parity gate toolchain issue: use a stable test-result collection mechanism, or explicitly pin/document nightly for the parity gate and CI smoke check.
2. Specify `excused.toml` validation against the frozen parity manifest, including duplicate/unknown ID rejection and empty-at-R0 behavior.
3. Justify or revise the `test_ticker_ticker_mode` `95/100` threshold.
4. Remove the live `CompoundOrder.__init__` / `create_db` contradiction from source-notes.
5. Clean the remaining scope-label inconsistencies: source-notes R1 `QuantityMatch`, R5 "minus Ticker", stale v7 headings, and stale-number grep noise.
