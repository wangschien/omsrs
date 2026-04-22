# Summary

- NACK. The phase order is broadly sane, but the plan has three spec mismatches that should be fixed before implementation: `AsyncVirtualBroker` return/stream contract, `AsyncReplicaBroker` identity, and async `Order` lifecycle reuse.
- Observed fact: current `AsyncBroker` is `order_place -> Option<String>` plus `order_modify/order_cancel -> ()` (`src/async_broker.rs:70-73`), while sync `VirtualBroker` returns `BrokerReply` (`src/virtual_broker.rs:27-43`, `src/virtual_broker.rs:163`, `src/virtual_broker.rs:281`, `src/virtual_broker.rs:325`) and sync `ReplicaBroker` returns `OrderHandle` / `Option<OrderHandle>` (`src/replica_broker.rs:72`, `src/replica_broker.rs:132`, `src/replica_broker.rs:171`).
- Observed fact: R12.2 describes a primary/replica async broker wrapper (`docs/R12-async-complete-plan.md:64-76`), but the existing sync `ReplicaBroker` is a standalone matching engine with instruments, orders, pending/completed/fills, and `run_fill` (`src/replica_broker.rs:23-36`, `src/replica_broker.rs:45-217`).
- Observed fact: R12.3 is not "mostly mechanical" unless async `Order::execute/modify/cancel` semantics are also added or duplicated. `CompoundOrder` currently delegates through sync `Order` methods (`src/compound_order.rs:403-414`, `src/compound_order.rs:421-433`; `src/order.rs:559`, `src/order.rs:619`, `src/order.rs:775`).
- Hypothesis: `polymarket-kernel` may be publish-safe, but that cannot be audited from this checkout. Evidence needed: its `Cargo.toml`, `build.rs`, packaged `.crate`, and a non-AVX/docs.rs-like build log.

# 5-item checklist walkthrough

## 1. Phase decomposition

CONCERN.

The high-level order Virtual -> Replica -> Compound/Strategy -> publish prep -> publish/migration is defensible because R12.3 consumes an async broker abstraction and R12.4/R12.5 should wait for API freeze (`docs/R12-async-complete-plan.md:28-147`).

But the phases need one split and one reorder:

- Split R12.3 into `AsyncOrder` lifecycle first, then `AsyncCompoundOrder` / `AsyncOrderStrategy`. Observed fact: current compound execution does not call broker methods directly; it calls `Order::execute`, `Order::modify`, and `Order::cancel` (`src/compound_order.rs:403-414`, `src/compound_order.rs:421-433`). Those methods are sync and accept `&dyn Broker` (`src/order.rs:559-563`, `src/order.rs:619-623`, `src/order.rs:775`).
- Move crates.io name verification before R12.4 README/badge/changelog work. R12.4 writes crates.io/docs.rs badges and docs (`docs/R12-async-complete-plan.md:114-119`), while the name fallback is only a risk mitigation at publish time (`docs/R12-async-complete-plan.md:168-170`).
- Make `polymarket-kernel on crates.io` an explicit predecessor to the pbot dependency flip, not just a coordinated side track. R12.5 flips both deps in pbot (`docs/R12-async-complete-plan.md:140-147`) and the acceptance gate requires polymarket-kernel published (`docs/R12-async-complete-plan.md:217-219`).

## 2. Technical feasibility

CONCERN.

`AsyncBroker` dyn compatibility is fine. Observed fact: the trait requires `Send + Sync` (`src/async_broker.rs:70`), current code intentionally uses `async_trait` for dyn compatibility (`src/async_broker.rs:47`), and a smoke test constructs `Arc<dyn AsyncBroker>` (`src/async_broker.rs:262-276`). Mechanism: `async-trait` documents that it transforms async trait methods into boxed futures and supports dyn traits when normal object-safety rules are met: https://docs.rs/async-trait.

The feasibility concerns are about API shape, not async_trait:

