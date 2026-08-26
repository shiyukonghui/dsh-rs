# 设计：服务装配单元 Phase 1（服务插件 entry 化 + A1 身份键 + A7 持久化写回）

日期：2026-08-26
阶段：系统设计（瀑布流阶段 2）——本文档为阶段关卡工件。
依据：`.spec/service-assembly/requirements.md`（需求结论文档，定稿）+ `docs/SERVICE-ASSEMBLY-HANDOFF.md`。

---

## 1. 设计目标

在需求定稿基础上，给出可落地的组件设计：

1. **E1 服务插件 entry 化**——`boot()` 装配循环只认「声明 `config.wasm`」的入口为 loop 插件；
   其余入口一律经 loader 按名解析 apply（服务插件、普通插件）。消除对 `dsh:services` 的名称特判与
   「非 services 必 config.wasm」假设。
2. **E2 A1 插件身份键**——插件身份 = 解析后的插件实现本体（Arc 指针/新生代 token），与
   deepseek harness 的「回调为身份」（`registry.has(callback)` / re-import=新身份）一致。
3. **E3 A7 持久化写回**——运行时 loader 变更（create/update/remove）真实写回目标 cordis.yml
   （原子写），重启按落盘配置恢复。

四块验收见 §7（requirements.md §7 同口径）；每块 TDD 红→绿、独立提交 = 回滚点。

---

## 2. 自下而上锚点（本阶段核实，改动点基址）

| 改动点 | 现状基址 | 设计落点 |
|---|---|---|
| boot 装配循环 | lib.rs:158-201（硬编码注册 + 循环特判）；lib.rs:255（HMR refresh 特判 `name != "dsh:services"`） | §3 E1 |
| 插件仓库 | loader.rs:40 `plugins: HashMap<String, Arc<dyn Plugin>>`；registry.rs:34 `PluginHandle = String` | §4 E2 |
| 写回记录 | loader.rs:41-42 `writes: Vec<String>`（持久化 no-op）；`write()` loader.rs:662-664 | §5 E3 |
| entries→YAML 先例 | lib.rs:564 `serde_yaml::to_string(entries)`（merge_path_for_include） | §5 E3（复用） |
| 原子写 | `dsh_persistence::fs_atomic::atomic_write`（dsh-cli 已依赖 dsh-persistence） | §5 E3 |
| 按名解析 | loader.rs:724-728 `plugins.get(&name)` → `plugin_arc(plugin, config)` | **不改**（已成立） |
| 等价性基建 | `scenarios/06-dependency-gate.json` 等 17 剧本 + `diff/ts-host/verify-diff.mjs` | §6 E4 |

---

## 3. E1 服务插件 entry 化（crates/dsh-cli）

### 3.1 目标态 boot() 装配（lib.rs）

```text
cordis = Cordis::new(); loader = Loader::new(&cordis)?;

// (a) 宿主可用服务插件登记面（新函数 register_host_service_plugins）
//     现登记 dsh:services（DshServicesPlugin::all()）；未来 genai/llm-pi-ai 适配器等在此追加。
loader.register_plugin("dsh:services", Arc::new(DshServicesPlugin::all()));

// (b) loop 装配：只认 config.wasm 入口（不再 continue dsh:services、不再要求非 services 必有 wasm）
let mut loop_plugin: Option<Arc<WasmLoopPlugin>> = None;
for entry in &entries {
    let Some(wasm) = entry.config.get("wasm").and_then(|v| v.as_str()) else { continue; };
    let bytes = load_component(wasm_base, wasm)?;
    let caps = Capabilities::from_json(entry.config.get("caps"));
    let plugin = Arc::new(WasmLoopPlugin::new_owned(&entry.name, &bytes, caps)?);
    loader.register_plugin(&entry.name, plugin.clone());
    loop_plugin = Some(plugin);
}
// loop 必需（boot/run_turn 引擎）：以「存在 config.wasm 入口」为判据
let loop_plugin = loop_plugin
    .ok_or_else(|| CordisError::Internal("boot: no loop entry (config.wasm) in cordis.yml".into()))?;
```

- 服务/普通插件入口（含 `dsh:services` 与**新增**自定义服务 entry）不再被循环触碰，统一由
  `include.load()` → loader 按名解析 apply（现路径不变）。
- HMR refresh 闭包的 loop 定位（lib.rs:255）同样改为 `find(|e| e.config.get("wasm").is_some())`
  （去掉 `name != "dsh:services"` 特判）。

### 3.2 语义保真

- `run_turn`/loop 引擎仍由 `WasmLoopPlugin` 具体类型承载（lib.rs:71/602），boot 必需 loop 门槛不变；
  「哪些是 loop」的判定从「非 services」改为「声明 config.wasm」——良性分型（config.wasm 是
  WASM loop 的事实标记）。
- `dsh:services` 行为零变化（仍注册 + include.load() 按名 apply）；等价性由既有回归保底。

