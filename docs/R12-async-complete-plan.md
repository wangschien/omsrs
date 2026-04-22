# R12 — AsyncBroker completion + crates.io publish

Date: 2026-04-22 (v2 after codex plan-audit NACK)
Status: PROPOSED — awaiting codex v2 audit before kickoff.

## Revision history

- **v1 (2026-04-22 initial)**: NACKed by codex plan audit
  (`docs/audit-R12-plan-codex-result.md`) for factual errors
  about the current codebase.
- **v3 (2026-04-22 this doc)**: v2 audit returned narrow NACK
  on two CI-coverage / publish-hygiene items. Added:
  1. `--all-targets` to clippy, `--all-features` to `cargo doc`
     in §R12.4 CI block
  2. Explicit "clean registry consumer check" as §R12.5 step 3,
     before pbot migration. Risk table + acceptance gate
     updated.
  No technical disagreement with v2 audit — both items were
  real gaps, fixed as requested.
- **v2 (2026-04-22)**: addresses every codex round-1 concern.
  Key deltas from v1:
  1. R12.2 is now "async port of the standalone
     `ReplicaBroker` matching engine" — not a primary/replica
     wrapper (my v1 read was wrong; `ReplicaBroker` is a
     matching engine with `instruments`/`orders`/`pending`/
     `completed`/`fills`, not a mirror wrapper).
  2. R12.3 split into **R12.3a AsyncOrder lifecycle** (must
     come first) + **R12.3b AsyncCompoundOrder +
     AsyncOrderStrategy** (depends on R12.3a). `CompoundOrder`
     delegates to `Order::execute/modify/cancel`, which take
     `&dyn Broker`. No async compound without async order.
  3. `AsyncVirtualBroker` keeps `BrokerReply` via **inherent
     methods** (`async fn place`/`modify`/`cancel` returning
     `BrokerReply`) while the `impl AsyncBroker` provides a
     lossy `Option<String>` adapter for trait-object use. No
     information loss for parity tests.
  4. **No `tokio` in prod deps.** Use `parking_lot::Mutex` with
     no await-while-locked (mirrors existing `AsyncPaper`
     pattern). `tokio` stays dev-only.
  5. No event stream (`mpsc::UnboundedReceiver`) — sync
     `VirtualBroker` returns one `BrokerReply` per call; async
     mirror does the same. Deleted the underspecified stream
     contract.
  6. R12.5: `polymarket-kernel` publish is an **explicit hard
     predecessor** to pbot migration, not a "coordinated track."
  7. CI doc command fixed: `RUSTDOCFLAGS="-D warnings" cargo
     doc --no-deps --lib`.
  8. `lib.rs` `pub use` export is a per-sub-phase acceptance
     item.
  9. Semver `0.3.0` kept as milestone marker but documented as
     a **policy choice, not a semver requirement** (0.2.1 would
     also be crates.io-compatible per pre-1.0 caret rules).
  10. crates.io name availability check moved to **before**
      R12.4 docs/README work.
  11. Test names corrected to real parity surface (`execute_all`,
      `check_flags`, aggregate views, `run_fn`, `add` — not the
      invented OCO/bracket/ladder/grid).
  12. `save_to_db` inside async Order methods — documented
      caveat: wrap with `spawn_blocking` at caller boundary if
      persistence is on the async path. pbot doesn't use
      persistence so this is non-blocking.

## Context

R11.1-R11.3 (2026-04-21) added `AsyncBroker` trait + `AsyncPaper`
implementation + async parity harness. `AsyncBroker` is additive
next to sync `Broker`; neither replaces nor breaks the other.

Current gap: **only `AsyncPaper` implements `AsyncBroker`**. The
rich omspy-parity types — `Order` lifecycle, `VirtualBroker`,
`ReplicaBroker`, `CompoundOrder`, `OrderStrategy` — are
sync-only. Consumers that want those semantics against an async
venue client must block-on the sync trait, which was the exact
problem R11 motivated getting rid of for direct broker-trait
calls.

R12 closes that gap + ships the crate to crates.io so downstream
projects (pbot, future bots) can depend on `omsrs = "0.3"` by
semver instead of path.

