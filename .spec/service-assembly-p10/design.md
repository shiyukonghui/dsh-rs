# 设计：服务装配单元 Phase 10 — A3 动态 check spike（m25 parity 锁定）

日期：2026-08-27
阶段：系统设计（瀑布流阶段 2，Phase 10）——本文档为阶段关卡工件。
依据：`.spec/service-assembly-p10/requirements.md`（定稿）+ cordis reflect/fiber 源码实证。

---

## 1. 设计目标

**验证型 spike**：不产生生产代码改动，用一个 m-series 测试（`m25_dynamic_check.rs`）把「动态 check
再求值」与 cordis 的触发点语义锁定为 parity，作为 A3 闭环证据。若 m25 红则回退到需求阶段重新评估
（红期回归流程）。

## 2. 自下而上锚点

| 锚点 | 基址 | 用途 |
|---|---|---|
| cordis provide/unprovide notify | reflect.ts:277-305（`if ACTIVE notify`；disposer `remove + notify + await allSettled`） | 触发点 1/2 |
| cordis 状态翻转 notify | fiber.ts:588-594（ACTIVE↔NON-ACTIVE 时 notify 已提供服务） | 触发点 3 |
| cordis `_checkImpl` | fiber.ts:597-609（strict `_getImpl` + `impl.check` 求值；不成立删 store） | 再求值语义 |
| cordis `_refresh`/epoch | fiber.ts:611-639（epoch = 提供者 uid 拼接；INACTIVE → unload） | 依赖激活/失效 |
| Rust provide/unprovide/finish_load notify | context.rs:1427/1437 + runtime.rs:703-722 | 触发点 1/2/3 等效 |
| Rust reload = 卸载+重 apply | `update_with` → run_unload（disposers）→ apply → finish_load notify | 谓词翻转生效径 |
| 静态门（既有） | m7_await:73-91 + scenario-10 golden | 对照基线 |

## 3. 设计分解

### S1（m25 测试，5 断言序列）

```text
provider: apply → provide_with("svc", v1, check = |{ flag.load })   // Arc<AtomicBool>
consumer: inject ["svc"]

1. flag=false → create both → provider Active, consumer Pending          [静态门]
2. flag=true（无任何操作）→ consumer 仍 Pending                         [纯翻转非反应式=parity]
3. update_with(provider, {v:2}, false) → await_idle → consumer Active    [重载+true→激活]
4. flag=false + update_with {v:3} → consumer 回 Pending                  [重载+false→失效]
5. flag=true + update_with {v:4} → consumer 再 Active                    [往返]
```

- `update_with` 触发 provider 卸载（provide disposer 跑 `remove_impl+notify`）→ 重 apply re-provide
  （谓词读当前 flag）→ finish_load notify → consumer 按 `check_impls`+`refresh_fiber` 重算。
- 断言用 `loader.fiber + cordis.fiber_state`（与 m7/T 同模式）。
- `flag` 以 `Arc<AtomicBool>` 共享（CheckFn 为 `Box<dyn Fn() -> bool>`，Send+Sync 约束）。

### S2（红期回归预案）

若 m25 任一断言红 → 证明触发点/身份偏差（如 reload 未跑 disposer、notify 未传递）→ 回需求阶段
补根因分析，再定修复（不在此阶段偷偷打补丁）。

## 4. 实现顺序（TDD）

1. **S1**：m25 红（预期绿——机制已在；红仅当偏差存在）。
2. **红绿判定**：绿 → 锁定 parity，阶段 3 完成；红 → 走 S2。
3. **阶段 4 回归**：workspace + clippy + verify-diff 23/23 + m24/m17 保持。
4. **阶段 5**：serve 冒烟 + acceptance。

## 5. DIV / 让步清单

- **DIV-10-1**：动态翻转不可 golden（TS 场景 DSL 无运行期 flag 翻转）；m-series 锁定（spike/pass
  判定明确——非逃逸证据）。
- **DIV-10-2**：谓词翻转不加自动 notify 广播（cordis 非反应式同位；加了即越界）。
- **DIV-10-3**：A3 闭环 = 「谓词存在（既有静态证据）+ 动态触发点 parity（m25）」，**零生产改动**。

## 6. 部署与回滚（阶段 5 预案）

- 部署：无生产代码路径变化（纯测试锁定）。
- 回滚：撤 m25 + acceptance 工件提交即可。
