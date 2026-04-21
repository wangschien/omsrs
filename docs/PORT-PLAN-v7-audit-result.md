# PORT-PLAN v7 audit result

Verdict: **NACK**. Do **not** start R1 yet.

v7 fixes the headline denominator model in `PORT-PLAN.md`: `test_ticker_ltp` is out of the parity denominator, the main phase table sums to 237, `VirtualBroker`/`ReplicaBroker` response construction is now described accurately, and the pre-authorized `#[ignore]` loophole is gone.

The plan is still not executable enough. The remaining blockers are smaller than v6, but they are in the exact areas v7 claims are closed: denominator/LOC consistency, phase allocation, and acceptance semantics for known failing parity tests.

## P0 findings

### P0.1 `omspy-source-notes.md` still has a live Rust test LOC section using the old 238 denominator

`PORT-PLAN.md` says every LOC calc uses 237 and gives the corrected math:

```text
237 * 20 = 4740
+ 500 fixtures
+ 300 proptest
+ 200 clock harness
+ 50 Ticker statistical replacement
= 5790
```

But `omspy-source-notes.md` still says:

```text
### Rust test LOC (v6-corrected, denominator = pytest items)
- 238 pytest items * 20 = 4760 LOC base
- Total test LOC ~5760
```

This is not just legacy text; it is the current Rust test LOC section immediately after the v7 §14 register. It contradicts the v7 decision and leaves two active test LOC budgets: 5790 in the plan, 5760 in the notes.

Required fix: update the source-notes Rust test LOC section to the v7 237+statistical math, or explicitly delete it and point to `PORT-PLAN.md` §3.

### P0.2 R3/R8 `test_order.py` split is not mechanically supported

The total `test_order.py` arithmetic is correct:

- 106 function bodies.
- 1 overwritten duplicate (`test_compound_order_update_orders`) => 105 collected bodies.
- `test_get_option` has 3 params, so +2 extra collected items => 107 total.
- Drop `test_get_option[...]` => 104 portable items.

But the v7 phase split says R3=63 and R8=41. A collected-name split gives:

```text
portable test_order.py items: 104
test_compound_order_* items: 40
non-compound portable items: 64
```

`omspy-source-notes.md` says classification is by test-function-name keyword, and `PORT-PLAN.md` says R3 excludes `test_compound_*`. Under that rule, the split is 64/40, not 63/41. If the intended 41st R8 item is `test_order_has_parent` or another mixed test, it must be named explicitly. Otherwise the phase gates sum to 237 only by arithmetic, not by an auditable assignment of upstream pytest items.

Required fix: add an exact R3/R8 item list or a deterministic rule that identifies the one non-`test_compound_order_*` item assigned to R8.

## P1 findings

### P1.1 §14(B) has no runnable acceptance mechanism

v7 says no `#[ignore]` anywhere, while allowing up to 7 codex-approved failures inside the 237-item denominator. That is disciplined, but `cargo test` exits non-zero on any failing test.

The acceptance line:

```text
cargo test -p omsrs --all-features: >= 230 of 237 parity tests pass
```

is therefore underspecified. Either there must be a custom parity runner that records approved failures, or §14(B) tests need a non-`#[ignore]` mechanism that still lets the command complete. As written, the plan simultaneously permits failures and names plain `cargo test` as the gate.

### P1.2 `test_ticker_ticker_mode` can become an excused failure without a statistical pass criterion

v7 correctly removes the `#[ignore]` escape hatch, but if `test_ticker_ticker_mode` moves into §14(B), the plan does not say what evidence is required. For a probabilistic parity test, "flake under statistical seeds" needs a threshold, for example pass rate over N deterministic seeds/runs, or a replacement property test that demonstrates the mode switch behavior.

Without that, the test can be excused rather than bounded.

### P1.3 `OrderStrategy::add(compound)` clock propagation is still incomplete

v7 says `OrderStrategy::add(compound)` overwrites `compound.clock`, and "compound then cascades to its contained orders on next mutation." If there is no next mutation, already-contained orders keep their old clocks indefinitely.

