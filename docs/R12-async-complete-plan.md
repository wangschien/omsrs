# R12 — AsyncBroker completion + crates.io publish

Date: 2026-04-22
Status: PROPOSED — awaiting codex audit before kickoff.

## Context

R11.1-R11.3 (2026-04-21) added `AsyncBroker` trait + `AsyncPaper`
implementation + async parity harness. `AsyncBroker` became an
additive trait next to the sync `Broker`; neither replaces nor
breaks the other.

Current gap: **only `AsyncPaper` implements `AsyncBroker`**. The
rich omspy-parity types — `VirtualBroker`, `ReplicaBroker`,
`CompoundOrder`, `OrderStrategy` — are sync-only. Consumers that
want those semantics against an async venue client must
block-on the sync trait, which was the exact problem R11
motivated getting rid of for direct broker-trait calls.

R12 closes that gap + ships the crate to crates.io so downstream
projects (pbot, future bots) can depend on `omsrs = "0.3"` by
semver instead of path.

## Scope

Five sub-phases, sequentially gated. Each concludes with a codex
audit prompt + ACK before the next starts. Same cadence pbot
used for R1-R10.

### R12.1 — `AsyncVirtualBroker`

**New file**: `src/async_virtual_broker.rs`.

API surface mirrors `VirtualBroker`:
- deterministic fill simulation with injected `Clock`
- queue advance + `BrokerReply` stream
- `submit_with_fill_ratio`, `cancel_pending`, `observe_quote`
- `impl AsyncBroker` delegating into simulation state

Implementation approach:
- Keep sync `Clock` trait (time advance is process-local and
  needs no I/O), wrap state in `tokio::sync::Mutex` so
  `async fn` methods can mutate under await without blocking
  other tasks on a sync `parking_lot::Mutex`.
- `BrokerReply` stream exposed via
  `tokio::sync::mpsc::UnboundedReceiver<BrokerReply>` instead of
  the sync `std::sync::mpsc`. Callers can wrap in
  `tokio_stream::UnboundedReceiverStream` for Stream ergonomics.
- All stochastic state (fill RNG) keyed off the same seed path
  as sync `VirtualBroker`, so golden parity fixtures from R6 /
  R7 replay bit-for-bit.

Tests:
- Mirror every sync `VirtualBroker` parity test under
  `tests/parity_async/virtual_*.rs` (`#[tokio::test]`, same
  seed, same assertions).
- Optional: add a `stress_concurrent_submit` test that sync
  `VirtualBroker` can't express — proves the async path doesn't
  serialize calls worse than sync.

Acceptance: async parity harness green; sync parity harness
unchanged.

### R12.2 — `AsyncReplicaBroker`

**New file**: `src/async_replica_broker.rs`.

API surface mirrors `ReplicaBroker`:
- `primary: Arc<dyn AsyncBroker>`, `replica: Arc<dyn AsyncBroker>`
- every `async fn` forwards to primary; on success, mirrors the
  observed fill/cancel onto replica
- primary error path: replica is NOT mutated (Python parity)

Implementation notes:
- `ReplicaFill` struct carries the mirrored delta; same shape
  as sync.
- Careful with `async fn order_place` — when primary returns
  `Some(oid)`, we issue `replica.order_place(args)` too and
  record the replica's oid (or warn on mismatch).

Tests:
- Every sync `ReplicaBroker` test has an async mirror.
- Added: primary-success + replica-timeout test (exercises
  async-specific failure mode).

### R12.3 — `AsyncCompoundOrder` + `AsyncOrderStrategy`

**New files**: `src/async_compound_order.rs`,
`src/async_order_strategy.rs`.

These two types are order-group state machines; they consume
a broker trait object rather than owning simulation state.
Porting is mostly mechanical — swap `&impl Broker` for
`&(dyn AsyncBroker + Send + Sync)`, add `async` to methods that
call broker methods, propagate `.await`.

Tests:
- Mirror sync parity suite.
- CompoundOrder: OCO / bracket / scale-in all expressible in
  async.
- OrderStrategy: ladder + grid builders fire correct order
  sequence.

### R12.4 — Publish preparation

**Docs hygiene**
- Fix `rustdoc::broken_intra_doc_links`: one broken link to
  `sqlite` surfaced by `cargo doc --lib -D warnings`. Locate,
  rewrite as external link or code-literal.
- Every public trait / struct gets a crate-level doc example
  that `cargo test --doc` compiles.

**README.md**
- crates.io / docs.rs / CI / license badges at the top.
- 10-line `Paper` quickstart + 10-line `AsyncPaper` quickstart.
- Feature flag table (`persistence`, `statistical-tests`).
- omspy→omsrs type mapping table (pulled from
  `omspy-source-notes.md`).

