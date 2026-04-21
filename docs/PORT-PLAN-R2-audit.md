# R2 audit prompt

R1 was ACKed (see `docs/PORT-PLAN-R1-audit-result.md`). R2 landed as
commit 98a433b and must pass your audit before R3.

## Scope

R2 per PORT-PLAN §8 (cumulative 30 = R1 20 + R2 10):
- 1 Quote-ish item (`test_order_book`)
- 3 OrderBook items (`test_orderbook_{is_bid_ask,spread,total_bid_ask_quantity}`)
- 6 OrderLock items:
  - `test_order_lock_defaults`
  - `test_order_lock_methods`
  - `test_order_lock_methods_max_duration`
  - `test_order_lock_can_methods_can_{create,modify,cancel}` (upstream 3-row parametrize)

## Deliverables landed in 98a433b

- `src/clock.rs` — `Clock` trait + `SystemClock` + `MockClock` + `clock_system_default()`.
- `src/models.rs` — `Quote`, `OrderBook`, `OrderLock`. OrderLock holds `Arc<dyn Clock + Send + Sync>` via `#[serde(skip, default = "clock_system_default")]`.
- `tests/parity/test_models.rs` — 10 new trials; reused the same `register_parity_tests!` macro path.
- `rust-tests/parity-item-manifest.txt` — 10 new ids appended; cumulative 30.

## Evidence captured in the commit message

- `cargo test --test parity` → 30 passed, gate exit 0
- `cargo test --test parity_runner_smoke` → 13 passed
- `cargo test` (everything) → green
- `scripts/parity_gate.sh` → exit 0
- `cargo clippy --all-features --all-targets -- -D warnings` clean
- `cargo build` + `cargo build --no-default-features` warning-free

## Design choices worth scrutinising

1. **`Quote.quantity` stays `i64`, `Quote.price` is `Decimal`.** PORT-PLAN
   §7 says Decimal for every price field. Upstream uses `int` for quantity
   and `float` for price. Keeping quantity as `i64` lets
   `total_bid_quantity` / `total_ask_quantity` sum natively without
   Decimal conversion; upstream never fractions a Quote quantity.

2. **`OrderLock` instants are `DateTime<Utc>`.** The `timezone` field is
   carried for forward compatibility but doesn't affect arithmetic.
   Pendulum's equality is instant-based (UTC-normalised), which is what
   `chrono::DateTime<Utc>` is too — so `Utc.with_ymd_and_hms(...)` and
   `pendulum.datetime(..., tz="Asia/Kolkata")` compare identically at the
   same instant.

3. **Seconds truncation.** Upstream wraps `seconds` via `int(seconds)`
   before `pendulum.now().add(seconds=...)`. We mirror that with
   `capped.trunc() as i64` inside `OrderLock::secs_delta`. Tests use
   whole-second values so truncation is a no-op today, but the coercion
   matters for future call sites that pass fractional seconds.

4. **Parametrized `test_order_lock_can_methods` → 3 separate Rust
   trials.** Upstream uses `getattr(lock, method)` + `getattr(lock,
   method[4:])` to drive the same assertions against each of the three
   lock methods. Rust can't do runtime attribute indirection, so we
   factor the flow into `run_can_methods(LockKind)` and register 3
   trials. Each maps 1:1 to an upstream parametrize case via manifest id.

5. **No `#[ignore]`; nothing added to `excused.toml`.** Candidate §14(B)
   entries from omspy-source-notes (`test_order_lock_can_methods[can_*]`
   — "Clock tick granularity") didn't need promotion here because
   `MockClock` makes time deterministic. Flag if you disagree.

## What to verify

- `cargo test` is green in your sandbox.
- Gate exits 0 on R2's 30-item manifest and trial set.
- `excused.toml` is still present-empty.
- The 13-row smoke matrix is still green after the manifest grew.
- No `#[ignore]` anywhere.
- Clippy + both feature configurations compile warning-free.
- `OrderLock`'s Arc<dyn Clock + Send + Sync> field serde-skips cleanly
  (the type still compiles without the clock in scope) and that the
  default-fn is wired in correctly.
- Any place where `timezone` being a no-op for arithmetic would surprise
  a reader of upstream-ported code later (e.g. R3 `Order.expires_at`
  math — the plan flags that DST-sensitive tests may become §14(B)
  entries at R3).

## Output format

Same as R1. Write the result to `docs/PORT-PLAN-R2-audit-result.md`.
