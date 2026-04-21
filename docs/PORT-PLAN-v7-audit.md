# Audit: omsrs Port Plan v7

Adversarial audit. Author NACK'd from self-auditing.

**Read first**:
- `~/omsrs/PORT-PLAN.md` (v7)
- `~/omsrs/omspy-source-notes.md` (v7-updated)
- `~/omsrs/PORT-PLAN-v6-audit-result.md` (prior NACK)

## Chain

v1-v6 all NACKed. v6 finding summary:
- P0.1 denominator internally inconsistent (test_ticker_ltp "excluded" AND "inside slack")
- P0.2 phase gates sum 234 ≠ 238
- P1.1 R5 2w tight; P1.2 CompoundOrder::add clock rule ambiguous; P1.3 Response clock propagation over-stated on ReplicaBroker; P1.4 test_ticker_ticker_mode had `#[ignore]` loophole
- P2.1 §11 total-items column wrong; P2.2 OHLCVI wording residue.

v7 claims all closed.

## Stance

Pure library port only. Adversarial on whether v7 numbers actually add up + clock rules hold against upstream.

## Checklist

### A. Denominator + gate arithmetic (P0.1+P0.2 closure)

- [ ] A1: v7 denominator = 237 uniformly. Search `PORT-PLAN.md` + `omspy-source-notes.md` for any "238" or "226" leftover.
- [ ] A2: Phase gate sum: R1 20 + R2 10 + R3 63 + R4 10 + R5 54 + R6 22 + R7 10 + R8 41 + R9 7 = **237**. Verify arithmetic.
- [ ] A3: R1 20 items = 17 portable utils + 3 portable models. Verify upstream `test_utils.py` portable item count = 17 (34 total − 1 tick − 8 stop_loss_step_decimal − 8 load_broker_ = 17). And `test_models.py` for QuantityMatch/BasicPosition = 3 items.
- [ ] A4: §11 total-items column: `test_utils` 34, `test_order` 107. Confirm by arithmetic:
  - `test_utils.py`: 22 function names − 2 (parametrize hosts) + 6 + 8 = 34. ✓
  - `test_order.py`: 105 bodies (post-dup-collapse) + 2 extra from `test_get_option[...]` parametrize = 107. Or is it: 106 names with 1 overwritten, then `test_get_option` adds 2 extra params for 3 items total → 105+2=107. Verify.
- [ ] A5: Confirm `test_ticker_ltp` is listed as §14(A) "removed from portable set" not as §14(B) "excused failure". v7 is unambiguous?

### B. Clock rules (P1.2 + P1.3)

- [ ] B1: `CompoundOrder::add(order)` overwrites `order.clock = self.clock`. Verify the plan says "overwrites" unconditionally (no detection heuristic). Does this semantic diverge from upstream? Upstream doesn't propagate clock at all (it uses implicit `pendulum.now()`). Rust-only addition for coherence — is this documented?
- [ ] B2: `VirtualBroker::order_modify` / `order_cancel` now explicitly list `OrderResponse` construction. Verify against upstream `simulation/virtual.py`:
  - `VirtualBroker.order_place` returns `OrderResponse` (virtual.py:588-625)?
  - `VirtualBroker.order_modify` returns what?
  - `VirtualBroker.order_cancel` returns what?
- [ ] B3: `ReplicaBroker.order_place` does NOT construct `OrderResponse`. Verify — what does upstream return from `ReplicaBroker.order_place`? (Check `virtual.py:768-791`.)

### C. §14 rules (P1.4 closure)

- [ ] C1: `test_ticker_ticker_mode` no longer has pre-authorized `#[ignore]`. v7 §14 says it moves to §14(B) at R5 gate only with codex approval. Verify.
- [ ] C2: "No `#[ignore]` anywhere" rule explicit.
- [ ] C3: Slack budget: 7 items (237 − 230). Empty at v7 start; fills only via phase-gate audits.

### D. Source-notes residue (P2 closure)

- [ ] D1: `OHLCVI` in §12 now says "NOT required by VQuote/VirtualBroker inheritance". Verify.
- [ ] D2: `utils.tick` = defer (confirmed in v5 already).
- [ ] D3: `VirtualBroker` = multi-user (confirmed in v6).
- [ ] D4: Rust test LOC = 5790 (237 × 20 + 500 + 300 + 200 + 50 Ticker replacement). Verify math.
- [ ] D5: "Legacy" §11 table — is it still lurking in the file? Remove or clearly mark superseded.

### E. New v7 issues

- [ ] E1: v7 says "no `#[ignore]` anywhere". Good for discipline but: R5 may discover that the Rust `test_ticker_ticker_mode` is legitimately flaky under all seeds. If it enters §14(B) as "probabilistic parity", is there a clear pass criterion (e.g. passes 9/10 runs)? Or the test just gets excused?
- [ ] E2: `OrderStrategy::add(compound)` overwrites `compound.clock`. That's a Rust-only addition. Does it cascade? v7 says "compound then cascades to its contained orders on next mutation." What if there's no next mutation? Contained orders keep their old clocks indefinitely. Flag if this can cause test divergence.
- [ ] E3: "non-parity Rust test" `test_ticker_ltp_statistical` lives in `tests/statistical/`. If R10 parity sweep runs `cargo test`, both parity and statistical tests run. The plan should say `cargo test --test parity_*` or similar to scope.
- [ ] E4: `rand = "=0.8"` hard-lock. Dependencies that upgrade `rand` transitively will fail to resolve. Flag as maintenance cost; is this really worth it given no RNG determinism requirement in parity tests (only in Ticker statistical)?

### F. Phase math

- [ ] F1: Phase sum: 0.75 + 1 + 3 + 1 + 2 + 1.25 + 1.25 + 1.5 + 1 + 3 = 15.75 weeks. + R0 0.5 = **16.25**. v7 says "~16 weeks". Close. Expected 31 = 16 + 15 rework (1.5 weeks × 10 impl phases).
- [ ] F2: R1 0.75 weeks for 20 tests + utils + 2 proptest modules. Rate ~27 tests/week.

### G. Hidden gap

- [ ] G1: Does v7 say anything about **cross-machine Decimal determinism** beyond the existing chrono note? Decimal arithmetic via `rust_decimal` is pure; should be byte-deterministic. Verify no risk.
- [ ] G2: If a test like `test_order_expires` uses wall-clock and enters §14(B), MVP gate passes. But what about production reliability? Clock-sensitive tests via MockClock should pass structurally.

## Deliverables

Write result to `~/omsrs/PORT-PLAN-v7-audit-result.md`.

ACK: start R1.
NACK: list P0/P1/P2.

Be adversarial but stay in scope. Prior 6 audits each found new P0s in "fixed" areas.
