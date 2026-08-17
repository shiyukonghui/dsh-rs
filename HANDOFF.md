# dsh-rs —— Cordis 等效迁移到 Rust：开发交接文档

> 本文档面向**接手本工程的新 agent**：项目是什么、做到哪了、代码在哪、怎么验证、
> 关键设计决策与陷阱、下一步做什么。开发过程中的完整设计依据见
> `PLAN-rust-cordis-equivalent-migration.md`（迁移方案 + 各里程碑交付记录）。

## 1. 项目概览

把 DeepSeek Harness 所用的 **Cordis 插件框架**（`deepseek-harness/vendor/cordis` 等）**行为等效移植到 Rust**，
使「一切皆插件」的运行时可以脱离 Node/TypeScript 运行，并支持把第三方插件编译成 **WASM** 加载。

- 核心：`dsh-core` —— Cordis 语义的 Rust 移植（context/fiber/events/registry/reflect/logger/schema + M7 async 基建）。
- 组装：`dsh-loader` —— 配置驱动插件树（entry/group/include/isolate/`!!js` 表达式）。
- 校验：`dsh-schema` —— Schemastery 移植；`dsh-eval` —— `!!js` 表达式子集求值。
- 验证：`dsh-diff` —— 场景 DSL 双运行时差分（Rust 侧 vs npm 原版 cordis）。
- 部署：`dsh-wasmrt` —— wasmtime 后端，WASM 插件适配为 `dsh-core::Plugin`。

**里程碑全部完成**（M0–M65）：`cargo test` 全量绿，`cargo clippy --all-targets` 零警告；
**16 个差分场景**（9 核心 + **5 个 loader 事务场景，含 group 嵌套 + disabled
entry + isolate-intercept** + **2 个 include 纯函数场景**)Rust↔TS trace 逐行一致；
组件模型路径（WASM 组件插件 + WASI preview2 精细授予 + host get bytes + PluginHost
统一加载 + **能力按 entry 配置**）与 DSH 层缝 WIT 化；WASM loop 插件经 `Plugin`
挂进 Cordis，三缝承载桥接，完整 loop 语义 + `dsh` CLI 交互式启动；
async 收尾（emit fire-and-forget + loader 事务 allSettled + **bail/serial/waterfall
同步分派对 async listener fire-and-forget** + **同步 unload 显式记录跳过 async
disposer**）+ **HMR 热重载**（文件指纹轮询 → `Include::refresh` /
**`boot.refresh` async 事务**，`dsh --watch`）+ **能力配置统一入口**
（`PluginManifest::from_config`，C ABI 与组件两路径均支持）+ **真实 HTTP llm
接入**（OpenAI 兼容 `/chat/completions`，手写 HTTP/1.1 客户端，声明式
`llm.http` 配置，本地 mock 端到端验证）+ **C ABI 路径 WASI 授予**
（WASI preview1，wasip1 插件按 caps 注入 env/fs/net，env+fs 端到端验证）+
**loader 差分集**（`Loader::sync_async` 真并行 join_all 复刻 TS
`Promise.allSettled` 交错）+ **Group 插件 fiber 形态 + apply 异步化**
（`EffectOutcome::Await`：Group 等子入口 Active 后再 Active，等价 TS
`[Service.init]`；group 入口为真实 `plugin:Group` fiber）+ **schema strict**
（`ResolveOptions.strict` 贯穿 object/tuple/intersect/dict）+
**date/regExp 组合子**（RFC3339 校验 + 正则可编译校验）。
**group 差分已纳入（M29）**：`unload_async` 卸载让出（begin 后 + finish 前
`yield_now`）→ 多 fiber 并行卸载先全部 Unloading 再逐个 Disposed（对齐 TS
`Promise.all`）——`loader-10-group-nested` 34 行逐行一致。

## 1.5 迁移完成状态（M0–M65）

**核心功能迁移已完成并验证。** 对照 npm 原版 cordis / vendored
cordis-plugin-loader 的完整功能面：

- **字段级对齐**：Rust `EntryOptions`（id/name/config/group/disabled/
  disabled_expr/inject/isolate/intercept）与 vendored TS `EntryOptions` 逐字段
  一一对应；`dsh-core` 门面 API（timeout/interval/debounce/throttle + 别名 +
  async、plugin/effect/on/once、emit/bail/serial/parallel/waterfall、provide/
  set/get/value/intercept/resolve_config/accessor/mixin、unload/update/fiber 查询）
  完整对齐 Cordis `ctx`。
- **差分**：16 场景（9 核心 + 5 loader + 2 include）Rust↔TS trace 逐行一致，覆盖
  effect/emit/serial/bail/waterfall/intercept/update/deep-nesting/依赖 gate、
  loader 事务/部分失败回滚/group 嵌套/disabled/isolate-intercept、
  include 纯函数 patch（insert 进 group/顶层追加/嵌套命中/各 warn 诊断/
  通用 overrides 字段覆盖）。
- **验证**：`cargo test --workspace` 245 项全绿、`cargo clippy --workspace
  --all-targets` 零警告。
- **二进制级冒烟（M62 轮）**：真实 `dsh.exe` 以 echo-loop 组件跑通完整
  生命周期——`--once "hello headless"` → 答案 `echo: hello headless`、reason=
  completed、退出码 0；`--session-out` 生成合法 JSONL（header +
  turn/start + user/message + assistant/message + turn/end）；`--session-in`
  恢复续接（seq 续接、多轮上下文）；`--dump-config` 转储生效配置（含
  isolate/intercept 字段）。
- **无遗留 TODO/unimplemented**。

**剩余候选均为自觉边界（非未完成功能）**：
- `image` block / uuid id / surfaceOp 必填：forward-compat（WIT 预留）+ 确定性
  id（不引入 uuid 依赖）+ 缝 append 宽松接受——均为设计决策，不做。
