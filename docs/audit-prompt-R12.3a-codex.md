# codex audit — R12.3a async Order lifecycle

## Context

R12.3a adds three `async` sibling methods to `Order`:
`execute_async`, `modify_async`, `cancel_async`. These are
**prerequisites** for R12.3b (AsyncCompoundOrder +
AsyncOrderStrategy), which consumes them.

Sync counterparts — `Order::execute` (`src/order.rs:559`),
`modify` (`:619`), `cancel` (`:775`) — are **unchanged**.
This is a hard invariant per the R12 plan's semver guard.

Landed commit: `6925991`.

## What shipped

- `src/order.rs` — new `impl Order { execute_async / modify_async
  / cancel_async }` block after the existing sync impl block
  (starts line ~951, ends ~1130). ~175 lines of additive code.
- `tests/parity_async_order.rs` — 10-item harness, including
  a local `AsyncMockBroker` (mirror of
  `tests/parity/mock_broker.rs`).

## Contract (locked here)

1. **Semver guard**: sync `execute` / `modify` / `cancel`
   signatures untouched. Confirmed by test #10
   (`sync_execute_still_works_after_r12_3a`) which type-checks
   calls through `&dyn Broker`.

2. **Structural parity** with sync:
   - Same early-return gates (`is_complete() ||
     order_id.is_some()` for execute; `lock.can_*` + counters
     for modify/cancel).
   - Same kwarg precedence (default keys win over caller
     kwargs on execute).
   - Same `attribs_to_copy` merge (union of broker + caller).
   - Same `num_modifications` bookkeeping.
   - Same `decimal_value` / `set_local_field` / `frozen_attrs`
     helpers — reused, not duplicated.

3. **`.await` points**:
   - `broker.attribs_to_copy_<phase>().await` — called once
     per method before building `order_args`.
   - `broker.order_place(order_args).await` (execute),
     `broker.order_modify(order_args).await` (modify),
     `broker.order_cancel(args).await` (cancel).
   - No other awaits. No `.await` while holding any lock
     (there are no broker-side locks in scope — `AsyncBroker`
     trait methods are self-managed).

4. **Persistence caveat** (documented non-goal per R12 plan):
   `save_to_db()` is called synchronously at the same points
   as sync. Blocks the async runtime if `persistence` feature
   is on. pbot doesn't use persistence; future R13 scope for
   async persistence.

## Audit scope

### 1. Sync parity body-for-body
Compare `execute_async` vs sync `execute`:
- Early-return gate (sync `:565-567` vs async)
- Default-keys HashSet contents match (7 keys: symbol, side,
  order_type, quantity, price, trigger_price, disclosed_quantity)
- Kwarg filtering: caller kwargs skip default keys
- Precedence: other_args (from attribs_to_copy) < default_keys
  < caller kwargs excluding default_keys.
- `save_to_db()` called at the same point (after
  `self.order_id = ret.clone()`)

Same for `modify_async` vs `modify` and `cancel_async` vs
`cancel`.

### 2. Broker trait plumbing
- `broker: &(dyn AsyncBroker + Send + Sync)` vs sync's
  `&dyn Broker`. Is `Send + Sync` tight on the dyn trait
  object, or could we just take `&dyn AsyncBroker`? (Sync
  version is just `&dyn Broker` without bounds.) Note: the
  R12 plan specified `&(dyn AsyncBroker + Send + Sync)` for
  method parameters — intentional because `async fn` futures
  auto-inherit `Send` from their captures, and the dyn
  trait's Send bound propagates to the future.
- `attribs_to_copy_<phase>().await` happens BEFORE any local
  state mutation. Correct — matches sync order.

### 3. Test coverage (10 items)
- 8 mirror sync test_simple_order_{execute, execute_kwargs,
  execute_do_not_update_existing_kwargs, do_not_execute_more
  _than_once, do_not_execute_completed_order, modify, cancel,
  cancel_none}
- 1 async-specific: `execute_merges_broker_async_attribs_to
  _copy` — exercises the `await` on `broker.attribs_to_copy_
  execute().await`, which sync tests can't.
- 1 semver guard: `sync_execute_still_works_after_r12_3a` —
  compile-time check that sync methods through `&dyn Broker`
  still type-check.

Missing-by-design (intentional): no mirror of sync tests that
drive `execute` through the Order's modify/cancel lock paths
because those aren't distinct from modify_async / cancel_async
(no separate lock logic added).

### 4. AsyncMockBroker correctness
Inspect `tests/parity_async_order.rs` AsyncMockBroker:
- Does it correctly drain `place_returns` front-to-back
  (same semantics as sync MockBroker's `set_place_side_effect`)?
- Does it record every call?
- Does `attribs_to_copy_<phase>()` return the configured
  Option, not always None (unlike the default trait impl)?

### 5. Persistence caveat documentation
`save_to_db()` is still called inline in async methods. R12
plan accepts this as a documented non-goal. Verify:
- The doc comment on the `impl` block explicitly calls this
  out
- Pbot does not enable `persistence` feature (check
  `~/pbot/Cargo.toml` — out of scope but a sanity data point)

## Out of scope

- R12.3b (AsyncCompoundOrder + AsyncOrderStrategy).
- Async persistence (R13).

## Output

`docs/audit-R12.3a-codex-result.md`. 5-item checklist
(PASS/CONCERN/FAIL + rationale). Final verdict:
- `R12.3a ACK — proceed to R12.3b`, or
- `R12.3a NACK — fix items X, Y, Z first`.

Per `feedback_codex_audit_judgment`: plan author assesses
each NACK on merit. Short + technical; line-cite specifics.
