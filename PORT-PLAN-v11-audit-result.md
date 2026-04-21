# Verdict: ACK

R1 may start: 20 items, Cargo + utils + 3 BasicPosition.

v11 closes both remaining v10 findings without introducing a blocker. Further plan-level audit has low expected return; the next quality gain should come from starting R1 under Cargo with codex-audited commits.

## P0 findings

None.

## P1 findings

None.

## P2 findings

None.

## Non-blocking nits

- The retained `§1.prev` changelog is acceptable for v11 convergence, but future bumps should move accumulated plan history to `PORT-PLAN-history.md` instead of growing `§1.prev`, `§1.prev2`, etc.
- `tests/parity_runner_smoke` now states that it injects fake manifests, fake trial sets, and scripted `excused.toml` contents. During implementation, keep the parity gate arithmetic and excused-file validation factored behind a testable helper so the smoke target does not duplicate the real runner logic.

## Verified closures

- v10 P1.1 is closed. §4.1.2 step 1 now enumerates absent, present-empty, and present-invalid cases. Absent and present-empty both yield `excused_set = {}`; present-invalid exits 6.
- The schema snippet uses `#[serde(default)]` on the top-level `excused: Vec<ExcusedRow>` field.
- `ExcusedRow` fields `id`, `rationale`, `approved_at`, and `approved_by` have no defaults. Missing row fields fail deserialization and route to exit 6.
- This is consistent with `omspy-source-notes.md` §14: `tests/parity/excused.toml` starts empty at R0 and now deserializes cleanly as `ExcusedFile { excused: vec![] }`.
- No remaining silent-empty path exists outside the two defined cases: absent file and present-valid file with zero `[[excused]]` rows.
- v10 P2.1 is closed. §4.1.5 now has a 13-row smoke-test matrix.
- Every exit code 0 through 6 appears at least once in the smoke matrix.
- Exit code 6 has five rows: malformed TOML, wrong shape, missing `rationale`, missing `approved_at`, and missing `approved_by`.
- The present-empty case is row 2 and expects exit 0.
- The final §4.1.5 sentence requires future §4.1.2 exit-code semantic changes to update the smoke matrix.

## Non-regression checks

- §4.1.1 still says the parity binary defines no custom argv flags. `--report` appears only in historical/negative contract text, not in a live invocation.
- §4.1.4 wrapper remains `exec cargo test -p omsrs --test parity --release --all-features` with no custom flags.
- `rg -n -- '\-Z unstable-options|--format json' PORT-PLAN.md` returns no hits.
- `rg -n -- '\(v7\)' omspy-source-notes.md` returns no hits.
- `rg -n -- 'cargo run .* --test parity' PORT-PLAN.md` returns no hits.
- `rg -n -- '4760|5760' PORT-PLAN.md` returns no hits. `238` appears only in the source-notes gross-count explanation, not as a plan budget claim.
- Phase math still holds: `20 + 10 + 64 + 10 + 54 + 22 + 10 + 40 + 7 = 237`.
- `#[ignore]` remains disallowed; mentions are negative only.
- `OrderStrategy::add(compound)` still immediately cascades the clock to already-contained child orders.
- Broker response construction rules remain intact: `VirtualBroker` place/modify/cancel construct `OrderResponse`; `ReplicaBroker::order_place` constructs `VOrder` only.
- Denominator 237 is uniform. `test_ticker_ltp` remains outside the denominator and outside slack.
- §6 D10 still quotes `random.gauss(0, 1) * self._ltp * 0.01` and uses `p = 0.0159566`.
- §7 dependency scope is unchanged in substance from v10: `libtest-mimic` and `toml` are dev-deps; no new production dependency scope was introduced.
- §12 remains non-normative and keeps async / HTTP / WS / sqlx / dashmap out of MVP scope.
- §9.4 statistical target remains `harness = false` with `libtest-mimic`.
- R8 schedule risk R.13 is retained.
