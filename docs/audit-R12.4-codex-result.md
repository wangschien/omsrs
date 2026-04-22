# R12.4 publish-prep audit

Scope confirmation: audited R12.4 packaging-only scope at HEAD `3151d8b` / substantive commit `b5c9ca6`; prompt scope lists only Cargo/package docs/workflows and says no library code changes (`docs/audit-prompt-R12.4-codex.md:5`, `docs/audit-prompt-R12.4-codex.md:9`, `docs/audit-prompt-R12.4-codex.md:13`).

## 1. Version bump correctness — PASS

- `Cargo.toml` is `version = "0.3.0"` and MSRV remains `rust-version = "1.78"` (`Cargo.toml:3`, `Cargo.toml:5`).
- `CHANGELOG.md` 0.3.0 header matches the crate version (`CHANGELOG.md:7`).
- README dependency example is updated to `omsrs = "0.3"` (`README.md:146`, `README.md:148`).
- Remaining `0.2` references are historical R11/v0.2 context, not stale dependency instructions: README status/async sections describe v0.2 history (`README.md:16`, `README.md:72`, `README.md:94`) and CHANGELOG backfills 0.2.0 (`CHANGELOG.md:77`, `CHANGELOG.md:83`).

## 2. CHANGELOG semantic accuracy — PASS

- 0.3.0 "Added" lists the R12 public surface: `AsyncVirtualBroker`, `AsyncReplicaBroker`, `Order::execute_async` / `modify_async` / `cancel_async`, `AsyncCompoundOrder`, and `AsyncOrderStrategy` (`CHANGELOG.md:12`, `CHANGELOG.md:14`, `CHANGELOG.md:20`, `CHANGELOG.md:28`, `CHANGELOG.md:33`, `CHANGELOG.md:37`).
- The two backwards-compatible widenings are recorded under "Changed": `ReplicaFill: Clone` and `VOrder::cloned_clone_weak` delay preservation (`CHANGELOG.md:41`, `CHANGELOG.md:43`, `CHANGELOG.md:46`). "Changed" is acceptable because both modify existing behavior/types; "Added" with a backwards-compatible qualifier would also be defensible, but this is not a blocker.
- Deferred non-goals are explicit: async persistence, `AsyncClock`, and N-replica fan-out (`CHANGELOG.md:63`, `CHANGELOG.md:65`, `CHANGELOG.md:70`, `CHANGELOG.md:72`).

## 3. README accuracy — CONCERN

- The v0.3 quickstart imports top-level async exports that exist: README imports `AsyncVirtualBroker`, `AsyncCompoundOrder`, and `AsyncOrderStrategy` (`README.md:98`, `README.md:100`); `src/lib.rs` re-exports those names (`src/lib.rs:23`, `src/lib.rs:24`, `src/lib.rs:26`).
- The supporting module imports exist through public modules: README uses `omsrs::clock::MockClock`, `omsrs::simulation::Ticker`, and `omsrs::order::OrderInit` (`README.md:101`, `README.md:102`, `README.md:103`); those modules are public in `src/lib.rs` (`src/lib.rs:10`, `src/lib.rs:13`, `src/lib.rs:18`).
- Concern: the quickstart uses `#[tokio::main]` (`README.md:107`), while the dependency snippet only shows `omsrs = "0.3"` (`README.md:146`, `README.md:148`). README does say "No `tokio` dependency in production" (`README.md:21`), and Cargo confirms `tokio` is dev-only (`Cargo.toml:33`, `Cargo.toml:40`), but the quickstart does not explicitly tell consumers to add their own `tokio` dependency. This is a real documentation issue for copy/paste quickstart users.

## 4. CI + release workflow safety — FAIL

