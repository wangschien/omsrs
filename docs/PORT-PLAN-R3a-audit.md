# R3.a audit prompt

R2 ACKed (see `docs/PORT-PLAN-R2-audit-result.md`). R3 is a 64-item phase
per PORT-PLAN §8; to keep the audit loop reviewable, R3 lands in two
sub-chunks:

- **R3.a** (this commit): 55 non-SQLite items from `tests/test_order.py`.
- **R3.b** (next): 9 SQLite-backed items (`test_order_create_db*`,
  `test_order_save_to_db` through `test_order_save_to_db_update_order`,
  `test_new_db*`). Lands under the `persistence` feature.

R3.a commit just dropped locally (not yet pushed pending your ACK). Full
manifest is now 85 items (20 R1 + 10 R2 + 55 R3.a); gate passes with 0
excused.

## Scope

55 new trials covering:
- Simple / property: `test_order_{simple, id_custom, is_complete,
  is_complete_other_cases, is_pending, is_pending_canceled,
  is_pending_rejected, is_done, is_done_not_complete, has_parent}` (10)
- `update()`: 9 trials including the "don't update when terminal" matrix
  and `pending_quantity` recalculation.
- Expiry: `test_order_{expires, expiry_times, has_expired}` (3)
- Execute: 5 trials (default / kwargs / do-not-reexecute / completed-skip).
- Modify / Cancel basics: 3 trials.
- Modify variants: 4 trials (quantity / by_attribute / extra / frozen).
- max_modifications: 2 trials.
- Clone: 2 trials.
- Timezone: 1 trial (see design note).
- Lock interaction: 3 trials driving `add_lock(1/2, seconds)` + counted
  broker calls across a stepped clock.
- Attribs-to-copy coverage: 11 trials across execute / modify / cancel.
- Persistence edge-cases: 2 "no connection" trials (the ones that return
  False without any real SQLite).

## Deliverables

- `src/broker.rs` — `Broker` trait (order_place / order_modify /
  order_cancel + 3 attribs_to_copy_* default-None hooks). Kwargs are
  `HashMap<String, serde_json::Value>` to mirror upstream `**kwargs`.
- `src/persistence.rs` — `PersistenceHandle` trait + `PersistenceError`
  scaffold. SQLite impl lands in R3.b.
- `src/order.rs` — `Order` struct (~40 fields), `OrderInit` builder,
  `from_init_with_clock`, all lifecycle + query methods listed above.
- `src/models.rs` — new `OrderLock::unlocked_with_clock` constructor
  (see design note below).
- `tests/parity/mock_broker.rs` — recording `MockBroker` with
  `place_calls()`, `modify_calls()`, `cancel_calls()`, `*_call_count()`,
  `set_place_return(...)`, `set_place_side_effect(...)` and
  `set_attribs_to_copy_{execute,modify,cancel}(...)`.
- `tests/parity/test_order.rs` — the 55 trials.
- `rust-tests/parity-item-manifest.txt` — 55 ids appended under an
  `R3.a` section; total now 85.

## Design choices worth scrutinising

1. **`OrderLock::unlocked_with_clock`.** Upstream `Order.__init__` does
   `self._lock = OrderLock(timezone=self.timezone)`, and `OrderLock()`
   sets all three `*_lock_till` to `pendulum.now()`. In real pendulum,
   microsecond advance between init and the first `can_modify` call
   keeps `can_modify == True`. With a frozen `MockClock`, same init
   permanently blocks modify.

   Our fix: `Order::from_init_with_clock` builds the embedded
   `OrderLock` via the new `unlocked_with_clock` constructor, which
   seeds `*_lock_till` to `UNIX_EPOCH`. `can_*` is trivially true until
   `add_lock()` is called. Standalone `OrderLock::with_clock` (used by
   the R2 `test_order_lock_*` trials) still initialises to `clock.now()`
   and is unchanged. Observable behaviour matches upstream; only the
   internal snapshot differs.

2. **Broker kwargs as dynamic `HashMap<String, Value>`.** A typed
   `OrderArgs` struct would fight upstream's open-ended kwargs —
   `test_order_modify_frozen` passes `tsym="meta"` as a passthrough;
   `test_order_modify_attribs_to_copy_broker` asserts the exact set of
   keys. JSON `Value` keeps the test-assertion shape (`kwargs ==
   expected`) readable.

3. **`Decimal` serialised as string inside broker kwargs.** Inside the
   dynamic kwarg map, `price`, `trigger_price`, `average_price` are
   emitted via `Decimal::to_string()` and asserted against string
   literals (`json!("650")`). Upstream asserts `kwargs["price"] == 650`
   which works because pydantic's floats can round-trip; the Rust
   port's Decimal → string keeps precision, at the cost of requiring
   string literals in test expectations. Flag if you'd prefer a
   numeric bridge.

4. **`Order.timezone` stays a String label; all instants UTC.**
   Consistent with R2's OrderLock decision. `test_order_timezone`
   upstream asserts `order.timestamp.timezone.name ==
   pendulum.now("local").timezone_name` — non-portable without host-tz
   dependence, so the Rust port only asserts the default label
   (`"local"`). Candidate §14(B) entry if you require the full
   upstream semantics at the phase gate.

5. **R3.a vs R3.b split.** 9 SQLite-backed trials are deferred to R3.b;
   2 "no connection" edge-case trials land in R3.a because they only
   exercise the `None` no-op path. R3's phase gate will ACK only after
   R3.b lands and both green together.

## What to verify

- `cargo test` green (all targets).
- Parity gate exits 0 with 85/85.
- `cargo clippy --all-features --all-targets -- -D warnings` clean.
- `cargo build` + `cargo build --no-default-features` warning-free.
- No `#[ignore]` anywhere.
- `excused.toml` still present-empty.
- Manifest size 85, smoke matrix unchanged.
- Any R3.b work I've accidentally pulled forward (or vice versa).
- `OrderLock::unlocked_with_clock` observability deviation — acceptable
  as an internal construction detail, or should the plan / source-notes
  be updated to document it?
- `Decimal` string serialisation in broker kwargs — acceptable, or
  should I add a numeric bridge before R3.b freezes the API?
- `test_order_timezone` — acceptable as a label-only assertion, or
  promote to §14(B) for the full upstream check?

## Output format

Same as prior audits. Write the result to
`docs/PORT-PLAN-R3a-audit-result.md`.
