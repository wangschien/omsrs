# codex audit — R12.3b AsyncCompoundOrder + AsyncOrderStrategy

## Context

R12.3b consumes R12.3a's async `Order` lifecycle. Last sub-phase
before R12.4 publish prep. R12.1, R12.2, R12.3a all ACKed.

Landed commit: `e9d092c`.

## What shipped

- `src/async_compound_order.rs` — new type mirroring sync
  `CompoundOrder`. Stores `Option<Arc<dyn AsyncBroker + Send +
  Sync>>`. ~400 LOC.
- `src/async_order_strategy.rs` — new type mirroring sync
  `OrderStrategy`. ~140 LOC.
- `tests/parity_async_compound.rs` — 10-item harness.
- `tests/parity_async_strategy.rs` — 5-item harness.
- `src/lib.rs` — `pub use` of `AsyncCompoundOrder` / `AsyncRunFn`
  / `AsyncOrderStrategy`.

Sync `CompoundOrder` / `OrderStrategy` (and every existing pub
method signature on them) unchanged.

## Design decision: duplication over generics

The sync and async types carry different broker trait objects
(`Arc<dyn Broker>` vs `Arc<dyn AsyncBroker + Send + Sync>`).
Unifying them would require either:
- trait-object casting between trait families (not possible), or
- generics over a broker trait bound (semver break — existing
  `CompoundOrder` signature would change)

R12 plan chose **per-type duplication**. ~350 LOC of pure-state
mirror — aggregate views (positions / buy_quantity /
average_price / mtm / net_value / update_ltp), add / add_order /
update_orders, index + keys lookup. Maintenance contract: any
semantic change to sync pure-state methods must land in async
same commit.

## Contract

1. **Semver guard**: sync `CompoundOrder::execute_all`, `:check_
   flags`, `OrderStrategy::run` all have unchanged signatures.
2. **Async broker interaction**: two methods on
   `AsyncCompoundOrder` — `execute_all_async` and
   `check_flags_async`. Both fan out to R12.3a's
   `Order::execute_async` / `modify_async` / `cancel_async`.
3. **`run_fn` stays sync** — closure signature is `Fn(&mut
   AsyncCompoundOrder, &HashMap<String, f64>) + Send + Sync`,
   same pattern as sync but with the async compound type (R12
   plan open Q #1 resolution).
4. **`save()` stays sync** — per persistence caveat
   (spawn_blocking at caller boundary if persistence is on the
   async path).
5. **Arc<dyn> stored + &dyn params** — plan open Q #3. Strategy
   / CompoundOrder store `Arc<dyn AsyncBroker + Send + Sync>`;
   Order methods take `&(dyn AsyncBroker + Send + Sync)` via
   `broker.as_ref()`.

## Audit scope

### 1. Pure-state parity (duplication correctness)
Each pure-state method in `AsyncCompoundOrder` should be
line-for-line equivalent to sync (except for type changes:
`Vec<Order>` → same, `Option<Arc<dyn Broker>>` → `Option<Arc<dyn
AsyncBroker + Send + Sync>>`). Items to spot-check:
- `positions()` — sum by symbol with sell sign flip
- `buy_quantity` / `sell_quantity` / `average_price` —
  side-filtered aggregates
- `mtm` — net_value sign inversion + ltp multiplication
- `add_order` / `add` — parent_id + index + keys collision
  handling + save_to_db inline
- `update_orders` — pending-only iteration + `order.update(data)`

Similarly for `AsyncOrderStrategy`:
- `positions()` / `mtm()` / `total_mtm()` aggregate across
  compounds
- `add()` cascades clock + inherits broker

### 2. Broker-interacting async methods
- `execute_all_async`: precedence of `order_args` + caller
  `kwargs` (caller wins). Inside each iteration, sync would
  call `order.execute(broker.as_ref(), None, merged)`; async
  calls `order.execute_async(broker.as_ref(), None, merged).
  await`. Verify the signatures line up.
- `check_flags_async`: for pending-and-expired orders, either
  converts to MARKET + `modify_async` or cancels via
  `cancel_async`. The MARKET conversion mutates
  `order.order_type` / `price` / `trigger_price` before calling
  `modify_async` — matches sync.

### 3. Test coverage
- 10 compound + 5 strategy = 15 items. Full sync parity is 40
  + 7 = 47 — we sample. Are the sampled items enough to catch
  realistic regressions (arg precedence / expiry / cascade)?
- AsyncMockBroker duplicated in each test file (~60 LOC).
  Acceptable trade-off for Rust integration test structure.

### 4. Semver guard
- Spot-check `src/compound_order.rs` and `src/order_strategy.rs`
  are unchanged (no pub method signature altered).
- `src/lib.rs` only adds new `pub mod` / `pub use`.

### 5. Lock / await discipline (vs R12.1/R12.2 pattern)
- `AsyncCompoundOrder` doesn't hold any locks internally — no
  inner Mutex. It wraps `Vec<Order>` directly with `&mut self`
  on mutating methods. So there's no deadlock scenario to worry
  about (distinct from R12.1/R12.2 which had interior mutability
  by necessity). Confirm.
- `execute_all_async` iterates `&mut self.orders` + calls
  `order.execute_async(broker.as_ref(), ...)` — the `order` is
  borrowed mutably across the await; no conflicting shared-state
  access.

## Out of scope
- R12.4 publish prep.
- Async persistence (R13).

## Output

`docs/audit-R12.3b-codex-result.md`. 5-item checklist
(PASS/CONCERN/FAIL + rationale). Final verdict:
- `R12.3b ACK — proceed to R12.4`, or
- `R12.3b NACK — fix items X, Y, Z first`.

Per `feedback_codex_audit_judgment`: plan author assesses each
NACK on merit. Short + technical; line-cite specifics.
