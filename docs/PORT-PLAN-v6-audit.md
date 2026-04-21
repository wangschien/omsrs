# Audit: omsrs Port Plan v6

Adversarial audit. Author (Claude) is NACK'd from self-auditing.

**Read first**:
- `~/omsrs/PORT-PLAN.md` (v6 — plan under audit)
- `~/omsrs/omspy-source-notes.md` (source notes, v6-updated)
- `~/omsrs/PORT-PLAN-v5-audit-result.md` (prior NACK — v6 must close)

## Audit chain

v1 NACK: Polymarket scope creep.
v2 NACK: file-level scope, invented `OrderRequest`.
v3 NACK: 3 P0 (OHLC inheritance, enum values wrong, persistence partial).
v4 NACK: 3 P0 (Ticker RNG not budgeted, Clock not threaded, SQL 36 not 37 columns) + 5 P1.
v5 NACK: 2 P0 (denominator 226 not pytest items, §14 Ticker placeholder) + 5 P1 (R3/R8 double-count, R5 tight, Clock naming inconsistent, single-user contradiction, 8-slack unauditable) + 6 P2 (AC 9 wrong, dep table missing, source notes stale, etc).

v6 claims to close all v5 findings.

## Audit stance

Stay in scope. Pure Rust port. Do NOT re-raise Polymarket / barter / `/poly/`.

Adversarial focus: every prior NACK iteration found new P0s in "apparently fixed" areas. Verify each v5 fix against upstream source.

## Audit checklist

### A. v5 P0 closure

- [ ] A1 (P0.1 denominator 238): verify per-file pytest-item counts in `omspy-source-notes.md` §11.
  - Run or reason about `pytest --collect-only` on each in-scope file.
  - Confirm `test_order_lock_can_methods` parametrize = 3 items.
  - Confirm `test_update_quantity` parametrize = 6 items.
  - Confirm `test_vorder_is_done` parametrize = 6 items.
  - Confirm duplicates collapse as documented (3 files).
  - Sum arithmetic: 10+13+17+104+7+55+32 = **238** — verify.
  - Check if any portable test has additional parametrize that v6 missed.
- [ ] A2 (P0.2 §14 exact ids): verify `omspy-source-notes.md` §14 now lists exact upstream test ids. Cross-check the 5 non-excluded `test_ticker_*` claims against upstream source:
  - `test_ticker_defaults` — structural?
  - `test_ticker_is_random` — structural?
  - `test_ticker_ohlc` — asserts?
  - `test_ticker_ticker_mode` — assertion on `ltp != 125`?
  - `test_ticker_update` — manual only?
- [ ] A3 (v5 P2.1 schema closed) — already done. Skip.

### B. v5 P1 closure

- [ ] B1 (P1.1 R3/R8 split): `omspy-source-notes.md` §11.1 now claims 63 Order / 41 CompoundOrder = 104. Verify by grepping `~/refs/omspy/tests/test_order.py` for `def test_compound_*` vs `def test_order_*` (roughly). Don't require exact counts but flag if off by >5.
- [ ] B2 (P1.2 R5 2 weeks): v6 R5 still 2 weeks. v5 audit said "remains optimistic". Is v6's 2 weeks for 54 portable tests + Clock threading in `VOrder`/`Response` + OrderFill port realistic? Or flag as P1 again.
- [ ] B3 (P1.3 Clock naming unified): v6 §6 D4 uses `clock_system_default` throughout. Verify no `Clock::system` residue.
- [ ] B4 (P1.3 Clock propagation map): v6 D4 enumerates construction sites + propagation paths. Verify against upstream source — specifically:
  - `CompoundOrder::add_order` propagation (upstream order.py:843-876).
  - `CompoundOrder::add` propagation (upstream order.py:1209-1259).
  - `OrderStrategy::add` propagation (upstream order.py:1423-1451).
  - `VirtualBroker.order_place` constructs `VOrder` (upstream virtual.py:588-625) and `OrderResponse` — does v6 say clock flows into both?
  - `ReplicaBroker.order_place` — same.
