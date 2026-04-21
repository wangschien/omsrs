# codex audit prompt — R11.2 AsyncPaper + 10-item parity harness

## Context

R11.1 ACKed at `docs/audit-R11.1-codex-result.md` (10/10 checklist +
9/9 acceptance). Non-blocking doc nits carried over:
- `async_trait` doc comment overstates the `'static` future boundary;
  should clarify `'async_trait` boxed-future semantics.
- Public trait docs should explicitly instruct implementors to put
  `#[async_trait]` on their impl blocks.

Both nits are addressed in this commit alongside the main R11.2 payload.

R11.2 scope:
- `AsyncPaper` reference impl in `src/brokers.rs` mirroring sync `Paper`
- `AsyncSymbolTransformer` re-exported from `lib.rs`
- 10-item async parity harness at `tests/parity_async.rs` mirroring
  `tests/parity/test_base.rs` — same fixtures, same assertions, same
  order. Uses `#[tokio::test]` and `AsyncDummyBroker` (local) +
  `AsyncPaper` (re-exported).

Commit: `TBD`.

## Files landed

- `src/brokers.rs` — adds `AsyncPaper` struct + `impl AsyncBroker for
  AsyncPaper` (with same `with_orders` / `with_trades` / `with_positions`
  builders + call recorders as sync `Paper`). Sync `Paper` unchanged.
- `src/async_broker.rs` — doc comment tightened per R11.1 nits (removed
  "must be 'static" claim, added explicit "implementors must put
  `#[async_trait]` on their impl blocks" note).
- `src/lib.rs` — also re-exports `AsyncSymbolTransformer` + `AsyncPaper`.
- `tests/parity_async.rs` — 10 `#[tokio::test]` items, one-to-one with
  `tests/parity/test_base.rs`.

## Acceptance (R11.2 manifest, 10 items mirror R4 exactly)

| ID | Async name |
|---|---|
| R11.2.1 | `async_test_dummy_broker_values` |
| R11.2.2 | `async_test_close_all_positions` |
| R11.2.3 | `async_test_cancel_all_orders` |
| R11.2.4 | `async_test_close_all_positions_copy_keys` |
| R11.2.5 | `async_test_close_all_positions_add_keys` |
| R11.2.6 | `async_test_close_all_positions_copy_and_add_keys` |
| R11.2.7 | `async_test_close_all_positions_quantity_as_string` |
| R11.2.8 | `async_test_close_all_positions_quantity_as_error` |
| R11.2.9 | `async_test_close_all_positions_symbol_transfomer` |
| R11.2.10 | `async_test_close_all_positions_given_positions` |

Plus R11.1's 2 unit tests in `src/async_broker.rs`.

Commands:
- `cargo test --test parity_async` — 10 passed
- `cargo test` — full suite green
- `scripts/parity_gate.sh` — exit 0, 237 / 236 / 1-excused shape

## Checklist

1. **10-item name parity** — the 10 async test fn names correspond
   1:1 to `tests/parity/test_base.rs` with an `async_` prefix, in
   identical order. The preserved misspelling
   `_symbol_transfomer` is carried through as `async_test_close_all_
   positions_symbol_transfomer`.

2. **Semantic equivalence** — each async test produces the same
   observables as its sync counterpart (same place_calls length,
   same field values, same side/quantity combinations for the
   GOLDGUINEA + LEADMINI Kiteconnect fixtures). Read both files
   side-by-side and flag any divergence beyond the `async` /
   `await` / `Arc<dyn Fn>` substitutions.

3. **`AsyncDummyBroker` mirrors `DummyBroker`** — loads the same JSON
   fixtures, applies the same `orders_rename` / `positions_rename`,
   sets `status=pending` on every order. Records calls in the same
   `place_calls` / `modify_calls` / `cancel_calls` shape.

4. **`AsyncPaper` = async sibling of `Paper`** — identical field
   layout (6 `Mutex` vs 6 `Mutex`), identical builders
   (`with_orders` / `with_trades` / `with_positions`), identical
   call recorders. Only the `AsyncBroker` impl differs (async fn
   methods instead of sync).

5. **`AsyncSymbolTransformer` plumbing** — R11.2 is the first
   real caller of the alias in a test. `Arc<|s: &str| format!(...)>`
   cast via `let transform: AsyncSymbolTransformer = Arc::new(...)`;
   call site passes `Some(transform)`. No lifetime issues.

6. **No locks held across `.await`** — `AsyncPaper` locks a
   `std::sync::Mutex` briefly to push a call / clone a Vec, then
   drops the guard before returning. No `.await` within a locked
   scope. Important because a parking_lot / std Mutex guard held
   across `.await` is a deadlock footgun.

7. **Doc nit closeouts from R11.1** — the `async_trait` doc comment
   in `src/async_broker.rs` no longer overstates `'static`; there's
   explicit language about `'async_trait` boxed future + a note
   that implementors must annotate `#[async_trait]` on impl blocks.

8. **Sync parity untouched** — `tests/parity/test_base.rs` byte-for-
   byte unchanged; sync `Paper` + `DummyBroker` unchanged. R4 still
   passes the 237-item parity gate.

9. **Regression** — `cargo test` full run green (all sync tests +
   13-row smoke + async parity + statistical target).
   `scripts/parity_gate.sh` exit 0, 237 / 236 / 1-excused.

10. **Lib surface review** — `lib.rs` now re-exports
    `AsyncBroker`, `AsyncSymbolTransformer`, `Paper`, `AsyncPaper`.
    Any ordering or visibility concern?

## Out of scope

- R11.3: README / PORT-PLAN v0.2 section, codex final v0.2 audit,
  `v0.2.0` tag, GitHub release.
- Pbot R3.3b rewiring (swap local LiveClientApi → omsrs::AsyncBroker).

## Output

Write to `docs/audit-R11.2-codex-result.md`. 10-item checklist + per-
R11.2.1..10 verdict. Final: does R11.2 gate open for R11.3?
