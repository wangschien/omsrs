# PORT-PLAN v0.2 — additive AsyncBroker

v0.1.0 shipped a sync `Broker` trait per `docs/PORT-PLAN.md` §2
(omspy is sync Python, so Rust port stayed sync for parity). Downstream
adoption (pbot at `github.com/wangschien/pbot`) surfaced a reality the
original plan didn't anticipate:

Every real prediction-market SDK in scope (Polymarket `rs-clob-client`,
Kalshi `kalshi-rs`) is **async**. Sync `Broker` forces every venue
adapter to add a `block_on` bridge, duplicating glue across venues.

v0.2 resolves this with a single **additive** trait: `AsyncBroker`.
Nothing in v0.1.0's public surface or semantics changes.

## Phase split

All three phases land on `main` with codex audit per phase (same
cadence as R1-R10).

- **R11.1** — `AsyncBroker` trait only. Mirrors sync `Broker`
  method-for-method (all async). Default method bodies reproduce sync
  `close_all_positions` / `cancel_all_orders` / `get_positions_from_orders`
  logic via `.await`.
  Commit: `5655a14` · Audit: `docs/audit-R11.1-codex-result.md` (ACK
  10/10 checklist + 9/9 acceptance).

- **R11.2** — `AsyncPaper` reference impl + 10-item async parity
  harness (`tests/parity_async.rs`) mirroring the sync R4
  `tests/parity/test_base.rs` 1:1 (same fixtures, same assertions,
  same order, preserved `_symbol_transfomer` misspelling).
  Commit: `8fb60c8` · Audit: `docs/audit-R11.2-codex-result.md` (ACK
  10/10 checklist + 10/10 acceptance).

- **R11.3** — README update + this file + final v0.2 codex audit
  + `v0.2.0` tag + GitHub release.

## Non-goals for v0.2

- **No migration path forced on v0.1.0 consumers.** `Broker`, `Paper`,
  `VirtualBroker`, `ReplicaBroker`, `CompoundOrder`, `OrderStrategy`
  are untouched. Downstream code that compiled against v0.1.0 keeps
  compiling.
- **No async `Order.execute/modify/cancel`.** `Order` still calls sync
  `Broker` methods. If a consumer wants async `Order`, they write an
  adapter or wait for a v0.3 scope decision. pbot's design avoids this
  by treating `Order` as lifecycle-only (used with `Paper` for tests /
  simulation) and driving live via direct `AsyncBroker` calls.
- **No async `CompoundOrder` / `OrderStrategy`.** Same reasoning. pbot
  market-making doesn't use these abstractions for live; Python pbot
  never did either.
- **No SemVer-breaking changes.** Minor bump `0.1.0` → `0.2.0` because
  the public surface grows but nothing existing changes.

## What pbot does with v0.2

pbot R3.3b (tracked at `github.com/wangschien/pbot` under that phase)
replaces its local `LiveClientApi` trait with `omsrs::AsyncBroker`.
`PolymarketBroker` impls `AsyncBroker` directly; sync `Broker` impl
stays for dry-run + parity tests + any `Paper`-style consumption.

Future Kalshi adapter, if it lands, impls `AsyncBroker` the same way —
no per-venue glue layer.

## Invariants preserved from v0.1.0

Full v0.1.0 verification still passes after v0.2:

- `scripts/parity_gate.sh` → exit 0, 237 / 236 / 1-excused shape
- `cargo test` → all existing sync tests green + 10 new async parity
- `cargo test --no-default-features` → 222-item effective manifest, all pass
- No file in the v0.1.0 core surface (`src/broker.rs`, `src/brokers.rs`
  sync `Paper` section, `src/compound_order.rs`, `src/order.rs`,
  `src/order_strategy.rs`, `src/virtual_broker.rs`,
  `src/replica_broker.rs`, `src/simulation.rs`, `src/persistence.rs`,
  `src/models.rs`, `src/utils.rs`, `src/clock.rs`) modified