- R12.1 says `AsyncVirtualBroker` exposes a `BrokerReply` stream and implements `AsyncBroker` (`docs/R12-async-complete-plan.md:35-49`). Current `AsyncBroker::order_place` can only return `Option<String>` (`src/async_broker.rs:71`), while current `VirtualBroker::order_place` returns `BrokerReply` (`src/virtual_broker.rs:163`). The plan needs to state how `OrderResponse`/`Passthrough` are mapped, whether the stream is the canonical response channel, and how callers correlate stream replies to requests.
- R12.1 names `submit_with_fill_ratio`, `cancel_pending`, and `observe_quote` (`docs/R12-async-complete-plan.md:35-37`), but those symbols are not in the current `src/virtual_broker.rs`; observed public methods are `order_place`, `order_modify`, `order_cancel`, `update_tickers`, `ltp`, `ltp_many`, and `ohlc` in the implementation span (`src/virtual_broker.rs:163-400`). Hypothesis: those names come from a future or downstream simulator design. Evidence needed: the intended source contract or new API spec.
- R12.2's stated design is not an async mirror of current `ReplicaBroker`. Current `ReplicaBroker` owns simulation state (`src/replica_broker.rs:28-36`) and `run_fill` mutates fills/completed state (`src/replica_broker.rs:190-217`). The plan's `primary: Arc<dyn AsyncBroker>, replica: Arc<dyn AsyncBroker>` wrapper (`docs/R12-async-complete-plan.md:64-76`) is a different type.
- R12.2's "primary error path: replica is NOT mutated" is only partially expressible through `AsyncBroker`: `order_place` can fail as `None`, but `order_modify` and `order_cancel` return `()` (`src/async_broker.rs:71-73`).

## 3. Semver + publish risk

CONCERN.

`0.2.0 -> 0.3.0` is acceptable only if the team wants an explicit opt-in boundary. It is not the only semver-correct choice for additive API. Cargo's version requirement rules treat `0.2` as `>=0.2.0, <0.3.0`, and compatibility is based on the left-most non-zero component: https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html. So `0.3.0` will not satisfy existing consumers pinned to `omsrs = "0.2"`. That may be fine for pbot, but call it an intentional pre-1.0 compatibility boundary, not merely "new public API = semver minor" (`docs/R12-async-complete-plan.md:136-139`).

Publish mechanics concerns:

- `tokio` is dev-only today (`Cargo.toml:40`). R12.1's `tokio::sync::Mutex`, `tokio::sync::mpsc::UnboundedReceiver`, and possible `tokio-stream` wrapper move tokio into normal dependency surface (`docs/R12-async-complete-plan.md:42-48`, `docs/R12-async-complete-plan.md:189-193`). That needs an explicit dependency/feature policy before API work starts.
- The CI doc command is wrong as written: `cargo doc --no-deps --lib -D warnings` (`docs/R12-async-complete-plan.md:126-129`) passes `-D` to Cargo, not rustdoc. Cargo documents `RUSTDOCFLAGS` as the way to pass flags to all rustdoc invocations: https://doc.rust-lang.org/cargo/reference/environment-variables.html. Use `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --lib --all-features`.
- The broken intra-doc link is real and correctly scoped for R12.4: unconditional docs link to `sqlite` (`src/persistence.rs:1-6`) while the module is feature-gated (`src/persistence.rs:25-27`).
- Crate name availability cannot be proven from this checkout. The plan's fallback (`docs/R12-async-complete-plan.md:168-170`) is directionally fine, but it should be checked before R12.4 because README badges and pbot migration text depend on the final crate name.

## 4. Non-goals sanity check

PASS with one sequencing caveat.