- 真实 API https：需可信证书（自签已被正确拒绝），无证书环境不可验。
- C ABI net：wasmtime preview1 socket stub + Rust std wasip1 未映射 preview2
  sockets——平台限制待工具链。
- 模块级 HMR（依赖图/partialReload）、schema `function`/`is(Class)`：超出
  当前迁移范围 / Value-land 本质限制。

**M34 session 缝消息形状对齐**：消息承载对齐 DSH 生产 `Message` 对象
（`{id, role, content: ContentBlock[], source}`）——WIT record 更新 +
写入端（3 个 loop 插件）+ 投影端（`derive_messages` 对齐
`deriveEventMessage`：user 逐字透传 / assistant 与 tool 取 `data.message`）+
llm 消费端（`messages_to_wire`：Message[] → OpenAI wire，对齐
`serializeMessages`）。
**M35 HMR 事件驱动**：`Hmr::watch`（notify crate，OS 文件系统通知）+ mpsc
桥接（后台线程仅持 `Sender<PathBuf>`，单线程纪律保持）——事件作唤醒信号、
指纹确认兜底；`poll()` 消费事件队列，无 watcher 退化轮询；CLI `--watch`
走事件驱动（notify 启动失败 fallback 轮询）。
**M36 session surface 折叠**：`SessionLog` 对齐 DSH `foldSurface`——
surface-eligible 事件（user/assistant/tool）入 surface 节点序列、
`append_with_op`（`SurfaceOp::Replace`）替换 [start,end] 范围（旧节点
shadow、`replaceGeneration` 递增）、`derive_messages` 只对当前 surface
节点投影（`append` 签名不变，纯 append 与遍历等价）。
**M37 surface 防御性校验**：`append_with_provenance`（source_event_seqs +
原子提交）对齐 `assertProvenance`（引用早于当前、无重复、空数组仅
assistant 允许、replace 必须覆盖全部 shadowed）与 `assertToolResultRewrite`
（tool/result replace 恰好 1 个节点、仅改 content 深比较）。
**M38 HMR 失败事件化**：`Hmr::set_error_sink`（refresh 失败 → 事件通知
`Fn(&str, &CordisError)`）对齐 Cordis `hmr/config-update-failed`（parallel
事件，注意非 `hmr/error`）；`take_errors` 查询保留（双通道）；CLI `--watch`
经 `ctx.parallel("hmr/config-update-failed", …)` emit + eprintln。
**M39 include patch warn sink**：`apply_entry_patches_with_warn`（未命中
诊断——id 找不到/非 group/name mismatch/缺 id，对齐 TS warn 消息）；
`apply_entry_patches` 保留静默版（委托）；`Include::take_warns` 收集
（每次 read 重置）。
**M40 timer 服务**：`Cordis::timeout`/`interval`/`debounce`/`throttle`
（对齐 `vendor/timer`）——宿主时钟（`set_timer_clock`）+ 主循环驱动
（`drive_timers`）；经 `effect` 绑定 fiber（卸载清除，`InactiveEffect` 拒绝
非 Active）；debounce/throttle 用 `TimerSlot`（leading/trailing 语义）。
**M41 timer 无回调形态**：`timeout_async(delay)`（Promise 等价——yield_now
轮询时钟 + 卸载 reject "Context has been disposed"）与 `interval_ticks(delay)`
（AsyncIterable 等价——`IntervalTicks` Stream，每 delay 一个 tick，卸载
结束）；纯自驱动（Rc<Cell> 状态，无 tokio 正式依赖）。
**M42 ctx.once**：`Cordis::once`/`once_async`（一次性监听器，对齐 `ctx.once`
——首次触发先 dispose 自身再调用；`Rc<RefCell<Option<Disposer>>>` 延迟绑定
disposer；手动移除/fiber 卸载均生效）。
**M43 internal/get、internal/set 拦截**：`get_value` 走 `internal/get`
waterfall（拦截器短路返回替代值）；`set_value` 走 `internal/set` waterfall
（拦截器 veto）；`AccessorGet`/`AccessorSet` 改 `Rc<dyn Fn>`（无借用调用，
修复 mixin 重入 RefCell 冲突）。
**M44 internal/listener 注册拦截**：`on_cb` 注册前 `bail("internal/listener",
[name, global, prepend])`——bail 值非 null → 注册被拦截（返回 no-op
disposer）；`once`/`once_async` 自动生效。
**M45 headless 单发**：`dsh_cli::run_headless` + `derive_headless`（从
session 事件推导最后非空 assistant 文本 + turn/end reason，对齐 DSH
`dsh --profile headless "job"`）；CLI `--once <task>`——打印答案、
completed → exit 0 否则 1。
**M46 timer 别名**：`set_timeout`/`set_interval`（对齐 `vendor/timer` 的
deprecated 别名，委托 timeout/interval）。
**M47 session JSONL 持久化**：`SessionLog::save_to`/`load_from`（对齐 DSH
`session-persistence-jsonl`——header 行 + 每事件一行，torn-tail 容忍，
重建 events + surface）；CLI `--session-out <file>`（headless 后保存）。
**M48 恢复会话继续**：`dsh_cli::restore_session` + CLI `--session-in <file>`
（从 JSONL 加载历史 → 后续 turn 的 llm 输入含前轮上下文，对齐 DSH
resume——多轮共享上下文延续）。
**M49 fork 分支会话**：`SessionLog::fork(boundary?)`（对齐 DSH `Session.fork`
——稳定前缀截取 + 边界校验 INVALID_BOUNDARY/OPEN_TURN + 前缀重放；父会话
不可变）。
**M50 dsh-eval 可选链**：`?.`（null 安全成员访问——基 null/未定义标识符/
缺失成员短路 Null，链式传播；`?.[expr]` 索引；普通 `.` 仍 fail loud）。
**M51 dsh-eval nullish coalescing**：`??`（仅 null 左侧取右侧——0/''/false
保留左侧，与 `||` 的 truthiness 短路不同；与 `||` 同级左结合）。
**M52 CLI --patch 别名**：`--patch <file>` 为 `--overlay` 别名（对齐生产
`dsh --patch`）；`merge_entries` 单测固化行级 config 替换 + insert 语义。
**M53 dsh-eval typeof**：`typeof` 一元运算符（null → "object" JS 遗留、
未定义标识符 → "undefined"、其余按 JSON 类型；优先级高于二元）。
**M54 dsh-eval 模板字符串**：反引号 + `${expr}` 插值（`Tok::Template` →
`parse_template` 拆段 → eval 拼接，隐式 String()）。
**M55 dsh-eval in 运算符**：`'key' in obj`（对象键存在性 + 数组索引检查 +
类型校验 fail loud；与 `<`/`>` 同级）。
**M56 CLI --dump-config**：`dsh_cli::dump_config` + CLI `--dump-config`
（合并 overlays 的生效配置 YAML 转储，不 boot loop；对齐生产
`dsh --dump-config`）。
**M57 Schema.extend**：`SchemaKind::Custom` + 全局注册表（OnceLock<Mutex>）+
`Schema::extend`/`custom`（对齐 Schemastery 自定义类型注册；resolve 查表、
未注册 unsupported fail loud）。
**M58 HMR 换 loop 组件**：`Boot.loop_plugin` 改 `Rc<RefCell<Arc<WasmLoopPlugin>>>`
——refresh 重挂载后按 config.wasm 重建插件并替换（config.wasm 变化时新
组件生效；对齐 Cordis loader 按名重解析）。
**M59 dsh-eval 可选调用**：`?.()`（callee null/未定义短路 Null；白名单
String/Number/Boolean 直调；dsh-eval 子集边界补齐）。
**M60 parallel_async 返回结果数组**：`parallel_async` 返回 `Vec<Value>`
（Promise.all 结果数组——Continue → null、Returned → v；错误仍聚合
AggregateError）。
**M61 disabled entry 差分**：`loader-11-disabled-entry`（sync 含 disabled
不 apply → update enabled 热更 apply → update disabled 卸载——TS↔Rust 15
行逐行一致；差分增至 13 场景）。
**M62 isolate/intercept 差分**：`loader-12-isolate-intercept`（sync 含
`isolate:{svc:true}`/label 与 `intercept:{svc:{...}}` → update 切换接线字段
触发卸载重载——TS↔Rust 23 行逐行一致）。`dsh_diff::to_entry_options` 补齐
isolate/intercept 透传（修复 Rust 侧差分静默丢弃这两个服务接线字段；TS 宿主
`{...e}` 原样透传天然对齐）。差分增至 14 场景。注：trace 层不体现服务 realm
实例差异，本场景验证字段透传与事务稳定性。
**M63 include 纯函数差分**：`include-01-apply-patches-full`（insert 进 group /
顶层追加 / 嵌套命中 / 各 warn 诊断——TS↔Rust 7 行逐行一致，差分增至 15 场景）。
`dsh_diff::run_include`（纯函数级 `apply_entry_patches_with_warn` 对比，无
Fiber）+ `diff/ts-host/include-host.mjs`（vendored `@deepseek-ai/cordis-plugin-
include@1.0.6` 经 `node_modules` 装入，`applyEntryPatches` 权威执行 + printf `%C`
展开对齐）+ `Patch` 补 `Serialize, Deserialize`（JSON patch → 运行时 patch）。
差分场景协约：entry 与 insert 显式全字段（含 `config:{}`），两侧规范化为按键
字典序，嵌套 group 子入口保持原始 Value 视角。
**M64 通用 overrides 字段覆盖**：`include-02-apply-patches-overrides`
（inject/isolate/intercept/disabled_expr 覆盖 + 空数组替换——TS↔Rust 2 行
逐行一致，差分增至 16 场景）。`Patch` 加 `#[serde(flatten)] overrides`（收集
显式 config/disabled/group 之外的任意覆盖键，对齐 TS `{id, insert, name,
...overrides} → target[key]=value`）+ `apply_entry_override`（逐字段类型收紧，
inject 整体替换/isolate/intercept 对象替换/disabled_expr 转 String，未知键忽略），
`patch_update` 应用显式字段后遍历 overrides。闭环了 include patch 的通用字段
覆盖语义（此前仅 config/disabled/group，为真实缺口）。