**CHANGELOG.md** (new file)
- Keep a Changelog format.
- Entry for `0.3.0`: all AsyncVirtual / AsyncReplica /
  AsyncCompound / AsyncOrderStrategy additions.

**GitHub Actions** — two workflows, new `.github/workflows/`:
- `ci.yml`: `cargo build --all-features`, `cargo test
  --all-features`, `cargo clippy -- -D warnings`, `cargo fmt
  --check`, `cargo doc --no-deps --lib -D warnings`. Matrix:
  stable + MSRV (1.78).
- `release.yml`: on tag push `v*`, `cargo publish` using
  `CRATES_IO_TOKEN` secret.

**`cargo publish --dry-run --all-features`** clean.

### R12.5 — Publish + pbot migration

1. Bump omsrs version `0.2.0 → 0.3.0` (new public API = semver
   minor). Tag `v0.3.0`, push tags, `cargo publish`. Verify
   docs.rs builds.
2. Check `polymarket-kernel` (bs-p crate) is publish-ready;
   separate publish track but coordinated so pbot can cut both
   path deps in one commit.
3. In pbot: flip `omsrs = { path = "../omsrs" }` → `omsrs =
   "0.3"` and likewise for polymarket-kernel. Run 305-test
   suite to confirm nothing regresses across the boundary.
4. Single pbot commit: `pbot: switch omsrs + polymarket-kernel
   to crates.io`.

## Explicit non-goals

- **Async persistence** — no async `rusqlite` / `sqlx`; persistence
  is sync-only. If future consumers need async writes, they
  wrap the sync layer in `spawn_blocking`.
- **`AsyncClock`** — the sync `Clock` trait is process-local + I/O
  free; adding an async variant buys nothing.
- **ReplicaBroker multi-venue fan-out (N replicas)** — current
  shape is 1 primary + 1 replica; generalizing to N is a
  separate future phase.
- **pbot being published** — R12 only unblocks pbot to *depend
  on crates.io omsrs*. Publishing pbot itself is a separate
  phase (needs its own docs/CI/license review).

## Risk + mitigation

| Risk | Mitigation |
| --- | --- |
| `async_trait` boxing overhead in hot paths | CompoundOrder / Strategy are state-machine wrappers, not hot loops. Real hot loop (pbot EventLoop) never goes through `AsyncCompoundOrder`. |
| AsyncVirtualBroker semantics diverge from sync VirtualBroker under concurrent calls | Internal state under single `tokio::sync::Mutex` serializes mutations. Stress test proves determinism with same seed. |
| crates.io namespace `omsrs` already taken | Check before publishing; fallback name `omsrs-core` reserved. |
| polymarket-kernel publish requires AVX-512 detection to not fail-hard on non-AVX-512 CI | `build.rs` already uses runtime detection; verify on CI matrix with a non-AVX-512 VM before publish. |

## Timing

Rough estimate, serial:
- R12.1: 1.5 days
- R12.2: 1 day
- R12.3: 1 day
- R12.4: 1 day
- R12.5: 0.5 day
- Each audit round adds ~0.5 day
- **Total: ~6-8 days**

Can run in parallel with pbot R11 (Rust L2 minimal A-S) since
the two touch non-overlapping files. R11 is blocked on Python
codex; R12 is not blocked on anything.

## Open questions for audit

1. Is the `tokio::sync::Mutex` + `UnboundedReceiver<BrokerReply>`
   choice the right async primitive for `AsyncVirtualBroker`, or
   should we use `async_channel` / `flume` for runtime-agnosticism?
   (Impact: tokio dependency leaks from dev-deps into a
   behind-feature-gate prod dep.)

2. PORT-PLAN.md §10 explicitly listed "tokio / any async runtime"
   as a non-goal ("omspy is fully synchronous; downstream
   consumers wrap Broker behind their own runtime"). R11.x
   already relaxed that by shipping `AsyncBroker` with
   `async_trait`. R12 extends the async coverage. Do we need
   to update PORT-PLAN.md §10 to reflect the new posture, or
   keep the historical record intact and document the divergence
   in CHANGELOG?

3. `AsyncCompoundOrder` / `AsyncOrderStrategy` take
   `&(dyn AsyncBroker + Send + Sync)` — is that the right
   ergonomics, or should they take `Arc<dyn AsyncBroker>` to
   match how pbot uses the trait? (The second form has a tiny
   clone cost but matches downstream usage.)

## Acceptance gate for R12 as a whole

Only "R12 complete" when all of:
- [ ] R12.1 codex ACK
- [ ] R12.2 codex ACK
- [ ] R12.3 codex ACK
- [ ] R12.4 codex ACK
- [ ] omsrs 0.3.0 on crates.io + docs.rs builds clean
- [ ] pbot migration commit lands + 305-test suite green
- [ ] polymarket-kernel on crates.io (coordinated track)

Until then, R12 is in-progress.
