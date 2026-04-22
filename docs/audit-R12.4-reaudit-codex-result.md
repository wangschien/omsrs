# R12.4 Re-audit (round 2)

## Scope
Commit e6b0717, items 3 + 4 only.

## Checklist
### Item 3 — README tokio dep clarity
- PASS. The quickstart lead-in says the sample uses `#[tokio::main]`, states why `.await` needs a runtime, and explicitly says `tokio` is not an `omsrs` production dependency (README.md:98, README.md:99).
  The dependency block is immediately before the Rust sample and includes both `omsrs = "0.3"` and `tokio = { version = "1", features = ["rt-multi-thread", "macros"] }` (README.md:102, README.md:104, README.md:105).
  Placement is copy-paste clear: the Rust block starts right after the deps block and uses `#[tokio::main]` at the shown entrypoint (README.md:108, README.md:117).

### Item 4a — --locked removed from all 8 cargo commands
- PASS. CI's six cargo command lines are clean: `cargo fmt --all --check`, `cargo build --all-features`, `cargo test --all-features`, `cargo clippy --all-features --all-targets -- -D warnings`, `cargo doc --no-deps --lib --all-features`, and `cargo publish --dry-run --all-features` (.github/workflows/ci.yml:44, .github/workflows/ci.yml:47, .github/workflows/ci.yml:50, .github/workflows/ci.yml:53, .github/workflows/ci.yml:58, .github/workflows/ci.yml:62).
  Release's two publish command lines are clean: `cargo publish --dry-run --all-features` and `cargo publish --all-features` (.github/workflows/release.yml:32, .github/workflows/release.yml:37).
  Literal `--locked` remains only in explanatory comments, not cargo commands: .github/workflows/ci.yml:12, .github/workflows/ci.yml:17, .github/workflows/release.yml:28.

### Item 4b — ci.yml rationale comment accuracy
- PASS. The comment accurately states `--locked` is intentionally omitted, the crate does not commit `Cargo.lock`, CI resolves current `Cargo.toml` ranges, and `--locked` should return if a lockfile is later committed (.github/workflows/ci.yml:12, .github/workflows/ci.yml:13, .github/workflows/ci.yml:14, .github/workflows/ci.yml:15, .github/workflows/ci.yml:16, .github/workflows/ci.yml:17).
  `.gitignore` does ignore `/Cargo.lock` (.gitignore:2).
  The library-crate characterization is consistent with the manifest shape and lib entrypoint: audited `Cargo.toml` contains package/dependency/test-target declarations with no bin target, and `src/lib.rs` is the crate root (Cargo.toml:1-58, src/lib.rs:1).

## Spot-check (items 1, 2, 5)
Item 1: no regression spotted; crate version is `0.3.0` and README dependency examples use `omsrs = "0.3"` (Cargo.toml:3, README.md:104, README.md:158).
Item 2: no regression spotted; CHANGELOG 0.3.0 still lists the R12 async public surface and keeps `tokio` dev-only (CHANGELOG.md:14, CHANGELOG.md:20, CHANGELOG.md:28, CHANGELOG.md:33, CHANGELOG.md:37, CHANGELOG.md:57, CHANGELOG.md:60).
Item 5: no regression spotted; PORT-PLAN supersession still says `tokio` is dev-only, production carries `async_trait`, and async persistence remains deferred (docs/PORT-PLAN.md:317, docs/PORT-PLAN.md:318).

## Verdict
R12.4 ACK — proceed to R12.5
