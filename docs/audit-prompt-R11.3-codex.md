# codex audit prompt — R11.3 v0.2 docs + release gate

## Context

R11.1 ACKed (`docs/audit-R11.1-codex-result.md`, 10/10 checklist + 9/9
acceptance). R11.2 ACKed (`docs/audit-R11.2-codex-result.md`, 10/10 +
10/10). R11.3 closes the v0.2 scope with docs + release gate.

After this audit ACKs, the flow is:
1. `git tag v0.2.0`
2. `git push origin v0.2.0`
3. `gh release create v0.2.0 --title "v0.2.0 — additive AsyncBroker" --notes-file ...`
4. pbot R3.3b rewiring to use `omsrs::AsyncBroker` (separate repo).

Commit: `TBD`.

## Files landed

- `README.md` — v0.2 status line, async-broker consumer section with
  full example, `tests/parity_async` verification entry, scope
  mentions `AsyncBroker` + `AsyncPaper`, Cargo dep example bumped to
  `0.2`.
- `docs/PORT-PLAN-v0.2.md` — short plan doc explaining the split
  (R11.1 / R11.2 / R11.3), non-goals, pbot-consumer direction,
  invariants preserved from v0.1.0.

No code changes in this commit.

## Checklist

1. **README v0.2 status line** — does it make it clear that v0.1.0
   parity is unchanged (237 / 236 / 1-excused) AND that v0.2 is
   additive non-breaking? Any operator-unfriendly wording?

2. **Async consumer example** — the code block compiles mentally and
   produces a correct `impl AsyncBroker`. Note the `async_trait`
   annotation on the impl block (required because the trait uses
   `#[async_trait]`). Any missing `#[async_trait]` attribute or
   method signature mismatch?

3. **AsyncPaper paragraph in README** — claims "If AsyncPaper passes
   an assertion, sync Paper passes the same assertion". Verify by
   reading `tests/parity_async.rs` alongside `tests/parity/test_base.rs`.

4. **Cargo.toml dep example in README** — bumped from `"0.1"` to
   `"0.2"`. The actual `Cargo.toml` has `version = "0.2.0"`. Are the
   two in sync?

5. **`docs/PORT-PLAN-v0.2.md`**
   - phase split (R11.1 trait / R11.2 AsyncPaper / R11.3 release)
     matches actual commits (`5655a14`, `8fb60c8`, this commit)
   - non-goals list (no forced migration, no async Order, no async
     CompoundOrder/OrderStrategy) — are these the right lines?
   - pbot consumer direction matches R3.3b scope
   - invariants-preserved list — any v0.1.0 core file that was
     touched and shouldn't have been?

6. **Cross-reference health** — README points at `docs/audit-R11.{1,2,3}
   -codex-result.md`. The R11.3 result doesn't exist yet (this
   audit produces it). OK pattern, or do we rephrase README to
   "see the R11 audit trail"?

7. **No regression** — rerun `cargo build` + `cargo test` +
   `scripts/parity_gate.sh`. All must stay green. This audit is
   docs-only but no-regression is a hard gate for release.

8. **Version bump consistency** — `Cargo.toml` version is `0.2.0`,
   `Cargo.lock` reflects. `README.md` dep example shows `"0.2"`. Any
   residual `0.1.x` reference that should be updated?

9. **Tag / release plan sanity** — the proposed
   `git tag v0.2.0 && gh release create` flow after ACK. Any
   release-hygiene item pbot / omsrs downstream would want in the
   release notes that isn't covered by README + PORT-PLAN-v0.2.md?

10. **R11 audit trail** — three audits total (R11.1 + R11.2 + this
    R11.3) form a coherent audit trail equivalent to how R1-R10 were
    done. Any audit pattern gap (e.g. no "v0.2 overall re-audit")
    worth filling before the tag?

## Out of scope

- Tagging / release (those happen after ACK)
- pbot R3.3b rewiring (separate repo)
- Any v0.3 planning

## Output

Write to `docs/audit-R11.3-codex-result.md`. 10-item checklist. Final
verdict: is v0.2 ready for `v0.2.0` tag + GitHub release?
