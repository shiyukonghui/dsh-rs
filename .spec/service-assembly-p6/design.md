# 设计：服务装配单元 Phase 6 — A2 !!js 求值作用域绑定注入服务

日期：2026-08-27
阶段：系统设计（瀑布流阶段 2，Phase 6）——本文档为阶段关卡工件。
依据：`.spec/service-assembly-p6/requirements.md`（需求定稿，A2-SCOPE=B / A2-BARE=A 用户确认）。
参照：fork `config/utils.ts`（`with(ctx) eval` + `interpolate`）+ `lib/index.js:338/370`（入口扩展 Context）。

---

## 1. 设计目标

给 dsh-loader 的 `!!js`/`__jsExpr` 求值作用域绑定**目标纤维的注入就绪上下文**：`ctx` = `{ 注入服务名 →
Value }`（经 `get_value`），服务名同时注入为**顶层裸标识符**（对齐 fork `with(ctx)` 语义）；与显式键
（config/process/env/ctx）冲突时显式键优先。不改 dsh_eval 求值器核心，不改 internal/config 触发时序。

## 2. 自下而上锚点（本阶段核实）

| 锚点 | 基址 | 用途 |
|---|---|---|
| fork eval / interpolate | fork config/utils.ts:5-22（`new Function('ctx','expr','with(ctx){…}')`；`{__jsExpr}` 节点递归） | ctx 语义 |
| fork ctx | 入口扩展 Context（lib/index.js:338 `ctx.extend({[Entry.key]:this})`，服务混入属性） | 成员+裸标识符 |
| eval_scope | loader.rs:124-136（`{config,process,ctx:{},env:{}}`） | 扩展点（空 services = 现状） |
| internal/config 触发 | context.rs:742-748 waterfall 早于 `current.push(fid)`(753) | 绑定目标 = `args[0]=fid` |
| 服务值通道 | `get_value`（context.rs:1513，经 internal/get 拦截）＋ `get_raw_value`（Value/accessor） | 暴露 Value 服务；waterfall 可重入（每次新 WfChain） |
| 纤维注入名单 | `FiberData.inject: Vec<String>`（fiber.rs:91，pub）＋ `Cordis::with`（context.rs:159） | 取目标纤维注入名 |

## 3. 设计分解

### S1（dsh-loader eval_scope 扩展，API 兼容）

```text
pub fn eval_scope_with_services(config: &Value, process: &Value, services: &Value) -> HashMap<String, Value> {
    let mut scope = eval_scope_with_process(config, process);   // config/process/env/ctx:{}（显式键）
    if let Some(map) = services.as_object() {
        scope.insert("ctx".into(), services.clone());           // ctx = 服务对象（成员访问）
        for (k, v) in map {
            if !scope.contains_key(k) { scope.insert(k.clone(), v.clone()); }  // 裸标识符（显式键优先）
        }
    }
    scope
}
```

- 空 `services`（`{}`）→ 与现状等价（m3/既有单测零回归）。
- **注入点**：`eval_scope`（禁用表达式用）与 `eval_scope_with_process` 均委托于此（空 services）。

### S2（loader 绑定注入就绪上下文）

- **internal/config 监听器**（loader.rs:249-264）：从 `args[0]` 取目标 `fid` →
  `services = { name → _ctx.get_value(name) }`（遍历 `fiber(fid).inject`）；`eval_scope_with_services(...)`。
- **disabled 表达式**（loader.rs:104）：绑定**当前纤维**服务（best-effort；cordis 侧入口 ctx 亦含
  装载 realm 服务）——`ctx_services_for(current_fiber)`。
- 辅助（loader 内部）：`fn current_service_ctx(ctx: &Cordis, fid: Option<FiberId>) -> Value`。

### S3（m-series 红测，crates/dsh-loader/tests/m21_eval_ctx.rs）

| # | 红测 | 断言（绿） |
|---|---|---|
| T1 | provider 提供 Value 服务 `svc={k:42}`；消费者插件 `inject:["svc"]`，config `{"__jsExpr":"svc.k"}` | apply 读 config 得 42（裸标识符读注入服务） |
| T2 | `context = {"__jsExpr": "ctx.svc.k"}`（成员访问）+ 服务名=`config` 的服务不与显式键冲突 | ctx 成员 OK；`config` 仍为配置（显式键优先） |
| T3 | config 引用未注入服务名 `nope.x` | 求值失败 → fail-loud（保留原 config + `eval-error` 写回标记） |

- provider 经 `ctx.provide("svc", Arc::new(Value))`（Value 型服务）；T1/T2 消费者入口独立装载。

### S4（回归 + 可观测）
- m3_expr 三测 + 既有 loader m1-m21 全量 + workspace + clippy 0。
- serve 冒烟（无运行面破坏）。

## 4. 实现顺序（TDD）

1. **S1**：`eval_scope_with_services` + 委托重构（现有测试保持绿）。
2. **S2**：listener/disabled 绑定；**S3**：m21 红→绿。
3. **S4→阶段 4**：全量 workspace + clippy；**阶段 5**：serve 冒烟 + acceptance + DECISIONS。

## 5. DIV / 让步清单

- **DIV-6-1**：仅 Value 型服务暴露（`Arc<dyn Any>` 非 JSON，不暴露；cordis 暴露任意对象——Rust
  受限子集）。
- **DIV-6-2**：`get_value` 基于监听时刻（target-apply 前）的 store 可见性——祖先提供的服务可读；
  仅在目标深层隔离可见的极端场景受限（DIV 记录）。

## 6. 部署与回滚（阶段 5 预案）

- 部署：eval_scope 行为在空 services 下不变；entry 有注入 Value 服务时 `!!js` 可读——纯增量。
- 回滚：`git revert` 本阶段提交（S1+S2+S3 特征级整体）。