**M65 Cordis 内核缺口补齐**（三子代理审查确认的运行时正确性缺口，逐项 TDD 闭环）：
- **`ctx.inject(deps, callback)`**：`m64_inject.rs`（4 测试全绿）——`InjectPlugin`
  （name="inject"）+ `Cordis::inject` 薄语法糖，依赖由既有 `Plugin::inject()`
  声明 → `check_impls` → `refresh_fiber` epoch 门控就绪后 `Load` 执行回调；
  依赖未就绪时回调推迟、全部就绪后一次执行、fiber 可正常卸载。
- **`internal/dispatch` 统一派发钩子**：`m64_dispatch.rs`（3 测试）——`report_dispatch`
  在各公开分派方法（emit/bail/serial/parallel/waterfall）collect 监听器前同步
  emit `internal/dispatch(mode, name, args, thisArg=Null)`；mode 照抄 Cordis
  （parallel 报 "emit"）；`internal/` 前缀跳过。16 差分零回归（无 internal/dispatch
  监听器时为 no-op）。
- **`fiber.update` veto + noSave**：`m64_update.rs`（5 测试）——**修复真 bug**：
  `config` 从瀑布前 eager 赋值移到 waterfall inner 内，**veto 的 update 不再使
  config 生效**（对齐 Cordis `this.config` 在 waterfall 内赋值）；`update_with`
  透传 `noSave`（loader write_back 依赖）；新增 `fiber_config` accessor；
  `update(fid,config)` 委托 `update_with(...,false)`。16 差分零回归。
