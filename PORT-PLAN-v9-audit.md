# Audit: omsrs Port Plan v9

Adversarial audit. Plan is at v9 after 8 prior NACKs. Each revision has closed the prior blocker and surfaced a new one. Stay adversarial on whether v9 does the same.

**Read first**:
- `~/omsrs/PORT-PLAN.md` (v9)
- `~/omsrs/omspy-source-notes.md` (updated)
- `~/omsrs/PORT-PLAN-v8-audit-result.md` (prior NACK — 1 P0 + 3 P1 + 6 P2)

## Chain

v1–v8 all NACKed. v8 finding summary (what v9 must close):

- **P0.1** Parity gate used `--format json -Z unstable-options`, nightly only.
- **P1.1** `test_ticker_ticker_mode` 95/100 threshold stated but not derived.
- **P1.2** `excused.toml` validation under-specified (dedupe, unknown IDs, R0 empty).
- **P1.3** Source-notes §5 still said `create_db` is called by `CompoundOrder.__init__` (contradicting §13).
- **P2.1** Source-notes §11.1a label still said "QuantityMatch, BasicPosition".
- **P2.2** §8 R5 row said "minus Ticker" instead of "minus `test_ticker_ltp`".
- **P2.3** §1 of v8 embedded stale strings `238`/`4760`/`~5760`.
- **P2.4** Stale v7 section headings in source-notes.
- **P2.5** R8 schedule risk not called out.
- **P2.6** `rand` fallback didn't explicitly cover statistical tests.

v9 claims all 10 closed.

## Stance

Pure library port only. Adversarial on `libtest-mimic` claim + derivation math + stale-text hygiene.

## Checklist

### A. v8 P0 closure (libtest-mimic harness)

