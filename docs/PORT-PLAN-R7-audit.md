# R7 audit prompt

R6 ACKed. R7 landed locally. Gate under `--all-features`: 190 manifest /
189 pass / 1 excused (R3.a `test_order_timezone`), exit 0.

## Scope

R7 per PORT-PLAN §8: 10 ReplicaBroker items from
`tests/simulation/test_virtual.py`.

## Deliverables

- `src/replica_broker.rs` — `ReplicaBroker` + `ReplicaFill` + `OrderHandle
  = Arc<Mutex<VOrder>>`. All collections (`orders`, `pending`,
  `completed`, `fills`, `user_orders`) hold shared handles so upstream's
  `id(order) == id(broker.orders[...])` chain translates to
  `Arc::ptr_eq`.
- `tests/parity/test_replica_broker.rs` — 10 trials.

## Design choices worth scrutinising

1. **`OrderHandle = Arc<Mutex<VOrder>>`.** Upstream `test_replica_broker_order_place`
   explicitly asserts Python object identity across 5 collections:
   ```python
   assert (id(order) == id(broker.orders[...]) == id(broker._user_orders[...][0])
           == id(broker.pending[0]) == id(broker.fills[0].order))
   ```
   Rust's R5/R6 by-value VOrder can't express that. ReplicaBroker's
   collections use shared handles; callers do `handle.lock()` to
   read/mutate.
2. **Instrument fixture is hardcoded** (AAPL=125, XOM=153, DOW=136)
   rather than `random.seed(1000) + generate_instrument`. Upstream's
   Python `random` byte-semantics aren't reproducible from Rust; the
   R7 `test_replica_order_fill` trial asserts specific avg_prices
   (125, 136, 136, 153, 153) that are derived from those three
   last_prices, so hardcoding pins the invariant the test actually
   checks.
3. **`order_type` dual-form parser** accepts numeric `1 / 2 / 3` from
   upstream `Inputs(... order_type=2 ...)` AND string `"MARKET" /
   "LIMIT" / "STOP"`. Upstream pydantic's enum-by-value resolution
   works with ints; Rust's explicit `value_to_order_type` mirrors both.
4. **`run_fill` fills filtering** mirrors upstream line 836 —
   `self.fills = [f for f in self.fills if not f.done]`. After each
   `run_fill`, done orders are retained in `orders` (canonical map)
   and ALSO appended to `completed`, while `fills` drops them.
5. **`no_symbol` rejection** uses literal upstream error message —
   `"REJECTED: Symbol {s} not found on the system"` — so any
   downstream caller that scrapes the message stays intact. `status()`
   returns `Status::Rejected` because `status_message` starts with
   `REJ`.

## What to verify

- `cargo test --test parity --all-features` → 190 / 189 / 1 excused,
  exit 0.
- `cargo test --no-default-features` → all targets green.
- `cargo clippy` clean in both feature configs.
- `scripts/parity_gate.sh` shows the 190 / 189 / 1 shape.
- Manifest size 190, no `#[ignore]`, excused.toml still 1 row.
- Upstream pytest collection: 10 `test_replica_broker_*` +
  `test_replica_order_fill` = 10 nodeids from `test_virtual.py`.
- The 5 design choices — any red flags or R8-blockers.
- R9.10's "byte-equal run_fill output across 3 consecutive calls"
  requirement: the R7 port is deterministic under fixed instrument
  prices + a frozen fills list (the RNG in `VOrder` isn't consulted
  during fill). Confirm run_fill idempotency in your sandbox.

## Output format

Same as prior audits. Write to `docs/PORT-PLAN-R7-audit-result.md`. If
ACK, R8 may start (40 `test_compound_order_*` items — the tightest
schedule week per R.13).