- **`fiber.getEffects()`**：`m64_effects.rs`（2 测试）＋ `EffectMeta{label,children}`
  ＋ `FiberData.effects`（注册序）＋ `get_effects` accessor；`begin_unload` 清空、
  重载重收集（不累积）。`children` 恒空——dsh-core 无 effect 父子结构，树形与
  `ctx.on→"ctx.on('ev')"` 精确语义标签为后续增强（见 §6）。

**缺口评审结论更新**：Cordis 内核四缺口（inject / internal/dispatch / update veto /
getEffects）已全部闭环；原先误判的「`update` 丢弃返回值导致 veto 语义丢失」经验证
**不成立**（`run_chain` 短路已正确拒绝 inner）——真实缺口是 eager config 赋值。

## 2. 目录结构

```
dsh-rs/
├── Cargo.toml                     # workspace（6 个 crate）
├── crates/
│   ├── dsh-core/                  # Cordis 核心：context/fiber/events/registry/reflect/logger/service/error + session/tools/llm（DSH 缝承载）
│   ├── dsh-loader/                # 配置驱动插件树：Entry/Group/Include/isolate/事务/7-case/HMR（文件指纹轮询）
│   ├── dsh-eval/                  # !!js 表达式子集（tokenizer+递归下降+求值+interpolate）
│   ├── dsh-schema/                # Schemastery 移植：SchemaNode/resolve/autofix/transform/lazy
│   ├── dsh-diff/                  # 场景 DSL 解释器 + golden 校验 CLI
│   ├── dsh-wasmrt/                # wasmtime 后端：WasmPlugin + WasmComponentPlugin + WasmLoopPlugin + Capabilities
│   └── dsh-cli/                   # DSH 层启动器（bin dsh）：cordis.yml → 插件仓库 → Include → run_turn（--watch HMR）
├── scenarios/                     # 16 个差分场景（*.json）：9 核心 + 5 loader 事务（含 group/disabled/isolate-intercept）+ 2 include 纯函数 + golden
├── diff/ts-host/                  # npm 工程：cordis 场景宿主 + loader-host（vendored loader）+ 生成/校验脚本
├── wasm-plugins/                  # WASM 插件：hello（C ABI）、hello-component / echo-loop / tool-loop / llm-loop（组件模型）
├── PLAN-rust-cordis-equivalent-migration.md   # 迁移方案 + M0-M33 交付记录（§9-§49）
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
`intercept`/`resolve_config`/`accessor`/`mixin`、`update`、`unload`、fiber 查询 accessor、
`set_spawn`（注入异步任务驱动钩子，emit 对 async listener fire-and-forget 用）。

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
- **组件模型（M8）**：`WasmComponentPlugin`（适配 `Plugin`）+ `wasmtime::component::bindgen!`
  host 绑定 + WASI preview2（`wasmtime_wasi::p2::add_to_linker_sync`）。WIT 契约：
  `wit/plugin.wit`（`dsh:plugin`：apply/handle-event/dispose + host log/emit/on/provide/get）、
  `wit-dsh/dsh-loop.wit`（`dsh:dsh`：session/tools/llm/agent-loop 四缝）。
- **DSH 层 loop 宿主（M8）**：`WasmLoopPlugin`（适配 `Plugin`）+ `LoopHost`
  （session/tools/llm 缝的 Host 实现 + WASI）——loop 是 WASM 插件、缝由宿主承载。
- 组件插件（cargo-component 0.21 + wit-bindgen 0.44 + wasip1）：`wasm-plugins/hello-component`、
  `wasm-plugins/echo-loop`（回显 loop）、`wasm-plugins/tool-loop`（经 tools 缝调宿主 add 工具）。

## 4. 验证命令

```sh
cargo test                # 182 项单元/集成测试（core/loader/eval/schema/diff/wasmrt/cli）
cargo clippy --all-targets # 必须零警告
cargo run -p dsh-diff -- <scenario.json> [--golden f] [--async]   # 单场景差分
cd diff/ts-host && node verify-diff.mjs                 # 全场景：TS 生成 golden + Rust 校验（16 场景含 loader/include）
# m6 需要 wasm32-unknown-unknown target；wasm 插件由测试按需 cargo build
# m8 需要 cargo-component 0.21+（cargo install cargo-component --locked）与 wasm32-wasip1 target
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
11. **组件模型 Send 纪律（M8）**：wasmtime 的 `IoView: Send` 要求 Store data 可跨线程，
    而 Cordis 是 `Rc<RefCell>` 非 Send——`ComponentHostState` 不含 Cordis，
    apply 时把当前 Cordis 存入 `thread_local`（`CURRENT_CTX`），host 回调经它访问
    （单线程内安全）。
12. **cargo-component path 依赖**：一个 wit 目录只能含**一个** package（`dsh:plugin` 与
    `dsh:dsh` 分放 `wit/` 与 `wit-dsh/`）；依赖放进 `[package.metadata.component.target.dependencies]`
    （顶层 `dependencies` 是发布用 registry 依赖）。
13. **wasip1 组件自动 import WASI**：cargo-component 构建的组件 import `wasi:cli/*`，
    宿主必须 `wasmtime_wasi::p2::add_to_linker_sync` 注册（否则实例化报
    `component imports instance wasi:cli/environment`）。

## 6. 已知差异与待办

- **async 基建（M7 已落地）**：`tokio current_thread` 执行环境 + `futures-util`（默认特性关闭）；
  `EffectOutcome::Async`（异步 disposer）、`AsyncListener`（`on_async`）、`parallel_async`/
  `serial_async`（真并发 join_all + AggregateError / 顺序 await + bail）、`yield_now`、
  `fiber_await`、loader `await_idle`、`dsh-diff --async`。
  深嵌套（3 层）微任务交错与 TS 一致（`_reload` 的两个让出点：apply 前 + apply 后，
  经 FIFO 微任务队列复刻）；同步路径（两阶段延迟）保留且 8 个同步场景不变。
