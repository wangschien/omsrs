R6 is ACKed. R7 may start: the R6 parity surface lands the requested 22
`tests/simulation/test_virtual.py` VirtualBroker items, keeps the only excused
failure as the R3.a `test_order_timezone` row, and passes the requested Cargo,
clippy, upstream-collection, and release gate checks.

## Findings

No blocking findings.

Non-blocking notes:

- `order_modify` and `order_cancel` do not currently route through
  `VirtualBroker::get` before mutating the stored order
  (`src/virtual_broker.rs:276`, `src/virtual_broker.rs:320`). Upstream calls
  `self.get(order_id)` in both methods, so a delayed order can be advanced by
  `modify_by_status(Status.COMPLETE)` before modification/cancellation. The R6
  upstream trials only exercise immediate modify/cancel calls, so this is not a
  gate blocker, but it is a real semantic edge if delayed broker orders are used
  outside the collected tests.
- `test_virtual_broker_order_place_same_memory` is intentionally weaker than
  upstream object identity. Rust stores a cloned `VOrder` shell in `VUser.orders`
  and asserts matching `order_id` (`tests/parity/test_virtual_broker.rs:380`),
  while upstream asserts the user order and `_orders[order_id]` are the exact
  same Python object. This is acceptable for the R6 gate as a documented Rust
  ownership tradeoff, but it means user-attached order shells can go stale after
  the broker's canonical order is mutated.
- `VOrder::cloned_clone_weak` is implemented as an inherent impl in
  `src/virtual_broker.rs`, not physically in `src/simulation.rs`. That is not a
  behavior problem, but it differs from the deliverable wording.

## Scope Check

- `rust-tests/parity-item-manifest.txt` has 180 active rows and appends exactly
  the 22 R6 `test_virtual_broker_*` ids.
- `tests/parity/main.rs` registers 22 R6 trials, and
  `tests/parity/test_virtual_broker.rs` defines 22 matching trial functions.
- Upstream source has 23 `def test_virtual_broker_*` lines in
  `/home/ubuntu/refs/omspy/tests/simulation/test_virtual.py`, with
  `test_virtual_broker_ltp` defined twice at lines 778 and 784. Pytest collects
  22 VirtualBroker nodeids, matching the Rust R6 surface.
- `src/virtual_broker.rs` exposes the requested `VirtualBroker` structure,
  seeded `SmallRng` failure path, `BrokerReply` return enum, clock-injected
  response timestamps, `get(order_id, status)`, multi-user client tracking, and
  `update_tickers` / `ltp` / `ltp_many` / `ohlc` helpers.
- `tests/parity/excused.toml` still has exactly one `[[excused]]` row:
  `test_order_timezone`.

## Design Review

1. Seeded `SmallRng` for `is_failure` is acceptable. It is a deterministic
   substitute for upstream's global `random.random()` and keeps the default
   `failure_rate = 0.001` checks stable.
2. `BrokerReply::{Order, Passthrough}` is the right Rust representation of
   upstream's `Union[OrderResponse, dict]`. Boxing the `OrderResponse` avoids
   the large enum variant warning without changing the tested API shape.
3. The validation error strings are sufficient for the collected upstream
   assertions. Full pydantic error fidelity is not required by R6.
4. The same-memory port is the main semantic weakening. I am not blocking R6 on
   it because the divergence is explicit and contained, but an `Arc<Mutex<VOrder>>`
   design would be the closer model if shared user-order state matters later.
5. Splitting `ltp` and `ltp_many` is acceptable in Rust. The collected upstream
   test only needs the single-symbol missing case plus iterable filtering.
6. Covering only the second `test_virtual_broker_ltp` definition is correct:
   pytest shadows the first definition and collects the second one only.
7. `cloned_clone_weak` is a pragmatic response-data bridge for the non-`Clone`
   `VOrder`. The fresh seed is harmless for R6 response inspection, but this
   helper should not be mistaken for shared order identity.

## Verification

- `cargo test --test parity --all-features` exited 0 via the parity gate:
  manifest 180, passed 179, failed 1, failing id `test_order_timezone`, gate
  `Pass`.
- `cargo test --no-default-features` exited 0. The effective no-default parity
  manifest is 171 because the 9 persistence trials are feature-gated out:
  passed 170, failed 1, failing id `test_order_timezone`, gate `Pass`. Smoke
  tests passed 13/13.
- `cargo clippy --all-features --all-targets -- -D warnings` passed.
- `cargo clippy --no-default-features --all-targets -- -D warnings` passed.
- `scripts/parity_gate.sh` exited 0 in release mode with the same 180 / 179 / 1
  gate shape.
- `rg -n "^\\s*#\\[ignore\\]" src tests Cargo.toml rust-tests scripts docs`
  returned no matches.
- `rg -n "^\\s*\\[\\[excused\\]\\]" tests/parity/excused.toml` returned exactly
  one row.
- Upstream collection was verified from `/home/ubuntu/refs/omspy` with
  `python3 -m pytest --collect-only -q tests/simulation/test_virtual.py`: 79
  total tests collected, with 22 `test_virtual_broker_*` nodeids.
