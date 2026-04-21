R7 is ACKed. R8 may start: the R7 parity surface lands the requested 10
`tests/simulation/test_virtual.py` ReplicaBroker items, preserves the only
excused failure as the R3.a `test_order_timezone` row, and passes the requested
Cargo, clippy, upstream-collection, release-gate, and `run_fill` determinism
checks.

## Findings

No blocking findings.

Non-blocking notes:

- `ReplicaBroker::order_modify` documents upstream's "any VOrder field"
  behavior, but currently implements only `price`, `trigger_price`, `quantity`,
  and `order_type` (`src/replica_broker.rs:132`). Upstream uses `hasattr` /
  `setattr` for every matching order attribute
  (`/home/ubuntu/refs/omspy/omspy/simulation/virtual.py:793`). This covers all
  R7 trials and is not an R8 blocker, but it is a narrower public surface than
  upstream if later callers modify fields such as `side`, `symbol`,
  `average_price`, or status quantities through ReplicaBroker.
- `order_place` inserts non-default users into `ReplicaBroker.users`
  (`src/replica_broker.rs:105`). Upstream only seeds `users` with `"default"`
  and stores per-user orders in `_user_orders`
  (`/home/ubuntu/refs/omspy/omspy/simulation/virtual.py:772`). The R7 tests
  inspect `user_orders`, not `users`, so this is harmless for the gate, but it
  is a small observable divergence.
- `run_fill` is more defensive than upstream if a fill references a missing
  instrument: Rust skips it, while upstream indexes `self.instruments[symbol]`
  first (`/home/ubuntu/refs/omspy/omspy/simulation/virtual.py:821`) and would
  raise before reaching its false-price warning. No collected trial exercises
  that edge.

## Scope Check

- `src/replica_broker.rs` exposes `ReplicaBroker`, `ReplicaFill`, and
  `OrderHandle = Arc<Mutex<VOrder>>`.
- The canonical `orders` map, `pending`, `completed`, `fills`, and
  `user_orders` all hold shared `OrderHandle` values. The Rust parity trial
  explicitly checks the upstream object-identity chain with `Arc::ptr_eq`
  (`tests/parity/test_replica_broker.rs:129`).
- `tests/parity/test_replica_broker.rs` defines the 10 R7 trials and
  `tests/parity/main.rs` registers the same 10 trial names.
- `rust-tests/parity-item-manifest.txt` has 190 active rows and appends exactly
  the R7 names:
  `test_replica_broker_defaults`, `test_replica_broker_update`,
  `test_replica_broker_order_place`,
  `test_replica_broker_order_place_multiple_users`,
  `test_replica_order_fill`, `test_replica_broker_order_modify`,
  `test_replica_broker_order_modify_market`,
  `test_replica_broker_order_cancel`,
  `test_replica_broker_order_cancel_multiple_times`, and
  `test_replica_broker_no_symbol`.
- Upstream collection from `/home/ubuntu/refs/omspy` reports 79 total
  `test_virtual.py` nodeids, including the same 10 ReplicaBroker / replica
  order-fill nodeids.
- `tests/parity/excused.toml` still has exactly one `[[excused]]` row:
  `test_order_timezone`.
- No `#[ignore]` markers were found under `src`, `tests`, `Cargo.toml`,
  `rust-tests`, `scripts`, or `docs`.

## Design Review

1. `OrderHandle = Arc<Mutex<VOrder>>` is the right Rust model for R7. It
   preserves upstream's same-object semantics across the five collections that
   matter for `test_replica_broker_order_place`.
2. The hardcoded AAPL/XOM/DOW instrument prices are acceptable. The collected
   assertions depend on the resulting fill prices, not on Python RNG
   byte-compatibility.
3. The dual-form `order_type` parser is correct for the upstream fixture shape:
   numeric enum literals and string names both resolve.
4. `run_fill` mirrors the important upstream transition: done orders are kept in
   `orders`, appended to `completed`, and removed from `fills`.
5. The no-symbol rejection path preserves the upstream error text and produces
   `Status::Rejected` through the existing status-message prefix logic.
6. `run_fill` is deterministic under fixed instrument prices and a frozen
   unfilled `fills` list. A temporary integration probe compared byte strings
   after three consecutive `run_fill` calls and passed.

## Verification

- `cargo test --test parity --all-features` exited 0 via the parity gate:
  manifest 190, passed 189, failed 1, failing id `test_order_timezone`, gate
  `Pass`.
- `cargo test --no-default-features` exited 0. With persistence trials
  feature-gated out, the effective parity manifest is 181: passed 180, failed
  1, failing id `test_order_timezone`, gate `Pass`. Parity-runner smoke tests
  passed 13/13.
- `cargo clippy --all-features --all-targets -- -D warnings` passed.
- `cargo clippy --no-default-features --all-targets -- -D warnings` passed.
- `scripts/parity_gate.sh` exited 0 in release mode with the same 190 / 189 / 1
  gate shape.
- `python3 -m pytest --collect-only -q tests/simulation/test_virtual.py` from
  `/home/ubuntu/refs/omspy` collected the 10 requested R7 nodeids.
- Active manifest count was verified with comment/blank-line stripping:
  `awk 'NF && $1 !~ /^#/ {n++} END {print n}' rust-tests/parity-item-manifest.txt`
  returned 190.
- The focused R9.10 sandbox probe
  `cargo test --test replica_idempotency_probe --all-features -- --nocapture`
  passed 1/1, then the temporary probe file was removed.