### 3.3 服务插件实现可用性（登记面）

- 「名称 → 实现」登记面收敛为一个注册函数：宿主把**可用**的实现登记进仓库；cordis.yml 声明某
  name 而仓库无实现 → `include.load()` 报 `loader: unknown plugin {name}`（fail-loud，诚实）。
- 新增服务插件 = ① Rust 侧多一行登记 ② cordis.yml 多一行声明 → 装配生效，**零** boot 循环改动。

---

## 4. E2 A1 插件身份键（crates/dsh-loader 为主）

### 4.1 模型（对齐 harness「回调为身份」）

```text
PluginIdentity = Arc<()>                    // 指针身份（复用 dsh-scope ScopeKey 的 Arc 身份纪律）
PluginRecord { identity: PluginIdentity, plugin: Arc<dyn Plugin>, generation: u64 }

plugins: HashMap<String, PluginRecord>      // name 仍是解析键；identity 承载「是谁的实现」
```

- `register_plugin(name, plugin)` 语义：
  - 无既有 → 铸新 `PluginIdentity`（新 `Arc<()>`），generation=1；
  - 既有且 `Arc::ptr_eq(旧插件, 新插件)` → **幂等**（身份不变，generation 不变——同实现=同身份）；
  - 既有且不同实现 → **新身份**（新 token），generation+=1（同名新实现=新身份 = harness re-import
    口径）。
- `load_plugin`（loader.rs:724-728）按 name 取当前 `PluginRecord`；把 `identity` 记录到 Entry
  （`Entry.identity: Option<PluginIdentity>`，id 由 fiber 生命周期持有）——为 HMR 换代 / case-4
  「插件自处置 vs 模块消失」提供身份判定（本阶段仅记录，不做完整 HMR 换代）。

### 4.2 范围控制（记入 DIV）

- 本阶段实现「注册语义 + 可观察身份 + Entry 记录」；**不做** B3（HMR 模块热更）完整链路。
- `PluginHandle`（registry.rs:34）从 `String` 注释升级为身份句柄；公开 ID 仍是 name（wire 不变）。

---

## 5. E3 A7 持久化写回（crates/dsh-loader + dsh-cli）

### 5.1 loader 侧（通用 seam，独立可测）

```text
type PersistSink = Rc<dyn Fn(&[EntryOptions]) -> Result<(), String>>;   // 落盘实现由宿主注入
Loader::set_persist(sink: Option<PersistSink>);
Loader::entry_options() -> Vec<EntryOptions>    // 按 root_group 序出的权威入口列表（含 config）
Loader::persist() -> Result<(), CordisError>    // sink.as_ref() → entry_options() → sink(..)
```

- 触发点：`create`/`update`/`remove` 非 disabled 分支的 `self.write(record)` 之后追加
  `if let Some(s) = &self.persist { s(&self.entry_options())?; }`（失败 fail-loud 回滚既有事务语义）。
- include.load() 的 sync 路径也走 create/update/remove —— 因宿主在 **boot 完成后**才挂 seam，启动期
  不含意外回写（见 §5.2）。

### 5.2 宿主侧接线（dsh-cli）

- `boot()` 返回的 `Boot.loader` 在 **include.load() 完成之后**由宿主（serve / 需要持久化的调用方）
  挂 seam：
  ```text
  loader.set_persist(Some(Rc::new(move |entries| {
      let yaml = serde_yaml::to_string(entries).map_err(|e| e.to_string())?;   // §2 既有先例
      dsh_persistence::fs_atomic::atomic_write(&config_path, yaml.as_bytes(), ...)
          .map_err(|e| format!("write-back {path}: {e}"))
  })));
  ```
- 目标文件 = 主 config_path（primary cordis.yml）；写合并后的权威列表（与 `dump_config`/include
  同序）。overlay 语义差异（merge 落主文件）记入 DIV（§8）。
- 场景：`dsh web` 的 dynamicCordisRunner（loader.create/remove）与任何运行时 loader API 变更
  即落盘；重启读主配置恢复。

### 5.3 配置反解（Config.simplify 对齐）

- 本仓库 config 就是 `serde_json::Value`（entry.rs:21-22），`serde_yaml::to_string(entries)` 即
  无损 YAML 反解；cordis 的 `Config.simplify`（把 config 序列化为可写面）在 Rust 无对象转换面，
  直接由 Value → YAML 承担（记入 DIV，不需 dsh-schema simplify）。

---

## 6. E4 等价性 & 测试计划（TDD 红→绿）

### 6.1 新增/扩展 dsh-diff 剧本（handoff §5 机制）

- 新增 `scenarios/06b-service-dependency-activation.json`：一个提供者插件 `provide("svc")` + 一个
  依赖方插件 `inject("svc")`（缺服务 PENDING → 提供后 ACTIVE），TS 原版 cordis 跑出 golden，
  Rust dsh-core 对齐逐行。若 06-dependency-gate 已足够表达，则在该剧本上扩展 `provide/notify`
  再观察依赖方激活（实现阶段按既有 06 剧本形态裁定，遵循「不重复造」）。