- Async persistence is not secretly gating R12. Observed fact: persistence is a sync trait (`src/persistence.rs:11-13`), SQLite is optional (`Cargo.toml:12`, `Cargo.toml:27`), and R12 explicitly excludes async persistence (`docs/R12-async-complete-plan.md:151-153`).
- `AsyncClock` is not gating R12. Observed fact: `VirtualBroker` uses a sync `Clock` and calls `clock.now()` inside local state transitions (`src/virtual_broker.rs:63`, `src/virtual_broker.rs:136-144`, `src/virtual_broker.rs:167`); no I/O is evident in the clock path.
- N-replica fan-out is not needed if R12.2 is corrected to mirror current `ReplicaBroker`. If R12.2 intentionally wants a primary/replica wrapper, then the non-goal needs to clarify that "N replicas" means "do not generalize the new wrapper beyond one replica" (`docs/R12-async-complete-plan.md:156-158`).
- pbot publishing is not gating. The pbot task is dependency migration only (`docs/R12-async-complete-plan.md:143-147`, `docs/R12-async-complete-plan.md:159-161`).
- Sequencing caveat: pbot migration is gated by both omsrs and polymarket-kernel being actually available on crates.io (`docs/R12-async-complete-plan.md:140-147`, `docs/R12-async-complete-plan.md:217-219`).

## 5. Open questions

See the next section for direct answers.

# Open questions

## 1. Tokio primitives vs runtime-agnostic channel

Recommendation: do not commit to tokio production dependencies for `AsyncVirtualBroker` until the response contract is fixed.

Observed fact: current `AsyncPaper` uses short sync `std::sync::Mutex` locks and never awaits while locked (`src/brokers.rs:110-116`, `src/brokers.rs:186-224`). Current `VirtualBroker` operations are local CPU/state mutations over `HashMap`, `SmallRng`, and `Clock` (`src/virtual_broker.rs:54-64`, `src/virtual_broker.rs:163-400`).

Mechanism: Tokio documents that its async `Mutex` is designed to be held across `.await`, but is more expensive and ordinary blocking mutexes are often preferred for plain data: https://docs.rs/tokio/latest/tokio/sync/struct.Mutex.html.

Position: if no await occurs while the state lock is held, keep a sync mutex or use a runtime-agnostic design. If a stream is necessary, prefer bounded backpressure unless the plan proves event volume is bounded by command volume and tests always drain it.

## 2. PORT-PLAN.md §10 update

Recommendation: update `PORT-PLAN.md` with a supersession note.

Observed fact: `PORT-PLAN.md` still says "No tokio" and lists `tokio / any async runtime` as an explicit non-goal (`docs/PORT-PLAN.md:228`, `docs/PORT-PLAN.md:317-318`). R11 already shipped `AsyncBroker` with `async-trait` (`src/async_broker.rs:1-19`, `Cargo.toml:31`), and R12 proposes broader async coverage (`docs/R12-async-complete-plan.md:8-20`).

Keep the historical record, but add a dated note that R11/R12 supersede the old runtime posture. CHANGELOG alone is not enough because future audits will still read PORT-PLAN as the governing scope document.

## 3. `&dyn AsyncBroker` vs `Arc<dyn AsyncBroker>`

Recommendation: expose `Arc<dyn AsyncBroker>` on owning structs and use `&dyn AsyncBroker` for per-call helpers.

Observed fact: sync `CompoundOrder` and `OrderStrategy` own broker handles as `Option<Arc<dyn Broker>>` and builder methods accept `Arc<dyn Broker>` (`src/compound_order.rs:35`, `src/compound_order.rs:88`, `src/order_strategy.rs:24`, `src/order_strategy.rs:52`). Observed fact: `AsyncBroker` was designed for `Arc<dyn AsyncBroker>` usage (`src/async_broker.rs:47`, `src/async_broker.rs:262-276`).

So `AsyncCompoundOrder { broker: Option<Arc<dyn AsyncBroker>> }` should match the sync API and pbot ergonomics. Internal async helper methods can still take `&(dyn AsyncBroker + Send + Sync)` to avoid clone churn when the caller already has a borrow.

# Per-subphase risks

## R12.1

