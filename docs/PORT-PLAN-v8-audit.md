# Audit: omsrs Port Plan v8

Adversarial audit. Author NACK'd from self-auditing. Plan is at v8 after 7 prior NACKs, each of which found a NEW P0 in an area the prior revision claimed closed.

**Read first**:
- `~/omsrs/PORT-PLAN.md` (v8)
- `~/omsrs/omspy-source-notes.md` (v8-updated)
- `~/omsrs/PORT-PLAN-v7-audit-result.md` (prior NACK — 2 P0 + 4 P1 + 3 P2)

## Chain

v1–v7 all NACKed. v7 finding summary (what v8 must close):
- **P0.1** `omspy-source-notes.md` Rust test LOC section still used 238/~5760.
- **P0.2** R3/R8 `test_order.py` split claimed 63/41 but mechanically it is 64/40.
- **P1.1** §14(B) + plain `cargo test` conflict (cargo test exits non-zero on failure).
- **P1.2** `test_ticker_ticker_mode` had no statistical pass criterion if promoted to (B).
- **P1.3** `OrderStrategy::add(compound)` clock cascade deferred to "next mutation".
- **P1.4** R10 parity sweep not separated from non-parity statistical test command.
- **P2.1** MVP summary still said OHLCVI "needed for VQuote inheritance".
- **P2.2** R1 label said "QuantityMatch 1, BasicPosition 2" but `test_models.py` has 3 `BasicPosition` items and 0 `QuantityMatch` items.
- **P2.3** §7 dep table replaced by "unchanged from v6" handwave; `rand =0.8` maintenance cost not documented.

v8 claims all 9 closed.

## Stance

Pure library port only. Adversarial on whether v8 actually closes the v7 findings without opening new ones.

## Checklist

### A. v7 P0 closure

- [ ] A1: `omspy-source-notes.md` Rust test LOC section now reads `237 × 20 = 4740` + 500 + 300 + 200 + 50 = 5790. No stray `238`/`4760`/`5760`/`~5760` left. Grep for those strings.
- [ ] A2: R3/R8 split is `64/40`:
  - `^test_compound_order_` in `~/refs/omspy/tests/test_order.py` has 41 `def`s; one name (`test_compound_order_update_orders`) defined twice → 40 unique collected items.
  - Non-compound portable = `104 − 40 = 64`.
  - Both the main plan §8 table and source-notes §11.1a + §11.1b reflect 64/40.
  - Phase sum: `20 + 10 + 64 + 10 + 54 + 22 + 10 + 40 + 7 = 237` ✓.

### B. v7 P1 closure

- [ ] B1: §14(B) mechanism. v8 says failing parity tests stay `#[test]` (no `#[ignore]`), and the gate is `scripts/parity_gate.sh` parsing `cargo test --format json` against `tests/parity/excused.toml`.
  - Does §4.1 + §9.3 define this cleanly?
  - `cargo test --format json -Z unstable-options` requires nightly `-Z` — is that documented or does v8 assume stable? Flag if v8 requires nightly without saying so.
  - Is the "runner is itself parity-tested by CI smoke check that injects a known excused failure" step runnable, or is it vaporware?
- [ ] B2: `test_ticker_ticker_mode` criterion. Source-notes §14(B) row now says "≥ 95/100 successes over `seed ∈ 0..100`". Is that threshold justified or arbitrary? (Note: Ticker `_ltp == 125` probability per step is small; 95/100 seems lenient — over 100 seeds even a broken implementation might hit that. Flag if threshold is too weak.)
- [ ] B3: `OrderStrategy::add(compound)` — §6 D4.4 now explicitly says "immediately cascades to every already-contained child order in `compound.orders` within the same call". Confirm no "next mutation" language remains anywhere.
- [ ] B4: R10 parity vs statistical. §8 R10 scope + §9.3/§9.4 specify two separate commands: parity via `--test parity`, statistical via `--test statistical --features statistical-tests`. Verify.

### C. v7 P2 closure

- [ ] C1: MVP summary (source-notes §12 summary block) no longer says OHLCVI is "needed for VQuote inheritance". It should now either omit OHLCVI from the VQuote-inheritance phrasing or explicitly state "kept only for `test_ohlcvi` parity".
- [ ] C2: R1 scope row says "17 utils + 3 BasicPosition" (no `QuantityMatch` mention). Source-notes §11.1a likely still says "QuantityMatch, BasicPosition" — flag if inconsistent with the plan.
- [ ] C3: §7 dep table restored inline. All crates present: `rust_decimal`, `chrono`/`chrono-tz`, `serde`, `thiserror`, `uuid`, `rand`=0.8, `rand_distr`=0.4, `parking_lot`, `rusqlite` (optional), `proptest`, `pretty_assertions`. Is anything MVP-critical missing?

