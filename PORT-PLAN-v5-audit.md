# Audit: omsrs Port Plan v5

Adversarial audit. Author (Claude) is NACK'd from self-auditing.

**Read first**:
- `~/omsrs/PORT-PLAN.md` (v5 — plan under audit)
- `~/omsrs/omspy-source-notes.md` (source notes, v5-updated)
- `~/omsrs/PORT-PLAN-v4-audit-result.md` (prior NACK — v5 must close all of it)

## Audit chain context

- v1 NACK: Polymarket scope creep.
- v2 NACK: file-level scope, invented `OrderRequest`, mis-mapped `simulation/virtual.py`.
- v3 NACK: 3 P0 (OHLC/Ticker inheritance deferred but used; wrong enum values; persistence cfg partial) + 5 P1 + 8 P2.
- v4 NACK: 3 P0 (Ticker RNG not budgeted; Clock declared but not threaded; SQL schema said 36 not 37 columns) + 5 P1 (pytest-collected count 226 not 229; R6/R7 split wrong; phase arithmetic 15 not 14; R5 tight; 30 not 22-28 weeks expected) + P2 consistency.

v5 claims to close all of v4's findings.

## Audit stance

Stay in scope. Pure Rust library port of omspy core. Do NOT raise Polymarket / barter / `/poly/` concerns.

Be adversarial on whether v5 actually fixes v4. Same framing: v4 **looked** fixed after v3, then v4 audit found 3 new P0. Every fix-claim must be rechecked against upstream source.

## Audit checklist

### A. v4 P0 closure

- [ ] A1 (P0.1 Ticker RNG): v5 §6 D9 declares `Ticker` seed-exact tests as parity exception (`rand_distr::Normal` + `SmallRng`, statistical asserts). Verify:
  - `omspy-source-notes.md` §14 exists and lists the specific excluded tests.
  - The exception is documented as **behavioral, not seed, parity**.
  - R5 in §7 explicitly says "minus seed-dependent `test_ticker_*`, expected ~3-4 excused".
  - Count expected excused tests: grep `~/refs/omspy/tests/simulation/test_models.py` for `test_ticker_` functions, assess which use `random.seed(...)` or expect exact float values.
- [ ] A2 (P0.2 Clock threading): v5 §6 D4 enumerates Clock use-sites. Verify:
  - Every time-reading upstream site from v4 P0.2 is listed (Order, OrderLock, VOrder, Response, VirtualBroker, ReplicaBroker).
  - `CompoundOrder` + `OrderStrategy` propagation explicitly noted (like `connection` propagation).
  - `Arc<dyn Clock>` with `#[serde(skip)]` is semantically sound — does serde `default` actually work for `Arc<dyn Trait>`? Check; this requires a default function.
  - `MockClock` uses `parking_lot::Mutex` (needs dep). Confirm dep added (or flag as missing).
- [ ] A3 (P0.3 SQLite schema 37 columns): v5 §6 D7 lists 37 columns. Verify:
  - Walk each of the 37 names; cross-check against upstream `order.py::create_db` `CREATE TABLE orders` DDL character-by-character.
  - Column **order matches** upstream (some SQLite ORMs care).
  - Type affinities match (text/integer/real).
  - Count: 37 exactly, not 36 or 38.
  - `JSON` column name is uppercase (upstream uses `JSON`).

### B. v4 P1 closure

- [ ] B1 (P1.1 226 denominator): source notes §11 revised table. Per-file: `test_base` 12, `test_models` 11, `test_utils` 22, `test_order` **105** (duplicate `test_compound_order_update_orders`), `test_order_strategy` 7, `test_simulation_models` **50** (duplicate `test_vorder_modify_by_status_partial_fill`), `test_simulation_virtual` **79** (duplicate `test_virtual_broker_ltp`). Portable subset: 10+11+12+104+7+50+32=**226**. Verify arithmetic + each duplicate claim against upstream line numbers.
- [ ] B2 (P1.2 R6/R7 split): v5 R6=22, R7=10. Verify by grep — how many pytest-collected non-FakeBroker, non-`generate_*` tests in `test_simulation_virtual.py`? After duplicate elimination.
- [ ] B3 (P1.3 phase arithmetic): v5 sums 0.75+1+3+1+2+1+1.25+1.5+1+3 = **14.5 + R0 0.5 ≈ 15**. Check arithmetic. v5 says expected 30 via "1.5 weeks rework × 10 phases". Verify rationale.
- [ ] B4 (P1.4 R5 2 weeks): v5 R5 now 2 weeks. Given `simulation/models.py` is 540 Python LOC + 50 tests + Ticker RNG design + 8 response types + 5 enums + OrderFill behavior — is 2 weeks realistic for 800 Rust prod + 600 test LOC? At 700 LOC/week that's 1.4 weeks prod + a week for tests. Flag if still optimistic.
- [ ] B5 (P1.5 test LOC with 226): 226 × 20 = 4520. v5 budgets 5520 total (4520 + 500 fixture + 300 proptest + 200 clock-harness). Plausible.

