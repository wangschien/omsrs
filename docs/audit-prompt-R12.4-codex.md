# codex audit — R12.4 publish prep

## Context

Last audit before R12.5 ships to crates.io. R12.4 is pure
packaging — no library code changes. R12.1, R12.2, R12.3a,
R12.3b all ACKed.

Landed commit: `b5c9ca6`.

## What shipped

- `Cargo.toml` version bump `0.2.0` → `0.3.0`
- `README.md` — crates.io / docs.rs / CI / license badges + new
  v0.3 section with AsyncVirtualBroker + AsyncCompoundOrder
  quickstart + Feature flag table + v0.3 dep in Cargo example
- `CHANGELOG.md` (new) — Keep a Changelog format; 0.3.0 section
  itemising R12 additions + backfill of 0.2.0, 0.1.0
- `docs/PORT-PLAN.md` — §10 supersession note on "no tokio /
  async runtime" (strike-through of the original line + cite
  R11/R12)
- `.github/workflows/ci.yml` — matrix (stable + MSRV 1.78):
  fmt / build / test / clippy (--all-features --all-targets
  -- -D warnings) / doc (RUSTDOCFLAGS=-D warnings --all-
  features) / publish --dry-run
- `.github/workflows/release.yml` — fires on `v*` tags,
  verifies tag = Cargo.toml version, dry-run then publish

## Context the audit should know

- crates.io name `omsrs`: **confirmed available** via
  `cargo search omsrs` → empty result
- `cargo publish --dry-run --all-features`: passes as
  omsrs-0.3.0 (124 files, 283KiB compressed, verification
  build succeeds)
- All code from R12.1–R12.3b untouched in R12.4

## Audit scope (5-item checklist)

### 1. Version bump correctness
- `Cargo.toml` version = `0.3.0`. Check.
- CHANGELOG 0.3.0 header matches Cargo.toml.
- README `omsrs = "0.2"` → `"0.3"` in dep example. Check.
- No other version strings need updating (scan for `"0.2"` /
  `0.2.0` elsewhere — spot-check).

### 2. CHANGELOG semantic accuracy
- 0.3.0 section lists every R12 addition (Async* types + Order
  async siblings). Missing anything?
- 0.3.0 "Changed" section documents the two backwards-compat
  widenings (ReplicaFill:Clone, cloned_clone_weak delay
  preservation). Is "Changed" the right Keep-a-Changelog
  category or should these be "Added"? (Keep-a-Changelog's
  convention: "Changed" = change in existing functionality.
  ReplicaFill deriving Clone is adding a capability to an
  existing type — arguably "Added" with "(backwards-compat)"
  qualifier.)
- Non-goals explicitly deferred: async persistence (R13),
  AsyncClock, N-replica fan-out. Complete?

### 3. README accuracy
- New v0.3 quickstart code sample compiles (it's in a
  markdown code fence). Trace the imports: `omsrs::
  {AsyncVirtualBroker, AsyncCompoundOrder, AsyncOrderStrategy}`
  — do these pub exports exist in `src/lib.rs`?
- Quickstart uses `#[tokio::main]` — need to mention that
  `tokio` is **not** an omsrs production dep (so consumers
  need their own `tokio` dep). Does the README make this
  clear? Could mislead a reader.

### 4. CI + release workflow safety
- `ci.yml`: MSRV 1.78 matrix — Cargo.toml says
  `rust-version = "1.78"`. Aligned.
- `ci.yml`: `--locked` flag on all cargo commands. Requires
  `Cargo.lock` committed — it IS in `.gitignore` though
  (sync/omsrs ignores `/Cargo.lock`). CI `cargo build
  --locked` without a Cargo.lock fails — is this a CI-only
  concern, or does it block the first green run? Either way
  a fix path:
  - generate `Cargo.lock` on first CI run (no `--locked`)
  - or un-ignore and commit `Cargo.lock`

  Note: library crates conventionally don't commit
  `Cargo.lock`; this is a real choice to make.
- `release.yml`: tag → publish. Safety:
  - `verify-version` step compares tag to Cargo.toml
    (prevents mismatched publishes). Good.
  - Uses `CRATES_IO_TOKEN` secret (must be set in repo
    secrets before a tag push — operator gate).
  - Does a dry-run before the real publish — gates on it
    succeeding.
  - Should it require CI to have passed before running? The
    current workflow runs independently of `ci.yml`. A CI
    failure on `main` would still allow a tag push to
    publish. Worth considering a `workflow_run` dependency
    or a branch protection rule.

### 5. PORT-PLAN §10 supersession note
- Strike-through on the original "no tokio" line + cite
  R11/R12 + explicitly say "tokio remains dev-only".
  Accurate?
- The §13 next-step section below refers to "v11" port plan
  state. Is it still accurate, or does the supersession
  warrant adjusting the closing meta? (Probably fine to leave
  v11 as-is — §13 describes what v11's codex audit was about;
  not load-bearing for R12 work.)

## Out of scope
- The actual `cargo publish` (R12.5).
- polymarket-kernel publish (R12.5).
- pbot migration (R12.5).

## Output

`docs/audit-R12.4-codex-result.md`. 5-item checklist
(PASS/CONCERN/FAIL + rationale). Final verdict:
- `R12.4 ACK — proceed to R12.5`, or
- `R12.4 NACK — fix items X, Y, Z first`.

Per `feedback_codex_audit_judgment`: plan author assesses each
NACK on merit. Short + technical; line-cite specifics.