## Hard constraint (applies to every sub-phase)

**No in-place replacement of sync methods with async.** Changing
`Order::execute` / `CompoundOrder::execute_all` / any currently-
pub sync method from sync to async would be a semver break for
every downstream (including pbot's 305-test suite). Every async
addition is a **new** method or **new** type:

```text
Order::execute            (sync, keep as-is)
Order::execute_async      (new, R12.3a)

CompoundOrder             (sync, keep as-is)
AsyncCompoundOrder        (new type, R12.3b)
```

Any commit that touches an existing pub sync method's signature
is rejected at the sub-phase gate.

## Scope

Six sub-phases, sequentially gated. Each concludes with a codex
audit prompt + ACK before the next starts. pbot R1-R10 cadence.

### R12.1 — `AsyncVirtualBroker` (matching engine)

**New file**: `src/async_virtual_broker.rs`.

API mirrors sync `VirtualBroker` (`src/virtual_broker.rs:136-401`).
Public methods, named to match sync:

```text
new() -> Self
with_clock(clock: Arc<dyn Clock + Send + Sync>) -> Self
with_clock_and_seed(clock, seed: u64) -> Self
with_tickers(tickers: HashMap<String, Ticker>) -> Self
failure_rate(&self) -> f64
set_failure_rate(&mut self, v: f64) -> Result<(), VirtualBrokerError>
is_failure(&self) -> bool
orders(&self) -> HashMap<String, VOrder>         (returns clone, no & with lifetime)
clients(&self) -> HashSet<String>                (returns clone)
clock(&self) -> Arc<dyn Clock + Send + Sync>     (returns clone)
get(&self, order_id: &str, status: Status) -> Option<VOrder>
get_default(&self, order_id: &str) -> Option<VOrder>
add_user(&self, user: VUser) -> bool

// BrokerReply-preserving inherent methods:
async fn place(&self, args: HashMap<String, Value>) -> BrokerReply
async fn modify(&self, args: HashMap<String, Value>) -> BrokerReply
async fn cancel(&self, args: HashMap<String, Value>) -> BrokerReply

update_tickers(&self, last_price: &HashMap<String, f64>)
ltp(&self, symbol: &str) -> Option<HashMap<String, f64>>
ltp_many(&self, symbols: &[&str]) -> HashMap<String, f64>
ohlc(&self, symbol: &str) -> Option<HashMap<String, OHLC>>
```

`impl AsyncBroker for AsyncVirtualBroker` provides the trait
surface with lossy adaptation:

```rust
async fn order_place(&self, args) -> Option<String> {
    match self.place(args).await {
        BrokerReply::Order(resp) if resp.success => Some(resp.order_id.clone()),
        _ => None,
    }
}
async fn order_modify(&self, args) { let _ = self.modify(args).await; }
async fn order_cancel(&self, args) { let _ = self.cancel(args).await; }
```

Callers that want the full `BrokerReply` call the inherent
`place`/`modify`/`cancel`. Callers that want dyn-dispatch go
through the trait and accept the lossy surface. This matches
how the SDK and pbot naturally want to use it.

**Locking**: interior mutability via `parking_lot::Mutex<Inner>`.
No await-while-locked. Pattern matches `AsyncPaper` (see
`src/brokers.rs:110-116`, `185-224` for reference). No `tokio`
dependency added to `[dependencies]`.

**Clock**: sync `Clock` trait unchanged.

**RNG**: same `SmallRng` seed path as sync `VirtualBroker` so
golden parity fixtures replay bit-for-bit.

**Tests** (`tests/parity_async/virtual_broker.rs`):
- Mirror of sync `tests/parity/test_virtual_broker.rs` (already
  covers `order_place`, `order_modify`, `order_cancel`, ticker
  updates, `ltp`, failure-rate paths).
- Every assertion that inspects `BrokerReply` in sync tests is
  preserved — the async test calls `.place()` (inherent) and
  asserts on the `BrokerReply` variants, not just the
  `Option<String>` adapter.
- Bit-for-bit parity requires same clock + same seed; verified
  by hash-comparing reply sequences.

**`src/lib.rs` export**: add `pub use async_virtual_broker::
AsyncVirtualBroker;` (also any new error types).

**Acceptance**:
- async parity harness green
- sync parity harness unchanged
- `cargo clippy --all-features -- -D warnings` clean
- `cargo doc --no-deps --lib` clean (with `RUSTDOCFLAGS="-D warnings"`)

### R12.2 — `AsyncReplicaBroker` (matching engine port)

**New file**: `src/async_replica_broker.rs`.

This is a **pure async port of the standalone matching engine**,
not a primary/replica wrapper. (My v1 read was wrong. Sync
`ReplicaBroker` at `src/replica_broker.rs:28-37` is a matching
engine with `instruments` / `orders` / `pending` / `completed` /
`fills` / `user_orders`; see `tests/parity/test_replica_broker.rs
:126-132` which asserts `Arc::ptr_eq` across collections.)

API mirrors sync (`src/replica_broker.rs:40-200+`):

```text
new() -> Self
update(&self, instruments: Vec<Instrument>)

// Inherent, returning OrderHandle (same shape as sync):
async fn place(&self, args: HashMap<String, Value>) -> OrderHandle
async fn modify(&self, args: HashMap<String, Value>) -> Option<OrderHandle>
async fn cancel(&self, order_id: &str) -> Option<OrderHandle>

async fn run_fill(&self)
```

`impl AsyncBroker` adapts the `OrderHandle` returns to the
`Option<String>` trait contract (lossy path), same pattern as
R12.1.

**Shared-identity preservation**: sync ReplicaBroker's tests
pin `Arc::ptr_eq` across `orders`/`pending`/`completed`/`fills`
collections. The async port must preserve this — the matching
engine's internal HashMap stores are `Arc<OrderHandle>` shared
between collections. `parking_lot::Mutex<Inner>` with Inner
holding the HashMaps keeps this intact.

**Tests** (`tests/parity_async/replica_broker.rs`):
- Full mirror of sync parity including the `Arc::ptr_eq`
  checks.
- All existing reason codes / fill states preserved.

**`src/lib.rs` export**: `pub use async_replica_broker::
AsyncReplicaBroker;`.

### R12.3a — Async Order lifecycle

**Prerequisite for R12.3b.** `CompoundOrder::execute_all` calls
`Order::execute(&dyn Broker, ...)` (`src/order.rs:559-615`),
`Order::modify(&dyn Broker, ...)` (`src/order.rs:619-689`),
`Order::cancel(&dyn Broker, ...)` (`src/order.rs:775-790`). Those
sync methods are **kept as-is** (no break); new async siblings
added:

```rust
impl Order {
    // Existing: execute(&mut self, broker: &dyn Broker, ...) -> String
    pub async fn execute_async(
        &mut self,
        broker: &(dyn AsyncBroker + Send + Sync),
        ..., // same attribs_to_copy params
    ) -> String;

    // Existing: modify(&mut self, broker: &dyn Broker, ...)
    pub async fn modify_async(
        &mut self,
        broker: &(dyn AsyncBroker + Send + Sync),
        ...,
    );

    // Existing: cancel(&mut self, broker: &dyn Broker, ...)
    pub async fn cancel_async(
        &mut self,
        broker: &(dyn AsyncBroker + Send + Sync),
        attribs_to_copy: Option<&[&str]>,
    );
}
```

**Persistence caveat**: existing `Order::execute` calls
`save_to_db(...)` (`src/order.rs:611-613`) which is sync
`rusqlite`. The async variants preserve this call. Persistence
is feature-gated and pbot does not use it, so the "sync save
inside async method" concern is documented as a caveat — callers
that enable `persistence = true` **and** drive the async path
should be aware the I/O blocks the runtime; wrap the async call
in `tokio::task::spawn_blocking` at the caller boundary if that
matters. Changing `save_to_db` to async is a larger change (R13
persistence overhaul) and not in R12 scope.

**Tests** (`tests/parity_async/order.rs`):
- For each sync test that drives `execute` / `modify` / `cancel`,
  add an async sibling that asserts identical state transitions.

**Acceptance**: `Order::execute` / `Order::modify` / `Order::cancel`
signatures **unchanged** (semver guard). New `_async` methods
only.

### R12.3b — `AsyncCompoundOrder` + `AsyncOrderStrategy`

**New files**: `src/async_compound_order.rs`,
`src/async_order_strategy.rs`.

Per R12.3a, `Order::_async` methods exist. Now wrap them in new
`Async*` types.

`AsyncCompoundOrder` stores `broker: Option<Arc<dyn AsyncBroker
+ Send + Sync>>` (not `&dyn`; storage needs owned handle —
confirmed by codex open-question #3). Builder/query methods
mirror sync surface (`src/compound_order.rs:~20 pub fns`:
`new`, `with_clock`, `with_id`, `with_orders`, `count`, `len`,
`is_empty`, `get_by_index`, `keys_map`, etc.). Execution method
`execute_all_async(&mut self)` iterates child `Order`s calling
`Order::execute_async` with `self.broker.as_ref()`.

`AsyncOrderStrategy`: same pattern. `run_async(&mut self, ltp:
&HashMap<String, f64>)` drives child `AsyncCompoundOrder`s. The
`run_fn` question from codex: current sync `run` uses a captured
sync closure (`src/order_strategy.rs:113-120`). The async
sibling accepts the same sync `Fn` closure — closures don't
need to be async if they don't do I/O. Keep it simple.

**Tests** (`tests/parity_async/compound_order.rs`,
`order_strategy.rs`):
Use the *actual* parity surface, not invented features:
- `execute_all_async` on a multi-child compound
- `check_flags` equivalent (or async sibling if check_flags
  transitions require broker calls)
- Aggregate views (`total_qty`, `remaining_qty`, `mtm`)
- `run_fn` closure callbacks fire
- `add` / `index_map` / `keys_map` behavior
- Ladder / bracket / OCO are NOT current omsrs parity
  surfaces — do not invent new features under the R12.3b
  banner.

**`src/lib.rs` exports**: `AsyncCompoundOrder`,
`AsyncOrderStrategy`, any new error types.

### R12.4 — Publish preparation

**Pre-step (gating)**: check crates.io name availability **before**
writing README badges or pbot migration copy:

```bash
cargo search omsrs
# verify owner / version; if squatted, fallback: omsrs-core.
# any fallback name change propagates to README + CHANGELOG +
# pbot Cargo.toml + this plan.
```

**Docs hygiene**
- Fix `rustdoc::broken_intra_doc_links` in
  `src/persistence.rs:1-7`: unconditional module docs link to
  `sqlite` which only exists under `#[cfg(feature =
  "persistence")]`. Rewrite as a plain-text reference or use
  the `[<crate>::<item>]` form that tolerates cfg-gated targets.
- Every public trait / struct / new `Async*` type gets a
  crate-level doc example that `cargo test --doc` compiles.

**README.md**
- crates.io / docs.rs / CI / license badges at the top (use
  final crate name from availability check).
- 10-line `Paper` quickstart + 10-line `AsyncPaper` quickstart
  + 15-line `AsyncVirtualBroker` quickstart.
- Feature flag table (`persistence`, `statistical-tests`).
- omspy → omsrs type mapping table (pulled from
  `docs/omspy-source-notes.md` §12).

**CHANGELOG.md** (new)
- Keep a Changelog format.
- `0.3.0` entry: AsyncOrder `_async` methods, AsyncVirtualBroker,
  AsyncReplicaBroker, AsyncCompoundOrder, AsyncOrderStrategy,
  PORT-PLAN.md §10 supersession (see below).

**Update `docs/PORT-PLAN.md`**
- §10 currently says "no tokio / any async runtime; downstream
  consumers wrap `Broker` behind their own runtime"
  (`docs/PORT-PLAN.md:317-318`). R11.x already shipped
  `AsyncBroker` + `async_trait`; R12 extends coverage. Add a
  v12 supersession note to §10 stating the current posture:
  "`async_trait` and `AsyncBroker` are part of the public
  surface; `tokio` remains dev-only (sync locks under
  async methods, no await-while-locked)."

**GitHub Actions** — two workflows, new `.github/workflows/`:
- `ci.yml`:
  ```yaml
  - run: cargo build --all-features
  - run: cargo test --all-features
  - run: cargo clippy --all-features --all-targets -- -D warnings
  - run: cargo fmt --check
  - run: cargo doc --no-deps --lib --all-features
    env:
      RUSTDOCFLAGS: "-D warnings"
  ```
  Flag rationale (v2 audit closeout):
  - `--all-targets` on clippy so test / bench / example
    warnings are caught alongside lib warnings (without it,
    `#[cfg(test)]` modules get a pass).
  - `--all-features` on `cargo doc` so feature-gated items
    (`persistence`) render; otherwise docs.rs could build
    fine and still miss links the published docs will show.
  Matrix: stable + MSRV (1.78).
- `release.yml`: on tag push `v*`, `cargo publish` using
  `CRATES_IO_TOKEN` secret.

**`cargo publish --dry-run --all-features`** clean. (Already
passes for the 0.2.0 code; verify still passes after R12.1-R12.3b
additions.)

### R12.5 — Publish + pbot migration

**Hard predecessor**: `polymarket-kernel` on crates.io.

1. **polymarket-kernel publish** (separate repo,
   `~/refs/bs-p/packages/crates/`):
   - Verify `build.rs` AVX-512 runtime detection handles the
     docs.rs builder (non-AVX-512 pool). Test: build on a VM /
     container with AVX-512 disabled via `-C target-feature=
     -avx512f`.
   - `cargo publish --dry-run` clean.
   - Tag + publish.
   - Verify docs.rs builds at
     `https://docs.rs/polymarket-kernel/<ver>/`.
   - Only after this step completes does pbot migration start.

2. **omsrs publish**:
   - Bump `0.2.0 → 0.3.0`. (Policy choice for milestone
     visibility; 0.2.1 would also be semver-legal per pre-1.0
     caret rules. 0.3.0 is chosen to signal a substantive
     additive block.)
   - Tag `v0.3.0`, push tags, CI release.yml fires
     `cargo publish`.
   - Verify docs.rs.

3. **Clean registry consumer check** (v2 audit closeout — hard
   predecessor to pbot migration):

   Publish-success on crates.io does not prove the
   published manifest is complete. A path dep can silently
   satisfy deps that the registry version omits (forgotten
   `pub use`, a feature gate wired only in the workspace
   `Cargo.toml`, an accidentally unpublished sibling crate).
   Catch this before touching pbot.

   Steps:
   - In a throwaway directory, `cargo new --bin registry-check
     && cd registry-check`.
   - `Cargo.toml` declares `omsrs = "0.3"` +
     `polymarket-kernel = "<pub-version>"` from the registry
     (no `[patch]`, no `path =`).
   - `src/main.rs` imports every top-level type pbot actually
     uses: `omsrs::{AsyncBroker, AsyncPaper, Paper}`,
     `polymarket_kernel::{calculate_quotes_logit, logit,
     sigmoid}` (list mirrors pbot's `use` statements).
   - `cargo build` must succeed. Any failure (missing export,
     missing feature, missing transitive dep) is a
     publish-regression that blocks the pbot commit — fix
     omsrs or polymarket-kernel in a patch release, then
     re-run this step.

4. **pbot migration** (depends on 1, 2, AND 3):
   - `Cargo.toml`:
     ```diff
     - omsrs = { path = "../omsrs" }
     - polymarket-kernel = { path = "../refs/bs-p/packages/crates", features = [...] }
     + omsrs = "0.3"
     + polymarket-kernel = { version = "<pub-version>", features = [...] }
     ```
   - `cargo test` — 305 tests green (prove no semantic drift
     across the crates.io boundary).
   - Single pbot commit: `pbot: switch omsrs + polymarket-kernel
     to crates.io`.

## Explicit non-goals

- **Async persistence**. Sync `rusqlite` stays. `save_to_db`
  inside async `Order` methods is a documented caveat, not a
  gate. pbot doesn't use persistence.
- **`AsyncClock`**. Sync `Clock` is process-local + I/O-free.
- **N-replica fan-out** for ReplicaBroker. R12.2 is a direct
  async port of the 1-engine shape.
- **pbot publish to crates.io**. R12 only unblocks pbot to
  *depend on* crates.io omsrs. Publishing pbot is a separate
  phase.
- **Changing sync methods to async in-place**. Explicitly
  forbidden (semver break). New async siblings only.

## Risk + mitigation

| Risk | Mitigation |
| --- | --- |
| `async_trait` boxing overhead in pbot's hot loop | pbot's event loop calls `broker.order_place(...)` directly on `Arc<dyn AsyncBroker>`; Compound/Strategy wrappers don't sit on the hot path. |
| AsyncVirtualBroker reply-sequence diverges from sync VirtualBroker | Same `SmallRng` seed, same `parking_lot::Mutex` call order, bit-for-bit hash comparison in parity test. |
| Shared-identity (`Arc::ptr_eq`) lost in AsyncReplicaBroker | `parking_lot::Mutex<Inner>` with Inner holding the shared HashMap<String, Arc<OrderHandle>>. Test pins `Arc::ptr_eq` exactly as sync does. |
| crates.io name `omsrs` squatted | Check **before** writing R12.4 docs / README badges / pbot Cargo.toml. If squatted, fallback to `omsrs-core`; name change propagates to all downstream docs. |
| `polymarket-kernel` docs.rs build fails on non-AVX-512 pool | Pre-test on a container with `target-feature=-avx512f`. Hard predecessor to pbot migration — if docs.rs fails, hold pbot. |
| Registry build succeeds but a consumer build fails (forgotten `pub use`, feature wired only in workspace) | Clean-registry consumer check (§R12.5 step 3) — throwaway crate that depends on `omsrs = "0.3"` + `polymarket-kernel = "<ver>"` from registry and imports every type pbot uses. Must `cargo build` clean before the pbot migration commit. |
| `save_to_db` blocks async runtime if `persistence` feature enabled | Documented caveat; pbot not affected. Future `spawn_blocking` wrapper is R13 scope. |

## Timing

Rough estimate, serial:
- R12.1: 1.5 days
- R12.2: 1.5 days (matching-engine port is more than wrapper)
- R12.3a: 1 day (new Order `_async` methods — small but
  touches many call sites)
- R12.3b: 1 day
- R12.4: 1 day (docs + CI + README + name check)
- R12.5: 0.5 day (publish + pbot flip)
- Each audit round adds ~0.5 day
- **Total: ~7-9 days**

Can run in parallel with pbot R11 (L2 minimal A-S) since R12
touches omsrs only, R11 touches pbot strategy. R11 is blocked on
Python codex; R12 is not blocked on anything.

## Resolved open questions (from codex v1 NACK)

1. **tokio vs runtime-agnostic** — **runtime-agnostic**. Use
   `parking_lot::Mutex` with no await-while-locked, matching
   existing `AsyncPaper`. `tokio` stays in dev-deps.
2. **PORT-PLAN.md §10 update** — **update in R12.4** with an
   explicit supersession note. CHANGELOG alone insufficient
   for a living port plan.
3. **`&dyn` vs `Arc<dyn>` ergonomics** — **stored fields
   `Arc<dyn AsyncBroker + Send + Sync>`** (matches sync shape
   + pbot usage); **method parameters `&(dyn AsyncBroker +
   Send + Sync)`** (no unnecessary clones for per-call helpers).

## Acceptance gate for R12 as a whole

"R12 complete" iff all of:
- [ ] R12.1 codex ACK
- [ ] R12.2 codex ACK
- [ ] R12.3a codex ACK
- [ ] R12.3b codex ACK
- [ ] R12.4 codex ACK
- [ ] polymarket-kernel on crates.io + docs.rs builds
- [ ] omsrs 0.3.0 on crates.io + docs.rs builds
- [ ] Clean-registry consumer check passes (`cargo build` in
      a throwaway crate using published `omsrs` +
      `polymarket-kernel`, importing every type pbot uses)
- [ ] pbot migration commit lands + 305-test suite green
- [ ] No pre-R12 sync method signature changed (semver guard)

Until then, R12 is in-progress.
