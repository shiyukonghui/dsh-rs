# dsh-rs —— Cordis 等效迁移到 Rust：开发交接文档

> 本文档面向**接手本工程的新 agent**：项目是什么、做到哪了、代码在哪、怎么验证、
> 关键设计决策与陷阱、下一步做什么。开发过程中的完整设计依据见
> `PLAN-rust-cordis-equivalent-migration.md`（迁移方案 + 各里程碑交付记录）。

## 1. 项目概览

把 DeepSeek Harness 所用的 **Cordis 插件框架**（`deepseek-harness/vendor/cordis` 等）**行为等效移植到 Rust**，
使「一切皆插件」的运行时可以脱离 Node/TypeScript 运行，并支持把第三方插件编译成 **WASM** 加载。

- 核心：`dsh-core` —— Cordis 语义的 Rust 移植（context/fiber/events/registry/reflect/logger/schema）。
- 组装：`dsh-loader` —— 配置驱动插件树（entry/group/include/isolate/`!!js` 表达式）。
- 校验：`dsh-schema` —— Schemastery 移植；`dsh-eval` —— `!!js` 表达式子集求值。
- 验证：`dsh-diff` —— 场景 DSL 双运行时差分（Rust 侧 vs npm 原版 cordis）。
- 部署：`dsh-wasmrt` —— wasmtime 后端，WASM 插件适配为 `dsh-core::Plugin`。

**里程碑全部完成**（M0–M6）：`cargo test` 85 项全绿，`cargo clippy --all-targets` 零警告；
8 个差分场景 Rust↔TS trace 逐行一致。

## 2. 目录结构

```
dsh-rs/
├── Cargo.toml                     # workspace（6 个 crate）
├── crates/
│   ├── dsh-core/                  # Cordis 核心：context/fiber/events/registry/reflect/logger/service/error
│   ├── dsh-loader/                # 配置驱动插件树：Entry/Group/Include/isolate/事务/7-case
│   ├── dsh-eval/                  # !!js 表达式子集（tokenizer+递归下降+求值+interpolate）
│   ├── dsh-schema/                # Schemastery 移植：SchemaNode/resolve/autofix/transform/lazy
│   ├── dsh-diff/                  # 场景 DSL 解释器 + golden 校验 CLI
│   └── dsh-wasmrt/                # wasmtime 后端：WasmPlugin + PluginHost + Capabilities
├── scenarios/                     # 8 个差分场景（*.json）+ TS 侧 golden（*.golden）
├── diff/ts-host/                  # npm 工程：cordis 场景宿主 + 生成/校验脚本
├── wasm-plugins/hello/            # WASM 插件模板（wasm32-unknown-unknown cdylib）
├── PLAN-rust-cordis-equivalent-migration.md   # 迁移方案 + M0-M6 交付记录（§9-§15）
├── PLAN-rust-wasm-everything-is-plugin.md     # 早期 WASM 方案（M2/M3 验收已在此核心复跑）
└── HANDOFF.md                     # 本文档
```

## 3. 各 crate 速览

### dsh-core（最核心）
| 模块 | 内容 |
|---|---|
| `context.rs` | `Cordis` 门面（`Rc<RefCell<Runtime>>`），**收集-再执行纪律**（见 §5） |
| `runtime.rs` | `Runtime`：fibers/impls/services/hooks/registry、`notify`、作用域、`deferred` 两阶段加载 |
| `fiber.rs` | `FiberData` 状态机、`collect_effect`（逆序+幂等）、`make_disposer` |
| `events.rs` | `DispatchMode`/`HookResult`（`isBailed`）/`Listener`（`NextRef`）/waterfall 链 |
| `reflect.rs` | `Impl`（`Arc<dyn Any>` 服务值）、`Property`（service/accessor）、`CheckFn` |
| `registry.rs` | `Plugin` trait（name/inject/config_schema/apply）+ `RuntimeRecord` |
| `logger.rs` | `LoggerState`/`Logger`/`Message`/printf 格式化/`hyphenate` |
| `service.rs` | `Service` trait（name/check） |

关键 API：`plugin`/`plugin_arc`、`effect`、`on`/`on_with`、`provide`/`provide_with`/`provide_service`、
`get`/`get_typed`/`get_value`、`set`、`emit`/`bail`/`serial`/`parallel`/`waterfall`、
`intercept`/`resolve_config`/`accessor`/`mixin`、`update`、`unload`、fiber 查询 accessor。

### dsh-loader
- `EntryOptions`（id/name/config/disabled/disabled_expr/group/inject/isolate/intercept，serde 可序列化）
- `Loader`（`new`/`register_plugin`/`create`/`update`/`remove`/`sync`/`fiber`/`is_disabled`/`entries`/`take_writes`）
- update **四分支事务 + 回滚**；Loader 服务 7-case 自处置检测；group 嵌套；realm（Local/Global）GC
- `Include`（YAML/JSON + `apply_entry_patches` + 写回 + 手动 `refresh`）

### dsh-schema / dsh-eval
- `Schema::object/array/union/intersect/transform/lazy/...` + meta 链；`resolve(data, schema, opts)`；
  autofix、default 填充、路径前缀错误（`$.a[1]`）、`schema_to_string`。
- `dsh_eval::evaluate(scope, expr)` / `interpolate` / `truthy`；白名单调用，fail loud。

