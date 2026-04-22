# codex audit — R12.2 `AsyncReplicaBroker`

## Context

Second sub-phase of R12 (plan v3 ACKed). R12.1
`AsyncVirtualBroker` landed + re-audit ACK (`bf5809d`). R12.2
is the async port of the standalone `ReplicaBroker` matching
engine — **not** a primary/replica wrapper (plan v1 had that
wrong; v2 corrected).

Landed commit: `7b362b9`.

## What shipped

- `src/async_replica_broker.rs` — new file, async port
- `src/replica_broker.rs` — surgical widenings:
  * `#[derive(Clone)]` on `ReplicaFill` (Arc + f64, backwards-
    compatible)
  * 4 private helpers (`value_to_side`, `value_to_order_type`,
    `apply_as_market`, `apply_fill_update`) bumped to
    `pub(crate)` so async module reuses them
- `tests/parity_async_replica.rs` — 11-item parity harness
- `src/lib.rs` — `pub use async_replica_broker::AsyncReplicaBroker`

## Contract (locked here)

1. **Shared identity preserved**: `OrderHandle = Arc<Mutex<VOrder>>`
   reused unchanged from sync module. Every collection holds the
   same `Arc`; `Arc::ptr_eq` across `orders()`/`pending()`/
   `completed()`/`fills()`/`user_orders(user)` returns true for
   handles pointing to the same inner order. The sync test
   `test_replica_broker_order_place` depends on this; the async
   mirror (`order_place_preserves_shared_identity`) asserts the
   same invariant.

2. **Locking**: single `parking_lot::Mutex<Inner>`. No `.await`
   inside `self.inner.lock()` scopes. `modify` deliberately
   releases the inner lock before grabbing the handle's own
   `Mutex<VOrder>` — inner lookup + handle write don't need to
   be atomic (sync path also serializes on `&mut self`, but
   handle mutation is separate).

3. **`order_id` via args** convention matches R12.1: async
   `modify` / `cancel` read `order_id` from `args["order_id"]`
   rather than taking it as a positional `&str` (sync does the
   latter; async normalizes to match the `AsyncBroker` trait
   shape).

4. **AsyncBroker trait adapter**: `order_place → Some(oid)`
   always (even on REJECTED — sync still returns a handle in
   that case, just with Rejected status). `order_modify` /
   `order_cancel` swallow the `Option<OrderHandle>` to `()`.

## Audit scope

### 1. Locking + concurrency
- Walk `place` / `modify` / `cancel` / `run_fill` — any
  `.await` while `self.inner.lock()` is held?
- `modify` acquires `inner.lock()`, clones a handle, **drops
  inner**, then does `handle.lock()`. Is this the right
  split, or is there a race where the caller could see the
  handle disappear between those two locks? (In practice
  the order lives in `inner.orders` forever once placed, so
  dropping inner before handle.lock is safe. Confirm.)
- `run_fill` has some dancing to avoid overlapping borrows on
  `inner.fills` vs `inner.instruments` vs `inner.orders`.
  The commit splits into price-compute loop → update loop →
  completion-promote loop. Is that semantically equivalent
  to the sync `run_fill` body, or does it subtly change the
  observable behavior (e.g. a fill at index i influencing
  index i+1 via instrument state)? Note: instrument state
  isn't mutated inside `run_fill`, so the order-independence
  should hold.

### 2. Sync parity
- 11 tests mirror sync. Intentionally skipped:
  - none (all 10 sync tests mapped; +1 added for AsyncBroker
    trait smoke)
- Values pinned: `replica_with_orders` fills at indices
  (0,3,4,5,7) with avg_prices (125,136,136,153,153) —
  identical to sync. If async ordering diverges, the
  `run_fill_applies_orderfill_rules` test trips.

### 3. `pub(crate)` widenings in sync module
- `ReplicaFill` derive `Clone` — check nothing in sync
  callers relied on it being non-Clone (unlikely, but
  Rust's orphan rules don't flag this).
- `apply_as_market` / `apply_fill_update` / `value_to_side` /
  `value_to_order_type` bumped to `pub(crate)` — these are
  internal-only and don't widen the public API. The crate
  visibility lets async reuse without duplication.

### 4. Semver guard
- No existing sync method signature or public struct field
  changed. Confirm by spot-checking `src/replica_broker.rs`
  diff.
- `src/lib.rs` only adds new `pub mod` + `pub use`.

### 5. Deadlock risk
- External caller holds an `OrderHandle` (from `place`) and
  does `handle.lock()` on their thread. Meanwhile another
  task calls `broker.modify(args)`. Modify's implementation
  acquires `inner.lock()` (to resolve order_id → handle),
  then releases, then acquires the handle's own lock. If
  the external holder still has the handle locked, modify
  blocks on the handle lock — but not under inner.lock, so
  other broker methods still make progress. Acceptable.

## Out of scope

- R12.3a+ (async Order lifecycle).
- Persistence interactions.

## Output

`docs/audit-R12.2-codex-result.md`. 5-item checklist
(PASS/CONCERN/FAIL + rationale). Final verdict:
- `R12.2 ACK — proceed to R12.3a`, or
- `R12.2 NACK — fix items X, Y, Z first`.

Per `feedback_codex_audit_judgment`: plan author assesses
each NACK item on merit. Be specific, technical, line-cite
when concrete.
