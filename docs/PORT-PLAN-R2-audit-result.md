# Verdict: ACK

R3 may start. R2 lands the requested 10 new parity trials, bringing the active manifest to 30 items. The parity gate exits 0 with no excused rows, and the Cargo/clippy/build matrix is clean in this sandbox.

## P0 findings

None.

## P1 findings

None.

## P2 findings

- P2.1 `OrderLock`'s serde shape is not upstream-equivalent if it is used as a public wire format. Upstream stores `creation_lock_till`, `modification_lock_till`, and `cancellation_lock_till` as Pydantic `PrivateAttr`s, and `OrderLock().model_dump()` emits only `max_order_*` plus `timezone`. Rust derives serde over the private timestamp fields (`src/models.rs:168`-`src/models.rs:172`) while skipping only `clock` (`src/models.rs:172`). That means Rust self-round-trips are plausible, and the `Arc<dyn Clock + Send + Sync>` skip/default is wired, but deserializing an upstream-shaped `OrderLock` object without timestamp fields would fail. This is not an R3 blocker if `Order._lock` remains private/skipped like upstream, but it should be fixed or explicitly documented before any serde/DB path exposes `OrderLock`.
- P2.2 The R2 OrderLock tests use UTC wall-clock labels for upstream timezone fixtures. Upstream `pendulum.datetime(2022, 1, 1, 10, 10, 15, tz="Asia/Kolkata")` is the instant `2022-01-01T04:40:15Z`; the Rust helper uses `2022-01-01T10:10:15Z` (`tests/parity/test_models.rs:107`). The current assertions are relative, so this does not invalidate R2, but R3 timezone/expiry tests should UTC-normalize upstream fixtures rather than reusing local wall-clock components as UTC. Also, `test_order_lock_defaults` uses the shared `10:10:15` helper while upstream uses `10:10:13`; harmless here, but worth tightening if the test is edited.

## Non-blocking findings

- `Quote.price: Decimal` and `Quote.quantity: i64` is acceptable for R2. Upstream `Quote.quantity` is an `int`, the R2 aggregation methods return integer totals, and no upstream R2 case uses fractional quote quantity. Keep this as a deliberate exception if later plan text is read as "all quantity fields are Decimal".
- `OrderLock` storing `DateTime<Utc>` is acceptable for R2. The tested methods compare instants and use strict `now > lock_till` semantics, matching upstream. The `timezone` no-op is the main thing to revisit for R3 `Order.expires_at` and DST-sensitive behavior.
- `secs_delta` caps then truncates with `trunc() as i64`, matching upstream's `seconds = min(...); int(seconds)` behavior for the tested domain.
- Splitting upstream `test_order_lock_can_methods` into three Rust trials is acceptable. The manifest has one id per upstream parametrize row, and `tests/parity/main.rs` cross-checks registered names against the manifest.
- No §14(B) entry is needed for the three `test_order_lock_can_methods_*` rows. `MockClock` removes the clock tick granularity issue that source-notes listed as a candidate risk.
- R1 P2.1 is closed: `tests/parity_runner_smoke` now asserts both the `GateExit` variant and `gate.code()` for rows 1-13.

## Verified closures

- R2 scope is present: `rust-tests/parity-item-manifest.txt` has 30 active ids, with the 10 R2 ids appended after the 20 R1 ids.
- The parity harness registers the same 30 trials and checks both directions: every registered trial is in the manifest, and every manifest id has a registered trial.
- `cargo test --test parity` passed: 30 passed, 0 failed, 0 ignored; gate report showed manifest size 30, passed 30, failed 0, `Pass (exit 0)`.
- `scripts/parity_gate.sh` passed in release/all-features mode and returned exit 0 with the same 30/30 gate report.
- `tests/parity/excused.toml` is still present-empty: it contains comments only and has no `[[excused]]` rows.
- `OMSRS_R0_GATE=1 cargo test --test parity` also passed, confirming the present-empty excused file does not trip the R0 empty-set guard.
- The 13-row smoke matrix is green after the manifest grew, and each row now directly asserts the numeric exit code.
- No `#[ignore]` appears in `src`, `tests`, `Cargo.toml`, `rust-tests`, or `scripts`.
- `OrderLock`'s `clock` field compiles with `#[serde(skip, default = "clock_system_default")]`, and the default function returns `Arc<dyn Clock + Send + Sync>` as required.
- `cargo clippy --all-features --all-targets -- -D warnings` is clean.
- `cargo build` and `cargo build --no-default-features` are both warning-free.

## Non-regression checks

- `cargo test`
- `cargo test --test parity`
- `scripts/parity_gate.sh`
- `cargo test --test parity_runner_smoke`
- `OMSRS_R0_GATE=1 cargo test --test parity`
- `cargo clippy --all-features --all-targets -- -D warnings`
- `cargo build`
- `cargo build --no-default-features`
- `rg -n "#\\[ignore\\]|ignore\\]" src tests Cargo.toml rust-tests scripts` returned no hits.
- `rg -v "^\\s*(#|$)" rust-tests/parity-item-manifest.txt | wc -l` returned `30`.
- `rg -n "^\\s*\\[\\[excused\\]\\]" tests/parity/excused.toml` returned no hits.