### D. Denominator + phase sum arithmetic

- [ ] D1: Grep both files for any leftover `238` (except explicit "238 gross" in §11 of source-notes) and any `63`/`41` in a phase-gate context (should now be `64`/`40`).
- [ ] D2: Week sum: `0.5 + 0.75 + 1 + 3 + 1 + 2 + 1.25 + 1.25 + 1.5 + 1 + 3 = 16.25` — matches "~16 weeks" claim? R8 still at 1.5 weeks for 40 items (~27/week); R3 at 3 weeks for 64 items (~21/week). Reasonable?

### E. Clock rules regression check

- [ ] E1: `CompoundOrder::add(order)` still says "overwrites `order.clock`".
- [ ] E2: `CompoundOrder::add_order(**kwargs)` still injects `clock: self.clock.clone()` before user kwargs.
- [ ] E3: `OrderStrategy::add(compound)` now cascades **during the add call itself**, not on next mutation. (B3 duplicates this — the important thing is there's no contradiction.)
- [ ] E4: `VirtualBroker.{order_place, order_modify, order_cancel}` construct `OrderResponse` with `self.clock`.
- [ ] E5: `ReplicaBroker.order_place` constructs `VOrder` only, does NOT construct `OrderResponse`.

### F. §14 rules regression

- [ ] F1: "No `#[ignore]` anywhere" rule still explicit.
- [ ] F2: §14(B) starts empty at v8 start; `excused.toml` is empty at R0. Entries added only at phase gates.
- [ ] F3: `test_ticker_ltp` is §14(A), not §14(B), not counted as slack.

### G. New v8 issues (adversarial)

- [ ] G1: `scripts/parity_gate.sh` is promised but the plan doesn't say it exists yet. Is that acceptable at R0 ACK time? (v8 is a plan doc; the script is implementation. Flag only if acceptance depends on it before R0.)
- [ ] G2: `cargo test --format json -Z unstable-options` — nightly requirement. If the repo targets stable Rust, v8 needs a stable alternative (e.g. parse human-readable output, or use `libtest-mimic`). Flag if plan assumes nightly silently.
- [ ] G3: The 95/100 threshold for `test_ticker_ticker_mode` — where does it come from? If `P(ltp == 125 | mode switch)` is very small (e.g. <1% per trial), a broken impl that always produces `ltp == 125` once would still pass 99/100. A broken impl that always returns `ltp == 125` fails 100/100. The threshold separates "flaky real impl" from "broken impl" — is 95 actually the right cut? Flag if plan doesn't justify.
- [ ] G4: `rand = "=0.8"` hard-pin — v8 now documents maintenance cost + fallback (vendor `ChaCha8Rng`). Does the fallback plan cover the statistical test's reproducibility requirement, or just the prod RNG?
- [ ] G5: R3 is 64 items in 3 weeks (~21 items/week). R8 is 40 in 1.5 weeks (~27/week). R8 looks tighter despite CompoundOrder being more complex than Order-lifecycle. Flag risk.
- [ ] G6: `tests/parity/excused.toml` format — is the rationale field a free string? Any risk of ambiguity (e.g. duplicate excused ids, test-id typos that silently let real failures through)? The runner should dedupe + verify every excused id corresponds to a known test name.
- [ ] G7: §7 says "`rusqlite` bundled". Does that match the `PersistenceHandle` design? (SQLite static-link via bundled C lib; fine for dev, but bloats no-default-features builds that shouldn't link sqlite at all. Verify `rusqlite` is `optional = true`.)

### H. Phase math + week honesty

- [ ] H1: Does v8 still claim "~16 weeks" when the sum is 16.25? Close enough; fine if stated as rounded.
- [ ] H2: Expected-with-rework 31 weeks = 16 + 1.5/phase × 10 impl phases = 16 + 15 = 31. Consistent.

## Deliverables

Write result to `~/omsrs/PORT-PLAN-v8-audit-result.md`.

Format:
- **Verdict** (ACK or NACK) at top.
- **P0 findings** — blockers. Each with required fix.
- **P1 findings** — substantial.
- **P2 findings** — polish.
- **Verified closures / notes** — what v8 actually fixed.
- **Minimum changes for ACK** — numbered list if NACK.

ACK permits starting R1.
NACK means fix and re-audit as v9.

Stay in scope. Be adversarial but fair — if v8 closes everything, say ACK. Prior 7 audits each surfaced a new P0 in "fixed" areas.