Material risk: API contract mismatch.

Observed fact: plan says `AsyncVirtualBroker` exposes queue advance, a `BrokerReply` stream, and implements `AsyncBroker` (`docs/R12-async-complete-plan.md:35-49`). Observed fact: current `AsyncBroker` only returns `Option<String>` from placement (`src/async_broker.rs:71`), while current `VirtualBroker` returns rich `BrokerReply` for place/modify/cancel (`src/virtual_broker.rs:27-43`, `src/virtual_broker.rs:163`, `src/virtual_broker.rs:281`, `src/virtual_broker.rs:325`). Evidence needed before implementation: exact mapping for `BrokerReply::Order`, `BrokerReply::Passthrough`, modify/cancel replies, stream ownership, and request/reply correlation.

Concurrency risk: unbounded stream memory. Tokio documents that unbounded mpsc messages are arbitrarily buffered and memory is the implicit bound: https://docs.rs/tokio/latest/tokio/sync/mpsc/fn.unbounded_channel.html. If the stream is retained, make capacity/backpressure/drop behavior explicit.

## R12.2

Material risk: wrong target type.

Observed fact: the plan's `AsyncReplicaBroker` is a primary/replica wrapper (`docs/R12-async-complete-plan.md:64-76`). Observed fact: current sync `ReplicaBroker` is not a wrapper around a primary broker; it is a matching engine with `instruments`, `orders`, `users`, `pending`, `completed`, `fills`, and `user_orders` (`src/replica_broker.rs:28-36`) plus `run_fill` (`src/replica_broker.rs:190-217`).

Resolve before coding: either rename/re-scope R12.2 as a new `AsyncReplicatingBroker` decorator, or implement an async mirror of the existing `ReplicaBroker` state machine. Do not call the current wrapper design "mirrors `ReplicaBroker`."

Async failure risk: the current `AsyncBroker` trait cannot report success/failure for `order_modify` or `order_cancel` (`src/async_broker.rs:71-73`), so "primary error path: replica is NOT mutated" (`docs/R12-async-complete-plan.md:68-70`) is underspecified for two of three operations. A timeout/panic test is useful, but the implementation needs an explicit timeout/cancellation/error policy; plain `.await` on replica can hang after primary success.

## R12.3

Material risk: async order lifecycle missing.

Observed fact: R12.3 says porting is mostly mechanical by swapping `&impl Broker` for `&(dyn AsyncBroker + Send + Sync)` (`docs/R12-async-complete-plan.md:86-92`). Observed fact: `CompoundOrder::execute_all` and `check_flags` delegate into `Order::execute/modify/cancel` (`src/compound_order.rs:403-414`, `src/compound_order.rs:421-433`), and those methods are sync `&dyn Broker` methods (`src/order.rs:559`, `src/order.rs:619`, `src/order.rs:775`).

Evidence needed before implementation: an `AsyncOrder` API or clearly scoped duplicated helper that preserves `Order::execute`, `Order::modify`, and `Order::cancel` kwarg precedence, local state mutations, save-to-db behavior, lock rules, and modification counters.

## R12.4

Concrete risk: CI commands need tightening before they become copied workflow YAML.

Observed fact: the plan lists `cargo clippy -- -D warnings` without `--all-features` / `--all-targets` and lists `cargo doc --no-deps --lib -D warnings` (`docs/R12-async-complete-plan.md:126-129`). The former misses feature/test code; the latter is syntactically the wrong mechanism for rustdoc warnings. Use `cargo clippy --all-features --all-targets -- -D warnings` and `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --lib --all-features`.

No other R12.4 blocker found. CHANGELOG and docs hygiene are correctly placed after API freeze (`docs/R12-async-complete-plan.md:106-133`).

## R12.5

Concrete risk: publish sequencing.

