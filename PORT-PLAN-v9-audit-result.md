# Verdict: NACK

v9 closes most of the v8 content bugs, and the move to `libtest-mimic` is directionally sound. It still opens a new blocker in the fixed parity-gate area: the gate command passes a custom `--report` flag, while the harness description hands argv directly to `libtest-mimic`.

## P0 findings

### P0.1 `--report` is not specified as being stripped before `libtest-mimic` parses argv

`PORT-PLAN.md:94-103` defines the gate as:

```sh
cargo test -p omsrs --test parity --release --all-features -- --report
```

But `PORT-PLAN.md:56-58` says `tests/parity/main.rs` collects trials and runs them via `libtest_mimic::run(args, trials)`. In `libtest-mimic ^0.7`, `Arguments::from_args()` / `from_iter()` accepts libtest-like flags such as `--format`, `--list`, `--ignored`, etc.; it does not define `--report`. If the implementer follows §4.1.1 literally, the parity binary exits during arg parsing before running the 237 trials.

Required fix: either remove `--report` and make the parity binary always emit the needed report, or explicitly specify custom argv handling: parse and remove `--report` before passing the remaining argv to `libtest_mimic::Arguments::from_iter`. Update §4.1.1, §4.1.4, and §9.3 consistently.

## P1 findings

### P1.1 `excused.toml` malformed-file handling still contradicts required validation

`PORT-PLAN.md:75` says "Missing / malformed file => treated as empty." `PORT-PLAN.md:78` then says rows missing `rationale` / `approved_at` / `approved_by` are rejected.

Those cannot both be true if deserialization into a strict row type fails on a missing field. A malformed file or schema-invalid row must not be silently treated as an empty excuse set; otherwise a bad `excused.toml` can pass whenever the current run has no failures, defeating the validation contract.

Required fix: only an absent file may be treated as empty. Invalid TOML, malformed shape, duplicate tables that cannot parse, or rows missing required fields must exit nonzero. Reuse an existing exit code or add one explicitly.

## P2 findings

### P2.1 Grep hygiene for the v8 P0 closure still fails

The audit checklist required zero hits for `-Z` / `unstable-options` in `PORT-PLAN.md` and `omspy-source-notes.md`. v9 still has live hits:

- `PORT-PLAN.md:12` embeds `--format json -Z unstable-options`.
- `PORT-PLAN.md:52`, `PORT-PLAN.md:214`, and `PORT-PLAN.md:240` contain `-Z` in negative/current text.

The operational command no longer uses nightly flags, so this is not the main blocker. It does fail the requested mechanical hygiene check.

### P2.2 §1 still embeds the stale numeric strings it says were removed

`PORT-PLAN.md:23` contains `238`, `4760`, and `~5760` while claiming `grep 238` / `grep 4760` in the main plan returns zero hits. This is exactly the stale-text pattern v9 claimed to close.

### P2.3 Source-notes still have a live stale v7 heading

`omspy-source-notes.md:334` still says `### MVP parity gate (v7)` in the current source-of-truth area. `omspy-source-notes.md:293` saying "v7 decision" is arguably historical provenance; line 334 is just stale labeling.

### P2.4 §1 describes an invalid or inconsistent gate command

`PORT-PLAN.md:12` says `scripts/parity_gate.sh` runs `cargo run -p omsrs --test parity --release -- --report`. The actual §4.1.4 wrapper uses `cargo test ... --test parity`, which is the right shape. Remove the `cargo run --test` summary to avoid implementing the wrong command.

### P2.5 Ticker derivation should quote the actual upstream constant and avoid rounded-probability ambiguity

The threshold is basically defensible, but the text should be tighter. Upstream is not a named `price_mean_diff` default; it is hard-coded as:

```py
diff = random.gauss(0, 1) * self._ltp * 0.01
```

For `_ltp = 125`, the zero-rounding window is `[-0.025, 0.025] / 1.25`, so `p = Phi(0.02) - Phi(-0.02) = 0.0159566`, and `P(Binomial(100, p) >= 6) = 0.00550`. The plan's `0.02` shorthand is acceptable only if it states the tail uses the unrounded `0.01596` value.

### P2.6 Manifest and statistical target mechanics need one line of path/config specificity

`rust-tests/parity-item-manifest.txt` is named, but the plan should state it is crate-root-relative and loaded via a stable mechanism such as `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/rust-tests/parity-item-manifest.txt"))` or an explicit runtime path.

Similarly, §9.4 says the `statistical` target also uses `libtest-mimic`; add the corresponding `[[test]] name = "statistical" harness = false` declaration or say it uses stock libtest. This is not a parity denominator issue, but it avoids target-shape ambiguity at R10.

## Verified closures

- `libtest-mimic` exists and `^0.7` is real (`0.7.3`, stable Rust MSRV 1.60; latest is `0.8.2`).
- The parity gate no longer operationally depends on nightly `--format json -Z unstable-options`; §4.1.4 has no shell-side JSON parsing and exits on the parity binary's status.
- `excused.toml` now specifies duplicate-id, unknown-id, R0-empty, and `|excused| <= 7` checks, subject to P1.1 above.
- `create_db` source notes now say it is NOT called by `CompoundOrder.__init__`; no stale "Called by `CompoundOrder.__init__`" hit remains.
- Source-notes R1 now says `3 x BasicPosition; no upstream QuantityMatch test`; no `QuantityMatch, BasicPosition` hit remains.
- R5 scope now says minus `test_ticker_ltp`, not minus all Ticker tests.
- Phase arithmetic is consistent: `20 + 10 + 64 + 10 + 54 + 22 + 10 + 40 + 7 = 237`.
- Week arithmetic is unchanged: `16.25` rounded to `~16 weeks`.
- `OrderStrategy::add(compound)` still immediately cascades clock to existing child orders.
- Broker response construction rules remain correct: `VirtualBroker::{order_place, order_modify, order_cancel}` construct `OrderResponse`; `ReplicaBroker::order_place` constructs only `VOrder`.
- `test_ticker_ltp` remains §14(A), outside the 237 denominator and outside slack.
- R8 schedule risk R.13 is present.
- The `rand` fallback explicitly covers both prod Ticker and `tests/statistical/test_ticker_ltp_statistical.rs`.
- `parity_runner_smoke` is a separate CI/self-test target and is not counted toward the 237 parity denominator.
- No new production dependencies were introduced; `libtest-mimic` and `toml` are dev-deps. The `serde` dev-dep note is not a Cargo duplicate-key problem because `serde` is already a normal dependency with `derive`.

## Minimum changes for ACK

1. Fix the parity binary argv contract: remove `--report`, or explicitly strip it before calling `libtest-mimic`; make §1, §4.1.1, §4.1.4, and §9.3 say the same thing.
2. Change `excused.toml` handling so only an absent file is empty; malformed TOML or schema-invalid rows are rejected.
3. Clean the mechanical stale-text failures: remove `-Z` / `unstable-options` / `--format json` from live plan text, remove `238` / `4760` / `5760` from `PORT-PLAN.md` §1, and relabel `omspy-source-notes.md:334`.
4. Tighten the ticker derivation to quote upstream's hard-coded `0.01` and show `p = 0.0159566` before rounding in prose.
5. Specify the crate-root manifest load path and the `statistical` test target's harness setting.
