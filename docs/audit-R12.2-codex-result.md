# R12.2 AsyncReplicaBroker audit result

Commit context: `HEAD` is `b2d6046`; substantive code commit is `7b362b9`.

## 1. Locking + concurrency: FAIL

Observed evidence: there is no `.await` inside an `inner.lock()` scope in the inherent methods. `place` takes `inner` at `src/async_replica_broker.rs:167` and returns at `src/async_replica_broker.rs:196`; `modify` takes `inner` only to clone the handle, then drops it before `handle.lock()` (`src/async_replica_broker.rs:207-212`); `cancel` and `run_fill` have no await in their locked regions (`src/async_replica_broker.rs:249-263`, `src/async_replica_broker.rs:270-314`). The trait adapter awaits only the inherent methods and does not itself hold `inner` (`src/async_replica_broker.rs:326-337`).

Observed evidence: the `modify` split matches the locked contract. It clones the handle while holding `inner`, explicitly drops `inner`, then mutates the order (`src/async_replica_broker.rs:207-239`). The handle cannot disappear in that gap because orders are inserted once at place time (`src/async_replica_broker.rs:167-168`) and neither `cancel` nor `run_fill` removes from `orders` (`src/async_replica_broker.rs:260-263`, `src/async_replica_broker.rs:306-314`).

Observed evidence: `cancel` and `run_fill` still take locks in the opposite order from external handle users: they hold `inner` and then lock existing order handles. `cancel` locks `inner` at `src/async_replica_broker.rs:249` and the handle at `src/async_replica_broker.rs:254`. `run_fill` locks `inner` at `src/async_replica_broker.rs:270`, locks fill handles while computing symbols (`src/async_replica_broker.rs:280-281`), calls `apply_fill_update` while still holding `inner` (`src/async_replica_broker.rs:294-297`; helper lock at `src/replica_broker.rs:284-285`), then locks handles again for completion and retain checks (`src/async_replica_broker.rs:298-299`, `src/async_replica_broker.rs:314`).

Hypothesis / reachable risk: an external caller can hold an `OrderHandle` returned by `place` (`src/async_replica_broker.rs:196`). If task B enters `cancel` or `run_fill`, it can hold `inner` while waiting on that handle. If task A then calls an accessor while still holding the handle, it waits on `inner` (`orders` / `fills` / `user_orders` all lock `inner` at `src/async_replica_broker.rs:111-132`). That is an ABBA deadlock: A holds handle and waits `inner`; B holds `inner` and waits handle. This is not observed in the green parity tests, but it is a concrete lock-order inversion. The module comment claiming the `inner`-while-handle path is only used where "no external caller has yet received the handle" is true for the new handle in `place`, but false for `run_fill` over existing fills (`src/async_replica_broker.rs:24-30`, `src/async_replica_broker.rs:280-297`).

## 2. Sync parity and shared identity: PASS

Observed evidence: the async port preserves the shared `Arc<Mutex<VOrder>>` identity contract. `place` inserts the same `handle.clone()` into `orders`, `user_orders`, `pending`, and `fills` (`src/async_replica_broker.rs:165-195`). Rejected orders push the same handle into `completed` (`src/async_replica_broker.rs:176-184`). `cancel` clones the handle fetched from `orders` into `completed` (`src/async_replica_broker.rs:249-262`). `run_fill` promotes completed orders by cloning from `orders` (`src/async_replica_broker.rs:306-312`). `fills()` returns cloned `ReplicaFill` values (`src/async_replica_broker.rs:127-128`), and `ReplicaFill` cloning clones the `Arc`, not the `VOrder` (`src/replica_broker.rs:20-29`).

Observed evidence: the parity harness directly asserts `Arc::ptr_eq` across the order returned by `place`, `orders`, `user_orders`, `pending`, and `fills` (`tests/parity_async_replica.rs:123-150`). It also exercises non-happy-path behavior: multi-user routing (`tests/parity_async_replica.rs:153-184`), multi-stage fill rules with pinned average prices (`tests/parity_async_replica.rs:186-250`), modify-to-refill (`tests/parity_async_replica.rs:252-282`), modify-to-market (`tests/parity_async_replica.rs:284-323`), cancel and cancel idempotency (`tests/parity_async_replica.rs:325-373`), rejected unknown symbols (`tests/parity_async_replica.rs:376-413`), and the `AsyncBroker` lossy adapter (`tests/parity_async_replica.rs:415-443`). The harness does not explicitly `Arc::ptr_eq` a completed handle, but implementation inspection above confirms completed uses cloned shared handles rather than cloned orders.

Observed evidence: the `run_fill` split is semantically equivalent to the sync body for single-threaded broker semantics. Sync reads instrument last price, updates `fill.last_price`, applies `apply_fill_update`, records done ids, promotes from `orders`, then retains non-done fills (`src/replica_broker.rs:194-220`). Async computes `(fill_index, last_price)` first (`src/async_replica_broker.rs:278-292`), then updates each fill and order (`src/async_replica_broker.rs:294-300`), then promotes from `orders` and retains (`src/async_replica_broker.rs:303-314`). The only state read from instruments is `last_price` (`src/async_replica_broker.rs:281-289`), and `apply_fill_update` mutates only the order handle fields, not `instruments` or broker collections (`src/replica_broker.rs:284-326`).

## 3. `pub(crate)` widenings: PASS

Observed evidence: the sync diff is limited to `ReplicaFill: Clone` and four helper visibility widenings. `ReplicaFill` derives `Clone` at `src/replica_broker.rs:22-29`; the widened helpers are `value_to_side`, `value_to_order_type`, `apply_as_market`, and `apply_fill_update` (`src/replica_broker.rs:224-251`, `src/replica_broker.rs:284-326`). The async module imports exactly those helpers plus `OrderHandle` and `ReplicaFill` (`src/async_replica_broker.rs:40-43`). These are crate-visible only and do not widen the public API.

## 4. Semver guard: PASS

Observed evidence: existing sync public structs and method signatures remain intact: `ReplicaBroker` fields are unchanged (`src/replica_broker.rs:32-40`), and the sync `order_place`, `order_modify`, `order_cancel`, and `run_fill` signatures remain the same (`src/replica_broker.rs:76`, `src/replica_broker.rs:136-140`, `src/replica_broker.rs:175`, `src/replica_broker.rs:194`). `src/lib.rs` adds only the new async module and re-export (`src/lib.rs:4`, `src/lib.rs:21`).

## 5. Deadlock risk: FAIL

Observed evidence: the specific `modify` deadlock scenario in the prompt is handled correctly: `modify` is not waiting on the handle while holding `inner` (`src/async_replica_broker.rs:207-212`), so a slow external handle holder does not block unrelated broker operations behind `inner`.

Observed evidence: the same property does not hold for `cancel` and `run_fill`. Both operate on handles that have already escaped to callers via `place` (`src/async_replica_broker.rs:196`), and both can wait on those handles while holding `inner` (`src/async_replica_broker.rs:249-254`, `src/async_replica_broker.rs:270-314`). This is enough to NACK the deadlock checklist item even though the parity tests are green.

R12.2 NACK — fix items 1, 5 first
