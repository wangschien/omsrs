# R6 audit prompt

R5 ACKed. R6 landed locally. Full gate under `--all-features`: 180
manifest / 179 pass / 1 excused (`test_order_timezone`, R3.a §14B row),
exit 0.

## Scope

R6 per PORT-PLAN §8: 22 VirtualBroker items from
`tests/simulation/test_virtual.py`, multi-user inclusive. pytest
collects 23 `test_virtual_broker_*` definitions with one duplicate
(`test_virtual_broker_ltp` twice); pytest keeps only the second. 23 − 1
duplicate = 22. ✓

## Deliverables

- `src/virtual_broker.rs` — `VirtualBroker` struct with:
  - multi-user `VUser` tracking (`Vec<VUser>` + `HashSet<String>`
    clients),
  - seeded `SmallRng` for `is_failure` (deterministic by default,
    seed=0),
  - clock-injectable `order_place` / `order_modify` / `order_cancel`
    flows returning a `BrokerReply` enum (`Order(Box<OrderResponse>)`
    vs `Passthrough(Value)`),
  - `get(order_id, status)` routing to `VOrder::modify_by_status(now)`
    with `now` pulled from the broker's clock,
  - `update_tickers` / `ltp` / `ltp_many` / `ohlc` read-side helpers.
- `src/simulation.rs` — `VOrder::cloned_clone_weak` bridge so broker
  responses can embed a fresh `VOrder` shell without cloning the
  `Mutex<SmallRng>`.
- `tests/parity/test_virtual_broker.rs` — 22 trials.

## Design choices worth scrutinising

1. **Seeded `SmallRng` in `is_failure`** (default seed=0). Upstream's
   `random.random()` is unseeded; my R6 tests would be flaky across
   runs under that semantic. Seeding makes `failure_rate=0.001 +
   is_failure() == false` stable on the first few reads (the trivial
   case R6 needs).

2. **`BrokerReply` enum vs union-typed return.** Upstream returns
   `Union[OrderResponse, dict]`. Rust can't express that without an
   enum; `BrokerReply` has `Order(Box<OrderResponse>)` (Box'd for
   `clippy::large_enum_variant`) and `Passthrough(Value)`.

3. **`test_virtual_broker_order_place_validation_error` error
   strings.** My Rust errors are formatted `"Found {n} validation
   errors; in field {fld} Field required"` — just enough for the
   `.starts_with("Found N validation")` + `.contains("quantity")`
   assertions. Full pydantic fidelity would require a validator
   framework; flag if you want that.

4. **`test_virtual_broker_order_place_same_memory` ported as id
   equivalence, not object identity.** Upstream asserts
   `id(order) == id(b._orders[order.order_id])`. Rust VOrder is owned
   not shared; the Rust trial asserts `canonical.order_id ==
   user_shell.order_id`. Semantic intent preserved — both references
   point at "the same order logically". Flag if you'd prefer a shared
   `Arc<Mutex<VOrder>>` design to get true pointer equality.

5. **`ltp` vs `ltp_many`** split. Upstream `ltp(symbol)` accepts
   `Union[str, Iterable]`. Rust surfaces two methods to avoid an
   `impl Trait` ambiguity. Parity trial uses both.

6. **`test_virtual_broker_ltp` port covers only the second (shadowing)
   upstream definition** — pytest collects only that one.

7. **`VOrder::cloned_clone_weak`.** `VOrder` holds `Mutex<SmallRng>` so
   it's not `Clone`. This helper produces a fresh shell with the same
   public fields + a new default-seeded RNG. Broker responses use it
   so the same VOrder "appears in" both `broker._orders` and
   `user.orders`.

## What to verify

- `cargo test --test parity --all-features` → 180 / 179 / 1 excused,
  exit 0.
- `cargo test --no-default-features` → all targets green.
- `cargo clippy` clean in both feature configs.
- `scripts/parity_gate.sh` shows the 180 / 179 / 1 shape.
- Manifest size 180, no `#[ignore]`, `excused.toml` still exactly 1
  row.
- Upstream collection: pytest should show 23 `test_virtual_broker_*`
  function nodeids with one duplicate, i.e. 22 collected.
- The 7 design choices above — any red flags or R7-blockers.

## Output format

Same as prior audits. Write to `docs/PORT-PLAN-R6-audit-result.md`. If
ACK, R7 may start (10 `ReplicaBroker` items).