- MSRV matrix is aligned: CI includes `1.78` and comments it as MSRV (`.github/workflows/ci.yml:19`, `.github/workflows/ci.yml:21`), matching `Cargo.toml` (`Cargo.toml:5`).
- Blocker: every CI cargo step uses `--locked` (`.github/workflows/ci.yml:40`, `.github/workflows/ci.yml:41`, `.github/workflows/ci.yml:43`, `.github/workflows/ci.yml:44`, `.github/workflows/ci.yml:46`, `.github/workflows/ci.yml:47`, `.github/workflows/ci.yml:49`, `.github/workflows/ci.yml:52`, `.github/workflows/ci.yml:54`, `.github/workflows/ci.yml:56`), but `/Cargo.lock` is ignored (`.gitignore:2`). A clean `git archive HEAD` checkout without `Cargo.lock` fails immediately on `cargo build --all-features --locked` with "cannot create the lock file ... because --locked was passed"; this is observed, not theoretical.
- The release workflow has the same clean-checkout blocker: dry-run and publish both use `--locked` (`.github/workflows/release.yml:28`, `.github/workflows/release.yml:29`, `.github/workflows/release.yml:31`, `.github/workflows/release.yml:34`) while `/Cargo.lock` is ignored (`.gitignore:2`).
- Release safety otherwise has useful gates: tag push only (`.github/workflows/release.yml:3`, `.github/workflows/release.yml:6`), tag/Cargo version check (`.github/workflows/release.yml:19`, `.github/workflows/release.yml:23`), dry-run before publish (`.github/workflows/release.yml:28`, `.github/workflows/release.yml:31`), and token-gated publish (`.github/workflows/release.yml:32`, `.github/workflows/release.yml:33`).

## 5. PORT-PLAN §10 supersession note — PASS

- The old "no tokio / any async runtime" line is struck through and superseded with R11/R12 context, explicitly stating `tokio` remains dev-only and production surface carries `async_trait` only (`docs/PORT-PLAN.md:315`, `docs/PORT-PLAN.md:317`).
- The async persistence boundary is correctly kept out of R12 and deferred to caller `spawn_blocking` / R13 scope (`docs/PORT-PLAN.md:318`).
- §13 still refers to the v11 audit next step (`docs/PORT-PLAN.md:323`, `docs/PORT-PLAN.md:325`, `docs/PORT-PLAN.md:330`). That is stale as a live next step, but it is historical plan text below the supersession note and not load-bearing for R12.4 packaging.

## Specific concerns

- Tokio disambiguation: partially disambiguated. README says no production `tokio` dependency (`README.md:21`) and Cargo keeps `tokio` in `[dev-dependencies]` (`Cargo.toml:33`, `Cargo.toml:40`), but the quickstart uses `#[tokio::main]` (`README.md:107`) and the shown dependency block omits `tokio` (`README.md:146`, `README.md:148`). Fix by adding a quickstart dependency line such as `tokio = { version = "1", features = ["rt-multi-thread", "macros"] }` or by adding a short sentence immediately before the sample.
- `--locked` vs `.gitignore`: observed blocker. `/Cargo.lock` is ignored (`.gitignore:2`) and CI/release require `--locked` (`.github/workflows/ci.yml:41`, `.github/workflows/ci.yml:44`, `.github/workflows/ci.yml:47`, `.github/workflows/ci.yml:52`, `.github/workflows/ci.yml:56`, `.github/workflows/release.yml:29`, `.github/workflows/release.yml:34`). First clean CI/release run will fail unless `--locked` is removed or `Cargo.lock` is unignored and committed.
- Release vs CI dependency: real but non-blocking for a solo-maintainer crate. Release runs independently on `v*` tag push (`.github/workflows/release.yml:3`, `.github/workflows/release.yml:6`) and does not run tests/clippy/docs itself (`.github/workflows/release.yml:15`, `.github/workflows/release.yml:34`), so a tag can publish after a failing `main` CI if the dry-run passes. Acceptable if tags are operator-controlled after green CI; stronger options are protected tags/branch policy, adding test/clippy/doc steps to release, or using a `workflow_run`-style release gate.

R12.4 NACK — fix items 3, 4 first