- [ ] A1: `PORT-PLAN.md` §4.1 no longer mentions `-Z unstable-options` or `--format json`. Grep both files for `-Z` and `unstable-options`. Must be zero hits.
- [ ] A2: Harness is `libtest-mimic`, declared with `[[test]] name = "parity" harness = false` in Cargo.toml. Verify the plan says `harness = false` and uses a crate that is actually stable-Rust compatible. (`libtest-mimic` is a well-known crate on stable; sanity-check the plan's version range `^0.7` is real.)
- [ ] A3: `scripts/parity_gate.sh` is a thin wrapper that exits on `cargo test --test parity -- --report` exit code. No shell-side JSON parsing. Verify.
- [ ] A4: The parity binary's argv (e.g. `--report`) is self-defined, not a libtest flag. Check §4.1.1/§4.1.4 are consistent.
- [ ] A5: §9.3 no longer implies nightly. Grep §9 for `unstable`, `nightly`, `-Z`.

### B. v8 P1 closure

- [ ] B1: `test_ticker_ticker_mode` threshold derivation (§6 D10):
  - Central Normal density at 0 ≈ 0.399; bin width 0.05.
  - Claimed per-trial `P(ltp == 125) ≈ 0.02` at `price_mean_diff = 0.01`.
  - Sanity-check: 0.05 × 0.399 / (0.01 × 125) = 0.01995/1.25 = 0.01596. Wait — the plan says 0.05 × 0.399 / (price_mean_diff × 125) = 0.05 × 0.399 / 1.25 = 0.0160 ≈ 0.02 rounded. Is the arithmetic sound? Flag if the plan multiplies 125 by price_mean_diff and divides incorrectly.
  - Is 95/100 really the 99.5th percentile? `Binomial(100, 0.02)`: `P(X ≥ 6) ≈ 0.0137` via normal approx, exact ≈ 0.005. The plan claims 0.005. Verify the cutoff choice (95 passes = ≤ 5 failures).
  - Could a broken impl still pass 95/100? If implementation is correct 80% of the time and broken 20% (returns 125), `P(pass) = 0.02 + 0.2 × 0.98 = 0.216`, so ~22% collision rate. Over 100 seeds, expected ~22 failures; passes 78/100, fails the 95 bound. Good.
- [ ] B2: `excused.toml` validation (§4.1.2):
  - Missing `rationale` / `approved_at` / `approved_by` rejected.
  - Duplicate ids rejected (exit code 2).
  - Unknown ids (not in `parity-item-manifest.txt`) rejected (exit code 3).
  - R0 gate via `OMSRS_R0_GATE=1` forces empty excused (exit code 4).
  - `|excused| ≤ 7` cap (exit code 5).
  - Is the manifest itself specified? `rust-tests/parity-item-manifest.txt` — when is it generated? (§4.1.1 says "per phase gate from `pytest --collect-only -q`".) Good enough?
- [ ] B3: Source-notes §5 (`create_db`) line updated to say NOT called by `CompoundOrder.__init__`. Grep for "Called by `CompoundOrder.__init__`" — must be 0 hits.

### C. v8 P2 closure

- [ ] C1: Source-notes §11.1a R1 row no longer says "QuantityMatch, BasicPosition"; now says "3 × BasicPosition; no upstream QuantityMatch test". Grep source-notes for `QuantityMatch, BasicPosition` — must be 0 hits.
- [ ] C2: §8 R5 row says "minus `test_ticker_ltp`", not "minus Ticker".
- [ ] C3: §1 of v9 contains no stale numeric strings `238 × 20 = 4760` or `~5760`. Grep `PORT-PLAN.md` for `4760`, `5760`. Must be 0 hits (the only legitimate place `238` can appear is the parity-denominator math explanation in source-notes §11).
- [ ] C4: Stale v7 headings in source-notes re-labelled to "current" or "v9". Grep for `v7` in source-notes — should be limited to history/audit-trail contexts.
- [ ] C5: R8 schedule risk R.13 added.
- [ ] C6: `rand` fallback in §7 explicitly mentions both prod Ticker and `tests/statistical/test_ticker_ltp_statistical.rs`.

### D. Regression checks (nothing from v7/v6 got un-fixed)

- [ ] D1: Denominator 237 uniformly. Grep for any stray `238` in a "denominator" context (the gross/pre-subtraction `238` is still fine in source-notes §11).
- [ ] D2: Phase gate sum `20 + 10 + 64 + 10 + 54 + 22 + 10 + 40 + 7 = 237`.
- [ ] D3: No `#[ignore]` mentioned as a legal mechanism. `#[ignore]` appears only in negative statements ("no `#[ignore]` anywhere").
- [ ] D4: `OrderStrategy::add(compound)` still immediately cascades.
- [ ] D5: `VirtualBroker.{order_place, order_modify, order_cancel}` construct `OrderResponse`; `ReplicaBroker.order_place` constructs `VOrder` only.
- [ ] D6: `test_ticker_ltp` in §14(A), 237-denominator, not slack.

### E. New v9 issues (adversarial)

- [ ] E1: `libtest-mimic` — does declaring `harness = false` on an integration test allow the test binary to consume `cargo test` args (`--release`, etc.) cleanly? Is there a known interaction with `--all-features` that the plan ignores?
- [ ] E2: `rust-tests/parity-item-manifest.txt` path — does the plan actually specify where this file lives relative to the crate root? (§4.1.1 says just the file name.) If the harness uses `include_str!`, the path must be stable.
- [ ] E3: The 95/100 derivation assumes `price_mean_diff = 0.01`. If upstream default is different, the derivation is off. The plan should quote the upstream default directly from `simulation/models.py`.
- [ ] E4: "`serde` (dev-dep, `derive`) — already listed" in §7 — but §7's main table already lists `serde ^1 (derive)` as a prod dep. Dev-dep duplication might conflict. Flag if the plan would cause a Cargo.toml duplicate-key error.
- [ ] E5: The self-test target `tests/parity_runner_smoke` — does it belong inside the parity test binary or as a separate target? The plan says separate. Any interaction with R10 acceptance (does parity_runner_smoke count toward the 237 denominator)? It must NOT.
- [ ] E6: §8 R10 scope says "plus `cargo test --test statistical --features statistical-tests`". Is this target also `harness = false` / libtest-mimic, or does it use stock libtest? §9.4 implies libtest-mimic. If stock libtest, the plan should say so to avoid inconsistency.

### F. Scope-drift checks

- [ ] F1: v9 adds `libtest-mimic` + `toml` dev-deps. No new prod deps. Verify.
- [ ] F2: No new phase R0.5 or similar. Phase count still 11 (R0 + R1..R10).
- [ ] F3: Week total still 16.25 rounded to "~16". No change.

## Deliverables

Write result to `~/omsrs/PORT-PLAN-v9-audit-result.md`.

Format:
- **Verdict** (ACK or NACK) at top.
- **P0 findings** — blockers, each with required fix.
- **P1 findings** — substantial.
- **P2 findings** — polish.
- **Verified closures** — what v9 actually fixed from v8.
- **Minimum changes for ACK** — numbered list if NACK.

ACK permits starting R1.
NACK means fix and re-audit as v10.

Be adversarial but fair. If v9 closes everything cleanly, return ACK. Prior 8 audits each surfaced a new P0 in "fixed" areas — this is the threshold at which the base rate starts to look like plan churn rather than real discovery.
