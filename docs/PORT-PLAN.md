# omsrs — Rust Port of omspy (v11)

v10 NACK (0 P0 + 1 P1 + 1 P2) closed. v9 and earlier closures held. Scope unchanged: pure Rust library port of omspy core.

Upstream: `~/refs/omspy/` (omspy 0.1.0, updated 2025-12-01).

Cumulative v1–v10 closures are folded into §4–§10; this §1 lists only the deltas relative to v10. Prior-version numeric residue lives in `PORT-PLAN-history.md`, not here.

## §1 v11 corrections over v10

### P1 fix
1. **`excused.toml` present-but-empty case defined** (§4.1.2): a file that exists and parses but contains zero `[[excused]]` rows is explicitly equivalent to absent (`excused_set = {}`). The TOML schema uses `#[serde(default)] excused: Vec<ExcusedRow>` so a committed empty R0 file deserializes cleanly. Missing row-level fields still route to exit code 6 via no-default serde on `ExcusedRow`. Resolves the conflict between §4.1.2 and `omspy-source-notes.md` §14 ("`excused.toml` starts empty at R0").

### P2 fix
2. **`parity_runner_smoke` coverage matrix expanded** (§4.1.5): replaces the prose "asserts 0/1/2/3/4/5" with a 13-row table covering every exit code including exit 6 (malformed TOML, wrong shape, missing `rationale` / `approved_at` / `approved_by`) plus the present-empty success case. Any future change to §4.1.2 exit semantics requires a matching smoke-matrix update.

## §1.prev v10 corrections over v9 (retained for continuity)

