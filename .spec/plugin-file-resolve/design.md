# 设计：插件包（文件夹）解析——wasm + 前端 装配胶水

日期：2026-08-27
阶段：系统设计（瀑布流阶段 2）——阶段关卡工件。
依据：`.spec/plugin-file-resolve/requirements.md`（v3 正式，Q1-Q4 用户确认）。

## 1. 总体结构

```
name(entry) ──[1 registry?]──→ 已注册（dsh:* / host）→ include.load() 按名 apply   (内置/host，既有)
            └─[2 folder?]──→ PluginPackage：wasm + web + caps + world
                                │
                                ├─ wasm → detect_component_kind（预检导出接口）
                                │          ├─ Loop  → WasmLoopPlugin；首个→Boot.loop_plugin（turn 句柄）
                                │          └─ Plugin→ WasmComponentPlugin（通用 apply）
                                │          └─ Unknown→ fail-loud 报错
                                ├─ register_plugin(name, Arc<dyn Plugin>)
                                └─ web 目录 → Boot.packages → dsh web 挂 /plugins/<name>/**
            └─[3 其它]──→ loader 未知插件 fail-loud（既有）
```

## 2. 组件（新）与改动（既有）

**dsh-wasmrt（新增 API）**
- `pub enum ComponentKind { Plugin, Loop, Unknown }`
- `pub fn detect_component_kind(bytes: &[u8]) -> ComponentKind`：
  `wasmtime::component::Component::from_binary` + `component_type().exports(engine)`
  （wasmtime 34 `types::Component::exports` 遍历导出名）：
  - 含 `plugin-api` → Plugin；含 `agent-loop` → Loop（优先 loop：短名字串后判）；
  - 其它/编译失败 → Unknown。（编译失败本身即 ABI 不符，明报。）

**dsh-loader（小改动）**
- `pub fn has_plugin(&self, name: &str) -> bool`：注册表查询（boot 判别「已注册 vs 包」必需）。

**dsh-cli（新模块 `src/plugin_pkg.rs` + 装配改造）**
- `PluginPackage { name, dir, wasm_file, web_dir: Option<PathBuf>, caps }`
- `PackageManifest`（serde）：`{ wasm?: String, web?: String, caps?: Value, world?: String }`
  （相对包目录；缺省：wasm = 既有约定 `<name>/target/wasm32-wasip1/debug/<name>_plugin.wasm`、
  web = `web/` 存在则取、caps = 缺省）。
- `resolve_package(wasm_base, name) -> Result<Option<PluginPackage>>`：`wasm_base/<name>` 非目录
  → Ok(None)；是目录 → 解析清单（JSON 错 → Err fail-loud）+ 定位 wasm（文件缺失 → Err）+ web。
- `load_loop_or_component(name, bytes, caps) -> (Arc<WasmLoopPlugin> | Arc<dyn Plugin>, bool_is_loop)`。
- **boot 改造**（替换 loop-only block，cli/lib.rs:210-229）：
  `for entry in entries { if loader.has_plugin(name) → continue; resolve_package… ;
  Some(pkg) → bytes + detect → Loop→LoopPlugin(首个→loop_plugin=Some) / Plugin→component plugin；
  register_plugin(name, …); if web → push 到 Boot.packages }`；`loop_plugin.ok_or("boot: no loop package (dsh-loop world)…")`。
- **Boot 增** `pub packages: Vec<PluginPackage>`（web 挂接数据源）。
- **HMR refresh**（cli/lib.rs:281-300）迁移：共享 `loop_plugin_for(wasm_base, entries)` helper
  （扫描条目→包解析→首个 dsh-loop），boot 与 refresh 同用；替换 config.wasm 判定。
- **web.rs 挂接**：`web_serve` 的静态分派在 SPA fallback 之前插入 `/plugins/<name>/<path>` →
  从包 web_dir 读文件（目录 → 目录索引/`index.html` 前缀；越界名 → 404/不落入 fallback）。
- **web-cordis.yml / web-smoke*.yml 迁移**：loop 条目 `config: { wasm: echo-loop }` → `config: {}`
  （name 已是文件夹名）。

## 3. 关键决策理由

- **world 判别用「预检导出接口」**：导出名 API 存在且确定性好（ABI 事实）；优先 loop（两个
  dsh-loop 导出名 `agent-loop`/`tools-handler`，plugin 导出 `plugin-api`，无歧义）。config 里可选
  `world` 提示作显式覆盖/快路径，仍以字节探测兜底。
- **registry 优先于 folder**：`dsh:*`/host 插件名即使恰有同名目录也不误当包（host 权威）。
- **web 挂到 `/plugins/<name>/**`**：与 SPA 隔离命名空间；不污染既有路由；SPA fallback 不吞
  插件路径（先路由后 fallback，miss → 404）。
- **turn loop 取「首个 dsh-loop 包」**：config 序第一个；多 loop 允许并存（load 侧），句柄唯一
  （run_turn 单义）。比旧「最后者胜」更可预期。
- **重构而非兼容**：D4 用户定「移除 config.wasm」；无外部消费者（项目自身立场），干净迁移。

## 4. 测试计划（TDD，红→绿）

1. wasmrt `detect_component_kind`：echo-loop→Loop、hello-component→Plugin、空/垃圾→Unknown（用已构建 .wasm，零慢速编译）。
2. cli `resolve_package`：#{} 走 hello-component 回退 OK；带 plugin.json（显式 wasm/web/caps）OK；非包目录 Ok(None)；目录无 wasm Err；坏 JSON Err。
3. cli boot 装配：cordis.yml = 测试目录里「services(dsh:services) + echo-loop(loop 包) + hello-component(plugin 包)」→ boot：loop 句柄 = echo-loop；hello-component 经包注册、`plugin-api` apply 生效（apply 写 log）；plugin.json 覆盖 caps/web。
   - 红验证：移除装配 glue（stash）→ hello-component 未注册 → include.load 失败（unknown plugin）。
4. cli web：`/plugins/hello-component/<asset>` 命中包 web 资源；`/plugins/<n>/` 目录索引；非插件路径 SPA fallback 保持。
5. 迁移后 serve 冒烟：web-cordis.yml（纯 folder）boot + serve HTTP 200/13270。

## 5. 诚实边界

- 前端组件以静态资源目录挂接（D2=a）；GUI 消费侧不纳入本轮（D3）。
- 不 watch 文件；HMR 显式 refresh 重解析（清单/组件变化经刷新生效）。
- `plugin.json` 为包级清单，非 loader 全局 schema；schema 版本暂无（初版，additive）。
