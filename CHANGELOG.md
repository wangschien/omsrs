# Changelog

All notable changes to `omsrs` are documented here. Format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and the project adheres to [Semantic Versioning](https://semver.org/).

## [0.3.0] — 2026-04-22

**R12 — async coverage completion.** Five sub-phases, each
codex-audited individually with ACK.

### Added

- `AsyncVirtualBroker` (R12.1) — async port of `VirtualBroker`.
  Inherent `async fn place` / `modify` / `cancel` return the
  full `BrokerReply`; `impl AsyncBroker` provides a lossy
  `Option<String>` adapter for trait-object use. Seed parity
  with sync `VirtualBroker` — same `SmallRng` seed produces
  bit-for-bit identical reply sequences.
- `AsyncReplicaBroker` (R12.2) — async port of the standalone
  `ReplicaBroker` matching engine (not a primary/replica
  wrapper — sync `ReplicaBroker` is a matching engine, not a
  mirror). Shared-identity contract via `Arc<Mutex<VOrder>>`
  preserved; `Arc::ptr_eq` across `orders()` / `pending()` /
  `completed()` / `fills()` / `user_orders()` returns true for
  the same order. Lock discipline: never hold inner while
  taking handle lock (ABBA deadlock prevention).
- `Order::execute_async` / `modify_async` / `cancel_async`
  (R12.3a) — async siblings of the sync lifecycle methods.
  Take `&(dyn AsyncBroker + Send + Sync)`; await the broker's
  `attribs_to_copy_<phase>()` and lifecycle calls. Sync
  signatures unchanged.
- `AsyncCompoundOrder` (R12.3b) — async port of `CompoundOrder`.
  Stores `Option<Arc<dyn AsyncBroker + Send + Sync>>`;
  `execute_all_async` + `check_flags_async` fan out to
  `Order::execute_async` etc.
- `AsyncOrderStrategy` (R12.3b) — async port of `OrderStrategy`.
  `run(ltp)` callback stays synchronous by design (closure
  body doesn't do I/O).

### Changed

- `ReplicaFill` now derives `Clone` (backwards-compatible
  widening — it holds an `Arc` and an `f64`). Needed for
  `AsyncReplicaBroker::fills()` owned-snapshot accessor.
- `VOrder::cloned_clone_weak` now preserves `delay` on the
  clone (previously reset to default). Backwards-compatible —
  no existing consumer inspects delay on the clone path.
- `tests/parity/persistence.rs` intra-doc link to `sqlite`
  rewritten as plain text (was a broken
  `rustdoc::broken_intra_doc_links` warning with
  `--all-features` disabled; now clean under both feature
  configurations).

### Dependency changes

- `tokio` dev-dep gains `time` + `rt` features (used only by
  the new deadlock regression test
  `external_handle_hold_does_not_deadlock_cancel_or_accessors`
  for `tokio::time::timeout`). `tokio` remains **dev-only**;
  no change to production surface.

### Non-goals (explicitly deferred to future phases)

- Async persistence — `save_to_db()` stays synchronous inside
  the async `Order::execute_async` / `modify_async` methods.
  Callers that enable the `persistence` feature on an async
  path should wrap with `tokio::task::spawn_blocking`. Full
  async persistence is R13 scope.
- `AsyncClock` — sync `Clock` is process-local, I/O-free, so
  no async variant is needed.
- N-replica fan-out for `AsyncReplicaBroker` — current shape
  is 1 matching engine (mirror of sync).

### Semver

Every 0.2.0 public signature is unchanged. The 237-item
parity gate still passes. 0.3.0 is a milestone marker for a
substantive additive block; 0.2.1 would also be semver-legal
under pre-1.0 caret rules, but 0.3.0 flags the async
completion as a visible milestone.

## [0.2.0] — 2026-04-21

**R11 — additive AsyncBroker trait.** Three sub-phases, all
codex-audited with ACK.

### Added

- `AsyncBroker` trait (R11.1) — async sibling of `Broker`,
  same method surface with `async fn`. Requires `async_trait`
  macro on impl blocks.
- `AsyncPaper` (R11.2) — reference `AsyncBroker` impl
  mirroring sync `Paper`.
- `tests/parity_async.rs` (R11.3) — 10-item async parity
  harness mirroring sync R4 base tests.

### Dependency changes

- Added `async-trait = "0.1"` to production deps (zero-runtime
  procedural macro).

### Non-breaking

- Every 0.1.0 public signature unchanged. 237-item parity
  gate passes.

## [0.1.0] — 2026-04-21

**MVP — 10 implementation phases (R1-R10), 237-item parity
manifest.**

### Added

- `utils` + `BasicPosition` + parity harness
  (libtest-mimic) + 13-row smoke matrix
- `Quote` / `OrderBook` / `OrderLock` + `Clock` trait +
  `MockClock`
- `Order` (~40 fields, full lifecycle) + `Broker` trait +
  `PersistenceHandle` trait + SQLite backend (behind
  `persistence` feature)
- `Paper` broker + close_all_positions /
  cancel_all_orders / get_positions_from_orders
- Simulation models: `VOrder`, `VTrade`, `VPosition`,
  `VUser`, `Ticker`, `OHLC*`, `OrderFill` + 8 response types
- `VirtualBroker` multi-user matching engine with seeded RNG
- `ReplicaBroker` standalone matching engine with
  `Arc<Mutex<VOrder>>` shared ownership
- `CompoundOrder` — indexed basket with aggregate views
  (positions, MTM, net_value, average_buy/sell_price),
  execute_all / check_flags / save
- `OrderStrategy` — clock-cascade on `add`, per-compound
  `run_fn` callback
