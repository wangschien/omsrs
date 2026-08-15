# omsrs 审计（2026-08-15）

仓：`/home/ubuntu/omsrs`。HEAD **`f02b43fa`**（与 kbot `deps.pin.toml` 的 `omsrs` rev 一致）。  
对照 hold-12 真钱漏记（L3 111 笔 vs OMS `Fill` 104 笔）和 kbot 规格 `kbot/docs/specs/OMS_CANCEL_FILL_RACE_2026_08_15.md`。

**总判：**

- hold-12 那 7 笔静默丢：**kbot 壳已修**（`da4ab86` + `24a30dc` + `d0ae3a2`：Canceled 缓删 30s + NoRow off-book）。那窗是旧二进制的历史账，不会自己长回 7 笔。
- omsrs 对「行还在的 Canceled + 迟到 Fill」本来就响亮（入账 + `PostTerminalFill`）。不要改软。
- 深审另开一条 **核 HIGH**：`Canceled` 定终态不校验 `filled + remaining == qty`。`filled=0, remaining=0, authority_complete=true` 会 Release。这和 hold-12「撤成空成交」是同一家族，但 **不是已修的 gc 洞**。

不要为了对账去改「策略 net = OMS 加总」。

---

## 0. 仓里其实是两套东西

| 块 | 路径 | kbot live 用不用 |
|---|---|---|
| omspy 移植：`Order` / `Broker` / Paper / Virtual / Replica / Compound | `order.rs` `broker.rs` `virtual_broker.rs` … | **基本不用**（pbot / 旧纸面） |
| **Kalshi 真钱生命周期** | `src/lifecycle.rs` | **用**：submit / cancel / fill / reconcile / Halted |
| YES 库存帮手 | `src/yes_inventory.rs` | kbot 挂 `OmsrsYesInventory`，但 latch 后净仓权威是 WS `post_position` |

审计真钱，看 `lifecycle.rs`。237 项 omspy parity 绿，罩不住 Kalshi 撤/成竞态。

`lifecycle` 自称：纯函数、无 I/O；journal / HTTP / WS 是壳（kbot `LiveOmsBook`）。终态只由 `try_finalize_terminal` 写出；`fill_id` 幂等；`Halted` 冻住、不接 `ReconcileResult`（宿主不能靠 reconcile 解 Halted 行）。

---

## 1. hold-12 和核的职责切分

7 笔私有成交：OMS 流水是 `Canceled` + `attributed_fill_qty=0`，没有 `Fill`。策略 `net_centi` 已随 WS `post_position` 变了。

**若行还在、状态是 `Canceled`，omsrs 会：**

`apply` 见 `is_restart_frozen`（含 `Canceled`）→ `restart_safe_handle` → 新 `fill_id` **入账** → `HaltReason::PostTerminalFill` + 必要时 `ReserveFull`。

钉：`g5_new_qty_fill_on_partial_terminal_post_terminal_halt`（`lifecycle.rs` ~9286：冻在 `Canceled` 上再来新 fill → `PostTerminalFill`，不是 `OverFill`）。`inv_restart_safe_frozen_late_fill_accounts_and_halts` 钉 `UnknownNoMatch` 上的迟到 fill。

**hold-12 当时：** 旧 kbot 立刻删 `Canceled` → fill 变 `Ok(false)` 空过。  
**现在 HEAD：** 缓删 + NoRow 补记 + `unattributed_fill`。omsrs 核这条不用再动。

| 层 | 当时 | 现在 |
|---|---|---|
| omsrs：冻态迟到 fill | 对（入账 + halt） | 仍对，别改软 |
| kbot：Canceled gc | 立刻删 | **30s 缓删** |
| kbot：行不在 | 静默 | **off-book Fill + 闩** |
| 策略净仓信 WS | 对 | 仍对 |

---

## 2. 发现

### 已归宿主、且已修（不是 omsrs 未修洞）

Canceled 立删 → fill `Ok(false)` 静默：kbot `da4ab86` 起已缓删 + off-book。规格 `kbot/docs/specs/OMS_CANCEL_FILL_RACE_2026_08_15.md`。

