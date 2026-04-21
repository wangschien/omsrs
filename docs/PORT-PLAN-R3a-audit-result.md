# Verdict: NACK

R3.a is mechanically green, but I would not ACK it as "85/85, 0 excused" yet. One active manifest item is intentionally weaker than the upstream test it claims to port, so the parity count is overstated unless that behavior is implemented or the item is promoted to a §14(B) exception.

## P0 findings

None.

## P1 findings

- P1.1 `test_order_timezone` is counted as an active passing parity item while omitting half of the upstream assertion. Upstream asserts both the default label and `order.timestamp.timezone.name == pendulum.now(tz="local").timezone_name` (`/home/ubuntu/refs/omspy/tests/test_order.py:1135`). The Rust trial documents the omission and only checks `order.timezone == "local"` (`tests/parity/test_order.rs:810`), while `Order` stores `timestamp: DateTime<Utc>` and keeps timezone as a detached string label (`src/order.rs:84`, `src/order.rs:173`). That is a valid design only if `test_order_timezone` is a codex-approved §14(B) exception; otherwise the R3 item needs real timezone semantics and a full assertion. Until then, the manifest should not claim 85 active parity passes with 0 excused.

## P2 findings

- P2.1 `Order::execute` reverses upstream precedence when copied attributes and caller kwargs contain the same non-default key. Upstream builds defaults, applies copied attributes, then applies the filtered caller kwargs, so kwargs override copied attributes (`/home/ubuntu/refs/omspy/omspy/order.py:507`-`/home/ubuntu/refs/omspy/omspy/order.py:509`). Rust inserts kwargs first and then inserts `other_args`, so copied attributes win (`src/order.rs:468`-`src/order.rs:475`). Current R3.a tests do not combine `attribs_to_copy` with an overriding kwarg, but the method doc and upstream implementation make the precedence clear.
- P2.2 `Order::execute` returns `None` when `order_id` is already present (`src/order.rs:447`-`src/order.rs:450`), while upstream returns the existing `self.order_id` in that branch (`/home/ubuntu/refs/omspy/omspy/order.py:523`-`/home/ubuntu/refs/omspy/omspy/order.py:525`). The existing do-not-reexecute test only checks call count, so this escaped the port trial.
- P2.3 Decimal-as-string is acceptable for broker kwargs if it is documented as the Rust dynamic-kwargs ABI, but the same helper is also used for persistence rows (`src/order.rs:712`-`src/order.rs:716`). R3.b SQLite tests assert database values such as `average_price == 780` upstream, so either the SQLite layer must coerce these string JSON values back to numeric storage, or the string representation should be explicitly accepted as a persistence deviation before R3.b freezes the API.

## Non-blocking findings

- `OrderLock::unlocked_with_clock` is acceptable as an internal `Order` construction detail. It preserves observable fresh-order modify/cancel behavior under frozen clocks, while `OrderLock::with_clock` still covers the standalone R2 semantics. The source comment in `src/models.rs` is enough for code readers; source-notes can be updated later if desired.
- The R3.a/R3.b split is clean. I did not find the deferred SQLite-backed test ids in the manifest, and `src/persistence.rs` contains only the unconditional trait scaffold plus an empty feature-gated `sqlite` module.
- `add_lock(1, ...)` / `add_lock(2, ...)` matches the actual upstream `Order.add_lock` implementation (`/home/ubuntu/refs/omspy/omspy/order.py:701`-`/home/ubuntu/refs/omspy/omspy/order.py:704`). The older source-note line that says 1=create, 2=modify, 3=cancel is stale.
- `Broker` kwargs as `HashMap<String, serde_json::Value>` is the right shape for this phase. It matches the open-ended upstream `**kwargs` surface and keeps the exact-call assertions readable.

## Verified checks

