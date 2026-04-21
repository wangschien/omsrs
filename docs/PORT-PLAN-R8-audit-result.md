R8 is not ACKed yet. The implementation and gate are mechanically green, but
two active R8 trials are weaker than the upstream tests they claim to port. R9
should wait until those assertions are restored.

## Findings

### P1.1 `test_compound_order_completed_orders` omits the post-mutation assertion

Upstream mutates the last order after the initial completed-order count and then
asserts that the completed list grows from 2 to 3
(`/home/ubuntu/refs/omspy/tests/test_order.py:615`). The Rust trial only checks
the initial length
(`/home/ubuntu/omsrs/tests/parity/test_compound_order.rs:441`).

Required fix: extend the Rust trial with the upstream mutation:
`orders[-1].status = "COMPLETE"`, `orders[-1].filled_quantity = 12`, then assert
`completed_orders().len() == 3`.

### P1.2 `test_compound_order_save_to_db` does not inspect the saved DB rows

Upstream checks that the compound fixture has 3 persisted rows and that their
quantities are 20, 10, and 12
(`/home/ubuntu/refs/omspy/tests/test_order.py:731`). The Rust trial creates a
concrete SQLite handle but does not wire that handle into the compound order;
it only asserts that `com.save()` returns 3
(`/home/ubuntu/omsrs/tests/parity/test_compound_order.rs:925`).

Required fix: use the R3.b concrete-Arc pattern in this trial too, then assert
`count() == 3` and the persisted quantities. Neighboring tests cover related
persistence paths, but this active manifest item should still port its own
upstream assertions.

## Scope Check

- `src/compound_order.rs` adds `CompoundOrder` with by-value `Vec<Order>`
  storage, index/key maps, aggregate views, `update_orders`, `execute_all`,
  `check_flags`, and `save`.
- `tests/parity/test_compound_order.rs` defines 34 non-persistence R8 trials and
  6 `#[cfg(feature = "persistence")]` SQLite-backed trials.
- `tests/parity/main.rs` registers the 34 non-persistence names in the base
  manifest and the 6 SQLite-backed names under `PERSISTENCE_PARITY_NAMES`.
- `rust-tests/parity-item-manifest.txt` has 230 active rows and its 40 R8 names
  match the 40 upstream collected `test_compound_order_*` nodeids exactly.
- `tests/parity/excused.toml` still has exactly one `[[excused]]` row:
  `test_order_timezone`.
- No active `#[ignore]` markers were found under `src`, `tests`, `Cargo.toml`,
  `rust-tests`, or `scripts`.

## Design Review

1. `Vec<Order>` by value is acceptable for Rust ownership, but the design note
   is slightly too broad: upstream `test_compound_order_save` mutates external
   `order1` / `order2` references after `com.add(order)`. The Rust trial mutates
   through `com.orders[i]`, which tests the same saved-state behavior under the
   consuming Rust API. I would document this as a Rust ownership narrowing rather
   than saying no upstream test holds an external reference.
2. `index: HashMap<i64, usize>` is sound for append-only order storage and
   preserves the upstream same-object `_index` behavior through positions.
3. String-only keys are a deliberate type-system narrowing. The tuple/hashable
   case is represented by a canonical string, and the unhashable-dict TypeError
   has no Rust equivalent.
4. `update_orders` correctly targets the later, pytest-collected DB-aware
   upstream definition. One non-blocking API difference remains: upstream returns
   `false` for pending orders missing from the input data, while Rust currently
   emits entries only for pending orders that had data. No collected R8 trial
   exercises the missing-data branch.
5. `check_flags` matches the conversion path: LIMIT becomes MARKET, `price` is
   cleared, and `trigger_price` is reset to zero before modify.
6. `execute_all` preserves kwargs precedence: `self.order_args` first, caller
   kwargs override.
7. The SQLite-backed tests use the concrete-Arc/upcast pattern in most places.
   `test_compound_order_save_to_db` is the exception and is covered by P1.2.

## Verification

- `cargo test --test parity --all-features` exited 0 via the parity gate:
  manifest 230, passed 229, failed 1, failing id `test_order_timezone`, gate
  `Pass`.
- `cargo test --no-default-features` exited 0. With persistence trials
  feature-gated out, the effective parity manifest is 215: passed 214, failed
  1, failing id `test_order_timezone`, gate `Pass`. Parity-runner smoke tests
  passed 13/13.
- `cargo clippy --all-targets --all-features -- -D warnings` passed.
- `cargo clippy --all-targets --no-default-features -- -D warnings` passed.
- `cargo build --all-features` passed.
- `cargo build --no-default-features` passed.
- `scripts/parity_gate.sh` exited 0 in release mode with the same 230 / 229 / 1
  gate shape.
- Bare `pytest` is not installed on PATH in this shell. The equivalent
  `python3 -m pytest --collect-only -q tests/test_order.py` from
  `/home/ubuntu/refs/omspy` collected 40 `test_compound_order_*` nodeids.
- Active manifest count was verified with comment/blank-line stripping:
  `awk 'NF && $1 !~ /^#/ {n++} END {print n}' rust-tests/parity-item-manifest.txt`
  returned 230.

## Re-audit (post-fix)

ACK. R8 is ACKed after the P1.1 and P1.2 fixes.

### Fix verification

- P1.1 is fixed: `test_compound_order_completed_orders` now mutates the last
  order to `status = "COMPLETE"` and `filled_quantity = 12`, then asserts
  `completed_orders().len() == 3`, matching the upstream post-mutation check.
- P1.2 is fixed: `test_compound_order_save_to_db` now uses the concrete
  `SqlitePersistenceHandle` Arc/upcast pattern, verifies `count() == 3`,
  inspects saved DB rows, and asserts the persisted quantities are 10, 12, and
  20 before asserting `com.save() == 3`. The Rust trial checks quantity set
  equality rather than row order; upstream uses `SELECT *` without `ORDER BY`,
  so this covers the material persisted-row/quantity assertion.

### Verification

- `cargo test --all-features` exited 0. Parity gate shape: manifest 230, passed
  229, failed 1, failing id `test_order_timezone`, gate `Pass`. Parity runner
  smoke tests passed 13/13; statistical tests and doc-tests passed.
- `cargo test --no-default-features` exited 0. Effective no-persistence parity
  shape: manifest 215, passed 214, failed 1, failing id
  `test_order_timezone`, gate `Pass`. Parity runner smoke tests passed 13/13;
  doc-tests passed.
- `cargo clippy --all-targets --all-features -- -D warnings` passed.
- `cargo clippy --all-targets --no-default-features -- -D warnings` passed.
- `scripts/parity_gate.sh` exited 0 in release mode with the expected
  230 / 229 / 1 excused gate shape.
- Active manifest count remains 230:
  `awk 'NF && $1 !~ /^#/ {n++} END {print n}' rust-tests/parity-item-manifest.txt`.
- `tests/parity/excused.toml` has exactly one `[[excused]]` row:
  `test_order_timezone`.