- **async 剩余（M13-M14 已收尾，M18 补全同步分派，M60 补返回）**：`emit` 对
  async listener **fire-and-forget**——`Runtime.spawn` 钩子（`Cordis::set_spawn`
  注入）+ `fire_async_listener`（`HookCallback::Async` 经钩子驱动；无钩子跳过
  并 trace "async-listener-skipped"）。**M18**：`bail`/`serial`（`run_serialish`）
  与 `waterfall`（`run_chain`）同样 fire-and-forget（调用但不 await，等价
  Cordis `Reflect.apply` 丢弃 Promise；bail 值不可同步判定 → 链继续）。
  **M60**：`parallel_async` 返回 Promise.all 结果数组（`Vec<Value>`——
  Continue → null、Returned → v；错误仍聚合 AggregateError）。
  **loader 事务 allSettled（M14）**：`Loader::sync_async` 复刻 Cordis
  `EntryGroup.update(config)`——全部入口都尝试 create/update（一个失败不阻断
  其他）、聚合失败（1 个 = 原错误 / 多个 = AggregateError）、失败**整事务回滚**
  （逆序移除新建 + 重建旧配置）；配套 `create_async`/`update_async`/`remove_async`
  （`plugin_arc_async`/`unload_async` 生命周期）与 `Include::load_async`/
  `refresh_async`。**M24**：同步 `unload` 无法 await 异步 disposer——显式记录
  `async-disposers-skipped` trace（不静默丢弃）；完整异步清理需
  `unload_async`（对照测试覆盖）。
- **HMR（M15 已落地，M23 async 事务，M35 事件驱动，M38 失败事件化）**：
  `Hmr`（`dsh-loader/src/hmr.rs`）——注册 `(路径, refresh 回调)`，检测
  add/change/unlink → 串行 refresh；失败记录 `take_errors`（对应 Cordis
  `hmr/config-update-failed` 事件）；首次 poll 建快照不触发（chokidar
  `ready`）。**M35**：`Hmr::watch(paths)` 用 **notify**（OS 文件系统通知）
  启动事件驱动 watcher——后台线程仅持 `Sender<PathBuf>`（mpsc 桥接，单线程
  纪律保持），事件作唤醒信号、指纹确认兜底（notify 事件可能重复/合并/误报
  临时文件）；`poll()` 消费事件队列；无 watcher 退化全量轮询（API 兼容）。
  **M38**：`set_error_sink`——refresh 失败时事件通知 `Fn(&str, &CordisError)`
  （对齐 Cordis `hmr/config-update-failed` 的 `ctx.parallel(filename, error)`；
  注意事件名非 `hmr/error`）；`take_errors` 查询保留（双通道）。
  `dsh-cli` 的 `--watch`：监视主配置 + overlays → `boot.refresh`（**M23：
  async 事务**——`Include::load_async` → `sync_async` allSettled + 整事务
  回滚，经 current_thread runtime block_on 驱动）重读重挂载；M35 启动
  notify watcher（失败 fallback 轮询）；M38 失败经 `ctx.parallel(
  "hmr/config-update-failed", …)` emit + eprintln。差异：Cordis 的模块级
  HMR（partialReload/依赖图、`hmr/change`/`hmr/reload`）非配置 HMR 范畴，
  Rust 侧无（配置驱动）；轮询路径保留为 fallback。
- **loader 场景差分（M20 已纳入，M22 Group 形态，M61 disabled）**：`dsh-diff`
  增加 loader 步骤（loader-sync/create/update/remove，`--async` 路径），TS
  参照 = vendored `@deepseek-ai/cordis-plugin-loader`（`diff/ts-host/loader-host.mjs`）。
  `Loader::sync_async` 真并行（join_all 复刻 TS allSettled 交错）。已对齐：
  loader-01（sync 成功 + update + remove）、loader-02（部分失败整事务回滚 +
  错误数量）、loader-10（group 嵌套）、**loader-11（M61：disabled entry——
  disabled 不 apply、enabled 热更、disabled 卸载）**。**M22 Group 插件
  fiber 形态**：group 入口为真实 `plugin:Group` fiber（`GroupPlugin`，apply 挂载
  子入口 → parent=Group、卸载 disposer 递归 stop）；子入口热更顺序修正为
  「先更新/新建、后移除缺席」（对齐 Cordis `EntryGroup.update`）。**M27 Group
  apply 异步化**：`EffectOutcome::Await`（fiber 标记 `await_children`，Finish
  等 Loading 后代完成）——Group 在子入口 Active 后再 Active（等价 TS
  `[Service.init]`）；同步路径 now_or_never 立即完成。**M28 修复**：`sync_async`
  old_map 仅收集根组入口（group 子入口由 group 分支管理，热更不再误删）；
  同步 `dispose_entry` group 入口先串行卸子入口（Async stop disposer 兜底）；
  GroupPlugin stop 改 `EffectOutcome::Async` 并行（unload_async 路径）。**M29
  group 差分纳入**：`unload_async` 卸载让出（begin 后 + finish 前 yield_now）→
  多 fiber 并行卸载先全部 Unloading 再逐个 Disposed（对齐 TS `Promise.all`
  卸载）——`loader-10-group-nested`（34 行）逐行一致，group 差分场景纳入。
- **dsh-eval（M50/M51/M53/M54/M55/M59）**：`!!js` 表达式子集（tokenizer +
  递归下降 + 求值 + interpolate）；M50 补 `?.` 可选链（null 安全成员访问——
  基 null/未定义标识符/缺失成员短路 Null，链式传播；`?.[expr]` 索引）；M51
  补 `??` nullish coalescing（仅 null 左侧取右侧——0/''/false 保留左侧，与
  `||` 同级左结合）；M53 补 `typeof`（null → "object" JS 遗留、未定义 →
  "undefined"）；M54 补模板字符串（`${expr}` 插值 + 隐式 String()）；M55 补
  `in`（键存在性 + 数组索引 + 类型校验）；M59 补 `?.()` 可选调用（callee
  null/未定义短路）。子集边界补齐（?. / ?? / typeof / 模板字符串 / in /
  ?.()）。
