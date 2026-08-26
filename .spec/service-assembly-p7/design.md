# 设计：服务装配单元 Phase 7 — B1 Service 派生作用域实例 + 可调用服务

日期：2026-08-27
阶段：系统设计（瀑布流阶段 2，Phase 7）——本文档为阶段关卡工件。
依据：`.spec/service-assembly-p7/requirements.md`（需求定稿，B1-SCOPE=A / B1-PROOF=A 用户确认）。
参照：fork `service.ts:65-73`（extend）+ `utils.ts:226`（createCallable）+ `logger.ts:208`。

---

## 1. 设计目标

给 dsh-core 的 `Service` 补**服务作者通用原语**：`extend`（派生作用域实例，默认恒等）+ `invoke`
（可调用服务，默认不可调）+ 独立 `srv` 通道（Service 类型直达，绕开 Any→dyn Service 无法下转型）
+ `ctx.get_extended(name)` / `ctx.call_service(name, args)`。生产 `ctx.logger(name)` 不改（DIV-7-1）。
m22 锁语义（B1-PROOF=A，无 golden）。

## 2. 自下而上锚点（本阶段核实）

| 锚点 | 基址 | 用途 |
|---|---|---|
| fork extend | `service.ts:65-73`（callable→createCallable 重建；else `Object.create(this)`+assign props） | 派生态：callable→重建；非 callable→派生（继承同方法集+新 props） |
| fork callable | `createCallable`（utils.ts:226）——`Service[invoke]` 存在 → 构造即 callable | invoke 触发的 callable 形态 |
| Service trait | service.rs:9-17（`name + check`，`Any + Send + Sync`） | 扩展点 |
| provide_service | context.rs:1450（`Arc<S>` → `provide_with(Arc<dyn Any>)`；唯一使用 m1_service） | 加 srv 通道（签名不变） |
| 作用域解析 | runtime.rs resolve_scope(301)/insert_impl(321)（`(ScopeId,name)` 键；沿 fiber 链 isolate 映射） | srv 同键镜像；get_extended/call_service 按当前纤维 scope 解析 |
| 效果/释放 | Disposer = `Rc<dyn Fn(&Cordis)>`（fiber.rs:30）；effect 同纤维载荷 | 组合 disposer（d1+d2） |

## 3. 设计分解

### S1（dsh-core：Service trait 原语）

```text
// service.rs（增）
pub trait Service: Any + Send + Sync {
    fn service_name(&self) -> &'static str;
    fn check(&self) -> bool { true }
    /// B1：派生作用域实例——`self: Arc<Self>` receiver（对象安全）；默认恒等。
    /// 自定义实现返回绑定访问方 ctx 的派生实例（对齐 fork `[Service.extend]`）。
    fn extend(self: Arc<Self>, ctx: &Cordis) -> Arc<dyn Service> { self }
    /// B1：可调用服务——默认不可调用；实现则 `ctx.call_service` 可调。
    fn invoke(&self, ctx: &Cordis, args: &[Value]) -> Result<Value, CordisError> {
        Err(CordisError::Internal("service is not callable".into()))
    }
    /// 下转型通道（测试/宿主读取派生字段用；`Arc<dyn Service>` 无原生 downcast）。
    fn as_any(&self) -> &dyn Any { self }
}
```

### S2（dsh-core：srv 通道 + 访问 API）

- `Runtime` 增 `pub srv: HashMap<(ScopeId, String), Arc<dyn Service>>`（new 初始化空）。
- **`provide_service`**（context.rs：签名不变，加 srv 注册）：保留 `provide_with`（Any 通道 + 属性
  声明 + notify + disposer），追加同作用域 srv 注册 effect（`resolve_scope(Some(fid), name)` 同键）+ 组合
  disposer（`Rc::new(move |ctx| { d1(ctx); d2(ctx); })`）。m1_service 零改动。
- **`srv_lookup(name)`**（按当前纤维 scope 链解析，镜像 impl 解析）：
  `resolve_scope(current_fiber, name)` → `srv.get((scope, name)).cloned()`。
- **`ctx.get_extended(name) -> Option<Arc<dyn Service>>`**：`srv_lookup(name)?.extend(self)`。
- **`ctx.call_service(name, args) -> Result<Value, CordisError>`**：`srv_lookup` → `invoke(self, args)`；
  未找到 → 明确 Err；默认 invoke → "not callable" Err。

### S3（m-series 红测，crates/dsh-loader/tests/m22_service_extend.rs）

| # | 红测 | 断言（绿） |
|---|---|---|
| T1 | 自定义 extend（返回 `DerivedCalc{caller: 访问方纤维名}`）→ 子纤维 `get_extended` | 派生实例 caller = 子纤维名（≠基）；`as_any().downcast_ref` 可读 |
| T2 | 无 custom extend 的 PlainService → `get_extended` | `Arc::ptr_eq`（默认恒等） |
| T3 | 实现 invoke（加和）的服务 → `ctx.call_service("calc",[1,2])` | `Ok(3)` |
| T4 | 无 invoke 的服务 → `call_service` | 明确 Err（"not callable"） |
| 回归 | `ctx.logger("x")` / m1_service | 不回归（DIV-7-1） |

## 4. 实现顺序（TDD）

1. **S1**：Service trait 增原语（编译红：m22 引用新 API → 实现 → 绿）。
2. **S2**：Runtime.srv + provide_service + get_extended/call_service。
3. **S3**：m22 T1-T4 全绿。
4. **S4→阶段 4**：workspace + clippy + verify-diff（无关 golden，仍全量）。**阶段 5**：serve 冒烟 +
   acceptance + DECISIONS。

## 5. DIV / 让步清单

- **DIV-7-1**：生产 `ctx.logger(name)` 保持方法（不动 Web serve/trace 路径）；原语供服务作者；
  演示服务在 m22 实现（logger 类 callable 不用真 logger）。
- **DIV-7-2**：srv 通道仅在 `provide_service`（Service 型）注册；`provide`（纯值）不进入（无 Service
  信息）——与 cordis 一致（非 Service 不具 extend/invoke）。
- **DIV-7-3**：`extend` 默认恒等而非 `Object.create(this)` 克隆——Rust 泛型克隆不可行；需要
  props 合并派生的服务自定义实现（对齐 fork 的 callable/自定义路径）。

## 6. 部署与回滚（阶段 5 预案）

- 部署：纯增量（trait 新方法默认值 + srv 通道 + 新 API）；`provide_service` 行为不变（多注册一表）。
- 回滚：`git revert` 本阶段提交（S1+S2+S3 整体）；m22 可独立删除。
