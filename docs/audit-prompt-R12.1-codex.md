# codex audit — R12.1 `AsyncVirtualBroker`

## Context

First sub-phase of R12 (plan: `docs/R12-async-complete-plan.md`
v3 ACK). `AsyncVirtualBroker` is the async port of sync
`VirtualBroker`. All subsequent R12 phases build on the
contract this phase establishes.

Landed commit: `249faf5` on main.

## What shipped

- `src/async_virtual_broker.rs` — new file, async port
- `tests/parity_async_virtual.rs` — 18-item parity harness
- `src/lib.rs` — `pub use async_virtual_broker::AsyncVirtualBroker`
- `src/persistence.rs` — opportunistic fix of the pre-existing
  `rustdoc::broken_intra_doc_links` warning (originally
  scheduled for R12.4; one-line fix done here since it
  unblocks `cargo doc --no-deps --lib` without
  `--all-features`)

## Contract (locked here, enforced by later R12 phases)

1. **Locking pattern**: single `parking_lot::Mutex<Inner>`, no
   `await` while locked. Matches existing `AsyncPaper`
   pattern (`src/brokers.rs` AsyncPaper body). Keeps `tokio`
   out of production deps (confirm it's still only in
   `[dev-dependencies]`).

2. **Clock**: `Arc<dyn Clock + Send + Sync>` lives outside the
   mutex. `clock.now()` read before lock acquisition so the
   clock's internal locking never nests with ours.

3. **RNG**: `SmallRng` lives inside Inner (same module as the
   state it feeds). Seeds match sync `VirtualBroker::with_clock
   _and_seed`.

4. **BrokerReply preservation**: inherent `place`/`modify`/
   `cancel` return the full `BrokerReply`. `impl AsyncBroker`
   adapts lossily to `Option<String>` / `()`. Callers pick.

5. **API divergence from sync** (intentional, documented):
   - `orders_mut()` not exposed (async can't safely hand out
     `&mut`)
   - Accessors (`orders()`, `clients()`, `clock()`) return
     owned clones

6. **`order_id`-via-kwarg convention**: inherent `modify` and
   `cancel` read `order_id` from `args["order_id"]` rather
   than taking it as a separate `&str` parameter (sync does
   the latter). This matches the `AsyncBroker` trait
   signature shape and makes the inherent + trait paths
   consistent.

## Audit scope

### 1. Locking + concurrency
- No `.await` inside any `self.inner.lock()` scope. Walk
  `place` / `modify` / `cancel` line by line.
- `parking_lot::Mutex` choice vs `std::sync::Mutex`: fine
  (parking_lot is cheaper + already a dep), but check nothing
  in the new code accidentally uses `std::sync::Mutex` and
  assumes poisoning semantics.
- RNG inside Inner: confirms seed parity. Is the
  `failure_rate` read under the lock alongside the RNG roll?
  (It should be — otherwise a concurrent `set_failure_rate`
  could leak a stale value into the comparison.)

### 2. Sync parity
- Inherent `place` body should mirror sync `order_place`
  behavior **including the failure-roll-before-validation
  order** (sync rolls the RNG even on validation failure —
  my implementation preserves that order so reply sequences
  match bit-for-bit). Verify.
- Validation error messages ("Found N validation errors; in
  field X Field required") match byte-for-byte.
- `userid` uppercase normalization preserved.
- `delay_us` fallback chain preserved (kwarg → Inner's
  default → `DEFAULT_DELAY_US`).

### 3. Test coverage vs actual surface
The 18 tests cover: defaults, failure-RNG sequence, place
success + field round-trip, place failure, passthrough,
validation errors, get_default, modify (success / failure /
passthrough), cancel (success / passthrough), add_user dedup,
place-with-userid attach, ticker accessors, AsyncBroker trait
adapter, and the **sync↔async reply-sequence parity** hash
compare. Is anything from sync
`tests/parity/test_virtual_broker.rs` missing that shouldn't
be?

Known-not-mirrored (intentional): `orders_mut()`-requiring
tests since we don't expose it; the exact cancel-failure
test that pokes `orders_mut` to manually set
`filled_quantity=50`.

### 4. Semver guard
- No existing sync method signature was changed. Confirm by
  spot-checking `src/virtual_broker.rs` remains untouched.
- `src/lib.rs` only added a new `pub use`; no existing
  exports removed or renamed.

### 5. Opportunistic persistence.rs fix
- The rewrite of the broken `[sqlite]` intra-doc link to
  plain text — does the new phrasing accurately describe
  what exists at runtime? Is there a better form (e.g.
  `[`sqlite`]` with `\\[` escaping, or
  `[crate::persistence::sqlite]` — but the latter still
  requires the feature)?

## Out of scope

- Anything in R12.2-R12.5.
- `cargo publish` dry-run (deferred to R12.4).

## Output

`docs/audit-R12.1-codex-result.md`. 5-item checklist
(PASS/CONCERN/FAIL + rationale). Final verdict:
- `R12.1 ACK — proceed to R12.2`, or
- `R12.1 NACK — fix items X, Y, Z first`.

Per `feedback_codex_audit_judgment`: the plan author will
assess every NACK item on technical merit. If an item is
debatable, be explicit about your reasoning so the response
can be specific. Short and technical beats long and hedged.