That breaks the stated Rust-only coherence goal. It can also affect clock-sensitive strategy paths if a pre-populated `CompoundOrder` is added and then read/run/saved without adding another child order.

Required fix: either cascade immediately to all contained orders during `OrderStrategy::add`, or document that `OrderStrategy` only owns the compound clock and does not own child order clocks.

### P1.4 R10 parity sweep is not separated from non-parity statistical tests

`test_ticker_ltp_statistical` is explicitly non-parity and outside the 237 denominator, but the R10/acceptance command is plain `cargo test -p omsrs --all-features`. That will run parity and non-parity tests together unless the Rust test layout or command is scoped.

Required fix: define separate commands, e.g. one parity-only command and one statistical command, or state the exact Rust test naming/filtering scheme.

## P2 findings

### P2.1 `OHLCVI` wording is fixed in the decision row but still stale in the MVP summary

The §12 row is now correct:

```text
OHLCVI ... NOT required by VQuote/VirtualBroker inheritance
```

But the summary later says:

```text
OHLC/OHLCV/OHLCVI/Ticker (needed for VQuote inheritance + VirtualBroker deps)
```

That reintroduces the same rationale residue. `OHLCVI` is kept only for `test_ohlcvi` parity, not because `VQuote` needs it.

### P2.2 R1 model labels are off

The 3 R1 `test_models.py` items are all `BasicPosition` tests. There is no upstream `QuantityMatch` test in `tests/test_models.py`. The count is fine, but the label "QuantityMatch 1, BasicPosition 2" is wrong and will confuse phase-gate tracking.

### P2.3 Dependency plan is not self-contained

`PORT-PLAN.md` §7 says "Cargo dependency plan (unchanged from v6)" but v7 does not include the dependency table. That means the prior `rand = "=0.8"` concern cannot be audited from the current plan. If the exact pin remains, document the maintenance tradeoff and its limited scope; if it was removed or loosened, state that in v7.

## Verified closures / notes

- Main plan denominator: `PORT-PLAN.md` consistently uses 237 for the parity gate and phase table.
- Phase sum arithmetic: `20 + 10 + 63 + 10 + 54 + 22 + 10 + 41 + 7 = 237`.
- `test_utils.py`: 22 function names; total 34 pytest items after `stop_loss_step_decimal` (8) and `update_quantity` (6). Portable = `34 - 1 tick - 8 stop_loss - 8 load_broker = 17`.
- `test_order.py`: total 107 and portable 104 are correct.
- `test_ticker_ltp` is in §14(A), removed from the 237 denominator, not counted as slack.
- `VirtualBroker.order_place`, `order_modify`, and `order_cancel` construct `OrderResponse`; upstream lines 588-668 confirm this.
- `ReplicaBroker.order_place`, `order_modify`, and `order_cancel` return `VOrder`, not `OrderResponse`; upstream lines 768-811 confirm this.
- `test_ticker_ticker_mode` no longer has a pre-authorized `#[ignore]`.
- §14(B) starts empty; listed rows are candidates only.
- `utils.tick` is deferred; `VirtualBroker` is correctly multi-user.
- The legacy 226 table is still present, but it is clearly marked superseded/history. Not a blocker by itself.
- Week math is acceptable: implementation phases sum to 15.75 weeks; plus R0 0.5 = 16.25, rounded to "~16"; expected rework total is about 31 weeks.

## Minimum changes for ACK

1. Replace the stale 238/~5760 Rust test LOC section with the v7 237/5790 math.
2. Make the R3/R8 `test_order.py` split auditable by listing the exact item assigned to R8 beyond the 40 `test_compound_order_*` items, or change the split to 64/40 and rebalance phases.
3. Define how approved §14(B) failures are represented in a Rust test run without `#[ignore]`, and separate parity sweep execution from the non-parity statistical test.
4. Fix the `OrderStrategy::add` clock cascade rule.
