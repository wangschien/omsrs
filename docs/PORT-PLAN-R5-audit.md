# R5 audit prompt

R4 ACKed. R5 landed locally. Full gate shape under `--all-features`:
158 manifest / 157 pass / 1 excused (the R3.a `test_order_timezone` §14B
row), exit 0. Statistical target also green.

## Scope

R5 per PORT-PLAN §8: 54 items from `tests/simulation/test_models.py`,
all minus `test_ticker_ltp` (replaced by `test_ticker_ltp_statistical`
per §14A). 55 pytest-collected minus 1 = 54.

## Deliverables

- `src/simulation.rs` — enums (`Status`, `ResponseStatus`, `Side`,
  `TickerMode`, `OrderType`); `OHLC` / `OHLCV` / `OHLCVI`; `Ticker`
  (`SmallRng + Normal(0,1)`, seeded); `VQuote`; `VTrade`;
  `VOrder` + `VOrderInit` builder with side-string parsing,
  delay-injection, and rng-driven `PARTIAL_FILL` / `PENDING` branches in
  `modify_order_by_status`; `VPosition`; `VUser`; `Response` +
  `OrderResponse` + `GenericResponse{Data::{VOrder|OHLC|Other}}` +
  `AuthResponse` / `LTPResponse` / `OHLCVResponse` / `QuoteResponse` /
  `OrderBookResponse` / `PositionResponse`; `Instrument`; `OrderFill`
  (constructor runs `as_market`; `update()` / `update_with_price()`
  walks MARKET / LIMIT / STOP); `generate_orderbook` helper
  (`virtual.py` port, seeded).
- `tests/parity/test_simulation_models.rs` — 54 trials.
- `tests/statistical/main.rs` — libtest-mimic target with
  `test_ticker_ltp_statistical` (1000-sample Z-mean ∈ (-0.2, 0.2),
  Z-std ∈ (0.7, 1.3)).
- `Cargo.toml` — `rand = { version = "=0.8", features = ["small_rng"] }`
  + `[[test]] name = "statistical" required-features = ["statistical-tests"]`.

## Design choices worth scrutinising

1. **`f64` for simulation values** (Ticker perturbation, VOrder
   quantity, OrderFill arithmetic). PORT-PLAN §7 says Decimal for
   every price/quantity field. The simulation module is Normal-driven
   (tick-rounded) and upstream uses `float` throughout its arithmetic
   (`order.value == 6000.0`, `fill.order.average_price == 128.0`).
   Using `f64` keeps arithmetic byte-equal to upstream assertions;
   Decimal would force all float literals into `dec!(…)` and introduce
   a float↔Decimal boundary inside `random.gauss` math. Real-money
   Order lifecycle (R1–R4) keeps Decimal.

2. **`test_ticker_ticker_mode` is NOT §14(B)-excused.** With
   `SmallRng::seed_from_u64(0)` the first Random-mode draw is
   deterministically non-zero, so `assert_ne!(t.ltp(), 125.0)` is
   stable. Plan §6 D10's `≥ 95/100 seed` fallback stays in reserve.

3. **`test_vorder_modify_by_status_partial_fill` — which definition.**
   Upstream has two functions with this name; pytest collects only
   the second (full-flow with `modify_by_status` + `travel_to`). Rust
   registers one trial matching the second definition. The "unit"
   behaviour of the first definition is still exercised inside
   `test_vorder_modify_by_status_{complete,canceled,open,pending}`
   which call `modify_order_by_status` directly.

4. **`VOrderInit` builder with `side` / `side_str` + `order_type` /
   `order_type_str` dual paths.** Upstream accepts both enum and
   string forms via pydantic validators. Rust can't have one field
   typed ambiguously — the init struct exposes both and
   `VOrder::from_init` resolves precedence (enum > string). Tests
   matching upstream `side="buy"` use `side_str`; tests passing
   `Side.BUY` use `side`.

5. **`ResponseStatus::parse`** renamed from `from_str` to avoid the
   `FromStr` trait-method clash (clippy::should_implement_trait). If
   you'd rather have `impl FromStr`, flag and I'll add.

6. **`GenericResponseData::VOrder(Box<VOrder>)`.** `VOrder` is ~264
   bytes; clippy::large_enum_variant forced the box indirection.
   Semantically equivalent — the tests match on the variant, not its
   size.

7. **Statistical tolerance.** Upstream's `random.seed(1000)` yields
   `_ltp == 120.5`, `_high == 125.3`, `_low == 120.5` after 15 draws.
   Rust's SmallRng byte-semantics differ, so we assert a 1000-sample
   distribution-shape bound instead: `|Z-mean| < 0.2`, `Z-std ∈
   (0.7, 1.3)`. Those bounds are wider than a strict Normal(0,1)
   would need because the 0.05-tick rounding introduces per-step
   bias; they pass cleanly under seed=1000.

## What to verify

- `cargo test --test parity --all-features` → 158 / 157 / 1 excused,
  exit 0.
- `cargo test --test statistical --features statistical-tests
  --release` → 1/1.
- `cargo test --no-default-features` → all targets green.
- `cargo clippy` clean in both feature configs.
- `scripts/parity_gate.sh` → 158 / 157 / 1 shape.
- Manifest size 158, no `#[ignore]`, excused.toml still has exactly
  the R3.a row.
- The 7 design choices — any red flags.
- R5.a/b split wasn't needed — R5 landed in a single commit. Please
  confirm the 54-item coverage against upstream pytest collection
  (you can re-run `pytest --collect-only -q tests/simulation/test_models.py | wc -l`
  — should be 55, and our manifest = 55 − 1 = 54).

## Output format

Same as prior audits. Write to `docs/PORT-PLAN-R5-audit-result.md`.
If ACK, R6 may start (22 `VirtualBroker` tests).