- [ ] B5 (P1.4 R6/R7 split source-notes): v6 source notes §12 now marks VirtualBroker as "multi-user per upstream". Verify contradiction is gone.
- [ ] B6 (P1.5 8-slack accountability): v6 §14 lists 1 confirmed (Ticker) + tz/DST candidates. Candidate resolution at phase gates. Is the process binding — will every final §14 entry require codex approval at its phase gate? Plan says yes. Verify.

### C. v5 P2 closure

- [ ] C1 (P2.2 AC 9 corrected): v6 §9 criterion 9 now says "state-transition determinism" for `ReplicaBroker.run_fill` (no RNG seed mention). Verify source — does `run_fill()` really have no RNG? Read `simulation/models.py:505+` for `OrderFill.update()` and `simulation/virtual.py:813+` for `run_fill`.
- [ ] C2 (P2.3 dep table): v6 §7 lists deps with versions + features. Verify:
  - `rand = "=0.8"` (major locked)
  - `rand_distr = "0.4"`
  - `parking_lot = "0.12"`
  - `chrono-tz = "0.8"`
  - `rusqlite = "0.31"` optional
- [ ] C3 (P2.4 source-notes cleanup): check whether:
  - Legacy §11 "function-name" table marked legacy / superseded.
  - `utils.tick` marked defer (not MVP).
  - `VirtualBroker` marked multi-user.
  - Rust test LOC updated to reflect 238 denominator.
- [ ] C4 (P2.5 `OHLCVI` minimum): v6 keeps `OHLCVI` to preserve test parity. OK noted. Flag only if plan misrepresents necessity.
- [ ] C5 (P2.6 cross-machine determinism): v6 §10 R.10 documents local-tz drift and CI matrix to-do. Acceptable for MVP or blocker?

### D. New v6 issues possible

- [ ] D1: v6 total 16 clean / 31 expected. Arithmetic: 0.75+1+3+1+2+1.25+1.25+1.5+1+3 = 15.75 + R0 0.5 = 16.25. Verify.
- [ ] D2: Rust test LOC = 5760 with 238 × 20 + 500 + 300 + 200 = 4760+1000 = 5760. Verify.
- [ ] D3: `rand = "=0.8"` lock — if 0.9+ is released during dev, port stuck. Is this worth the lock? Plan says yes for OrderFill determinism; re-question: upstream has no RNG in OrderFill (C1), so the determinism-of-SmallRng argument is only for Ticker statistical tests. Is the lock justified?
- [ ] D4: v6 adds 2 proptest modules in R1 (`quantity.rs`, `position.rs`), 1 in R3 (`lifecycle.rs`). Plan's "3 proptest modules" matches. Verify.
- [ ] D5: Clock propagation through `CompoundOrder::add` backfill — this is a side-effect mutation on an existing `Order`. Does Rust have interior mutability here, or must callers re-construct? Plan silent. Flag.

### E. Source-notes residue

- [ ] E1: `omspy-source-notes.md` §11 now pytest-item based. Legacy section labeled or removed?
- [ ] E2: §12 `utils.tick` = defer, `VirtualBroker` = multi-user. Verify.
- [ ] E3: §14 Ticker exception exact id (`test_ticker_ltp`), no "+ related" prose.
- [ ] E4: "MVP totals" summary mentions multi-user VirtualBroker.

### F. Implementation-detail bombs

- [ ] F1: `Arc<dyn Clock + Send + Sync>` — is `Send + Sync` required for any MVP path? If `Order` is Send+Sync-passed across threads, yes; if not, relax bound. Plan assumes yes.
- [ ] F2: `MockClock.advance(Duration)` — `parking_lot::Mutex` is `!Send` on some features; confirm default build allows `Send`.
- [ ] F3: Parametrize tests in Rust — `rstest` crate offers parametrize. Plan doesn't list it. Alternative: duplicate test fns per param (acceptable but verbose). Flag.

## Deliverables

Write result to `~/omsrs/PORT-PLAN-v6-audit-result.md`.

- **ACK**: start R1.
- **NACK**: list P0 / P1 / P2.

Be adversarial. Every prior plan passed some audit clauses and failed others. v6 must either pass clean or surface what's left.
