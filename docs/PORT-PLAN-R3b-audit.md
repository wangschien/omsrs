# R3 final audit prompt (R3.a ACKed; R3.b now landed)

R3.a was ACKed after the NACK-cycle fix (see
`docs/PORT-PLAN-R3a-audit-result.md` §Re-audit). R3.b adds 9 SQLite-backed
items on top, completing PORT-PLAN §8's R3 phase (total 64).

Commits since R3.a re-audit ACK:
- `e7208a1` — R3.b implementation (`SqlitePersistenceHandle`, `Order::from_row`,
  the 9 new trials).
- `0821df1` — `#[cfg(feature = "persistence")]` gating so
  `cargo test --no-default-features` still works per PORT-PLAN §9.5.

## Scope

9 new trials from `tests/test_order.py`:
- `test_order_create_db`
- `test_order_create_db_primary_key_duplicate_error`
- `test_order_save_to_db`
- `test_order_save_to_db_update`
- `test_order_save_to_db_multiple_orders`
- `test_order_save_to_db_update_order`
- `test_new_db`
- `test_new_db_with_values`
- `test_new_db_all_values`

Full R3 gate math: 55 (R3.a) + 9 (R3.b) = 64 ✓ (matches PORT-PLAN §8 R3
= 64). Cumulative manifest now 94 (R1 20 + R2 10 + R3 64).

## Deliverables

- `src/persistence.rs`: `sqlite` module with
  `SqlitePersistenceHandle` (`in_memory` / `open` / `insert_raw` /
  `upsert` / `query_all` / `count`), schema 1:1 with upstream
  `order.create_db`, and a `PersistenceError::Unique` variant for
  PK-conflict classification.
- `src/order.rs`: `OrderInit::from_row` + `Order::from_row` — row →
  OrderInit → Order reconstruction, handling bool 0/1 ↔ bool, float ↔
  Decimal, ISO-string ↔ DateTime<Utc>, JSON-string ↔ HashMap.
- `tests/parity/test_order.rs`: 9 new trials, all
  `#[cfg(feature = "persistence")]`-gated along with their `use`s.
- `tests/parity/main.rs`: split trial registration into
  `BASE_PARITY_NAMES` (unconditional) + `PERSISTENCE_PARITY_NAMES`
  (feature-filtered). `parse_manifest` drops the 9 ids from the
  effective manifest when the feature is off; `persistence_trials()`
  returns empty. Cross-check in both directions still enforces 1:1
  manifest↔trial.
- `rust-tests/parity-item-manifest.txt`: 9 ids appended under a
  `R3.b: SQLite-backed` section.

## Design choices worth scrutinising

1. **Dual-feature cross-check.** The effective manifest + the
   registered trial set both shrink/grow together with the
   `persistence` feature. The cross-check in `main.rs:229–249` runs
   exactly the same under both configurations; only the set
   size differs.

2. **JSON field serialisation.** `to_row()` emits the HashMap as
   `serde_json::to_string(v)` (compact). Upstream writes
   `json.dumps(v)` (spaced). Compact form is semantically equivalent;
   the Rust test asserts against the compact form. Flag if you want
   upstream-exact string output at the cost of an extra serializer.

3. **Decimal on persistence path.** R3.a introduced
   `decimal_persistence_value()` → f64 for SQLite REAL columns. Round
   trip via `OrderInit::from_row` parses the Decimal back from the
   REAL value. Lossy for > f64 precision; matches upstream's float
   storage. Decimal in broker kwargs (`decimal_value()`) stays
   string-shaped.

4. **`Order::from_row` goes through `from_init`.** Upstream
   `Order(**row)` runs `__init__`, which unconditionally sets
   `pending_quantity = self.quantity` and recomputes `expires_in` when
   it's 0. The Rust port replicates that: stored `pending_quantity` is
   ignored (overwritten to `quantity`); stored `expires_in` survives
   because it's non-zero after the first init. Consequently a
   round-tripped Order matches upstream `model_dump` equivalence.

5. **Primary-key-duplicate = `PersistenceError::Unique`.** Upstream
   raises `sqlite3.IntegrityError`; we map the rusqlite
   `ConstraintViolation` code to our own enum variant. Test asserts
   `matches!(err, PersistenceError::Unique(_))`.

6. **`cargo test` with default features + no explicit `--all-features`
   runs the R3.a subset (85 manifest) not the full R3 (94). That's by
   design — default features don't include `persistence`, per plan §7
   ("persistence default off at MSRV-minimum build"). The full gate
   with 94 items runs via `scripts/parity_gate.sh` (which passes
   `--all-features`).

## What to verify

- Full gate under `--all-features`: 94 / 93 pass / 1 excused
  (test_order_timezone), exit 0.
- Default-feature gate: 85 / 84 / 1 excused, exit 0.
- No-default-features: `cargo test --no-default-features` passes all
  targets (parity manifest shrinks to 85; R3.b trials skip).
- `cargo clippy` in both feature configurations is clean.
- `cargo build --no-default-features` is warning-free.
- `scripts/parity_gate.sh` shows manifest 94, pass 93, fail 1, exit 0.
- No `#[ignore]` anywhere.
- `excused.toml` still has exactly the 1 R3.a-approved row.
- `OMSRS_R0_GATE=1` still trips the R0 guard (expected, excused.toml
  is non-empty).
- The 6 design choices above — any red flags or R4-blockers.

## Output format

Same as R3.a. Write the result to `docs/PORT-PLAN-R3b-audit-result.md`.
If ACK, R4 may start (10 `test_base` items).
