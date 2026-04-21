# Audit: omsrs Port Plan v11

Adversarial audit. Plan is at v11 after 10 prior NACKs. v10 had 0 P0 + 1 P1 + 1 P2; v11 must close those without introducing new issues. If v11 is clean, return **ACK** — we are at the end of the revision cycle.

**Read first**:
- `~/omsrs/PORT-PLAN.md` (v11)
- `~/omsrs/omspy-source-notes.md`
- `~/omsrs/PORT-PLAN-v10-audit-result.md` (prior NACK — 0 P0 + 1 P1 + 1 P2)

## Chain

v1–v10 all NACKed. v10 findings v11 must close:

- **P1.1** Present-but-empty `excused.toml` / no-`[[excused]]`-rows case undefined, conflicts with source-notes "`excused.toml` starts empty at R0".
- **P2.1** `parity_runner_smoke` asserts only exit codes 0/1/2/3/4/5, missing exit code 6 cases and the present-empty success path.

v11 claims both closed via:
1. §4.1.2 adds a "present-and-empty ⇒ empty excused_set" clause with explicit `#[serde(default)]` schema that makes the R0 committed-empty TOML file deserialize to `excused: vec![]`. Missing row fields still fail deser and route to exit 6.
2. §4.1.5 replaces prose with a 13-row smoke-test coverage matrix (absent, present-empty, all exit codes 0–6 including three missing-field variants and two malformed variants).

## Stance

Pure library port only. This is a convergence audit: verify the two v10 fixes land correctly and nothing else regressed.

## Checklist

### A. v10 P1 closure

- [ ] A1: §4.1.2 step 1 enumerates three cases: absent, present-empty, present-invalid. Absent and present-empty both yield `excused_set = {}`. Present-invalid exits 6.
- [ ] A2: The schema snippet uses `#[serde(default)]` on the top-level `excused: Vec<ExcusedRow>`. Verify.
- [ ] A3: `ExcusedRow` fields (`id`, `rationale`, `approved_at`, `approved_by`) have **no** defaults — a missing field on a row is a deser failure and routes to exit 6.
- [ ] A4: Cross-check against source-notes §14 "No pre-authorized entries at R0; `tests/parity/excused.toml` starts empty". The R0 committed-empty file must deserialize cleanly with the schema — yes via `#[serde(default)]`.
- [ ] A5: Any remaining silent-empty path that is not one of {absent, present-empty}? Should be none; confirm.

### B. v10 P2 closure

- [ ] B1: §4.1.5 has a table with 13 rows. Each exit code 0–6 appears at least once. Exit code 6 has at least 5 rows (malformed, wrong shape, missing rationale, missing approved_at, missing approved_by).
- [ ] B2: Present-empty case (row 2) expects exit 0.
- [ ] B3: The final sentence requires future §4.1.2 changes to update the smoke matrix. Good ratchet.

### C. Non-regression from v10

- [ ] C1: §4.1.1 parity binary still has no custom argv flags. `--report` only in negative/history text.
- [ ] C2: §4.1.4 wrapper is still `exec cargo test -p omsrs --test parity --release --all-features` (no custom flags).
- [ ] C3: `rg -n -- '\-Z unstable-options|--format json' PORT-PLAN.md` — 0 live-command hits.
- [ ] C4: `rg -n -- '\(v7\)' omspy-source-notes.md` — 0 hits.
- [ ] C5: `rg -n -- 'cargo run .* --test parity' PORT-PLAN.md` — 0 hits.
- [ ] C6: `rg -n -- '4760|5760' PORT-PLAN.md` — 0 hits. `238` allowed only in gross-count explanation contexts (not claiming budget).
- [ ] C7: Phase sum `20 + 10 + 64 + 10 + 54 + 22 + 10 + 40 + 7 = 237` still holds.
- [ ] C8: `#[ignore]` disallowed. Only negative mentions.
- [ ] C9: `OrderStrategy::add(compound)` still immediately cascades clock.
- [ ] C10: Broker response construction rules intact (`VirtualBroker` place/modify/cancel → `OrderResponse`; `ReplicaBroker::order_place` → `VOrder` only).
- [ ] C11: Denominator 237 uniform. `test_ticker_ltp` §14(A), outside denominator, outside slack.
- [ ] C12: §6 D10 Ticker derivation still quotes `random.gauss(0, 1) * self._ltp * 0.01` and uses `p = 0.0159566`.
- [ ] C13: §7 deps unchanged from v10 (`libtest-mimic` + `toml` are dev-deps; no new prod deps).
- [ ] C14: §12 Rust-idioms table still non-normative; no async / HTTP / WS / sqlx / dashmap mentioned as in-scope.
- [ ] C15: §9.4 statistical target still `harness = false` + libtest-mimic.
- [ ] C16: R8 schedule risk R.13 retained.

### D. New v11 issues (adversarial — last pass)

- [ ] D1: Does `#[serde(default)]` on a `Vec` field actually work with the `toml` crate? Yes — serde's `default` attribute is crate-agnostic; `toml` uses serde. No red flag.
- [ ] D2: The smoke matrix row 3 says "Failing trial id ∈ excused_set, ≥ 230 pass, |excused| ≤ 7 ⇒ exit 0". Does the harness actually model a failing trial in the smoke target? The smoke target uses stable libtest (`#[test]` fns) and invokes the parity-gate logic via a library function, passing constructed trial results. Should be feasible; the plan implies a public library function behind the scenes. Flag if the plan is silent on how the smoke target accesses the gate logic.
- [ ] D3: Row 10 says "Well-formed TOML, unexpected shape (e.g. `excused` is a string, not array) ⇒ exit 6". With `#[serde(default)] excused: Vec<ExcusedRow>`, a string value for `excused` fails deser — correct. But is it distinguishable from plain malformed TOML for error reporting? Not an ACK blocker; stylistic.
- [ ] D4: Did v11 rename §1 to include a `§1.prev` subsection for v10's changelog? Is that change-log hygiene sound, or does it risk accumulating a §1, §1.prev, §1.prev2, …? Suggest plan-history should move to `PORT-PLAN-history.md` on every bump instead.

### E. Final convergence

- [ ] E1: Have 10 consecutive revisions actually been producing smaller NACK surface areas? v9 → 1 P0 + 3 P1 + 6 P2; v10 → 0 P0 + 1 P1 + 1 P2. If v11 has 0 P0, 0 P1, ≤ 1 polish P2, return ACK.
- [ ] E2: ACK means "R1 (20 items, Cargo + utils + 3 BasicPosition) may start". NACK means "one more round".

## Deliverables

Write result to `~/omsrs/PORT-PLAN-v11-audit-result.md`.

Format:
- **Verdict** (ACK or NACK) at top.
- If NACK: P0 / P1 / P2 findings, each with required fix + minimum changes list.
- If ACK: state that R1 may start, list any nits that are not blocking.
- **Verified closures** — what v11 fixed from v10.

If v11 genuinely closes both v10 items without opening new blockers, return ACK. The return-on-investment for continued plan-level audit is low past this point; the next marginal quality gain comes from starting R1 under Cargo + codex-audited commits.
