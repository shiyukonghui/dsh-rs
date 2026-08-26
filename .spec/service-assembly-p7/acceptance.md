# 验收报告：服务装配单元 Phase 7 — B1 Service 派生作用域实例 + 可调用服务

日期：2026-08-27
阶段：测试验证（阶段 4）+ 部署与维护（阶段 5）——本文档为阶段关卡工件（验收收口）。
依据：`.spec/service-assembly-p7/requirements.md`（定稿）+ `design.md`（定稿）+ `docs/DECISIONS.md` D-147/D-148。
范围：B1-SCOPE=A（可调用 + 派生全流程）+ B1-PROOF=A（m-series 锁）——用户确认，无 golden。

---

## 1. 交付范围（对需求/设计逐条核对）

| 项 | 要求 | 交付 | 证据 |
|---|---|---|---|
| S1 派生 | `extend` 派生作用域实例（None=恒等） | ✅ `Service::extend`（service.rs） | m22 T1-T2 |
| S1 可调用 | `invoke` 可调用服务（默认不可调） | ✅ `Service::invoke`（默认 Err） | m22 T3-T4 |
| S2 通道 | Service 类型直达（srv）+ 访问 API | ✅ `Runtime.srv` + `srv_lookup`/`get_extended`/`call_service` | m22 全 |
| S2 provide_service | 签名不变 + srv 注册 + 组合 disposer | ✅ context.rs（m1_service 零改动） | workspace 回归 |
| T1 | 自定义派生绑定访问方纤维 ctx | ✅ m22 | `extend_produces_derived_bound_to_accessing_fiber` |
| T2 | 默认派生恒等 | ✅ m22 | `default_extend_returns_identity` |
| T3 | invoke 加和可调用 | ✅ m22 | `callable_service_invokes_with_args` |
| T4 | 不可调用明确 Err | ✅ m22 | `non_callable_service_errors_clearly` |

## 2. 阶段 4（测试验证）证据

- **m22 4/4 绿**：T1 观察日志 `derived:child`（extend 在访问方（child）纤维 ctx 上运行）/ T2 `Arc::ptr_eq`
  （默认恒等）/ T3 `call_service("calc",[1,2])` → 3 / T4 `call_service("plain")` → "not callable" Err。
- **红测/编译实证修正**：`self: Arc<Self>` receiver 的默认体在 unsized `Self`（dyn 使用）下 E0277；
  `as_any` 同 unsized 限制弃用 → 改 `&self` + `Option`（None=恒等）+ 观察日志模式（T1 证据不变）。
- **`cargo test --workspace`**：EXIT=0，**202 目标 0 失败**（+m22，含 m1_service 既有 `provide_service`
  用法零改动）。
- **`cargo clippy --workspace --all-targets -- -D warnings`**：EXIT=0。
- **`node diff/ts-host/verify-diff.mjs`**：**23/23 PASS**（golden 零回归）。

## 3. 编码期发现与取舍（如实记录）

- **E0277（unsized Self）**：`self: Arc<Self>` + 默认体 `{ self }` 在 `dyn Service` 上无法编译 → 改
  `&self -> Option<Arc<dyn Service>>`（`None` = 恒等，`get_extended` 内保留原 Arc）。语义与 fork
  一致（每次访问派生；默认无派生信息 → 原实例）。
- **作用域键对齐**：srv 注册的拆分 effect 晚于 `insert_impl` 执行——若靠「执行顺序已预填 scopes」
  会键错位；显式用与 `insert_impl` 完全相同的 `resolve_scope(...).unwrap_or_else(scope_for)` 同源解析。
- **Send + Sync**：`Service: Any + Send + Sync` → 测试观察日志用 `Arc<Mutex<Vec<String>>>`（Rc 不满足）。
- **B1-PROOF=A 边界**：证据 m22 + 单测（无 golden；TS host 无 Service 子类支持，用户确认）。

## 4. 阶段 5（部署与维护）证据

- **部署冒烟**：`dsh web target/web/cordis.yml --port 60887`（本轮含 dsh-core Service 改动）→
  `GET /` **HTTP 200**（len 13270 与基线一致），进程干净停止——真实启动链路零回归。
- **部署面**：纯增量（trait 新方法默认值 + srv 通道 + 新 API；`provide_service` 行为不变）；生产
  `ctx.logger` 不改（DIV-7-1）。回滚 = `git revert 962986d`。

## 5. 诚实边界（未做 / 延后）

- 生产 logger 保持方法（DIV-7-1）；srv 通道仅 Service 型（DIV-7-2）；extend 默认恒等、自定义
  派生需服务作者实现（DIV-7-3）。
- B2 Group 折叠 / B4 config simplify + A3 动态 check spike = 后续优先级目标。

## 6. 决策链互查

`D-145 需求（971eb3f）→ D-146 设计（f9a1bee）→ D-147 编码（962986d）→ 本验收（D-148，待提交）`。
改动 → git 提交 → DECISIONS 条目一一对应。

## 7. 结论

**通过**：B1（Service 派生作用域实例 + 可调用服务）五阶段闭环。Rust 服务作者现可表达 cordis 的
`Service[extend]`（派生/恒等）与可调用服务（invoke）语义——`get_extended`（访问方 fiber 绑定派生）+
`call_service`（可调用/不可调用明确报错）；m22 4/4、202 目标全绿、clippy 0、23 golden 零回归、
serve 冒烟 HTTP 200。