- **dsh-schema（M57 extend）**：**M25 strict 已贯穿**（`ResolveOptions.strict`：
  object/tuple/intersect 不合并多余键/项、dict 的 sKey 失败跳过）；**M26 regex
  flags 测试覆盖 + date/regExp 组合子**（`Schema::date` RFC3339 校验、
  `Schema::reg_exp` 可编译校验；`i/m/s` 生效，`u` 默认，`g/y` 忽略）；
  **M57 `Schema.extend` 自定义类型注册**（`SchemaKind::Custom` + 全局注册表
  OnceLock<Mutex> + `Schema::extend`/`custom`——对齐 Schemastery，resolve 查
  表、未注册 unsupported fail loud）。剩余差异：`function`/`is(Class)` 在
  Value-land 不可表达（按 JSON 类型名映射，本质限制）。
- **dsh-wasmrt**：
  - C ABI 路径（`WasmPlugin`）：同一实例多 fiber 挂载共享同一 wasm 实例（M6 单挂载）。
  - **C ABI 路径 WASI（M19/M21）**：`WasmHostState` 移出非 Send 的 Cordis（thread_local
    `CURRENT_CTX` + `mounted` 桥接，同组件路径）→ 满足 WASI preview1
    `add_to_linker_sync<T: Send>`；`Capabilities::build_wasi_p1_ctx` 按位注入
    （无 WASI 位 → None 不注册，wasip1 插件 import 解析失败 = 能力拒绝）。
    验证：`wasm-plugins/hello-wasi`（wasip1 C ABI 插件读 env + fs）——`wasi-env`
    位授予可读环境变量、`wasi-fs` 位授予可读预打开根目录文件（无位则读失败
    记录）；无 WASI 位 apply 失败（fiber Failed）。
  - 组件模型路径（`WasmComponentPlugin`，M8）：host `get` **bytes 版**（M10）；
    WASI preview2 **精细授予**（`build_wasi_ctx`：env/fs-readonly/net 按位）；
    thread_local `CURRENT_CTX` 桥接使单线程纪律与 Send 约束共存。**M30 net
    验证**：`wasm-plugins/hello-net`（wasip1 组件经 `std::net::TcpStream` 尝试
    TCP）——wasm32-wasip1 的 `std::net` 未实现（Rust std 不映射 preview2
    sockets）→ 连接返回平台错误（NET_ERR）不崩溃；能力授予机制
    （`inherit_network`/`allow_tcp`/`check_allowed_tcp`）已配置。端到端 TCP
    受 wasmtime 34 / Rust std 平台限制（已知）。
  - **能力按 entry 配置（M10/M16）**：组件路径（boot 的 loop entry）经
    `Capabilities::from_json(config.caps)` 授予；**M16 统一入口**
    `PluginManifest::from_config(name, kind, config)`——C ABI 与组件两路径共用
    （native 直通，caps 无 host 侧检查）。
- **DSH 层缝（M8-M10 闭环）**：`dsh-loop.wit` 定义 session/tools/llm/agent-loop
  四缝；`WasmLoopPlugin` + `LoopHost` 承载缝——echo-loop（回显）、tool-loop
  （经 tools 缝调宿主 add 工具）、llm-loop（完整 turn 流 + 多轮共享上下文）
  三个 WASM loop 插件经 `Plugin` 挂进 Cordis。三缝承载全部桥接 Cordis 服务
  仓库；`DshServicesPlugin` 按配置（`services` 子集 + 声明式工具 + 声明式 llm
  适配器）provide/注册。**组件模型完善（M10）**：WASI preview2 **精细授予**
  （`Capabilities.build_wasi_ctx`：env/fs-readonly/net 按位）；llm 缝带
  **provider 参数**；host `get` **bytes 版**；**PluginHost 统一加载**（native/
  core-module/组件三形态）；**能力按 entry 配置**（`config.caps` 数组：
  provide/emit/get/wasi-env/wasi-fs/wasi-net，`Capabilities::from_json`，缺省
  = abi_only）。**`dsh-cli` 启动器**：`boot(config, overlays, wasm_base)` →
  Include 挂载 → 多轮 run_turn。
- **真实 HTTP llm（M17/M31）**：`dsh-core/src/llm_http.rs`——OpenAI 兼容
  `/chat/completions` 客户端（手写 HTTP/1.1，`std::net::TcpStream`；单线程
  纪律）；**M31：https 支持**（native-tls 包裹，`parse_base` 解析 https://
  默认 443）；`LlmService::register_http`/`register_http_default`；
  声明式 `llm: {provider, http: {base, api_key, model}}` 经 `DshServicesPlugin`
  注册（默认或按 provider）。本地 mock 服务器端到端验证（请求形状 Bearer/
  model、响应解析、非 2xx/形状不符/连接失败 → error JSON）；https TLS 路径
  验证（openssl 自签证书 + native-tls 服务端——客户端证书验证拒绝自签，
  证明 TLS 层可达；生产验证需可信证书）。

