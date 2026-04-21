# Audit: omsrs Port Plan v10

Adversarial audit. Plan is at v10 after 9 prior NACKs. Each revision has closed the prior blocker and the prior audit has almost always found a new issue. Stay adversarial, but if v10 genuinely closes every finding without introducing new ones, return ACK.

**Read first**:
- `~/omsrs/PORT-PLAN.md` (v10)
- `~/omsrs/omspy-source-notes.md` (updated)
- `~/omsrs/PORT-PLAN-v9-audit-result.md` (prior NACK — 1 P0 + 1 P1 + 6 P2)

## Chain

v1–v9 all NACKed. v9 findings v10 must close:

- **P0.1** `scripts/parity_gate.sh` passed `--report` to a `libtest-mimic` binary that doesn't parse that flag.
- **P1.1** `excused.toml` validation said "missing / malformed file ⇒ treated as empty", contradicting the required-field checks.
- **P2.1** Live plan text still contained `-Z`, `unstable-options`, `--format json`.
- **P2.2** §1 still embedded stale numeric strings `238`/`4760`/`5760`.
- **P2.3** `omspy-source-notes.md` still had `### MVP parity gate (v7)`.
- **P2.4** §1 mentioned `cargo run --test parity`, inconsistent with §4.1.4's `cargo test`.
- **P2.5** Ticker derivation used `≈ 0.02` without quoting upstream's literal `0.01` or showing `p = 0.0159566`.
- **P2.6** Manifest load path + statistical target harness setting not pinned.

v10 claims all 8 closed.

## Stance

Pure library port only. Adversarial on argv contract + excused.toml edge cases + grep hygiene. v10 also adds a non-normative **§12 Rust idioms leveraged** orientation table — verify it introduces no new scope (no async, no HTTP/WS, no new prod deps).

## Checklist

### A. v9 P0 closure (argv contract)

- [ ] A1: `scripts/parity_gate.sh` (per §4.1.4) passes **no custom flags** to the parity binary. Verify the bash snippet ends with `exec cargo test ... --test parity --release --all-features` and nothing after.
- [ ] A2: §4.1.1 explicitly says the parity binary defines no custom argv flags. Grep the plan for `--report` — should appear only in the P0 explanation/closure text, not in any live invocation.
- [ ] A3: §1, §4.1.1, §4.1.4, §9.3 all describe the same gate command shape: no `--report`, no `-Z`, no `--format`.
- [ ] A4: The parity binary's report-emission is described as unconditional on every run. (Good for observability; the exit code is the gate.)

### B. v9 P1 closure (excused.toml validation)

- [ ] B1: §4.1.2 distinguishes **absent** (→ empty, silent) from **present-but-invalid** (→ exit code 6). Verify wording.
- [ ] B2: Required-field checks (rationale / approved_at / approved_by) route to exit code 6, not silent-empty.
- [ ] B3: Duplicate id / unknown id / R0 non-empty / |excused| > 7 still have their own exit codes (2/3/4/5).
- [ ] B4: Is there any remaining failure mode that could silently zero the excused set? Think about: present file that TOML-parses but the root table has no `[[excused]]` arrays at all (well-formed empty list) — that should be equivalent to absent. Clarify if the plan is silent.

### C. v9 P2 closure (mechanical hygiene)

- [ ] C1: `rg -n '\\-Z unstable-options|--format json' ~/omsrs/PORT-PLAN.md` returns 0 live-command hits. Meta-references inside a "what we removed" paragraph are acceptable only if clearly labeled as history.
- [ ] C2: `rg -n '238|4760|5760' ~/omsrs/PORT-PLAN.md` — the only acceptable hits are §3's math (`237 × 20 = 4740`), nothing claiming `238/4760/5760` as live budget.
- [ ] C3: `rg -n '\\(v7\\)' ~/omsrs/omspy-source-notes.md` — 0 hits (everything re-labelled to `(current)` or historical).
- [ ] C4: `rg -n 'cargo run .* --test parity' ~/omsrs/PORT-PLAN.md` — 0 hits.
- [ ] C5: §6 D10 Ticker derivation cites upstream's literal `random.gauss(0, 1) * self._ltp * 0.01` and uses exact `p = Φ(0.02) − Φ(−0.02) = 0.0159566`.
- [ ] C6: §4.1.1 specifies manifest load via `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/rust-tests/parity-item-manifest.txt"))`.
- [ ] C7: §9.4 declares the statistical target as `harness = false` and says it uses libtest-mimic.

### D. Regression

- [ ] D1: Phase gate sum `20 + 10 + 64 + 10 + 54 + 22 + 10 + 40 + 7 = 237` still holds. Confirm §8 table.
- [ ] D2: `#[ignore]` still disallowed as a legal mechanism. Only negative mentions allowed.
- [ ] D3: `OrderStrategy::add(compound)` still immediately cascades clock to already-contained children.
- [ ] D4: Broker response construction rules unchanged (`VirtualBroker` all three construct `OrderResponse` with `self.clock`; `ReplicaBroker::order_place` only `VOrder`).
- [ ] D5: Denominator 237 uniformly. `test_ticker_ltp` §14(A), outside 237 and outside slack.
- [ ] D6: Week math `16.25` → "~16 weeks".
- [ ] D7: R8 schedule risk R.13 still present.
- [ ] D8: `rand` fallback still covers both prod Ticker and `tests/statistical/test_ticker_ltp_statistical.rs`.

### E. New v10 issues (adversarial — look for what v10 broke)

- [ ] E1: §4.1.2 new exit code 6 — any collision with existing codes 0/1/2/3/4/5? All distinct.
- [ ] E2: §4.1.4 wrapper uses `--release`. That means the gate runs with optimizations. Is that intended, or would some parity tests want debug assertions? (Not an ACK blocker — style question.)
- [ ] E3: `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), ...))` — `CARGO_MANIFEST_DIR` is set by cargo at compile time; `include_str!` takes a string literal. The `concat!` of macros usually works but is lint-y. Flag if plan has known-wrong macro usage, otherwise fine.
- [ ] E4: Present-file-but-no-rows case (well-formed empty TOML table): §4.1.2 step 1 says "absent ⇒ empty", steps 2-6 check rows. An empty file that parses cleanly should land in a no-op path. Is that the author's intent? If so, add one sentence: "present-and-empty is equivalent to absent, excused_set = {}".
- [ ] E5: §9.4 harness = false statistical target — does this require its own `tests/statistical/main.rs` entry point? Plan says yes implicitly; confirm there's no ambiguity about whether statistical = libtest-mimic vs stock libtest.
- [ ] E6: `scripts/parity_gate.sh` uses `exec cargo test`. Does `cargo test` forward the parity binary's exit code faithfully? Yes, but if cargo compile fails the exit code is cargo's, which is fine. Flag if any edge case isn't handled.

### F. Scope

- [ ] F1: No new prod deps in §7. `libtest-mimic` and `toml` remain dev-deps. Verify.
- [ ] F2: Phase table unchanged (11 rows, 237 total, 16.25 clean-path weeks).

## Deliverables

Write result to `~/omsrs/PORT-PLAN-v10-audit-result.md`.

Format:
- **Verdict** (ACK or NACK) at top.
- **P0 findings** — blockers, each with required fix.
- **P1 findings** — substantial.
- **P2 findings** — polish.
- **Verified closures** — what v10 actually fixed from v9.
- **Minimum changes for ACK** — numbered list if NACK.

Be fair: if v10 genuinely closes everything, say ACK and state that R1 can start. We're 9 revisions in; returning ACK on a revision that has actually closed all real findings is not a failure of audit rigor — it's the end of the loop.
