# codex v3 audit — R12 plan after v2 NACK closeout

## Context

v2 audit (`docs/audit-R12-plan-v2-codex-result.md`) NACKed on two
narrow items:
1. CI command coverage: `cargo clippy` missing `--all-targets`;
   `cargo doc` missing `--all-features`
2. Missing "clean registry consumer check" between publish and
   pbot migration

All other round-1 + v2 concerns verified PASS.

Both items were accepted by the plan author — neither was
pushed back as over-rotation.

## What changed v2 → v3

Plan doc: `docs/R12-async-complete-plan.md`. Specific deltas:

1. **§R12.4 CI block** now reads:
   ```yaml
   - run: cargo clippy --all-features --all-targets -- -D warnings
   - run: cargo doc --no-deps --lib --all-features
     env:
       RUSTDOCFLAGS: "-D warnings"
   ```
   Plus a "Flag rationale" paragraph explaining why each flag
   matters.

2. **§R12.5 step 3** is new: "Clean registry consumer check"
   between omsrs publish (step 2) and pbot migration (step 4).
   Specifies:
   - Throwaway directory + `cargo new --bin registry-check`
   - `Cargo.toml` declares `omsrs = "0.3"` + `polymarket-kernel
     = "<ver>"` from **registry**, no `[patch]`, no `path =`
   - `src/main.rs` imports every top-level type pbot actually
     uses (mirrors pbot's `use` statements)
   - `cargo build` must succeed before the pbot migration
     commit
   pbot migration step renumbered to step 4, gated on 1+2+3.

3. **Risk table** adds one row: "Registry build succeeds but
   consumer build fails" with the new check as mitigation.

4. **Acceptance checklist** at the bottom adds:
   ```
   - [ ] Clean-registry consumer check passes
   ```
   as a required gate between "omsrs 0.3.0 on crates.io" and
   "pbot migration commit".

5. **Revision history** updated with v3 entry.

## What to audit (narrow — v3-specific)

### 1. Did v3 fix both v2 NACK items?
- CI clippy has `--all-targets`? (yes — check the ```yaml``` block)
- CI doc has `--all-features`? (yes — check same block)
- Registry consumer check is a real gate, not a soft
  recommendation? (step 3, blocks step 4)
- Does the risk table + acceptance checklist reflect the
  change?

### 2. Did v3 introduce anything new?
Plan author's intent: the two items are isolated, so v3 should
not have ripple effects. But double-check:
- The CI rationale paragraph doesn't contradict anything
  elsewhere in the plan
- The new step 3 doesn't accidentally drop a dep or shadow
  step 4's existing pbot-side verification

### 3. Everything else that was PASS in v2 — still PASS?
No changes to R12.1 / R12.2 / R12.3a / R12.3b / R12.4 body /
§R12.5 steps 1+2+4, hard constraint, non-goals, open-question
resolutions. Spot-check by diffing if uncertain.

## Output

Write to `docs/audit-R12-plan-v3-codex-result.md`:

- **v2 NACK item 1 (CI flags)** — PASS/FAIL + citation
- **v2 NACK item 2 (registry consumer check)** — PASS/FAIL + citation
- **New problems in v3** — list or "none"
- **Prior PASS items still PASS** — spot-check summary

Final verdict:
- `R12 PLAN v3 ACK — proceed to R12.1 implementation`, or
- `R12 PLAN v3 NACK — revise again: <specific items>`

## Meta

This is expected to be a short audit. Narrow scope. Don't pad.
If ACK, just say so. Per
`feedback_codex_audit_judgment`, the author reads your verdict
for technical substance, not ceremony.