- **session 缝投影（M32/M34/M36/M37/M47）**：`SessionLog::derive_messages`
  对齐 DSH `deriveEventMessage` 规则——`assistant/message` **空 content 跳过**（仅
  承载 usage 的 max-tokens 助手消息不入模型历史）。**M34 消息形状完整对齐**：
  消息承载为 DSH 生产 `Message` 对象（`{id, role, content: ContentBlock[],
  source}`）——`user/message` data 即完整 Message（逐字透传）、
  `assistant/message`/`tool/result` data 为 `{turn, step, message}` 包装
  （投影取 `data.message`）；WIT record（`dsh-loop.wit` session 接口）同步
  更新；3 个 WASM loop 插件写入端改生产形状；`llm_http::messages_to_wire`
  把 Message[] 序列化为 OpenAI wire（对齐 DSH `serializeMessages`，含
  tool-result → `{role:'tool', tool_call_id}`）。**M36 surface 折叠**：
  对齐 DSH `foldSurface`/`SessionSurface`——surface-eligible 事件入
  surface 节点序列（`surface_nodes()`）、`append_with_op`（`SurfaceOp::
  Replace`）替换 [start,end] 范围（旧节点 shadow、`replace_generation()`
  计数）、`derive_messages` 只对当前 surface 节点投影（`append` 签名不变，
  纯 append 与遍历等价）。**M37 防御性校验**：`append_with_provenance`
  （source_event_seqs + 原子提交）对齐 `assertProvenance`（引用早于当前、
  无重复、空数组仅 assistant 允许、replace 必须覆盖全部 shadowed）与
  `assertToolResultRewrite`（tool/result replace 恰好 1 个节点、仅改 content
  深比较）。**M47 持久化**：`save_to`/`load_from`（JSONL——header 行 + 每
  事件一行，torn-tail 容忍，重建 events + surface；对齐 DSH
  `session-persistence-jsonl` 核心格式）。**M49 fork**：`fork(boundary?)`
  （对齐 DSH `Session.fork`——稳定前缀截取 + INVALID_BOUNDARY/OPEN_TURN
  校验 + 前缀重放；父会话不可变）。剩余差异：`image` block 为
  forward-compatibility（WIT 预留）；消息 id 为确定性生成而非 uuid（不引入
  依赖）；生产 `Session.append` 要求 surface-eligible 事件必须带 surfaceOp
  （Rust WIT 缝 append 无 op 参数，宽松接受）；持久化无 id/createdAt/zstd
  （SessionLog 无元数据 + 无压缩）。
- **include patch（M33/M39）**：`apply_entry_patches` 对齐 Cordis `applyEntryPatches`
  ——insert 带 id → 向 **group** config 数组插入（非 group 跳过）；id patch 命中
  **嵌套** group 子入口（entryMap 含子入口语义）；递归重建（无借用冲突）。
  **M39 warn sink**：`apply_entry_patches_with_warn`（未命中诊断：id 找不到/
  非 group/name mismatch/缺 id，消息对齐 TS printf `%C`）；`Include::take_warns`
  收集（每次 read 重置）；`apply_entry_patches` 保留静默版（委托）。
- **timer 服务（M40/M41/M46）**：`Cordis::timeout`/`interval`/`debounce`/
  `throttle`（对齐 deepseek-harness `vendor/timer`）——生命周期绑定（`effect`
  + disposer，fiber 卸载清除）+ 宿主驱动（`set_timer_clock` 注入毫秒时钟、
  `drive_timers()` 主循环调用）；Once/Interval 到期重排；debounce 只执行
  最后一次、throttle leading+trailing（`TimerSlot`，`NEVER` 哨兵）；
  CLI 已注入时钟并在主循环驱动。**M41 无回调形态**：`timeout_async(delay)`
  （Promise 等价——yield_now 轮询 + 卸载 reject）与 `interval_ticks(delay)`
  （`IntervalTicks` Stream，AsyncIterable 等价，卸载结束）。**M46 别名**：
  `setTimeout`/`setInterval`（deprecated，委托 timeout/interval）。差异：
  `interval(delay)` 的 `return()`/`throw()` 显式终止（Rust Stream 经 drop
  等价）。
- **dsh-cli（M45/M47/M48/M52/M56）**：`dsh <cordis.yml> [--overlay … | --patch
  …] [--wasm-base …] [--watch] [--once <task>] [--session-in <file>]
  [--session-out <file>] [--dump-config]`——交互式 stdin 逐行 JSON +
  `--watch` HMR + `--once` headless 单发（对齐 DSH `dsh --profile headless
  "job"`：提交任务 → 从 session 事件推导最后非空 assistant 文本 + turn/end
  reason → 打印答案，completed → exit 0 否则 1）；`--session-in`/
  `--session-out` 恢复/保存会话 JSONL（M47/M48，对齐 DSH resume +
  `session-persistence-jsonl`）；`--patch` 为 `--overlay` 别名（M52，对齐
  生产 `dsh --patch`——行级 config 替换 + insert）；`--dump-config`（M56，
  生效配置 YAML 转储，不 boot）。差异：无持久化 session 元数据；成功不写
  stderr；无 `--dump-default-config`（bundle 模板机制）。
- **CLI watch（M15/M23/M35/M38/M58）**：`boot.refresh` async 事务 +
  `Hmr::watch` notify 事件驱动 + `hmr/config-update-failed` sink；**M58**：
  refresh 后按 config.wasm 重建 loop 插件（`Boot.loop_plugin` 改
  `Rc<RefCell<>>`——HMR 换组件生效，对齐 Cordis loader 重解析）。
- **事件（M42 once / M44 注册拦截）**：`Cordis::once`/`once_async`（一次性
  监听器，对齐 `ctx.once`——首次触发先 dispose 自身再调用；disposer 延迟
  绑定，手动移除/fiber 卸载均生效）。**M44**：`on_cb` 注册前 `bail(
  "internal/listener", [name, global, prepend])`——bail 值非 null → 注册被
  拦截（no-op disposer；once 自动生效）。差异：bail 值仅作拦截标记（无法
  替换 disposer，Value-land 限制）。
- **服务读写拦截（M43）**：`get_value` 走 `internal/get` waterfall（拦截器
  短路返回替代值，对齐 Cordis `ctx.get` Proxy handler）；`set_value` 走
  `internal/set` waterfall（拦截器 veto，对齐 `ctx.set`）；`AccessorGet`/
  `AccessorSet` 改 `Rc<dyn Fn>`（clone 取出后无借用调用——修复 mixin 重入
  RefCell 冲突）。差异：`Arc<dyn Any>` 形态的 `get`/`set` 不拦截（Value-
  land 本质限制）。

