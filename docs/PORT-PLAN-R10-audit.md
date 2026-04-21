# R10 audit prompt — final parity sweep + stabilisation

R9 ACKed (after NACK → clock-cascade fix → re-audit ACK). R10 is the
**final phase** per PORT-PLAN §8: pure parity sweep + stabilisation, no
new parity items. All 10 implementation phases have their own codex
audit result in `docs/`.

This audit closes the project. Please verify the full PORT-PLAN §9
acceptance matrix and flag anything that would block calling the port
"done".

## PORT-PLAN §9 acceptance checklist

Please run (or re-run) each and report pass/fail:

1. `cargo build -p omsrs` — zero warnings.
2. `cargo build -p omsrs --no-default-features` — zero warnings.
3. `scripts/parity_gate.sh` exits 0 (expected shape: manifest 237,
   passed 236, failed 1 [`test_order_timezone`], gate `Pass`).
4. `cargo test -p omsrs --test statistical --release --features statistical-tests`
   exits 0.
5. `cargo test -p omsrs --no-default-features` passes (effective
   manifest 222, same 1 excused).
6. `cargo clippy -p omsrs --all-features --all-targets -- -D warnings`
   clean.
7. **`Broker` trait object-safe**. `Arc<dyn Broker>` is used in
   `OrderStrategy.broker`, `CompoundOrder.broker` — confirms object
   safety holds.
8. `Paper` passes the 10 R4 `test_base` items.
9. `VirtualBroker` passes the 22 R6 items.
10. `ReplicaBroker::run_fill` state-transition determinism — byte-equal
    across 3 consecutive calls. Codex verified this during R7 via a
    scratch probe; re-verify or cite.
11. `CompoundOrder` passes 40 R8 items; `OrderStrategy` passes 7 R9
    items.

## Stabilisation checks

- `cargo fmt --check` clean (R10 commit applied a sweep).
- No `#[ignore]` anywhere in `src`, `tests`, `Cargo.toml`,
  `rust-tests`, `scripts`, `docs`.
- `tests/parity/excused.toml` still has exactly 1 row (the R3.a
  `test_order_timezone` §14B entry).
- Manifest `rust-tests/parity-item-manifest.txt` still 237 active ids.
- Per-phase audit docs (`docs/PORT-PLAN-R{1..9}-audit-result.md`) all
  contain ACK verdicts. R3.a / R8 / R9 had NACK → fix → ACK cycles; the
  final verdict in each is ACK.
- README.md reflects the completed state.
- Any non-blocking items flagged in prior audits (e.g. R6's
  `order_modify` not routing through `get()`, R8's `update_orders`
  missing-data branch, R4's Python-truthiness edges) — flag whether
  any of these should block "done" status or carry forward as
  known-narrowings.

## What counts as R10 ACK

The project "done" criteria: all 11 §9 checks green, all stabilisation
checks green, no P0/P1 findings beyond what's already excused.
Non-blocking items from prior audits can be called out as known
narrowings (some already are, e.g. Python-truthiness edges in
`close_all_positions` or the same-memory weakening in VirtualBroker).

If ACK, omsrs v0.1.0 is ready for downstream consumers (pbot +
Polymarket adapter). If NACK, list the blocking items.

## Output format

Same as prior audits. Write to `docs/PORT-PLAN-R10-audit-result.md`.
