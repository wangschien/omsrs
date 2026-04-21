# Verdict: ACK

R3.b may start R4. The full `--all-features` parity gate is at the expected R3 total
of 94 manifest items, with 93 passing and the single R3.a-approved
`test_order_timezone` exception. The default and no-default builds both shrink the
effective parity set to 85 and keep the same excused-failure shape.

## P0 findings

None.

## P1 findings

None.

## P2 findings

- P2.1 `Order::from_row` does not actually ignore stored `pending_quantity`,
  despite the R3.b design note saying it does. Upstream `Order.__init__` always
  executes `self.pending_quantity = self.quantity` after pydantic validation
  (`/home/ubuntu/refs/omspy/omspy/order.py:216` and
  `/home/ubuntu/refs/omspy/omspy/order.py:224`). Rust routes row reconstruction
  through `OrderInit::from_row` and then preserves an explicit stored value via
  `init.pending_quantity.unwrap_or(quantity)` (`src/order.rs:92`,
  `src/order.rs:266`, `src/order.rs:289`). The current R3.b upstream tests only
  reconstruct a freshly saved row where `pending_quantity == quantity`, so this
  does not invalidate the 94-item gate. It is still a real drift to close before
  any later phase relies on loading partially updated orders from SQLite.

## Non-blocking notes

- The dual-feature manifest split is sound for the current state. With
  `persistence` enabled, the registered set and embedded manifest both contain
  the 9 R3.b ids. With it disabled, `parse_manifest` removes those ids and
  `persistence_trials()` returns empty, while the cross-check remains active in
  both directions.
- The SQLite schema matches upstream's 37-column `orders` table in column names,
  order, and type affinities. `CREATE TABLE IF NOT EXISTS` is a benign wrapper
  difference around the upstream DDL.
- Compact JSON storage is an intentional byte-level drift from upstream
  `json.dumps` spacing. I do not see it as an R4 blocker because `from_row`
  parses it semantically, but byte-exact upstream parity would require a small
  Python-style serializer adjustment.
- Decimal persistence via f64-backed JSON numbers matches upstream SQLite REAL
  storage for the covered cases. The precision tradeoff is correctly isolated
  from broker kwargs, which still use string-shaped decimal values.
- `PersistenceError::Unique` is correctly hit for the primary-key duplicate
  path. The schema has no other active constraints, so mapping SQLite constraint
  violations to this variant is acceptable for R3.b.

## Verified checks

- `cargo test --all-features` passed overall. Parity reported manifest size 94,
  passed 93, failed 1, gate `Pass (exit 0)`, failing id `test_order_timezone`;
  smoke passed 13/13.
- `cargo test` passed overall. Parity reported manifest size 85, passed 84,
  failed 1, gate `Pass (exit 0)`, failing id `test_order_timezone`; smoke
  passed 13/13.
- `cargo test --no-default-features` passed overall with the same 85 / 84 / 1
  parity shape and smoke 13/13.
- `cargo clippy --all-features --all-targets -- -D warnings` passed.
- `cargo clippy --no-default-features --all-targets -- -D warnings` passed.
- `cargo build --no-default-features` passed warning-free.
- `scripts/parity_gate.sh` passed in release mode: manifest size 94, passed 93,
  failed 1, gate `Pass (exit 0)`, failing id `test_order_timezone`.
- `OMSRS_R0_GATE=1 cargo test --test parity --all-features` returned exit 4
  with gate `R0GateViolation`, as expected because `excused.toml` is now
  non-empty.
- `rg -n "#\\[ignore\\]|ignore\\]" src tests Cargo.toml rust-tests scripts`
  returned no hits.
- `rg -v "^\\s*(#|$)" rust-tests/parity-item-manifest.txt | wc -l` returned
  `94`.
- `tests/parity/excused.toml` has exactly one row, `test_order_timezone`, with
  `rationale`, `approved_at`, and `approved_by`.

## Non-regression commands

- `cargo test --all-features`
- `cargo test`
- `cargo test --no-default-features`
- `cargo clippy --all-features --all-targets -- -D warnings`
- `cargo clippy --no-default-features --all-targets -- -D warnings`
- `cargo build --no-default-features`
- `scripts/parity_gate.sh`
- `OMSRS_R0_GATE=1 cargo test --test parity --all-features`
- `rg -n "#\\[ignore\\]|ignore\\]" src tests Cargo.toml rust-tests scripts`
- `rg -v "^\\s*(#|$)" rust-tests/parity-item-manifest.txt | wc -l`
- `rg -n "^\\s*\\[\\[excused\\]\\]" tests/parity/excused.toml`