### HIGH — `Canceled` 定终态不校验 filled+remaining（**已撤销,见 §6**——修法被真 REST 数据证伪,勿按本节实现）

`lifecycle.rs` ~3159–3189：`BackfillOrderStatus::Canceled` 走 `try_finalize_terminal`，只看 `attributed == fill_obligation == venue_filled` 且 `authority_is_fresh()`。**没有** `venue_filled + venue_remaining == qty`。开仓零成交 submit 路径反倒要求 `remaining_count == qty`（~2371）。

Kalshi 撤完常报 `remaining=0`；若 `fill_count` 还是 0、宿主又标 `authority_complete`：

`ReconcileResult { Canceled, filled:0, remaining:0, fills:[], authority_complete:true }` → `Canceled` + `ReleaseReservation`，`attributed=0`。之后 WS 成交变成冻态迟到 fill（或再被 gc）。

现有 cancel 权威测试全是 `filled=0, remaining=10` 这种有剩余。**没有** `filled=0, remaining=0` 夹具。

~~修法：Canceled 必须 leftover 恒等（`filled+remaining==qty`）；`remaining=0 && filled=0 && qty>0` 不得 latch/release。~~ **已撤销(§6):真 REST 987/1000 canceled 报 0/0 常态,恒等 0/1000 成立——任何此类门都 false-halt 超时恢复流。**

### 核里已钉、要对齐的契约

| 契约 | 位置 | 含义 |
|---|---|---|
| 冻态新 fill → 入账 + `PostTerminalFill` | `lifecycle.rs` ~1301–1347 | 不许吞迟到成交 |
| 同 `fill_id` 不同 payload → `ConflictingFillPayload` | HaltReason | 不许静默改写 |
| 超单量 → `OverFill` | HaltReason | |
| `Halted` 不接 `ReconcileResult` | 状态机 + 注释 | 解冻只走 REST/探测，不是再 reconcile |
| Cancel 重试幂等 | HEAD `66ad81f` / `f02b43f` | `CancelPending` + 再 `CancelRequested` 不再 reject |

这些是 omsrs 该守的。kbot 把行删了，等于绕过整组闸。

### MEDIUM（核 / 接线）

| # | 问题 | 说明 |
|---|---|---|
| M1 | `YesInventory` 不是 live 净仓权威 | `set_net_yes_contracts` / `on_fill_contracts` 给 kbot 双写。第一笔 `post_position` 后 `NetAuthority::VenuePosition`，OMS 加总会被 `gc` 成 0。别把 `YesInventory` 再当成「真仓」 |
| M2 | 两套 OMS 心智 | omspy `Order.quantity` 是张；lifecycle fill `qty` 在 kbot 是 **centi**。混用 Broker API 和 lifecycle 会错 100 倍。live 只走 lifecycle |
| M3 | `PostTerminalFill` 后状态是 `Halted` | 正确响亮。kbot 必须留 Halted 行（C4 已留）并停新开。若只 journal 不 halt，会和核不一致 |
| M4 | journal `OrderTerminal` 抬高 attributed，不写入 `applied_fills` | 残缺 journal 重放后再来同一 `fill_id` 会当新成交。fold 时应 `attributed == Σ applied` |
| M5 | `ReconcilePending` + `CancelRequested` 仍 `reject_illegal` | 幂等只钉了 `CancelPending`。第一笔 `CancelOutcome` 后已是 `ReconcilePending`，宿主再重试 cancel 会 reject |

### LOW

| # | 问题 |
|---|---|
| L1 | omspy parity 与 lifecycle 几乎不交。`cargo test` 绿不等于 Kalshi 竞态绿 |
| L2 | 仓文档仍写 v0.1–0.3 omspy 故事；`lifecycle` 是后来 `57be79f` 迁进来的，README 没把它当主产品面 |

---

## 3. 绿灯罩不住什么

- **omspy 237 项**：不跑 `lifecycle::apply`。  
- **kbot `Ok(false)`**：行不在，根本不进 omsrs，lifecycle 测试全绿。  
- **hold-12 日志无 FATAL**：没走到 `Err(PostTerminalFill)`，不是核吞了 Err。