- 该剧本进 `diff/ts-host` 或 in-crate 差分：以既有机制（`verify-diff.mjs` / `dsh-diff`）承载。

### 6.2 红测清单（每个：先红后绿）

| # | 红测 | 断言（绿） | 归属 |
|---|---|---|---|
| T1 | boot 带第二个服务 entry（fixture `dsh:test-svc`，宿主已登记）→ 修改前红（`needs config.wasm`） | boot 成功；`dsh:test-svc` 按名 apply、提供的服务被消费者拿到；`dsh:services` 零回归 | dsh-cli 集成 |
| T2 | HMR refresh 的 loop 定位不再依赖 `name != "dsh:services"` | config 只有 loop(wasm)+service 时 refresh 正常、loop 仍重建 | dsh-cli |
| T3 | `register_plugin` 同名**新**实现再注册 → 新身份 | 身份句柄变化、generation 递增；fiber/Entry 记录新身份 | dsh-loader |
| T4 | `register_plugin` 同名**同**实现（同一 Arc）再注册 | 幂等：身份不变、generation 不变 | dsh-loader |
| T5 | loader.set_persist(写 temp 文件) 后 create/update/remove | 文件真实落盘为 YAML 权威列表；重启（重读）恢复 | dsh-loader（seam 级） |
| T6 | 宿主挂 seam 后 runtime 变更 → 主配置落盘；重启 boot 恢复 | cordis.yml 磁盘内容变化 + 重启后 entry 存在/更新/移除 | dsh-cli 集成 |
| E4 | 服务依赖激活 dsh-diff 剧本 | golden 逐行对齐（TS↔Rust） | dsh-diff |

### 6.3 回归门槛

- `cargo test -p dsh-cli -p dsh-loader -p dsh-wasmrt -p dsh-core` 全绿 + workspace 全量无失败；
- `cargo clippy --workspace --all-targets -- -D warnings` 零告警；
- 既有 `dsh web` / `--agent-loop` 冒烟不回归（部署阶段门控）。

---

## 7. 实现顺序（瀑布流阶段 3 编码计划）

1. **S1 = E2 身份键**（loader.rs/entry.rs/registry.rs）：`PluginIdentity`/`PluginRecord` +
   `register_plugin` 幂等/换代 + `load_plugin` 记录身份。T3/T4 红→绿。**独立提交 A1 身份键**。
   （身份键是 E1/E3 的地基——E2 先行，与 handoff「A1 必须先定」一致。）
2. **S2 = E1 entry 化**（lib.rs boot + HMR refresh）：只认 config.wasm 的 loop 装配 +
   `register_host_service_plugins` 登记面。T1/T2 红→绿。**独立提交 entry 化**。
3. **S3 = E3 持久化写回**（loader set_persist/persist/entry_options + dsh-cli 接线）：T5/T6 红→绿。
   **独立提交 A7 写回**。
4. **S4 = E4 等价性**（dsh-diff 剧本）：T-E4 红→绿。**独立提交 dsh-diff golden**。
5. 每步：改动 → git 提交 → DECISIONS 补记互查。

---

## 8. DIV / 让步清单（与 TS 的分叉，如实记录）

| # | 项 | 说明 |
|---|---|---|
| DIV-1 | 插件实现可用性来源 | Rust 静态登记 + WASM 动态包（D-115-Web 已有），非 Node 模块系统；`unknown plugin` fail-loud 诚实 |
| DIV-2 | A7 写回目标 = 主 config_path（合并后权威列表） | overlay 变更会物化进主文件（cordis 写回 entry 源文件 → 简化接受）；D-086「无 YAML 注释保真 leaf-diff」沿用 |
| DIV-3 | Config.simplify | Rust config 即 Value，`serde_yaml::to_string` 无损反解，不需 dsh-schema simplify 对象面 |
| DIV-4 | A1 完整 HMR 换代 | 本阶段做「注册语义 + 可观察身份 + Entry 记录」；B3（HMR 模块热更）后续阶段 |
| DIV-5 | 前端组件行的 Rust 引擎激活 | 显式排除（D-S1），另立项 |

---

## 9. 部署 & 回滚（阶段 5 预案）

- **部署冒烟**（验收）：`dsh web target/web/cordis.yml --agent-loop ...` 在 cordis.yml 追加一个
  自定义服务 entry（fixture）→ boot 成功、`dsh:services` 与 loop 零回归；runtime `session.prompt`
  一个真实 turn 可跑（按既有门控纪律，无 key 则诚实 skip）。
- **回滚**：逐提交（S1..S4）独立 `git revert`；A1 身份键/entry 化/A7 写回各自独立，互不耦合，
  S4（dsh-diff golden）单撤不影响运行面。
- 部署/回滚说明随 M-ACCEPTANCE 时归档到 `.spec/service-assembly/`。