### C. v4 P2 closures — new Ticker/Clock-specific

- [ ] C1: v5 acknowledges `OHLCVI` has no transitive MVP user aside from R5 test bodies. Is there a cleaner cut — exclude `OHLCVI` tests and keep it out of MVP? Or is OHLCVI's inclusion load-bearing?
- [ ] C2: v5 moves `VirtualBroker` away from "single-user MVP" — does the **expected time** for R6 (1 week) still hold for the multi-user surface? Multi-user state + per-user order isolation is extra work.
- [ ] C3: v5 §14 exception register names only `test_ticker_ltp`. If there are other seed-dependent tests (`test_ticker_update`, etc.), they need entries too.

### D. New v5 issues

- [ ] D1: `Arc<dyn Clock>` with `#[serde(skip, default = "clock_system_default")]`. Serde `default` callbacks return an owned value; does it return `Arc<dyn Clock>` safely? Verify the function signature + ensure `Arc<dyn Clock + Send + Sync>` is serializable-skippable without trait-object complications.
- [ ] D2: Clock propagation in `CompoundOrder::add_order` + `add` — need to modify the upstream-faithful logic to inject both `connection` AND `clock`. Adds complexity to R8 not budgeted.
- [ ] D3: **New dep**: `rand_distr` for `Normal` distribution. `parking_lot` for `Mutex` in MockClock. Both are widely used. Confirm in Cargo.toml design.
- [ ] D4: v5 says parity gate is `218 of 226 pass`. 8 slack = 3-4 Ticker + ~4 tz/DST edge cases. Enumerate tz/DST candidates: which upstream tests are tz-sensitive? OrderLock timezone tests? Order expiry with `timezone="local"`?
- [ ] D5: v5 R3 test count "104 of 105" (CompoundOrder share moved to R8) — but the R3 phase gate says "104 tests in scope". Is that 104 Order-only tests or 104 total (Order + CompoundOrder)? Double-count risk. Explicitly split: how many Order-only vs CompoundOrder-only in `test_order.py`?
- [ ] D6: v5 acceptance criterion 9 requires `ReplicaBroker.run_fill` byte-exact over 3 runs with seed `0xDEADBEEF` — but Rust `SmallRng` determinism requires freezing its algorithm too (xoshiro256++ in current rand). Future `rand` upgrades could change output. Lock `rand` MAJOR version in Cargo.toml.

### E. Source-notes consistency

- [ ] E1: `omspy-source-notes.md` §11 revised table matches v5's 226 denominator.
- [ ] E2: `omspy-source-notes.md` §14 parity exception register exists and lists Ticker tests explicitly (not a placeholder).
- [ ] E3: Decision table §12 still lists `OHLC/V/VI/Ticker` as MVP.
- [ ] E4: `Status`/`OrderType` enum values in notes match v4 corrections.

### F. Acceptance robustness

- [ ] F1: Criterion 3 is binary: ≥218 pass AND each failure in §14. What if a Ticker test passes unexpectedly (e.g. unlucky seed match)? Should §14 entries be excused permanently or require fresh audit?
- [ ] F2: Criterion 9 determinism: "byte-exact over 3 runs" = self-consistent; is there also a "byte-exact across CI machines" requirement? Needed if OrderFill lives cross-platform.

### G. What v5 could still hide

- [ ] G1: Does `OrderFill` upstream use Python `random`? Grep `omspy/simulation/models.py:505+`. If yes, it's a second Ticker-style RNG parity exception that v5 hasn't budgeted.
- [ ] G2: Do response timestamps use `pendulum` (so Clock-threaded) or `datetime`? Either way, if wall-clock, must be Clock-injected.
- [ ] G3: `VUser` state — does it have any time-sensitive field that needs Clock?

## Deliverables

Write result to `~/omsrs/PORT-PLAN-v5-audit-result.md`.

- **ACK**: v5 sound, start R1.
- **NACK**: list P0 / P1 / P2.

Be adversarial. The previous four audits each found new P0s where prior plans "looked fixed". Do not let v5 pass unless every claimed fix actually holds against upstream source.