### dsh-diff
- 场景 DSL（`scenarios/*.json`）：`plugins[].apply` 微型 DSL + `steps`。
- CLI：`dsh-diff <scenario.json> [--golden f | --record f]`。
- trace 行写入 `Runtime.trace`（框架层）+ 解释器层 + 宿主层；`verify-diff.mjs` 一键生成 golden 并校验。

### dsh-wasmrt
- C ABI：插件导出 `alloc/dealloc/plugin_apply/plugin_handle_event/plugin_dispose`；
  导入 `host_log/host_emit/host_on/host_provide/host_get`（wasm32-unknown-unknown，纯 env）。
- `WasmPlugin`（适配 `Plugin`）、`Capabilities`（PROVIDE/EMIT/GET 能力位）、`PluginHost`/`NativeHost`。

## 4. 验证命令

```sh
cargo test                # 85 项单元/集成测试（core/loader/eval/schema/diff/wasmrt）
cargo clippy --all-targets # 必须零警告
cargo run -p dsh-diff -- <scenario.json> [--golden f]   # 单场景差分
cd diff/ts-host && node verify-diff.mjs                 # 全场景：TS 生成 golden + Rust 校验
# m6 需要 wasm32-unknown-unknown target；wasm 插件由测试按需 cargo build
```

## 5. 关键设计决策与陷阱（新 agent 必读）

1. **单线程 + `Rc<RefCell>`**：整个运行时刻意单线程（`Arc` 仅共享所有权，
   clippy `arc_with_non_send_sync` 在各 crate 顶层豁免并注明原因）。
2. **收集-再执行纪律**：`Cordis` 门面方法先 `borrow_mut()` 完成数据变更并收集用户代码，
   **释放借用后再执行**用户代码（监听器/disposer/apply）——保证重入安全
   （监听器内再 emit、disposer 内触发依赖方重载均无借用冲突）。
   ⚠️ 新方法若在借用中调用用户代码会 panic（`RefCell already borrowed`）。
3. **notify 时机**：Cordis 只在提供者 fiber **ACTIVE** 时 notify 依赖方
   （apply 期间 provide 不通知，`finish_load` 通知并返回依赖方转换）。
4. **两阶段延迟加载**（M5 差分发现）：apply 期间触发的嵌套/依赖加载模拟 Cordis 微任务让出
   ——Loading 状态同步、apply 在父 Active 前、Finish 在父 Active 后（`Runtime.deferred`）。
5. **loader 部分更新语义**：`update()` 仅合并传入键（None/空 = 保留现值），
   对应 Cordis「只合并传入的键」。
6. **Logger 阈值语义反直觉**：`targetLevel < level → skip`，阈值 = 最高显示级别，
   默认 INFO(1) 下 warn(2)/debug(3) 被过滤（忠实 Cordis，实测确认）。
7. **wasmtime 34 `func_wrap` 要求 Send+Sync 闭包**：宿主 import 闭包只经
   `caller.data()` 读写；监听器注册移出闭包、由 apply 返回后统一完成。
8. **`ctx.get` 是全局 store 查询**（按 isolate 标签），不是 fiber 链。
9. **内部事件不记 trace**（`internal/*` 前缀在 emit/serial/bail/waterfall 中跳过），
   否则差分多行。
10. **差分 golden 由 TS 侧生成**（npm cordis 4.0.0-rc.8 ≈ vendored 4.0.1）；
    `fiber.update()` 不返回 restart promise，TS 宿主需补 `await fiber.await()`。

## 6. 已知差异与待办

- **async 基建**：核心同步；两阶段延迟覆盖 1-2 层嵌套，深嵌套微任务交错不完全等价；
  `yield_now`/并行 `allSettled`/`EntryTree.await()` 未实现。
- **loader 场景未纳入差分集**（TS 侧需 `@koishijs/loader` 依赖）；Rust 侧已由 m2 单测覆盖。
- **dsh-schema**：`strict` 标志未贯穿；regex 仅 `i/m/s` flags；`function`/`is(Class)` 按 JSON 类型名映射。
- **dsh-wasmrt**：轻量 core-wasm FFI（非组件模型）；WASI preview2 fs/网络能力授予未做；
  同一 `WasmPlugin` 实例多 fiber 挂载共享同一 wasm 实例。
- **DSH 层未移植**：tools/session/agent-loop 等（本工程交付框架 + 示例插件）。

## 7. 下一步方向（建议顺序）

1. **async 基建**：tokio current_thread + async listener/effect + `yield_now`，替代两阶段延迟近似。
2. **组件模型升级**：cargo-component + WIT world + WASI preview2 能力授予（替代手写 ABI）。
3. **loader/include 场景纳入差分集**（TS 侧装 `@koishijs/loader`）。
4. **DSH 层示例**：用 `dsh-core` 实现 tools/session 最小插件，验证「loop 本身可替换」。
5. **热重载（HMR）**：文件 watcher + `Include::refresh` 自动化。

## 8. 工具链要求

- Rust 1.94+（edition 2021）；`rustup target add wasm32-unknown-unknown`（m6 测试用）。
- Node 22+/npm（仅 `diff/ts-host` 差分用；`cd diff/ts-host && npm install`）。
- 中文终端建议用 `cmd`（chcp 65001）或 `bash`，避免 PowerShell 5.1 的 `&&`/编码问题。
