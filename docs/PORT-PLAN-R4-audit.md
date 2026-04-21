# R4 audit prompt

R3 ACKed. R4 landed locally; gate 104 / 103 pass / 1 excused (the R3.a
`test_order_timezone` §14B row), exit 0.

## Scope

R4 per PORT-PLAN §8: 10 items from `tests/test_base.py`, all minus the 2
`test_cover_orders*` (deferred per source-notes §12 since `tick()` /
`cover_orders` aren't in MVP).

## Deliverables

- `src/broker.rs` — extends `Broker` with default methods:
  - `close_all_positions(positions, keys_to_copy, keys_to_add, symbol_transformer)`
  - `cancel_all_orders(keys_to_copy, keys_to_add)`
  - `get_positions_from_orders(filters)`
  - `orders()` / `positions()` / `trades()` read-accessors (default empty)
  - `rename(dct, keys)` free function (upstream `Broker.rename` staticmethod)
- `src/brokers.rs` — `Paper` broker (echo-kwargs in-memory stand-in).
  `with_orders` / `with_trades` / `with_positions` builders.
- `tests/parity/test_base.rs` — `DummyBroker` (kiteconnect JSON fixture-
  backed, applies zerodha-yaml rename at load time) + 10 new trials.
- `tests/data/kiteconnect/{orders,positions,trades}.json` — copied from
  upstream.

## Design choices worth scrutinising

1. **Override YAML loading deferred.** Per source-notes §12. Rather
   than threading a live `pre/post` override system through the
   trait, `DummyBroker::new()` pre-applies the zerodha-yaml renames
   (`tradingsymbol → symbol`, `transaction_type → side`) to fixture
   rows at load time. `rename()` is exposed as a free function for
   when a real override layer lands.

2. **`test_dummy_broker_values` return-vs-recorded-call.** Upstream
   asserts `broker.order_place(symbol="aapl") == {"symbol": "aapl"}`
   (Dummy echoes kwargs). Rust trait returns `Option<String>` /
   `()`; the Rust trial asserts on the recorded call via
   `broker.place_calls()[0] == {"symbol": "aapl"}`. Observable-
   equivalent; call me out if you'd rather have the return-value
   parity.

3. **`symbol_transformer` is `Option<&dyn Fn(&str) -> String>`.**
   Upstream `test_close_all_positions_quantity_as_error` passes a
   non-callable string `"string"`; upstream's `callable(f)` check
   falls back to identity. Rust's signature won't compile a non-
   callable, so the Rust trial passes `None` and relies on the same
   identity fallback.

4. **`close_all_positions` quantity coercion.** Upstream does
   `int(position.get("quantity"))` — works for `"10"`, `10`, `10.0`,
   raises ValueError on `"O"`. Rust's `coerce_quantity` accepts
   numeric + numeric-string, returns `None` on non-coercible → skip
   (matches upstream's `try/except: log+skip`).

5. **`Paper` stores the `positions/orders/trades` override behind a
   `Mutex<Option<_>>`.** Matches upstream's "None → return [{}]"
   fallback. Builders mutate through `get_mut` (single-owner path)
   rather than `.lock()` to avoid double-locking during construction.

6. **`cancel_all_orders` terminal-status check preserves upstream's
   American-only spelling.** `("COMPLETE", "CANCELED", "REJECTED")`;
   "CANCELLED" (British) — which orders.json uses — is **not**
   terminal per upstream, but the test pre-mutates every order's
   status to "pending" so the difference is invisible. Keeping parity
   intact here instead of silently hardening.

## What to verify

- `cargo test --test parity --all-features` → 104 / 103 pass / 1
  excused, exit 0.
- `cargo test --no-default-features` green (R3.b + R4 skip the 9 R3.b
  trials but all 95 non-persistence trials pass including R4).

  Wait — R4 doesn't require persistence. R4's 10 trials should run
  under both feature configurations. Confirm the effective manifest
  under `--no-default-features` is 95 (R1 20 + R2 10 + R3.a 55 + R4
  10), not 85.

- `cargo clippy` in both feature configs clean.
- `scripts/parity_gate.sh` shows the 104 / 103 / 1 shape.
- No `#[ignore]` anywhere.
- `excused.toml` still exactly 1 row (the R3.a-approved
  `test_order_timezone`).
- The 6 design choices above — any red flags.

## Output format

Same as prior audits. Write the result to
`docs/PORT-PLAN-R4-audit-result.md`. If ACK, R5 may start (54 items
from `tests/simulation/test_models.py` minus `test_ticker_ltp`).
