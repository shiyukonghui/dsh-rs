# 需求结论：服务装配单元 Phase 7 — B1 Service 派生作用域实例 + 可调用服务

日期：2026-08-27
阶段：需求分析（瀑布流阶段 1，Phase 7）——本文档为阶段关卡工件。
状态：**定稿（范围用户确认：B1-SCOPE=A 可调用+派生全流程；B1-PROOF=A m-series 锁）**
依据：`docs/SERVICE-ASSEMBLY-HANDOFF.md` §3 B1（[Service.extend] 派生作用域实例 + 可调用服务，
`service.ts:65-73`）+ fork 源码实证 + §0 验收。

---

## 1. 目标（Top-down → Bottom-up）

第一性原理（把 B1 剥到基本事实）：
- **可调用服务**：cordis `createCallable`（fork utils.ts:226/logger.ts:208）——服务值本身可**被调用**
  （如 `ctx.logger('x')`）且同时带方法（`ctx.logger.exporter()`）。fork LoggerService 即 callable。
- **派生作用域实例**：`Service[extend]`（service.ts:65-73）——从现有服务派生一个绑定另一作用域的
  实例（callable → 重建 callable；否则 `Object.create(this)` + `Object.assign(props)`），使服务方法
  观察**访问方纤维**的 ctx（isolate/intercept/事件）。
- **Rust 现状**：`Service = name + check`（service.rs）；`Provide_service` 注册为 `Arc<dyn Any>`；
  `get(name)` 返回平 Arc；**无**通用 invoke（可调用）/ extend（派生）。`ctx.logger(name)` 是写死的
  `Cordis` 方法（context.rs:1728），不是通用服务原语。

B1 补齐**服务作者通用原语**（engine 可表达 cordis 的可调用/派生语义），m-series 锁语义。

**验收** = Service trait 增 `extend`（self: Arc<Self>，默认恒等）+ `invoke`（默认不可调）+ 运行时
`srv_store`（Service 通道）+ `ctx.get_extended(name)` / `ctx.call_service(name, args)` + m-series
（派生子纤维绑定/默认恒等/可调用/不可调用 Err）全绿 + 既有 201 目标零回归 + clippy 0 + serve 冒烟。

## 2. 非目标

- **不**改生产 `ctx.logger(name)`（回归风险：Web serve 与 trace 依赖；保持方法；原语供服务作者，
  DIV-7-1）。
- **不**做 TS host / golden（B1-PROOF=A 用户确认——TS host 无 Service 子类/可调用场景；等价证据退档）。
- **不**做 `[Service.init]` class-plugin 构造即注册重构（A6 已覆盖 init 生成器；Service 构造注册渠道
  保留 provide_service，不引入 new-Service 语法）。
- **不**做 B2 Group 折叠 / B4 config simplify / A3（后续优先级）。

## 3. 假设（复盘确认）

- **H1 通道**：运行时加 `srv_store: { name → Arc<dyn Service> }`（仅 `provide_service` 写入；
  `get`/`get_value` 的 Any 通道不动）→ `get_extended`/`call_service` 经 srv_store，零侵入既有。
- **H2 派生态**：`fn extend(self: Arc<Self>, ctx: &Cordis) -> Arc<dyn Service>`（默认恒等 `self`）；
  自定义派生实例携带访问方 ctx 信息（m-series 断言）。
- **H3 可调用**：`fn invoke(&self, ctx: &Cordis, args: &[Value]) -> Result<Value, CordisError>`
  （默认 Err——非可调用服务 fail 明确）；`call_service` 对非 Service/不可调用 → 明确错误。
- **H4 证据**：m22 T1-T4 + 单测（B1-PROOF=A）。

## 4. 硬约束

- `Service` trait 保持对象安全（`self: Arc<Self>` receiver 对象安全）；`dyn Service: Any + Send + Sync`。
- 新语义落 m22 红→绿；既有 201 目标 + workspace + clippy 0。
- DECISIONS 追加；改动 → git 提交 → 决策条目互查。

## 5. 现状缺口（自下而上核实，带依据）

| 项 | 现状（源码实证） | 结论 |
|---|---|---|
| fork 可调用 | `createCallable`（fork utils.ts:226/logger.ts:208；`Service[invoke]` → callable） | ✅ 参照已锁定 |
| fork 派生 | `Service[extend]`（service.ts:65-73：callable→rebuild；else `Object.create(this)`+assign） | ✅ 参照已锁定 |
| Rust Service | `name + check`（service.rs:9-17），`provide_service` 注册 `Arc<dyn Any>`（context.rs:1450） | ⬜ 缺 invoke/extend 原语 |
| Rust 服务通道 | `get`（Any 下转型）/ `get_value`（Value 服务）——**无 Service 类型直达通道**（Any→dyn Service 无法下转型） | ⬜ 需 srv_store 通道 |
| Rust logger | `ctx.logger(name)` 写死方法（context.rs:1728）；`logger_auto` 读 intercept + 纤维名 | ✅ 保持（DIV-7-1） |
| 测试落点 | m20/m21 先例（dsh-loader tests）+ dsh-core 单测 | m22 |

## 6. 测试与验收标准（阶段关卡）

- **T1 派生**：父纤维 provide_service（custom extend 返回带访问方 ctx 信息的派生实例）→ 子纤维
  `get_extended("svc")` 得**子绑定**派生（≠ 基实例）。
- **T2 默认恒等**：无 custom extend 的服务 → `get_extended` 返回同一 Arc（ptr_eq）。
- **T3 可调用**：含 `invoke`（加和）的服务 → `call_service("calc","add",[1,2])` → 3；其结果可序列化。
- **T4 不可调用**：无 invoke 的服务 → `call_service` 返回明确 Err（非静默）。
- **回归**：ctx.logger(name) 仍工作；workspace + clippy 0；serve 冒烟（部署阶段）。

## 7. 决策收敛

| 决策 | 结论 |
|---|---|
| B1-SCOPE | **A：可调用 + 派生全流程**（用户确认）——invoke + extend + srv_store + get_extended/call_service + logger 演示（DIV-7-1 不改生产 logger） |
| B1-PROOF | **A：m-series 锁**（用户确认）——无 golden，m22 T1-T4 + 单测 |

## 8. 遗留边界

- 生产 logger 不改（DIV-7-1）；Service 构造注册渠道保持 provide_service（不引入 new-Service 语法）。
- 后续优先级：B2 Group 折叠 / B4 config simplify + A3 动态 check spike。

## 复盘追问结论（需求阶段已向用户确认）
- **假设**：srv_store 独立通道（H1）、派生态（H2）、可调用默认 Err（H3）、证据 m-series（H4）。
- **缺失信息**：fork 真实消费 extend/callable 的服务形态（多为 TS dsh-* 包，非 Rust engine 需求）
  ——已向用户点明，确认 B1 以**服务作者通用原语**交付 + m-series 证明，不追 TS 逐行等价。
- **常见错误**：把「可调用服务」误解为「给 ctx 加一堆方法」或反向把 logger 强改成 Service 破坏
  Web/trace 回归——本轮以独立 srv_store + 演示服务交付，生产路径逐字节不动。
