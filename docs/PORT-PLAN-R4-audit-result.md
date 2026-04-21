R4 is ACKed. R5 may start: the R4 parity surface lands the requested 10 `tests/test_base.py` items, keeps the only excused failure as the R3.a `test_order_timezone` row, and passes the requested Cargo, clippy, and release gate checks.

## Findings

No blocking findings.

Non-blocking notes:

- `test_dummy_broker_values` is weaker than upstream for direct `order_modify` / `order_cancel`: the Rust trial calls both but only asserts the recorded `order_place` kwargs (`tests/parity/test_base.rs:147`). The implementation does record modify/cancel calls (`tests/parity/test_base.rs:110`, `tests/parity/test_base.rs:114`), and `cancel_all_orders` separately exercises `order_cancel`, so I am not blocking R4 on the trait return-shape difference. If this area is touched again, add a `modify_calls()` accessor and assert both direct calls.
- A few Python truthiness edges are not exactly mirrored: `close_all_positions(keys_to_copy=...)` copies any non-null JSON value, while upstream copies only truthy `position.get(key)` values (`src/broker.rs:100`); `cancel_all_orders(keys_to_copy=...)` omits absent keys, while upstream includes `None` for requested missing keys (`src/broker.rs:151`). The current R4 fixtures only exercise truthy copied keys, so this is not a gate blocker, but it is worth tightening if future tests cover falsy kwargs.

## Scope Check

- `rust-tests/parity-item-manifest.txt` is at R4 cumulative 104 and appends exactly the 10 R4 `test_base.py` items, excluding `test_cover_orders` and `test_cover_orders_multiple` (`rust-tests/parity-item-manifest.txt:111`).
- `tests/parity/test_base.rs` defines `DummyBroker`, loads `orders.json`, `positions.json`, and `trades.json`, pre-applies the zerodha-style renames, and mutates orders to `pending` before trials run (`tests/parity/test_base.rs:43`).
- The copied fixture files are byte-identical to upstream:
  - `tests/data/kiteconnect/orders.json`
  - `tests/data/kiteconnect/positions.json`
  - `tests/data/kiteconnect/trades.json`
- `src/broker.rs` exposes the R4 default methods and free `rename()` helper (`src/broker.rs:68`, `src/broker.rs:129`, `src/broker.rs:171`, `src/broker.rs:191`).
- `src/brokers.rs` adds the `Paper` broker with `with_orders`, `with_trades`, and `with_positions` builders plus the upstream `[{}]` fallback for unset snapshots (`src/brokers.rs:30`, `src/brokers.rs:85`).

## Design Review

1. Deferred override YAML loading is acceptable for R4. Pre-applying the rename tables in `DummyBroker::new()` preserves the fixture-facing behavior without introducing a half-built live override layer.
2. The return-value-to-recorded-call adaptation is acceptable given the Rust trait shape. I would only strengthen the direct modify/cancel assertions as noted above.
3. `symbol_transformer: Option<&dyn Fn(&str) -> String>` is the right Rust shape; using `None` to cover upstream's non-callable fallback is acceptable.
4. Quantity coercion accepts numeric values and numeric strings, and skips non-coercible values. That matches the upstream try/except skip behavior for the R4 cases.
5. `Paper` storing override snapshots behind `Mutex<Option<_>>` is fine. The builders use `get_mut()` on the single-owner construction path, and runtime accessors lock only when needed.
6. The American-only `CANCELED` terminal spelling is correctly preserved for upstream parity. Keeping `CANCELLED` non-terminal is intentional.

## Verification

- `cargo test --test parity --all-features` exited 0 via the parity gate: manifest 104, passed 103, failed 1, failing id `test_order_timezone`, gate `Pass`.
- `cargo test --no-default-features` exited 0. The effective no-default parity manifest is 95, not 85: passed 94, failed 1, failing id `test_order_timezone`, gate `Pass`. The R4 10 trials run under no-default; only the 9 R3.b persistence trials drop out.
- `cargo clippy --all-targets --all-features` passed.
- `cargo clippy --all-targets --no-default-features` passed.
- `scripts/parity_gate.sh` exited 0 in release mode with the same 104 / 103 / 1 gate shape.
- `rg -n "#\\[ignore\\]" src tests Cargo.toml rust-tests scripts` returned no matches.
- `tests/parity/excused.toml` has exactly one `[[excused]]` row: `test_order_timezone`.
