# codex audit prompt — R11.1 AsyncBroker trait (omsrs v0.2 open)

## Context

v0.1.0 shipped sync `Broker` trait per the PORT-PLAN. Downstream pbot
consumers (2026-04-21 R3.3b planning) discovered that every real
prediction-market SDK in scope (Polymarket `rs-clob-client`, Kalshi
`kalshi-rs`) is async, so every venue adapter implementing `Broker`
needs a sync-over-async bridge (`tokio::task::block_in_place` +
`block_on`). Adding parallel async methods per adapter duplicates the
glue.

**R11 opens omsrs v0.2** with an additive `AsyncBroker` trait that lives
alongside `Broker`. None of the v0.1.0 surface changes — `Broker`,
`Paper`, `VirtualBroker`, `ReplicaBroker`, `CompoundOrder`,
`OrderStrategy`, the 237-item parity manifest — all untouched.

This audit is **R11.1 scope only**: the trait definition + dyn-compat
+ default-method semantics. `AsyncPaper` and the 10-item async parity
harness land in R11.2; release + pbot rewiring in R11.3 / R3.3b.

Commit: `TBD`.

## Files landed

- `Cargo.toml` — `async-trait = "0.1"` in deps; `tokio = "1"` in
  dev-deps (for R11.2's `#[tokio::test]`); version bumped to `0.2.0`.
- `src/async_broker.rs` — `AsyncBroker` trait + `AsyncSymbolTransformer`
  alias (`Arc<dyn Fn + Send + Sync>`) + `coerce_quantity` /
  `order_record_from_dict` helpers that mirror the sync broker's
  private helpers. 2 unit tests: dyn-compat Arc construction, default
  methods return empty.
- `src/lib.rs` — `pub mod async_broker;` + `pub use async_broker::AsyncBroker;`.

## Acceptance

| ID | Check |
|---|---|
| R11.1.1 | `AsyncBroker` trait exists with `async fn order_place / order_modify / order_cancel` as required methods |
| R11.1.2 | Default methods `orders` / `trades` / `positions` / `attribs_to_copy_*` return empty / None, mirroring sync `Broker` |
| R11.1.3 | Default `close_all_positions` implements the same static-keys + keys_to_copy + keys_to_add + symbol_transformer pipeline as sync `Broker` |
| R11.1.4 | Default `cancel_all_orders` uses the same TERMINAL set (COMPLETE / CANCELED / REJECTED) and status-uppercasing as sync `Broker` |
| R11.1.5 | Default `get_positions_from_orders` filters CANCELED / REJECTED, applies `dict_filter`, aggregates via `create_basic_positions_from_orders_dict` — matching sync |
| R11.1.6 | `Arc<dyn AsyncBroker>` constructs (dyn-compatible / object-safe) |
| R11.1.7 | `AsyncSymbolTransformer` aliased to `Arc<dyn Fn(&str) -> String + Send + Sync>` so future-lifetime issues are avoided |
| R11.1.8 | v0.1.0 237-item parity gate still passes (`scripts/parity_gate.sh` → exit 0, 237/236/1-excused) |
| R11.1.9 | v0.1.0 `Broker`, `Paper`, etc. unchanged — no line in any v0.1.0 file touched |

Commands:
- `cargo build` — clean
- `cargo test --lib async_broker` — 2 passed
- `cargo test` — all v0.1.0 tests still green (including 237-item parity)
- `scripts/parity_gate.sh` — exit 0

## Checklist for codex

1. **Additive contract** — does v0.1.0's public API change at all?
   Specifically: `Broker` trait signature, default-method bodies,
   `Paper` / `VirtualBroker` / `ReplicaBroker` / `CompoundOrder` /
   `OrderStrategy` types and methods. Should be zero delta.

2. **AsyncBroker surface parity** — does `AsyncBroker` mirror
   `Broker` method-for-method with matching semantics? Any subtle
   drift (e.g. async version accepts different args, different
   default behavior)?

3. **`AsyncSymbolTransformer` type choice** — `Arc<dyn Fn + Send +
   Sync>` was picked because `&dyn Fn` with an anonymous lifetime
   couldn't survive `async_trait`'s `'static` future boundary.
   Tradeoff: ergonomic loss (caller wraps in Arc). Is this the right
   call vs. adding a trait lifetime `'async_trait` or forcing
   `#[async_trait(?Send)]`?

4. **Default-method helpers reuse** — `coerce_quantity` and
   `order_record_from_dict` are duplicated from `broker.rs` rather
   than shared. Acceptable for additive change, or should R11 also
   hoist them to `utils` for single-source?

5. **async_trait dependency** — added unconditionally (not
   feature-gated). Size is ~0 runtime overhead; compile-time adds a
   proc-macro. Any objection to leaving it always-on?

6. **`#[async_trait]` attribute on trait declaration** — trait
   methods declared `async fn`. Must implementors also write
   `#[async_trait]` on `impl`? (Yes — that's the macro's contract.)
   Does the doc comment make this clear?

7. **Object safety** — `Arc<dyn AsyncBroker>` must construct. The
   trait uses `Send + Sync` supertrait + async methods. Verify the
   dyn-compat regression guard test covers this.

8. **Parity non-regression** — re-run `scripts/parity_gate.sh`.
   Verify exit 0 and 237/236/1-excused shape unchanged.

9. **Version bump semantics** — `0.1.0` → `0.2.0` is a minor bump
   per SemVer (additive API change). OK?

10. **R11.2/R11.3 staging** — the deferred items are `AsyncPaper` +
    10-item async parity harness (R11.2), doc + codex audit + v0.2.0
    tag + GitHub release (R11.3). Does the staging make sense given
    R11.1's narrow scope?

## Out of scope

- `AsyncPaper` reference impl → R11.2
- 10-item async parity harness (mirrors `tests/parity/test_base.rs`)
  → R11.2
- doc updates (README, PORT-PLAN new section) → R11.3
- v0.2.0 tag + GitHub release → R11.3
- pbot-side rewiring (swap local `LiveClientApi` for
  `omsrs::AsyncBroker`) → R3.3b after R11.3

## Output

Write to `docs/audit-R11.1-codex-result.md`. 10-item checklist + per-
R11.1.* verdict. Final: does R11.1 gate open for R11.2?
