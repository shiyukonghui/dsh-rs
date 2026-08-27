# 设计：beyond 目标 —— A1 插件身份键模型收口（remove_plugin + case-4 验证 + 文档化偏差）

日期：2026-08-27
阶段：系统设计（瀑布流阶段 2，Phase A1）——本文档为阶段关卡工件。
依据：`.spec/service-assembly-a1/requirements.md`（定稿，用户确认 A/m-series/remove_plugin 语义）
+ fork registry/fiber 源码实证 + 现有实现自读。

---

## 1. 设计目标

按「文档化偏差收口」分支补齐 A1 剩余三件：
1. `Loader::remove_plugin(name)`（cordis `registry.delete(plugin)` 同径：**先**从 core registry 移除
   记录 + 从 loader `st.plugins` 移除 → dispose 该名所有存活 fiber）。
2. case-4 专用 m-test（m26）：模块消失 → self-dispose 合法（entry 不入 disabled、无 disable 写回）；
   对照：插件仍注册时 self-dispose → entry 落 disabled（case-7 既有语义复锁）。
3. A1 偏差文档化（identity.rs doc 补 case-4 触发 + DECISIONS 条目）。

**关键顺序不变量**：先删 registry 记录、**后** unload fiber——否则 seven_case case-4 在 unload 时
看到 name 仍在 → 误落入 case-7（disabled）。

## 2. 自下而上锚点（本阶段核实）

| 锚点 | 基址 | 用途 |
|---|---|---|
| cordis registry.delete | registry.ts:258-267（删 Map 记录 + 逐 fiber.dispose） | remove_plugin 语义 |
| cordis seven_case case-4 | index.ts:140-156（`!ctx.registry.has(callback)` → 合法 return） | 判定语义 |
| Rust seven_case case-4..7 | loader.rs:220-256（case-4 `contains_key` Continue；case-7 落 `disabled=true`+disable 写） | 触发点 |
| fiber 级 dispose | context.rs:1835-1842 `ctx.unload`（dispose_fiber → internal/plugin(dispose) → run transition） | unload 通道 |
| core registry | runtime.rs:132 `pub registry: HashMap<String, RuntimeRecord>`；`rec.fibers` | 枚举/删除 |
| 现有身份模型 | identity.rs（Arc token）+ loader.rs:467-512（register/replace + generation） | A1 已形核 |
| case-7 对照 | seven_case case-7（loader.rs:245-256）：插件仍注册 → self-dispose → disabled | m26-T2 |

## 3. 设计分解

### S1（`Loader::remove_plugin`，loader.rs）

```text
pub fn remove_plugin(&self, name: &str) -> Result<usize, CordisError> {
    // 1. 先从 core registry 删记录（取该名 fibers）——case-4 判定依赖「name 已消失」
    let fibers: Vec<FiberId> = self.ctx.with(|rt| {
        match rt.registry.remove(name) {
            Some(rec) => rec.fibers.clone(),
            None => Vec::new(),
        }
    });
    // 2. loader 记录移除
    let existed = self.state.borrow_mut().plugins.remove(name).is_some();
    // 3. 逐 fiber dispose（internal/plugin(dispose) → seven_case case-4：name 已消失 → Continue）*
    for fid in fibers.clone() { let _ = self.ctx.unload(fid); }
    Ok(fibers.len())
}
```
- 未注册 name → `Ok(0)`（幂等；existed=false）。
- 返回值 = 被 dispose 的 fiber 数（诊断/观测，cordis 返回 runtime）。
- entry 的 `e.identity` 保留（供 stale 判定）；entry 本身 **不**落 disabled、**不**写 `disable:`。

### S2（m26_case4.rs 红测）

| # | 场景 | 断言（绿） |
|---|---|---|
| T1 | create entry "a"(plugin "p") → `remove_plugin("p")` | `Ok(fibers)`；entry fiber 状态 Disposed/Pending；`entry.options.disabled == false`；`take_writes()` **无** `disable:a`；`plugin_identity("p") == None` |
| T2（对照） | create entry "a"(plugin "p"); 插件仍注册时 `ctx.unload(a_fiber)` | entry `disabled == true`；`take_writes()` **有** `disable:a`（case-7 复锁） |
| T3 | `remove_plugin("ghost")` | `Ok(0)` 幂等；无副作用 |

- T1 红（编译期无 `remove_plugin`）→ 实现 S1 → 绿。

### S3（文档化偏差）

- identity.rs 模块 doc 补一段：身份 token 模型 = 回调身份 的 Rust 同构；`remove_plugin` = registry.delete；
  case-4 语义 = 模块消失时 self-dispose 合法、entry 不落 disabled；**同名多实现同时共存 = 宿主层责任**
  （dsh-rs 宿主-owned 注册表，一名一当前实现，replace/remove 皆顺序换代）。

## 4. 实现顺序（TDD）

1. S2 m26 红（remove_plugin 缺失 → 编译红）。
2. S1 remove_plugin 实现 → m26 绿。
3. S3 文档化。
4. 阶段 4 回归：workspace（205→206）+ clippy 0 + verify-diff 23/23 + m16/m18 保持。
5. 阶段 5：serve 冒烟 + acceptance。

## 5. DIV / 让步清单

- **DIV-A1-1（文档化偏差·本轮）**：一名一实现（顺序换代）+ 身份 token = 回调身份同构；同名多实现
  同时共存由宿主 import 层负责（HANDOFF「或显式声明为文档化偏差」分支）。不做 (来源,name)+版本 键。
- **DIV-A1-2**：case-4 用 m-series（无 TS delete-plugin 场景；DSL 不含 registry 删除操作）。
- **DIV-A1-3**：`remove_plugin` 不写持久化（cordis delete 不动 entry.options；宿主自治后续处置）。

## 6. 部署与回滚（阶段 5 预案）

- 部署：纯增量 API + case-4 触发路径（此前不可达）；既有 replace/HMR 零行为变化。
- 回滚：`git revert` 阶段提交（remove_plugin + m26 + identity doc）。
