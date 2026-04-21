# R11.1 Codex Audit Result

Commit audited: `5655a14` (`R11.1 (v0.2 open): AsyncBroker trait`)

Final verdict: **R11.1 gate opens for R11.2.** The change is additive, the requested gates pass, and the async default-method bodies match the sync `Broker` semantics. I found one non-blocking documentation precision note around `async_trait`/`AsyncSymbolTransformer`.

## Findings

- **Non-blocking doc nit:** `src/async_broker.rs:27-32` says `async_trait` creates a `'static` future boundary. More precisely, `async_trait` boxes futures behind a generated `'async_trait` lifetime; borrowed parameters can work when the bounds are expressible. The `Arc<dyn Fn(&str) -> String + Send + Sync>` choice is still acceptable for an object-safe, easy-to-clone async trait argument, but the rationale should avoid implying that every `async_trait` future must be `'static`.
- **Non-blocking doc gap:** `src/async_broker.rs:37-43` does not explicitly tell implementors to put `#[async_trait]` on their `impl` blocks. The unit tests demonstrate it at `src/async_broker.rs:242` and `src/async_broker.rs:258`, but the public trait docs should say this directly in R11.2/R11.3 docs.

## 10-Item Checklist

1. **Additive contract:** Pass. `git diff --name-status v0.1.0..HEAD -- src Cargo.toml Cargo.lock tests scripts docs` shows only `Cargo.toml`, `src/lib.rs`, new `src/async_broker.rs`, and the audit prompt doc changed. Existing `Broker`, `Paper`, `VirtualBroker`, `ReplicaBroker`, `CompoundOrder`, and `OrderStrategy` implementation files are unchanged from `v0.1.0`.
2. **AsyncBroker surface parity:** Pass. `AsyncBroker` mirrors the sync trait's order methods, accessor defaults, and position/order helper defaults. The only intentional signature drift is `close_all_positions(..., symbol_transformer: Option<AsyncSymbolTransformer>)` instead of the sync borrowed `&dyn Fn`.
3. **AsyncSymbolTransformer type choice:** Pass with doc nit. `pub type AsyncSymbolTransformer = Arc<dyn Fn(&str) -> String + Send + Sync>` at `src/async_broker.rs:32` is a reasonable v0.2 additive API choice for dyn usage and future storage. It costs callers an `Arc`, but avoids exposing a trait-level lifetime or `?Send` futures.
4. **Default-method helpers reuse:** Pass. Duplicating `coerce_quantity` and `order_record_from_dict` in `src/async_broker.rs:199-228` is acceptable for R11.1's narrow additive scope because the copies match `src/broker.rs:209-238`. Hoisting can wait until there is a third caller or a real maintenance problem.
5. **async_trait dependency:** Pass. `async-trait = "0.1"` is unconditionally added at `Cargo.toml:28-31`. For a crate exporting `AsyncBroker`, always-on is cleaner than feature-gating the public trait behind a feature.
6. **#[async_trait] on trait/impl:** Pass for implementation mechanics, doc improvement recommended. The trait has `#[async_trait]` at `src/async_broker.rs:43`; test impls correctly repeat it. Public docs should explicitly state implementors must annotate impl blocks too.
7. **Object safety:** Pass. `Arc<dyn AsyncBroker>` is constructed by `async_broker_trait_is_dyn_compatible` at `src/async_broker.rs:235-250`; `cargo test --lib async_broker` passed.
8. **Parity non-regression:** Pass. `scripts/parity_gate.sh` exited 0 and reported manifest size 237, passed 236, failed 1, gate Pass, with the known excused `test_order_timezone`.
9. **Version bump semantics:** Pass. `Cargo.toml:3` bumps `0.1.0` to `0.2.0`; an additive public trait is a minor-version change under SemVer.
10. **R11.2/R11.3 staging:** Pass. Keeping R11.1 to the trait and default semantics is coherent. `AsyncPaper` plus a focused async parity harness belongs in R11.2; README/PORT-PLAN/release tagging belongs in R11.3 after the reference impl validates the trait.

## R11.1 Acceptance Verdicts

| ID | Verdict | Notes |
|---|---|---|
| R11.1.1 | Pass | `AsyncBroker` exists with async required `order_place`, `order_modify`, and `order_cancel` at `src/async_broker.rs:44-47`. |
| R11.1.2 | Pass | `orders`, `trades`, `positions`, and `attribs_to_copy_*` default to `Vec::new()` / `None` at `src/async_broker.rs:49-70`. |
| R11.1.3 | Pass | `close_all_positions` preserves static-key skipping, copied keys, added keys, symbol transformation, opposite side, absolute quantity, and MARKET order placement at `src/async_broker.rs:78-136`. |
| R11.1.4 | Pass | `cancel_all_orders` uses `COMPLETE` / `CANCELED` / `REJECTED`, uppercases status, skips null/missing `order_id`, and awaits `order_cancel` at `src/async_broker.rs:140-177`. |
| R11.1.5 | Pass | `get_positions_from_orders` filters `CANCELED` / `REJECTED`, applies `dict_filter`, converts to `OrderRecord`, then aggregates at `src/async_broker.rs:180-196`. |
| R11.1.6 | Pass | Regression test constructs `Arc<dyn AsyncBroker>` at `src/async_broker.rs:235-250`. |
| R11.1.7 | Pass | Alias is exactly `Arc<dyn Fn(&str) -> String + Send + Sync>` at `src/async_broker.rs:32`. |
| R11.1.8 | Pass | `scripts/parity_gate.sh` exit 0; report shape is 237 manifest / 236 passed / 1 excused failure. |
| R11.1.9 | Pass | Existing v0.1.0 core files are unchanged versus `v0.1.0`; only additive module export/dependency/version files changed. |

## Verification

- `cargo build` -> pass.
- `cargo test --lib async_broker` -> pass, 2 tests.
- `cargo test` -> pass overall; parity harness internally reports the known excused `test_order_timezone`.
- `scripts/parity_gate.sh` -> pass, 237 / 236 / 1 excused.
