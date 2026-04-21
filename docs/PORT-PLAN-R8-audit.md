# R8 audit prompt

R7 ACKed. R8 landed locally. Gate under `--all-features`: 230 manifest /
229 pass / 1 excused (R3.a `test_order_timezone`), exit 0.

## Scope

R8 per PORT-PLAN §8: 40 items from `test_order.py::test_compound_order_*`.
pytest collects 40 from 41 upstream defs (duplicate
`test_compound_order_update_orders` — pytest keeps the second, DB-aware,
definition).

## Deliverables

- `src/compound_order.rs` — `CompoundOrder` with Vec<Order> orders +
  HashMap index/key lookup, aggregate views (positions, buy/sell
  quantity, avg price, net_value, mtm, total_mtm), update_orders,
  execute_all, check_flags, save.
- `tests/parity/test_compound_order.rs` — 34 non-persistence trials +
  6 SQLite-backed trials (update_orders mapped to the persistence
  variant matching pytest collection).

## Design choices worth scrutinising

1. **`Vec<Order>` by value, not `Arc<Mutex<Order>>`.** Unlike R7
   ReplicaBroker, R8 tests don't hold external references to orders
   after adding. Mutations happen through `compound.orders[i]` or via
   compound methods. Saves me the lock noise; R7's shared-handle
   pattern stays isolated to ReplicaBroker.

2. **`index_map: HashMap<i64, usize>`** stores logical index →
   position in `orders`. Upstream's `_index: Dict[int, Order]` holds
   Order references; ours points at positions because `orders` is
   append-only (indices are stable).

3. **String-keyed `keys_map`** accepts any string. Upstream's
   `_keys: Dict[Hashable, Order]` takes tuples, ints, strings.
   `test_compound_order_keys_hashable` ports the tuple case via JSON
   serialisation (`"[4,5]"`); the upstream "dict keys raise
   TypeError" half isn't portable to Rust's type system — documented
   as deliberate narrowing, not parity loss.

4. **`update_orders` points at the second upstream definition**
   (persistence-required, line 749). Marked `#[cfg(feature =
   "persistence")]` and registered under `PERSISTENCE_PARITY_NAMES`.
   Mirrors pytest's "second definition shadows first" semantic.

5. **`check_flags` conversion path** clears `price = None` and
   `trigger_price = Decimal::ZERO` when converting a LIMIT → MARKET
   on expiry. Matches upstream's line 1177-1179.

6. **`execute_all` kwargs precedence**: `self.order_args` first, then
   caller kwargs override. Mirrors upstream's `current_order_args =
   deepcopy(self.order_args); current_order_args.update(kwargs)`.

7. **5 SQLite tests** delegate to `SqlitePersistenceHandle::in_memory`
   and use the R3.b pattern (concrete Arc for query_all/count,
   upcast to `Arc<dyn PersistenceHandle>` for compound.connection).

## What to verify

- `cargo test --test parity --all-features` → 230 / 229 / 1 excused,
  exit 0.
- `cargo test --no-default-features` → all targets green.
- `cargo clippy` clean in both feature configs.
- `scripts/parity_gate.sh` shows the 230 / 229 / 1 shape.
- Manifest 230, no `#[ignore]`, excused.toml still 1 row.
- Upstream collection: `pytest --collect-only` should report 40
  `test_compound_order_*` nodeids in `test_order.py`.
- The 7 design choices — any red flags or R9-blockers.

## Output format

Same as prior audits. Write to `docs/PORT-PLAN-R8-audit-result.md`. If
ACK, R9 may start (7 `test_order_strategy` items — last implementation
phase before R10 sweep).
