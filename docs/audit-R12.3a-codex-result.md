R12.3a ACK — proceed to R12.3b

## Checklist

| Item | Status | Evidence (file:line) | Notes |
|---|---|---|---|
| 1. Sync parity body-for-body | PASS | src/order.rs:559, src/order.rs:565, src/order.rs:568, src/order.rs:591, src/order.rs:603, src/order.rs:609, src/order.rs:612, src/order.rs:978, src/order.rs:984, src/order.rs:987, src/order.rs:1004, src/order.rs:1016, src/order.rs:1022, src/order.rs:1026 | `execute_async` matches sync `execute` apart from the two required awaits. Sync and async both merge copied attrs after defaults, then filter caller kwargs against default keys. |
| 2. Broker trait plumbing | PASS | src/order.rs:980, src/order.rs:987, src/order.rs:1022, src/order.rs:1035, src/order.rs:1042, src/order.rs:1101, src/order.rs:1113, src/order.rs:1122, src/order.rs:1129 | Params use `&(dyn AsyncBroker + Send + Sync)`. Each method awaits the async attrib hook before building args and awaits only the broker lifecycle call afterward. |
| 3. Test coverage (10 items) | PASS | tests/parity_async_order.rs:144, tests/parity_async_order.rs:171, tests/parity_async_order.rs:200, tests/parity_async_order.rs:235, tests/parity_async_order.rs:263, tests/parity_async_order.rs:291, tests/parity_async_order.rs:308, tests/parity_async_order.rs:332, tests/parity_async_order.rs:353, tests/parity_async_order.rs:384 | 8 sync-mirror lifecycle tests, 1 async attrib merge test, 1 sync semver type-check guard. |
| 4. AsyncMockBroker correctness | PASS | tests/parity_async_order.rs:56, tests/parity_async_order.rs:72, tests/parity_async_order.rs:86, tests/parity_async_order.rs:108, tests/parity_async_order.rs:117, tests/parity_async_order.rs:121, tests/parity_async_order.rs:125, tests/parity_async_order.rs:129 | Records place/modify/cancel calls, drains queued place returns front-to-back with `remove(0)`, and returns configured attrib options for all phases. |
| 5. Persistence caveat documentation | PASS | src/order.rs:965, src/order.rs:971, src/order.rs:1024, src/order.rs:1026, src/order.rs:1103, src/order.rs:1104, /home/ubuntu/pbot/Cargo.toml:21 | Doc comment calls out sync persistence; async execute/modify call `save_to_db()` at the same points as sync. pbot depends on `omsrs` by path without enabling `persistence`. |

## Findings

### Blocking

- None.

### Non-blocking

- Copied attributes can overwrite execute default keys in both sync and async because defaults are inserted first, then `other_args` is inserted (`src/order.rs:571`, `src/order.rs:588`, `src/order.rs:990`, `src/order.rs:1001`). This is not an async divergence; R12.3b should preserve the actual sync behavior unless a separate contract change is approved.

### Nits

- `tests/parity_async_order.rs` says it mirrors 9 sync tests, but the concrete harness is 8 sync mirrors plus the async-specific attrib test and the sync semver guard (`tests/parity_async_order.rs:3`, `tests/parity_async_order.rs:144`, `tests/parity_async_order.rs:353`, `tests/parity_async_order.rs:384`).
- AsyncMockBroker does not expose a `set_place_side_effect` helper name, but repeated `set_place_return` calls feed the same front-drained queue used by `order_place` (`tests/parity_async_order.rs:72`, `tests/parity_async_order.rs:110`, `tests/parity_async_order.rs:117`).

## Semver / parity spot-checks

- Semver guard: sync signatures remain `execute(&dyn Broker, ...)`, `modify(&dyn Broker, ...)`, and `cancel(&dyn Broker, ...)`; async methods are additive after the sync impl (`src/order.rs:559`, `src/order.rs:619`, `src/order.rs:775`, `src/order.rs:951`, `tests/parity_async_order.rs:384`).
- Body-for-body parity: execute/modify/cancel gates, arg construction, copied-attr merge, filtered execute kwargs, modify broker-key override, and `num_modifications` increment match sync except for required awaits (`src/order.rs:565`, `src/order.rs:625`, `src/order.rs:684`, `src/order.rs:689`, `src/order.rs:775`, `src/order.rs:984`, `src/order.rs:1039`, `src/order.rs:1097`, `src/order.rs:1102`, `src/order.rs:1116`).
- `save_to_db()` remains sync and is called inline after async execute/modify success, same as sync execute/modify; cancel has no persistence call in either path (`src/order.rs:612`, `src/order.rs:691`, `src/order.rs:789`, `src/order.rs:1026`, `src/order.rs:1104`, `src/order.rs:1129`).
- Async params use the planned broker type `&(dyn AsyncBroker + Send + Sync)` on all three methods (`src/order.rs:980`, `src/order.rs:1035`, `src/order.rs:1113`).

## Recommendation

- Proceed to R12.3b.
- Carry forward actual sync lifecycle semantics exactly, especially copied-attribute merge order and modify counter behavior (`src/order.rs:588`, `src/order.rs:672`, `src/order.rs:684`, `src/order.rs:689`, `src/order.rs:1001`, `src/order.rs:1085`, `src/order.rs:1097`, `src/order.rs:1102`).