### P0 fix
1. **Parity gate argv contract made unambiguous.** v9 said `scripts/parity_gate.sh` passes `--report` through `cargo` to the parity binary, which would collide with `libtest_mimic::Arguments::from_iter` (it doesn't know `--report`). v10 removes `--report` entirely: the parity binary **always** emits its report and the gate arithmetic, unconditionally, on every run. `scripts/parity_gate.sh` passes no custom flags — only libtest-mimic-recognised ones if the caller specified any. §1, §4.1.1, §4.1.4, and §9.3 all say the same thing now.

### P1 fix
2. **`excused.toml` malformed-file handling aligned with required validation** (§4.1.2): only an **absent** file is treated as empty. Any file that exists but fails TOML parse, schema-deserialization, or missing-required-field checks exits non-zero with a dedicated exit code (6, "excused.toml invalid"). No silent fall-through to empty.

### P2 fixes
3. **Stale parity-gate command residue removed from §1.** v10's §1 no longer embeds the prior-version nightly flags or custom `--report` argv; the only live gate command lives in §4.1.4.
4. **Stale pre-v7 numeric strings removed from §1.** The prior-version LOC budget numbers that v9's §1 was still embedding live only in `PORT-PLAN-history.md` now.
5. **Source-notes stale v7 heading fixed.** `omspy-source-notes.md` "MVP parity gate" heading relabelled from `(v7)` to `(current)`.
6. **Ticker derivation tightened** (§6 D10): quotes upstream's literal `random.gauss(0, 1) * self._ltp * 0.01` (not a named default), and uses the exact `p = Φ(0.02) − Φ(−0.02) = 0.0159566` before rounding to 0.02 in prose.
7. **Manifest path and statistical target harness settings specified** (§4.1.1, §9.4): the manifest is loaded via `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/rust-tests/parity-item-manifest.txt"))`; the statistical target declares `[[test]] name = "statistical" harness = false` in `Cargo.toml` so it uses the same `libtest-mimic` wrapper pattern as parity.
8. **v9 P2.1 grep hygiene cleared.** Prior-version nightly-libtest flag strings no longer appear in live plan text (only in prior-version history in `PORT-PLAN-history.md`).

## §2 MVP symbol inventory (unchanged; see `omspy-source-notes.md` §12)

## §3 LOC + test budget (v8, unchanged from v7)

### Python MVP: ~2850 LOC (unchanged)

### Rust prod LOC: 5000 (unchanged)

### Rust test LOC (denominator 237)

- 237 × 20 = **4740**
- Shared fixtures / helpers: 500
- Proptest modules (3): 300
- Clock test harness + MockClock fixtures: 200
- Ticker statistical replacement (separate module, not in 237): 50
- **Total test LOC = 5790**

## §4 Parity gate

**≥ 230 of 237 portable pytest items pass** (≥ 97.0%), enforced by `scripts/parity_gate.sh`.

7-item slack reserved for §14(B) excused failures. Every §14(B) entry requires codex approval at its phase gate and must be listed in `tests/parity/excused.toml` with upstream test id + rationale. No `#[ignore]` anywhere.

`test_ticker_ltp` out of scope (not denominator, not slack).

### §4.1 §14(B) runner mechanics (stable Rust, no `-Z`)

#### §4.1.1 Harness

- Parity tests live under `tests/parity/` as one integration target (`[[test]] name = "parity" harness = false` in `Cargo.toml`).
- The binary uses **`libtest-mimic`** (stable-Rust test-harness library): `tests/parity/main.rs` collects every parity test as a `libtest_mimic::Trial`, parses argv via `libtest_mimic::Arguments::from_args()` (so libtest-compatible flags like `--list`/`--test-threads` keep working), runs them via `run(args, trials)`, then post-processes the `Conclusion` (pass/fail sets) before exiting.
- The parity binary defines **no custom argv flags** of its own. The gate report is emitted unconditionally on every run; the exit code encodes gate pass/fail (see §4.1.3). No `--report` / `--gate` / etc. flags are parsed.
- Individual tests are written as normal `fn()` with a stable name (e.g. `fn test_order_timezone()`); they are registered by a `register_parity_tests!` macro that emits a static `&[Trial]` list. No `#[test]` / `#[ignore]` attributes, no nightly flags.
- Every parity test name is mirrored to `rust-tests/parity-item-manifest.txt` — the frozen list of the 237 upstream pytest ids mapped 1:1 onto Rust trial names. The manifest is generated once per phase gate from `pytest --collect-only -q` and committed. It is loaded at runtime via:

  ```rust
  const MANIFEST: &str = include_str!(concat!(
      env!("CARGO_MANIFEST_DIR"), "/rust-tests/parity-item-manifest.txt"
  ));
  ```

  Path is crate-root-relative; `include_str!` is a stable-Rust macro and the file is embedded in the parity binary at compile time, so R10 runs have no filesystem-path ambiguity.

#### §4.1.2 `excused.toml` schema + validation

`tests/parity/excused.toml`:

```toml
[[excused]]
id = "test_order_timezone"
rationale = "pendulum vs chrono DST semantics"
approved_at = "R3"          # phase gate at which codex signed off
approved_by = "codex"
```

At parity-binary startup, before any trial runs, the harness:

1. Checks for `tests/parity/excused.toml`:
   - File **absent** ⇒ `excused_set = {}` (empty). Silent-empty case #1.
   - File **present and parses successfully as TOML but contains zero `[[excused]]` rows** (empty file, empty root table, or a document whose only content is comments/whitespace) ⇒ `excused_set = {}`. Silent-empty case #2. This is the intended R0 committed-empty state (`omspy-source-notes.md` §14 "No pre-authorized entries at R0").
   - File **present but fails TOML parse**, **or** deserialization into the schema below fails, **or** any individual `[[excused]]` row fails the field-presence checks ⇒ exit code **6** ("excused.toml invalid"), with the underlying parse / deser error printed. **No** silent fall-through to empty.

   Schema (concretely):

   ```rust
   #[derive(Deserialize)]
   struct ExcusedFile {
       #[serde(default)]
       excused: Vec<ExcusedRow>,
   }
   #[derive(Deserialize)]
   struct ExcusedRow {
       id: String,
       rationale: String,
       approved_at: String,
       approved_by: String,
   }
   ```

   `#[serde(default)]` on the top-level `excused` field is what makes silent-empty case #2 work: a present-and-empty TOML file deserializes to `ExcusedFile { excused: vec![] }`. Missing `id` / `rationale` / `approved_at` / `approved_by` on a row fails deserialization and routes to exit code 6 (no defaults on row fields).

2. Rejects duplicate `id` rows (exit code 2, "duplicate excused id").
3. Rejects any row whose `id` is not present in the manifest (exit code 3, "unknown parity id").
4. Rejects rows missing `rationale` / `approved_at` / `approved_by` (exit code 6 via serde, per schema above).
5. If env var `OMSRS_R0_GATE=1` is set: rejects any non-empty `excused_set` (exit code 4, "R0 must start with empty excused set").
6. Caps `|excused_set| ≤ 7` (exit code 5).

#### §4.1.3 Gate arithmetic

After all trials run, the harness computes:

- `passing_set` — trials whose `Outcome::Passed`.
- `failing_set` — trials whose `Outcome::Failed`.
- `excused_set` — ids loaded from `excused.toml`.

Exit code:
- `0` iff `failing_set ⊆ excused_set ∧ |passing_set| ≥ 230 ∧ |excused_set| ≤ 7`.
- `1` otherwise, with human-readable + JSON report on stdout.

#### §4.1.4 `scripts/parity_gate.sh`

Thin wrapper:

```sh
#!/usr/bin/env bash
set -euo pipefail
cargo test -p omsrs --test parity --release --all-features --no-run
exec cargo test -p omsrs --test parity --release --all-features
```

No custom flags forwarded to the parity binary — it emits its gate report and exit code unconditionally. No JSON parsing in shell; the `cargo test` exit code (returned by `exec`) is the gate. Invoked by §9.3.

#### §4.1.5 Self-test of the runner

A second integration target `tests/parity_runner_smoke` (`#[test]` fns, uses stable libtest) injects a scripted `excused.toml` + fake manifest + fake trial set and asserts each exit code is produced under the matched condition. Coverage matrix (each row is one `#[test]` fn):

| # | Case | Expected exit |
|---|---|---:|
| 1 | Absent `excused.toml` + all parity trials pass | 0 |
| 2 | Present-empty (zero rows) + all pass | 0 |
| 3 | Failing trial id ∈ excused_set, ≥ 230 pass, `|excused| ≤ 7` | 0 |
| 4 | Failing trial id ∉ excused_set | 1 |
| 5 | Excused trial id duplicated | 2 |
| 6 | Excused trial id absent from manifest | 3 |
| 7 | `OMSRS_R0_GATE=1` + non-empty excused | 4 |
| 8 | `|excused| > 7` | 5 |
| 9 | Malformed TOML (syntactically invalid) | 6 |
| 10 | Well-formed TOML, unexpected shape (e.g. `excused` is a string, not array) | 6 |
| 11 | `[[excused]]` row missing `rationale` | 6 |
| 12 | `[[excused]]` row missing `approved_at` | 6 |
| 13 | `[[excused]]` row missing `approved_by` | 6 |

This runs in CI alongside parity. Any change to §4.1.2 exit-code semantics requires a matching smoke-matrix update.

## §5 Crate layout (unchanged)

## §6 Design decisions

### D1–D3, D5–D9 unchanged from v6/v7.

### D4. Clock — v8 refinement

Unchanged mechanics (serde skip + `clock_system_default` default fn + `MockClock`). Semantic rules:

1. Every struct holding a `clock: Arc<dyn Clock + Send + Sync>` field. Default = `clock_system_default()`.
2. `CompoundOrder::add(order)`: **overwrites** `order.clock = self.clock.clone()`. Upstream analogue: `order.connection = self.connection` backfill becomes an unconditional overwrite for clock — simpler and avoids "was default supplied?" detection.
3. `CompoundOrder::add_order(**kwargs)`: new `Order` is constructed with `clock: self.clock.clone()` before any user-provided clock override. (Upstream inherits `connection` similarly.)
4. `OrderStrategy::add(compound)`: **overwrites `compound.clock = self.clock.clone()` AND immediately cascades to every already-contained child order in `compound.orders` within the same call** (iterate `compound.orders.iter_mut()` and assign each `order.clock = self.clock.clone()`). No "on next mutation" deferral. Not upstream behavior — upstream doesn't propagate clock at all; this is a Rust-only addition for coherence, and the immediate cascade is required for pre-populated compounds whose children may never mutate again.
5. Broker response clock propagation:
   - `VirtualBroker::order_place`: constructs `VOrder` + `OrderResponse`, both receive `self.clock`.
   - `VirtualBroker::order_modify`: constructs `OrderResponse` with `self.clock`.
   - `VirtualBroker::order_cancel`: constructs `OrderResponse` with `self.clock`.
   - `ReplicaBroker::order_place`: constructs `VOrder` with `self.clock`. **Does NOT construct `OrderResponse`** — upstream returns `VOrder` directly.
   - `ReplicaBroker::order_modify` / `order_cancel`: same; upstream returns `VOrder`.

### D10. Ticker RNG exception (v8 unchanged from v7, criterion added)

- Rust `Ticker` uses `rand_distr::Normal::new(0.0, 1.0)` + `SmallRng::seed_from_u64(seed)`.
- `test_ticker_ltp` is the **only** upstream Ticker test replaced. Rust equivalent lives in a separate non-parity module (`tests/statistical/test_ticker_ltp_statistical.rs`) and asserts mean/std over 1000 samples. Not counted toward parity ceiling.
- All 5 other `test_ticker_*` tests are parity items (no `#[ignore]`).
- `test_ticker_ticker_mode` asserts `ltp != 125` probabilistically. If Rust runs show flake, it becomes a §14(B) probabilistic-parity entry at R5 gate with codex approval — NOT `#[ignore]`. Its Rust parity test then asserts **≥ 95/100 successes** over `SmallRng::seed_from_u64(seed)` for `seed ∈ 0..100`, not a single-seed equality.
  - **Threshold derivation.** Upstream `simulation/models.py` hard-codes the perturbation as `diff = random.gauss(0, 1) * self._ltp * 0.01` (no named `price_mean_diff`; the `0.01` is literal). For `_ltp = 125`, a single mode-switched read yields `diff = Z · 1.25` with `Z ~ Normal(0, 1)`. The Rust Ticker rounds the resulting price to the nearest 0.05 tick, so "collision with 125" means rounded `diff` is exactly 0, i.e. `diff ∈ [−0.025, 0.025]`, i.e. `Z ∈ [−0.02, 0.02]`. Exactly:
    - `p = P(ltp == 125) = Φ(0.02) − Φ(−0.02) = 0.0159566…` (≈ 0.02 in prose below).
  - Over 100 independent seeds, `X ~ Binomial(100, p = 0.0159566)` has `E[X] ≈ 1.596`, `σ ≈ 1.25`, and `P(X ≥ 6) ≈ 0.00550` (exact binomial CDF). Therefore:
    - **Correct impl** passes ≥ 95/100 with probability ≈ 99.45% ⇒ false-reject ≈ 0.55%.
    - **Broken impl** that leaves `ltp == 125` every time fails 100/100 and is rejected with probability 1.
    - **Broken impl** that collides say 50% of the time fails ~50/100, far below the 95 bound.
  - 95/100 is the ~99.5th-percentile cutoff of the correct-impl distribution, rounded to the nearest integer outward for headroom.

## §7 Cargo dependency plan (restored inline in v8)

Pure library; no async runtime in the core crate. Two features: `persistence` (default off at MSRV-minimum build) and `statistical-tests` (test-only, gates `tests/statistical/*`).

| Crate | Version pin | Use |
|---|---|---|
| `rust_decimal` | `^1.33` | all money/price/quantity math (`Order.price`, `VOrder.filled_quantity`, `BasicPosition` arithmetic). Replaces Python `Decimal`. |
| `rust_decimal_macros` | `^1.33` | `dec!()` literals in tests + fixtures. |
| `chrono` | `^0.4` (`clock`, `serde`) | `DateTime<Utc>` + `NaiveTime` for `expires_at` / `exchange_timestamp`. |
| `chrono-tz` | `^0.9` | timezone-aware expiry logic; replaces `pendulum` tz. |
| `serde` | `^1` (`derive`) | on `Order`, `CompoundOrder`, response types, `VOrder`, `OrderFill`. |
| `serde_json` | `^1` | persistence payload + test golden values. |
| `thiserror` | `^1` | broker + order error enums. |
| `uuid` | `^1` (`v4`) | `CompoundOrder::__init__` → `uuid.uuid4().hex` parity. |
| `rand` | `=0.8` | Ticker RNG + proptest. **Hard-pinned** so Ticker statistical test means/stds are reproducible across machines for a given seed; `rand 0.9` changed `SmallRng` internals. Maintenance cost: dependents that pull `rand 0.9` transitively must be held back. Scope is limited to one test module + one prod use (Ticker). |
| `rand_distr` | `=0.4` | `Normal::new(0.0, 1.0)`. Paired with `rand =0.8`. |
| `parking_lot` | `^0.12` | `Mutex<HashMap<…>>` guards in `VirtualBroker` multi-user state. |
| `rusqlite` | `^0.31` (`bundled`), **optional** | `PersistenceHandle` SQLite impl behind `persistence` feature. |
| `proptest` | `^1.4` (dev-dep) | `update_quantity` conservation, `Order` lifecycle invariants, `BasicPosition` arithmetic. |
| `pretty_assertions` | `^1.4` (dev-dep) | readable diff on struct mismatches in parity tests. |
| `libtest-mimic` | `^0.7` (dev-dep) | custom `tests/parity` + `tests/statistical` harness (`harness = false`); stable-Rust replacement for nightly libtest-json. |
| `toml` | `^0.8` (dev-dep) | parse `tests/parity/excused.toml`. |
| `serde` (dev-dep, `derive`) | already listed | deserialize excused rows. |

**Fallback for the `rand =0.8` pin** (§6 D10): if a transitive dep forces `rand 0.9+`, replace `SmallRng` + `rand_distr::Normal` in **both** the production Ticker path (`src/simulation/ticker.rs`) and the test module (`tests/statistical/test_ticker_ltp_statistical.rs`) with a vendored `ChaCha8Rng` + a hand-rolled Box–Muller `Normal(0,1)`. Identical RNG path in prod and in the statistical test keeps the reproducibility argument intact.

No tokio, no reqwest, no serde_yaml. `yaml` load/save in `base.Broker` is dropped for MVP (§2 non-goals).

## §8 Phase delivery (v8 — gates sum to 237)

Every phase = 1 commit + 1 codex audit ACK before next.

| Phase | Gate items | Clean-path weeks |
|---|---:|---:|
| R0 | (audit this plan) | 0.5 |
| R1 | 20 (17 utils + 3 BasicPosition) | 0.75 |
| R2 | 10 (Quote + OrderBook + OrderLock incl. parametrize 3) | 1 |
| R3 | 64 (Order / persistence / lifecycle; non-`test_compound_order_*`) | 3 |
| R4 | 10 (Broker trait + Paper) | 1 |
| R5 | 54 (simulation/models minus `test_ticker_ltp`) | 2 |
| R6 | 22 (VirtualBroker multi-user) | 1.25 |
| R7 | 10 (ReplicaBroker) | 1.25 |
| R8 | 40 (`test_compound_order_*` exactly) | 1.5 |
| R9 | 7 (OrderStrategy + persistence chain) | 1 |
| R10 | (parity sweep + stabilisation, all 237) | 3 |
| **Total items** | **237** | |
| **Clean-path total** | | **~16 weeks** |

Verification: `20 + 10 + 64 + 10 + 54 + 22 + 10 + 40 + 7 = 237` ✓  
Weeks: `0.5 + 0.75 + 1 + 3 + 1 + 2 + 1.25 + 1.25 + 1.5 + 1 + 3 = 16.25` → "~16 weeks".

Expected with rework (~1.5 weeks/phase × 10 implementation phases): **~31 weeks**.

### Per-phase test scope (detail)

- **R1 (20)**: 17 portable `test_utils` items (34 total − 1 tick − 8 `stop_loss_step_decimal` − 8 `load_broker_`) + 3 `test_models` items, all `BasicPosition` (no upstream `QuantityMatch` test in `tests/test_models.py`).
- **R2 (10)**: `test_models` — 1 Quote + 3 OrderBook + 6 OrderLock (including 3-item `test_order_lock_can_methods` parametrize).
- **R3 (64)**: all collected pytest items from `test_order.py` whose function name does **not** start with `test_compound_order_`, minus `test_get_option[...]`. Exact list frozen at R3 gate by `pytest --collect-only -q tests/test_order.py | grep -v '^test_compound_order_'` and committed to `rust-tests/parity-item-manifest.txt`.
- **R4 (10)**: all of `test_base.py` minus 2 `cover_orders` tests.
- **R5 (54)**: all 55 `test_simulation_models.py` items minus `test_ticker_ltp`.
- **R6 (22)**: `test_simulation_virtual.py` `VirtualBroker` subset (multi-user inclusive).
- **R7 (10)**: `test_simulation_virtual.py` `ReplicaBroker` subset.
- **R8 (40)**: all collected pytest items from `test_order.py` whose function name **does** start with `test_compound_order_`. Upstream has 41 such `def`s, but `test_compound_order_update_orders` is defined twice; Python collects the later definition only, so the count is 40.
- **R9 (7)**: all `test_order_strategy.py`.
- **R10**: full parity sweep via `scripts/parity_gate.sh`; ≥ 230 of 237 pass, excused failures only from `tests/parity/excused.toml`. Plus `cargo test -p omsrs --test statistical --features statistical-tests` exits 0 for non-parity statistical module.

## §9 Acceptance (binary, v8)

1. `cargo build -p omsrs` zero warnings.
2. `cargo build -p omsrs --no-default-features` zero warnings.
3. `scripts/parity_gate.sh` exits 0. Internally it just `exec`s `cargo test -p omsrs --test parity --release --all-features` with no custom flags; the parity binary (custom `libtest-mimic` harness, stable Rust, `harness = false`) loads `tests/parity/excused.toml`, validates it against `rust-tests/parity-item-manifest.txt` (dedup + unknown-id + R0-empty + absent-vs-malformed checks per §4.1.2), runs all 237 trials, and enforces `failing ⊆ excused ∧ passing ≥ 230 ∧ |excused| ≤ 7`. The parity binary's exit code is the gate. No custom argv flags, no nightly toolchain.
4. `cargo test -p omsrs --test statistical --release --features statistical-tests` exits 0 (`test_ticker_ltp_statistical` + any promoted probabilistic-parity sibling). This target is declared `[[test]] name = "statistical" harness = false` in `Cargo.toml` and uses the same `libtest-mimic` wrapper pattern as parity, so `scripts/` can wrap it symmetrically if needed.
5. `cargo test -p omsrs --no-default-features`: non-persistence parity tests pass (persistence tests skipped via `#[cfg(feature = "persistence")]`). Same gate rule as §9.3.
6. `cargo clippy -p omsrs --all-features -- -D warnings` clean.
7. `Broker` trait object-safe.
8. `Paper` passes all 10 portable `test_base` items.
9. `VirtualBroker` passes all 22 (multi-user + clock-driven).
10. `ReplicaBroker::run_fill` state-transition determinism: given fixed `OrderFill` config + `VOrder` set + `MockClock` time, `run_fill()` produces byte-equal output across 3 consecutive calls and across Linux/macOS CI runners.
11. `CompoundOrder` passes 40; `OrderStrategy` passes 7.

## §10 Risks

R.1–R.7 as v5.  
R.8 Ticker RNG — handled via D10 / §14(A).  
R.9 `VirtualBroker` multi-user + Clock — R6 at 1.25 weeks.  
R.10 Cross-machine determinism — parity tests use `MockClock`; wall-clock paths with tolerance; `rust_decimal` arithmetic is byte-deterministic (no float). CI matrix spec at R10 gate.  
R.11 R5 optimistic — 54 items + Ticker design + Clock threading in 2 weeks. Re-evaluate at R5 commit; if slipping, extend to 2.5 w and re-audit.  
R.12 `rand = "=0.8"` hard-pin — any future dep that pulls `rand 0.9` transitively will break resolution. Scoped to Ticker RNG only; fallback (see §7): vendor local `ChaCha8Rng` + hand-rolled Box–Muller `Normal(0,1)` in **both** prod Ticker and `tests/statistical/test_ticker_ltp_statistical.rs` so the statistical-test reproducibility argument survives (documented at R5 gate).

R.13 R8 schedule — 40 `test_compound_order_*` items in 1.5 weeks (~27/week) is tighter than R3's ~21/week despite `CompoundOrder` being the most stateful MVP symbol (nested `Order`s, persistence fan-out, positions/MTM aggregation, clock cascade on add). Re-evaluate at R6 commit; if R8 looks tight, extend to 2w and re-audit. Schedule-only risk, not an ACK blocker.

## §11 Explicit non-goals (unchanged)

## §12 Rust idioms leveraged (orientation map — not new scope)

Non-normative: this section summarises which Rust strengths each decision relies on, so reviewers can see the port isn't a mechanical transliteration. All entries are already encoded in §5–§10; nothing new is introduced here.

| Idiom | Where used | Python analogue |
|---|---|---|
| `struct` + `#[derive(Serialize, Deserialize)]` | `Order`, `CompoundOrder`, `VOrder`, `OrderResponse`, `OrderFill`, `Instrument`, all `models.py` MVP types | `pydantic.BaseModel` |
| Rich `enum` + exhaustive `match` | `OrderStatus` (6 variants), `OrderType` (3), `Side`, `Mode`, all response discriminants | Python `str` enums, runtime dispatch |
| `thiserror` error enum at API boundary | `OmsError` + per-module sub-errors; returned from every `Broker` method | Python exception hierarchy |
| `trait` + `dyn Trait` (object-safe) | `Broker` (§9.7), `Clock`, `PersistenceHandle` | Python duck typing |
| `Arc<dyn Clock + Send + Sync>` | `Order.clock`, `CompoundOrder.clock`, `OrderStrategy.clock`, `VirtualBroker.clock` | implicit `pendulum.now()` global |
| `parking_lot::Mutex<HashMap<…>>` | `VirtualBroker` multi-user `users: Mutex<HashMap<String, VUser>>`; `positions`, `orders` similarly | Python `dict` + no explicit lock (GIL implied) |
| `rust_decimal::Decimal` (no float) | every price/qty/PnL field; byte-deterministic arithmetic (R.10) | Python `Decimal` |
| `chrono::DateTime<Utc>` + `chrono_tz::Tz` | `Order.expires_at`, `exchange_timestamp`, expiry math | `pendulum` |
| `uuid::Uuid::new_v4()` then `.simple().to_string()` | `CompoundOrder::new` `id` field | `uuid.uuid4().hex` |
| `#[cfg(feature = "persistence")]` on one impl block (not every call site) | `SqlitePersistenceHandle: PersistenceHandle` lives behind the feature; trait + `Option<Box<dyn PersistenceHandle>>` field are unconditional — no `cfg` spaghetti in `Order::update/execute/modify` | Python duck-type optional `self.connection` |
| `libtest-mimic` (`harness = false`) | `tests/parity` + `tests/statistical` — custom exit codes, excused-list validation, manifest cross-check, all on stable Rust | pytest custom plugin |
| `proptest` invariants | `update_quantity` conservation, `Order` lifecycle reachability, `BasicPosition` arithmetic | hypothesis |
| `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "…"))` | embed parity manifest at compile time | file open at runtime |

**Explicitly NOT used** (scope guardrails; adding any of these changes MVP scope):

- ~~`tokio` / any async runtime~~ — **superseded by R11 (2026-04-21) + R12 (2026-04-22).** Original v11 scope was fully synchronous. R11 added the additive `AsyncBroker` trait + `async_trait` procedural macro (zero-runtime) + `AsyncPaper` reference impl. R12 completed async coverage with `AsyncVirtualBroker` / `AsyncReplicaBroker` / `Order::execute_async` + siblings / `AsyncCompoundOrder` / `AsyncOrderStrategy`. `tokio` remains a **dev-only** dependency (used by test harnesses + one deadlock regression test); production surface carries `async_trait` only. All R11/R12 additions are non-breaking — the 237-item sync parity gate still passes unchanged. Full rationale: `docs/R12-async-complete-plan.md` + `docs/audit-R12.{1,2,3a,3b}-codex-result.md`.
- `sqlx` / async persistence — same reason; `rusqlite` sync-only. (R12 note: `Order::execute_async` / `modify_async` call `save_to_db()` synchronously when the `persistence` feature is on — documented caveat; spawn_blocking at caller boundary is the migration path. Full async persistence is R13 scope.)
- `dashmap` — `parking_lot::Mutex<HashMap<…>>` is sufficient for `VirtualBroker`'s contention profile (test-harness load, not production traffic). Can revisit post-MVP if a real downstream hits hot-path contention.
- HTTP / WebSocket client crates — broker adapters are out of MVP.
- `serde_yaml` — `Broker.yaml_load`/`yaml_save` dropped for MVP.

## §13 Next step

Codex audit this v11. Expected focus:
- Does §4.1.2's new present-but-empty clause fully resolve the v10 P1 (TOML schema `#[serde(default)]`, R0 committed-empty file deserializes cleanly)?
- Does §4.1.5's smoke matrix cover every exit code including 6 (malformed / wrong-shape / missing required fields / present-empty success)?
- Are the v10-accepted items all still intact (argv contract, grep hygiene, Ticker derivation, manifest include_str, statistical harness)?
- Any new drift from scope, phase math, or Rust-idiom orientation in §12?

**No Rust code until ACK.**
