R9 is not ACKed yet. The seven `test_order_strategy.py` parity trials are
registered, passing under the parity gate, and the requested command shape is
green. The blocker is R10 readiness: current `PORT-PLAN.md` still makes
`OrderStrategy.clock` and immediate clock cascade in `OrderStrategy::add`
normative, but the R9 implementation does not include that field or cascade.
R10 is supposed to be a pure parity sweep, so this needs to be resolved before
R9 can close.

## Findings

### P1.1 `OrderStrategy::add` does not implement the PORT-PLAN clock cascade

`PORT-PLAN.md` says every clock-owning MVP type includes
`Arc<dyn Clock + Send + Sync>`, explicitly lists `OrderStrategy.clock`, and
requires `OrderStrategy::add(compound)` to overwrite the compound clock and
immediately cascade that clock to already-contained child orders
(`docs/PORT-PLAN.md:177`, `docs/PORT-PLAN.md:180`,
`docs/PORT-PLAN.md:305`).

The implemented `OrderStrategy` has only `broker`, `id`, `ltp`, and `orders`
(`src/order_strategy.rs:17`-`src/order_strategy.rs:22`). Its `add` method only
fills a missing broker before pushing the compound order
(`src/order_strategy.rs:105`-`src/order_strategy.rs:109`). There is no
strategy clock to propagate and no immediate child-order cascade.

This does not break today's parity gate because the seven R9 trials do not
exercise the clock path. It does block a true R10 audit if R10 is expected to
verify PORT-PLAN invariants without allowing new implementation work.

Required fix: either implement the documented clock field and immediate cascade
before R9 ACK, or amend `PORT-PLAN.md` to remove `OrderStrategy.clock` and the
`OrderStrategy::add` cascade rule from the normative R10 surface. Given the
v8/v9/v10 plan audits explicitly accepted the cascade rule, implementation is
the cleaner fix.

## Scope Check

- `src/order_strategy.rs` defines `OrderStrategy` with `broker`, `id`, `ltp`,
  `orders`, and methods `positions`, `update_ltp`, `update_orders`, `mtm`,
  `total_mtm`, `run`, `add`, and `save`.
- `src/compound_order.rs` adds `run_fn: Option<RunFn>`, with `RunFn` as an
  `Arc<dyn Fn(&mut CompoundOrder, &HashMap<String, f64>) + Send + Sync>`.
- `tests/parity/test_order_strategy.rs` contains the seven R9 trials and
  `tests/parity/main.rs` registers the same seven names.
- `rust-tests/parity-item-manifest.txt` has 237 active entries after stripping
  comments and blanks, and its R9 section contains the seven
  `test_order_strategy_*` names.
- `tests/parity/excused.toml` still has exactly one `[[excused]]` row:
  `test_order_timezone`, approved at R3.a.
- No `#[ignore]` attributes were found.

## Design Review

1. The `run_fn` closure field is a reasonable Rust analogue for the upstream
   subclass override used by `CompoundOrderRun`. Cloning the `Arc` before
   calling the closure also avoids a self-borrow conflict cleanly.
2. `CompoundOrderNoRun` as `run_fn = None` matches the upstream non-callable
   path for the collected trial.
3. Strategy MTM aggregation uses Decimal accumulation per symbol, matching the
   upstream `Counter.update` aggregation shape.
4. Broker propagation in `OrderStrategy::add` matches the scoped upstream guard
   when the incoming compound has no broker. Skipping the mismatch warning is
   acceptable for this port layer.
5. The audit prompt's `update_ltp` wording is inverted: upstream stores
   strategy-level LTP first and then propagates to compounds
   (`/home/ubuntu/refs/omspy/omspy/order.py:1347`-`1357`). The Rust
   implementation does the same (`src/order_strategy.rs:62`-`69`), so there is
   no behavior issue.
6. The MTM test avoids hardcoding the upstream typo for `amzn`; because the
   expression evaluates to zero either way, the Rust assertion is faithful to
   the observable expected result.

## Verification

- `cargo test --test parity --all-features` exited 0 via the parity gate:
  manifest 237, passed 236, failed 1, failing id `test_order_timezone`, gate
  `Pass`.
- `cargo test --no-default-features` exited 0. Effective manifest 222, passed
  221, failed 1, failing id `test_order_timezone`, gate `Pass`; parity runner
  smoke passed 13/13.
- `cargo clippy --all-features -- -D warnings` passed.
- `cargo clippy --no-default-features -- -D warnings` passed.
- `cargo clippy --all-targets --all-features -- -D warnings` passed.
- `cargo clippy --all-targets --no-default-features -- -D warnings` passed.
- `scripts/parity_gate.sh` exited 0 in release mode with manifest 237, passed
  236, failed 1, failing id `test_order_timezone`, gate `Pass`.
- Active manifest count:
  `rg -v '^\\s*(#|$)' rust-tests/parity-item-manifest.txt | wc -l` returned
  `237`.

## R10 Readiness

The parity-gate exit-0 invariant is currently satisfied. The remaining blocker
is not gate arithmetic; it is the unresolved `OrderStrategy.clock` /
`OrderStrategy::add` cascade contract in the current plan. If that contract is
fixed or formally removed before R10, the R10 sweep can start from a clean
237 / 236 / 1 gate shape.