## 7. 下一步方向（建议顺序）

1. **Group apply 异步化已落地（M27）、热更误删已修复（M28）、group 差分已
   纳入（M29）、disabled 差分已纳入（M61）、isolate/intercept 差分已纳入
   （M62）**——loader 事务/嵌套/disabled/服务接线字段与 TS 逐行一致。
   M62：`dsh_diff::to_entry_options` 补齐 isolate/intercept 透传（修复 Rust
   侧差分场景静默丢弃这两个服务接线字段），新增 `loader-12-isolate-intercept`
   场景（含 `isolate:{svc:true}`/label 与 `intercept:{svc:{...}}` 及 update
   切换，23 行逐行一致）；TS 宿主 `{...e}` 原样透传天然对齐。注：trace 层
   （plugin/status/log）不体现服务 realm 实例差异，本场景验证的是字段透传与
   事务稳定性。
2. **session 缝消息形状已对齐（M34）+ surface 折叠（M36）与防御性校验
   （M37）已对齐**：消息承载为 DSH 生产 `Message` 对象（WIT + 写入端 +
   投影端 + llm wire 序列化）；surface append/replace + shadow 语义 +
   provenance/tool-result 校验（compaction 能力完整）。可继续：`image`
   block / uuid id / surfaceOp 必填（预留差异，非阻塞）。
3. **真实模型 https 已支持（M31）**：`llm_http` 走 TLS（native-tls）；可继续：
   真实 API 验证（https 客户端证书验证需可信证书——自签已被正确拒绝）。
4. **HMR 已完善（M23 async 事务 + M35 事件驱动 + M38 失败事件化）**：
   `boot.refresh` 走 async 事务（allSettled + 回滚）；`Hmr::watch`（notify）
   事件驱动（poll 消费事件队列，指纹确认兜底；无 watcher 退化轮询）；
   refresh 失败经 `set_error_sink` → `hmr/config-update-failed` parallel
   事件（CLI 已接入）+ `take_errors` 查询。可继续：模块级 HMR（依赖图/
   partialReload，非配置驱动，超出当前迁移范围）。
5. **include patch 已完整（M33 insert/嵌套 + M39 warn sink + M63 差分 +
   M64 通用 overrides）**：`include-01` 纯函数差分（insert 进 group/顶层追加/
   嵌套命中/各 warn 诊断）+ `include-02` overrides 差分（inject/isolate/
   intercept/disabled_expr 覆盖 + 空数组替换），TS↔Rust 逐行一致；`Patch`
   serde 反序列化 + `#[flatten] overrides`（对齐 TS 任意 entry 字段覆盖）。
6. **timer 服务已完整（M40 回调形态 + M41 无回调形态 + M46 别名）**：
   timeout/interval/debounce/throttle + timeout_async/interval_ticks
   （Promise/AsyncIterable 等价）+ setTimeout/setInterval 别名 + 宿主驱动
   （CLI 已接入）。
7. **ctx.once（M42）+ 服务读写拦截（M43）+ 注册拦截（M44）已落地**：
   once/once_async（一次性监听器，自移除 + disposer 幂等）；internal/get、
   internal/set waterfall 拦截（短路/veto）；internal/listener bail 注册
   拦截（once 自动生效）。差异：bail 值仅拦截标记（无法替换 disposer）。
8. **CLI headless（M45）+ session 持久化（M47）+ 恢复会话（M48）+ fork
   （M49）+ --patch 别名（M52）+ --dump-config（M56）+ HMR 换组件（M58）已
   落地**：`dsh --once <task>`（对齐 DSH `dsh --profile headless "job"`——
   session 事件推导最后非空 assistant 文本 + turn/end reason → 打印答案 +
   退出码）；`--session-out`/`--session-in` 保存/恢复 JSONL（多轮上下文延续，
   对齐 resume）；`SessionLog::fork`（稳定前缀分支，对齐 DSH `Session.fork`）；
   `--patch` = `--overlay` 别名（行级 config 替换 + insert）；`--dump-config`
   （生效配置 YAML 转储）；`boot.refresh` 换 loop 组件（config.wasm 变化时
   重建插件）。session 能力集完整（append/surface/replace/provenance/持久化/
   恢复/fork）。
9. **dsh-schema**：strict（M25）+ date/regExp（M26）+ `Schema.extend` 自定义
   类型（M57）。剩余为 `function`/`is(Class)` 的 Value-land 本质限制。
10. **dsh-eval**：`!!js` 子集 + `?.`（M50）+ `??`（M51）+ `typeof`（M53）+
    模板字符串（M54）+ `in`（M55）+ `?.()`（M59）。子集边界补齐。
11. **async 分派返回（M60）**：`parallel_async` 返回 Promise.all 结果数组
    （`Vec<Value>`——Continue → null、Returned → v；错误仍聚合
    AggregateError）。
12. **C ABI 路径能力配置已由 `PluginManifest::from_config` 统一（M16）、WASI
   env+fs 已验证（M19/M21）、net 路径可达已验（M30）**——端到端 TCP 受
   wasmtime 34（preview1 socket stub）+ Rust std wasip1（std::net 未映射
   preview2 sockets）平台限制，待工具链支持。

## 8. 工具链要求

- Rust 1.85+（workspace rust-version，组件模型 MSRV 下限；本机 1.94）；`rustup target add
  wasm32-unknown-unknown`（m6）+ `wasm32-wasip1`（m8 组件）。
- `cargo component`（`cargo install cargo-component --locked`，0.21+）+ wit-bindgen 0.44
  （cargo-component 自带）——m8 组件插件构建用。
- Node 22+/npm（仅 `diff/ts-host` 差分用；`cd diff/ts-host && npm install`）。
- 中文终端建议用 `cmd`（chcp 65001）或 `bash`，避免 PowerShell 5.1 的 `&&`/编码问题。