- `cargo test` passed: parity reported 85 passed, 0 failed, 0 ignored; smoke reported 13 passed.
- `scripts/parity_gate.sh` passed in release mode: manifest size 85, passed 85, failed 0, `Pass (exit 0)`.
- `cargo clippy --all-features --all-targets -- -D warnings` passed.
- `cargo build` and `cargo build --no-default-features` both passed warning-free.
- `cargo test --test parity_runner_smoke` passed the unchanged 13-row smoke matrix.
- `OMSRS_R0_GATE=1 cargo test --test parity` passed, confirming the present-empty excused file remains valid for the R0 empty-set guard.
- `rg -n "#\\[ignore\\]|ignore\\]" src tests Cargo.toml rust-tests scripts` returned no hits.
- `rg -v "^\\s*(#|$)" rust-tests/parity-item-manifest.txt | wc -l` returned `85`.
- `tests/parity/excused.toml` is still present-empty: no `[[excused]]` rows.

## Non-regression commands

- `cargo test`
- `scripts/parity_gate.sh`
- `cargo clippy --all-features --all-targets -- -D warnings`
- `cargo build`
- `cargo build --no-default-features`
- `cargo test --test parity_runner_smoke`
- `OMSRS_R0_GATE=1 cargo test --test parity`
- `rg -n "#\\[ignore\\]|ignore\\]" src tests Cargo.toml rust-tests scripts`
- `rg -v "^\\s*(#|$)" rust-tests/parity-item-manifest.txt | wc -l`
- `rg -n "^\\s*\\[\\[excused\\]\\]" tests/parity/excused.toml`

## Re-audit (post-fix)

Post-fix verdict: ACK. R3.b may start: the prior P1/P2 blockers are closed, the normal parity gate now matches the expected 85 manifest / 84 passing / 1 failing / 1 excused shape, and the lone failure is the codex-approved `test_order_timezone` §14(B) row.

### Prior NACK closure

- P1.1 closed. `test_order_timezone` is no longer counted as a silent active pass. It asserts the portable `timezone == "local"` half, then deliberately panics with a `§14B` message (`tests/parity/test_order.rs:810`-`tests/parity/test_order.rs:821`). `tests/parity/excused.toml` contains the matching required row with `id`, `rationale`, `approved_at`, and `approved_by` (`tests/parity/excused.toml:4`-`tests/parity/excused.toml:8`).
- P2.1 closed. `Order::execute` now applies copied `other_args` first and then filtered caller kwargs, so kwargs override copied attributes without overriding default order fields (`src/order.rs:468`-`src/order.rs:493`). `test_order_execute_attribs_to_copy_override` covers the conflict case (`tests/parity/test_order.rs:1144`-`tests/parity/test_order.rs:1177`).
- P2.2 closed. `Order::execute` returns the existing `order_id` when an order is already complete or already placed (`src/order.rs:442`-`src/order.rs:500`), matching upstream's existing-id branch.
- P2.3 closed. Broker kwargs still serialize `Decimal` values as strings via `decimal_value`, while persistence rows now use `decimal_persistence_value` to emit JSON numbers for `price`, `trigger_price`, and `average_price` (`src/order.rs:729`-`src/order.rs:739`, `src/order.rs:840`-`src/order.rs:855`).

### Verification matrix

- `cargo test` passed. The parity harness reported manifest size 85, passed 84, failed 1, gate `Pass (exit 0)`, failing id `test_order_timezone`; smoke also passed 13/13.
- `scripts/parity_gate.sh` passed in release mode with the same shape: manifest size 85, passed 84, failed 1, gate `Pass (exit 0)`, failing id `test_order_timezone`.
- `cargo clippy --all-features --all-targets -- -D warnings` passed.
- `cargo build` passed warning-free.
- `cargo build --no-default-features` passed warning-free.
- `cargo test --test parity_runner_smoke` passed 13/13.
- `OMSRS_R0_GATE=1 cargo test --test parity` returned `R0GateViolation (exit 4)`, because the R0 guard intentionally rejects the now non-empty `excused.toml`. This is expected after approving the R3.a §14(B) row and is not a post-fix NACK blocker.
- `rg -n "#\\[ignore\\]|ignore\\]" src tests Cargo.toml rust-tests scripts` returned no hits.
- `rg -v "^\\s*(#|$)" rust-tests/parity-item-manifest.txt | wc -l` returned `85`.
- `tests/parity/excused.toml` has exactly one row, `test_order_timezone`, with the required fields.

No new R3.a drift found.