Observed fact: the plan publishes omsrs, checks polymarket-kernel, flips pbot to both crates, then commits pbot (`docs/R12-async-complete-plan.md:136-147`). Observed fact: acceptance requires both omsrs docs.rs and polymarket-kernel on crates.io (`docs/R12-async-complete-plan.md:215-219`). Make the pbot migration contingent on both crates already being published and resolvable from a clean cache.

Hypothesis: docs.rs / crates.io build risk for polymarket-kernel depends on its build script. Evidence needed: non-AVX VM build and docs.rs-like build for the packaged crate, not just local runtime detection (`docs/R12-async-complete-plan.md:140-142`, `docs/R12-async-complete-plan.md:170`).

# Semver / crates.io specific concerns

- `0.3.0` is okay as an intentional pre-1.0 boundary, but not required for purely additive API. Cargo treats `0.2` as `<0.3.0`, so consumers on `omsrs = "0.2"` will not receive R12 automatically. Mechanism: Cargo version requirement docs: https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html.
- Check `omsrs` crate-name availability before R12.4 docs/badges. The plan currently delays that to publish risk mitigation (`docs/R12-async-complete-plan.md:168-170`), but R12.4 writes name-dependent README material (`docs/R12-async-complete-plan.md:114-119`).
- `tokio` production dependency policy must be explicit before publish. Today `tokio` is dev-only (`Cargo.toml:40`); R12.1 makes it public dependency surface if `tokio::sync` types appear in public APIs (`docs/R12-async-complete-plan.md:42-48`).
- `cargo publish --dry-run --all-features` is a good gate (`docs/R12-async-complete-plan.md:133`), but add a clean consumer check after publish: new temp crate with `omsrs = "0.3"` and no path overrides. This specifically validates pbot's intended dependency mode (`docs/R12-async-complete-plan.md:20-22`, `docs/R12-async-complete-plan.md:143-147`).
- The known broken docs link is correctly included in R12.4 (`docs/R12-async-complete-plan.md:106-109`), and its concrete source is `src/persistence.rs:1-6` pointing at a feature-gated `sqlite` module (`src/persistence.rs:25-27`).

# Concurrency hazards specific concerns

- `tokio::sync::Mutex` is not wrong, but the plan's justification is weak for local simulation state. Tokio's own docs say async mutex is mainly useful when holding a guard across `.await`; for plain data, blocking mutex is usually appropriate: https://docs.rs/tokio/latest/tokio/sync/struct.Mutex.html. Current `VirtualBroker` state mutation is synchronous (`src/virtual_broker.rs:163-400`).
- Do not hold the `AsyncVirtualBroker` state lock while awaiting external broker/user code. Hypothesis: this hazard only appears if R12.1 adds await points inside state transitions; current sync code has no such await points. Evidence needed: final method bodies.
- `UnboundedReceiver<BrokerReply>` has an explicit memory hazard if producers outpace consumers. Tokio documents arbitrary buffering with system memory as the implicit bound: https://docs.rs/tokio/latest/tokio/sync/mpsc/fn.unbounded_channel.html. If retained, tests should cover an undrained receiver or the API should use a bounded channel/backpressure.
- R12.2 replica timeout semantics are underspecified. If primary succeeds and replica await hangs, the wrapper either hangs the caller, needs an internal `timeout`, or needs fire-and-forget plus reconciliation. The plan mentions a timeout test (`docs/R12-async-complete-plan.md:78-81`) but not the policy being tested.
- Panic mid-await is not handled by ordinary `.await`; catching it requires task boundaries or unwind handling. Hypothesis: if `AsyncReplicaBroker` directly awaits `replica.order_place(args).await`, a panic aborts the operation after primary mutation. Evidence needed: implementation decision on direct await vs spawned task and whether panics are allowed to propagate.

# Final verdict

R12 PLAN NACK — revise before kickoff: fix AsyncVirtualBroker reply/stream contract, correct or rename AsyncReplicaBroker design, split R12.3 to include async Order lifecycle, define tokio/channel dependency policy, and make publish sequencing/CI commands explicit
