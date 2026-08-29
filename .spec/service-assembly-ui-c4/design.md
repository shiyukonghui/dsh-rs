# 设计结论：桌布 C4 —— status/list 渲染器 + 插件清单服务单元 + 载体泛化

日期：2026-09-05
阶段：系统设计（瀑布流阶段 2）——requirements.md 自主过闸。决策记录 **D-185**。

---

## 1. 改动总览（三块，互不越界）

```
A. 渲染器点亮（assets/canvas/core.js + app.js）      —— 契约 §4.1 实现，零新契约
B. 新服务装配单元（wasm-plugins/panel-plugin-inventory/）—— 照 llm-deepseek 型
C. 宿主泛化（Boot.remote_carriers + serve 发现挂载）    —— 关掉「再硬编码」的路
```

## 2. A：渲染器（core.js 纯函数增量）

```js
IMPLEMENTED = ["form", "status", "list"];            // chat/chart/table 仍预留
export function extractPath(obj, dottedPath)          // "items" / "a.b"；缺 → undefined
export function listRows(view, dataValue)
  // rows = extractPath(dataValue ?? {}, view.rowsPath) 数组优先；否则 view.rows 静态兜底；
  // 都无 → { rows: [], columns: view.columns || [], emptyText: view.emptyText || "暂无条目" }
  // 永不伪造行；dataValue 为 null（拉失败由 app 层传 null + 错误文案另显）
export function statusItems(view, dataValue)
  // items = extractPath(dataValue ?? {}, "items") 数组优先，否则 view.items；无 → []
  // 每项 {label,value,kind?,tone?} 透传（渲染层映射 CSS 类）
```

app.js：`loadBody` 泛化——`validateDeclaration` 通过后按 kind 分派：
form → 现路径；status/list → dataRpc `rpc()` 拉数据（**拉失败显示诚实错误行** + 静态兜底
`view.items/view.rows` 照常）→ `statusItems/listRows` 画 items 表 / 表格；有 dataRpc 的卡
渲染器内置「刷新」按钮 = 重放 dataRpc（渲染 affordance，非契约动作）。

`validateDeclaration` 的 list 体校验补一行：`rowsPath` 缺 → view-malformed（status 无体
要求，items 可缺省空态）。

## 3. B：`wasm-plugins/panel-plugin-inventory/`（照 llm-deepseek 型）

| 件 | 内容 |
|---|---|
| `wit/panel-plugin-inventory.wit` | 复用 `dsh:host-remote` remote+host_services 接口身份（照抄 llm-deepseek 的 wit，改 world 名注释） |
| `src/lib.rs` | `handle`：`describeUI`（v2 list 卡声明）；`list`（`host_services.get("loader")` → 行投影）；未知 → fail-loud |
| `plugin.json` | `{ "web": "web", "caps": ["remote"], "world": "remote" }`（wasm 走缺省构建约定） |
| `web/ui.json` | v2 卡：`cardId:"panel-plugin-inventory.list"`、`type:"runtime"`、`title:"插件清单"`、`size 4×4`、`view:{kind:"list", dataRpc:["panel-plugin-inventory","list"], rowsPath:"items", columns:[{key:"name",label:"插件"},{key:"id",label:"入口"},{key:"state",label:"状态"}], actions:[], emptyText:"暂无已组装入口"}` |
| `Cargo.toml` + `Cargo.lock` | 照抄 llm-deepseek（lock 手工复制改根包名——离线必需，接手文档 §4-4） |

行投影（wasm 内，双权威禁令：行语义只在这定义）：
`entries.filter(!group).map(e => { name, id, state: e.disabled ? "disabled" : (e.fiber ? "active" : "ready") })`
→ `{ok:true, value:{items:[...]}}`；loader 服务失败 → `{ok:false,...}` 透传（不伪造空列表！）。

## 4. C：宿主泛化

```rust
// lib.rs（Boot）：删 llm_deepseek_remote 字段（行为并入 map）
pub remote_carriers: Vec<(String, Rc<RefCell<WasmRemoteEndpointPlugin>>)>, // namespace → 载体

// web.rs dispatch_wasm_remote：
let plugin = boot.remote_carriers.iter().find(|(ns, _)| ns == namespace).map(|(_, p)| p)
    .or(boot.remote_plugin.as_ref())   // 未命中 → host-remote（既有语义零变）
    ...否则 not-implemented/internal（现状文案）

// web.rs serve：硬编码 llm-deepseek 块 →
pub fn scan_remote_units(wasm_base: &Path) -> Vec<PluginPackage>
// 纯扫描：子目录逐个 resolve_package；Err（缺构建物/坏清单）→ eprintln 跳过（不炸 serve）；
// 只收 world==Some("remote") 且 name != "host-remote"（宿主桥非装配单元）。
// serve 对每包：读 pkg.wasm 字节 → 载体入 remote_carriers（名字=文件夹名=namespace）→ push packages。
// 构建物缺失的单元：**跳过 + 诚实提示**（死卡不上桌布）。
```

`remote_unit_component_bytes(dir)` 泛化 `llm_deepseek_component_bytes`：缺构建 →
**尝试构建一次**（保持既有开发体验：serve 起来自动构建 llm-deepseek 的行为不变），
构建失败 → 跳过该单元（eprintln），serve 继续。

## 5. 测试计划（TDD 红→绿）

**m33（`crates/dsh-wasmrt/tests/m33_panel_plugin_inventory.rs`，仿 m32）**
1. `describe_ui_returns_valid_list_declaration`（card/list/cardId/type 闭集/rowsPath/columns）
2. `static_ui_json_matches_describe_ui`（一份契约）
3. `list_projects_loader_entries`（LoaderProjector 桩：group 过滤、disabled→disabled、
   fiber→active、其余 ready）
4. `list_service_failure_is_fail_loud`（loader 桩报错 → ok:false 透传，**不伪造空表**）
5. `unknown_endpoint_fail_loud`

**core.test.mjs 增量**
6. `validateDeclaration implements status/list, reserves only chat/chart/table`
7. `listRows rowsPath extraction + static fallback + honest empty`
8. `statusItems extraction + empty fallback`；`extractPath dotted and missing`
9. 探针红：桩 listRows 返回伪造默认行 → 测试抓（诚实断言非恒真）

**web.rs 增量**
10. `scan_remote_units_mounts_by_world_and_skips_broken`（temp 目录：合格 remote 包 /
    无 wasm → 跳 / 坏 plugin.json → 跳 / world 缺失 → 跳 / host-remote 名 → 跳）
11. `llm_deepseek_remote_routes_and_serves_static` 迁移到 `remote_carriers`（断言不动，
    证明分流行为零变——载体泛化的回归锚）

**回归**：全套 + clippy + verify-diff + node --test。

## 6. 边界重申

rowActions 只读 · chat/chart/table 不做 · SSE 不做 · loader entry 化不做 ·
harness TS 件零改动 · 宿主数据面零新后端（复用 "loader" 投影）。

## 7. 回滚点

A/B/C 各自独立可回退：A 是 assets 纯增量；B 是新目录（撤目录即消失）；
C 撤提交回 `1b0708a`（llm_deepseek_remote 字段随提交回来）。