冻态迟到 fill 的钉是真的——行在就会喊。`filled=0+remaining=0` 定终态 **没有钉**，全绿也挡不住 HIGH。

---

## 4. 下一刀（不要开错仓）

| 做 | 仓 | 状态 |
|---|---|---|
| Canceled 缓删 + no-row 补记 | **kbot** | **已并** |
| 策略净仓保持 `VenuePosition` | **kbot** | 保持 |
| 冻态 Fill → `PostTerminalFill` | **omsrs** | 已有，别改软 |
| Canceled 终态校验 `filled+remaining==qty` | **omsrs** | **撤销**（§6:被真 REST 证伪,正向钉防再加） |

**不要：** 无行也当核入账；Canceled 默默吃 fill 且不 Halt；用 OMS `Fill` 张数当经济成交额。

---

## 5. 一句话

那 7 笔的 **壳已经修了**。omsrs 对「行还在的迟到成交」本来就会喊。~~还没修的是核里另一条：撤单权威若报 `filled=0, remaining=0` 仍会放行终态。~~ **复核后撤销(§6):0/0 是 Kalshi canceled 常态表示,放行终态是正确行为;防线在 invariant 2/PostTerminalFill/壳 off-book。**

---

## 6. 复核(2026-08-15 双审二轮,HIGH **撤销**)

§2 的 HIGH(Canceled 定终态不校验 filled+remaining)修法在实现双审中被**真 REST 数据推翻**:

- **判据来源错误**:journal 的 Canceled `ReconcileObserved` 几乎全部来自 `on_cancel_http_success` 的**壳自合成回显**(本地 attributed/remaining 算出),对场端语义零鉴别力;「零成交撤恒报 remaining==qty」是回显假象。
- **真 REST 实测**(auditor o2a 只读探针,`GET /portfolio/orders?status=canceled` 1000 单,脚本留存 o2a `/data/tmp/audit_canceled_probe{,2}.py`):**987/1000 报 fill_count=0/remaining_count=0**(撤后清零=常态表示);13/1000 部分成交同样 remaining=0;恒等 `filled+remaining==qty` **0/1000** 成立。
- **真 REST 进核的唯一路径** = SubmitUnknown 超时恢复 backfill——恒等门或 0/0 零踪迹门在此 **98.7% false-halt**(资金锁死,差分实跑坐实)。
- **「撤成空成交」的真防线**(本就存在):fill 已投影 ⇒ `note_fill_evidence` 抬 obligation ⇒ invariant 2 挡;未投影且行在 ⇒ `PostTerminalFill`;行不在 ⇒ 壳 off-book(已修)。REST 瞬时少计 + WS 同丢 = 本地任何门不可见,无门可加。
- **处置**:门撤销;正向钉 `inv_canceled_zero_zero_rest_is_normal_release`(两路径:ReconcileResult 0/0 与 backfill 0/0 必须正常 Canceled+Release)防将来再加守恒门。

教训:[[只用实测值]] 的升级——**实测也要测对流**(壳回显 ≠ 场端观测)。

## 7. 欠账(durable,末轮 auditor MED 落账)

- **`Effect::BackfillFills` / `RequestAuthorityReconcile` 在 kbot 壳无处理者**(pre-existing):核在 `try_finalize_terminal` G2/H1 与 `continue_reconcile_pending` 发射,壳 `apply_effects` 堆进 `other_effects` 永不消费 ⇒ `ReconcilePending` 只能靠 WS fill 自然到达解套,核的「可恢复」承诺在壳侧半开路。影响面:not_ready 路径的滞留时长(窗尾清场兜)。归属 kbot 壳,另开一线,勿与本审计线捆绑。
- site-2(`finalize_reconcile_target` Canceled 臂)无撤销注释、正向钉不覆盖该臂独活复活——该臂 `target.venue_filled==0` 的 finalize 形态经现路径不可达(fill 驱动必抬 attributed),记录即可。
