# Verdict: NACK

v10 closes the v9 argv-contract blocker and the requested grep-hygiene items. One `excused.toml` edge case remains unresolved: a present-but-empty file / root table with no `[[excused]]` rows is not defined, and the current wording conflicts with the source-notes statement that `tests/parity/excused.toml` starts empty.

## P0 findings

None.

## P1 findings

### P1.1 Present-but-empty `excused.toml` is still ambiguous and likely conflicts with R0

`PORT-PLAN.md:82-84` says only an absent file is silently empty, and that a present file failing schema deserialization exits 6. It never defines the well-formed empty case: empty file, empty root table, or a TOML document with no `[[excused]]` arrays.

That matters because `omspy-source-notes.md:474` says `tests/parity/excused.toml` starts empty. If the implementer uses a strict schema like `struct ExcusedFile { excused: Vec<Row> }`, a committed empty file deserializes as missing `excused` and exits 6, even though the intended R0 state is no excused rows. If the implementer special-cases it, the plan does not say so.

Required fix: in §4.1.2, explicitly state that a present file which parses successfully but contains zero `[[excused]]` rows is equivalent to absent: `excused_set = {}`. Specify the schema default, e.g. `#[serde(default)] excused: Vec<ExcusedRow>`, while keeping invalid TOML, malformed row shapes, and rows missing `rationale` / `approved_at` / `approved_by` on exit code 6.

## P2 findings

### P2.1 Runner smoke tests omit the new exit code 6

`PORT-PLAN.md:118` still says `tests/parity_runner_smoke` asserts only exit codes `0/1/2/3/4/5`. v10 added exit code 6 specifically to close the malformed/required-field validation bug, but the smoke target does not list any case that proves invalid TOML, schema-invalid TOML, or missing required fields fail with 6.

Required fix: extend §4.1.5 to include exit code 6 cases: invalid TOML, wrong schema shape, and a row missing `rationale` / `approved_at` / `approved_by`. Also include a present-empty/no-row case asserting success with `excused_set = {}` once P1.1 is fixed.

## Verified closures

- v9 P0.1 is closed. The live wrapper in §4.1.4 ends with `exec cargo test -p omsrs --test parity --release --all-features` and passes nothing after it to the parity binary.
- §4.1.1 now says the parity binary defines no custom argv flags, and report emission is unconditional on every run.
- `--report` appears only in historical closure text / negative contract text, not in a live invocation.
- `rg -n -- '\-Z unstable-options|--format json' PORT-PLAN.md` returns no hits.
- `rg -n -- '238|4760|5760' PORT-PLAN.md` returns no hits.
- `rg -n -- '\(v7\)' omspy-source-notes.md` returns no hits, and the old `### MVP parity gate (v7)` heading is now `### MVP parity gate (current)`.
- `rg -n -- 'cargo run .* --test parity' PORT-PLAN.md` returns no hits.
- §4.1.2 routes duplicate id / unknown id / R0 non-empty / `|excused| > 7` to distinct exit codes 2/3/4/5, and missing required row fields to exit code 6.
- §6 D10 quotes upstream's literal `random.gauss(0, 1) * self._ltp * 0.01` and uses `p = Φ(0.02) - Φ(-0.02) = 0.0159566`.
- §4.1.1 specifies manifest embedding via `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/rust-tests/parity-item-manifest.txt"))`; this macro form is valid Rust.
- §9.4 declares `[[test]] name = "statistical" harness = false` and says it uses the same `libtest-mimic` wrapper pattern as parity.
- Phase math still holds: `20 + 10 + 64 + 10 + 54 + 22 + 10 + 40 + 7 = 237`.
- `#[ignore]` remains disallowed as a legal mechanism; mentions are negative only.
- `OrderStrategy::add(compound)` still immediately cascades the clock to already-contained child orders.
- Broker response construction rules remain unchanged: `VirtualBroker` constructs `OrderResponse` for place/modify/cancel, while `ReplicaBroker::order_place` constructs only `VOrder`.
- Denominator 237 is uniform; `test_ticker_ltp` stays outside the denominator and outside slack.
- Week math remains `16.25` -> `~16 weeks`.
- R8 schedule risk R.13 is still present.
- The `rand` fallback covers both production Ticker and `tests/statistical/test_ticker_ltp_statistical.rs`.
- §7 adds no new production deps; `libtest-mimic` and `toml` remain dev-deps. §12 is non-normative and does not add async, HTTP/WS, or new production dependency scope.
- `cargo test` via `exec` will faithfully surface the parity binary exit code; cargo compile failure exiting nonzero is acceptable.
- `--release` on the wrapper is an intentional gate-shape choice, not an ACK blocker.

## Minimum changes for ACK

1. Define the present-but-empty / no-`[[excused]]` case in §4.1.2 as equivalent to absent, with `excused_set = {}`, and make the schema default explicit.
2. Extend §4.1.5 runner smoke coverage to include exit code 6 invalid-file/invalid-row cases and the present-empty success case.
