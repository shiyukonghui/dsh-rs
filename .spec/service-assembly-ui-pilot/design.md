# 设计结论 v1：llm-deepseek 服务装配单元试点（rust + ui 声明 + wasm）

日期：2026-08-28
阶段：系统设计（瀑布流阶段 2）——基于 `.spec/service-assembly-ui-pilot/requirements.md`（试点选定）。
依据：P2 架构模型（`.spec/service-assembly-ui/design.md`）+ host-remote 既有载体模式
（D-115-Web D3）+ 插件包文件夹形态（D-175）。
范围：声明 schema 子集 + 渲染器契约 + 发现面 + RPC 动作面 + wasm world 契约 + 宿主接线。

> **✅ 声明形态已迁移（v1→v2 已落地：D-181 定契约 / D-182 落编码）**：本文 §2/§4 写的是
> **v1 历史设计**（顶层 `kind:"form"` 平铺），该形态已被**桌布架构 v2**
> （`.spec/service-assembly-ui-canvas/design.md`）取代为 `kind:"card"` +
> `view:{kind:"form",…}`，并加了 `cardId` / `type:"model"` / `size 2×3` / `view.dataRpc`。
> **代码现况**：`web/ui.json` 与 wasm `ui_declaration()` **均已是 v2**；m32 的
> `no_legacy_v1_top_level_declaration_anywhere` 断言阻止 v1 顶层形态回潮（红验证已做）。
> **本文其余部分仍然有效**——wasm world 契约、RPC 动作面
> （describeUI/currentValues/save/discoverModels）、host-services 落盘、
> 「声明=数据、静态与 describeUI 逐字段一致」、宿主接线与试点边界，全部沿用不变。
> §10 给出 v1→v2 的逐项对照与「什么没动」。

## 1. 架构落点（延续 P2 主线）


```
cordis.yml 声明行 / wasm_base 文件夹 ──▶ 插件包 llm-deepseek
   ├─ plugin.json（清单：wasm + web + world:remote）
   ├─ wasm 组件（export remote：describeUI/save/discoverModels；import host-services）
   └─ web/ui.json（静态声明）+ web/index.html + web/renderer.js（最小通用渲染器 demo）
                │ 声明=数据（JSON 文本）
                ▼
          GUI 壳 / 通用渲染器：读声明 → 渲染表单 → 动作 `POST /api/llm-deepseek/save`
                │
                ▼ host-services
          Rust 宿主：kv 落盘（key: llm-deepseek/settings）；动作白名单受宿主校验
```

- 静态声明起步（ui.json），动态 describeUI 作增强（有状态/敏感场景）——按 requirements.md P2 §2 倾向。
- Rust 角色 = 生声明 / 数据面 / 动作执行；渲染归浏览器 GUI 壳（P2 design §1.1 要点三）。

## 2. 声明 schema 子集（v1，对齐 MCP Apps/A2UI/Adaptive Cards 方向）

一个 UI 声明 = 一个 JSON 对象（数据，非代码）：

```jsonc
{
  "$schema": "dsh/plugin-ui/v1",
  "kind": "form",
  "title": "DeepSeek Provider",
  "description": "llm-deepseek 服务装配单元设置表单（P2 试点）",
  "namespace": "llm-deepseek",
  "fields": [
    { "name": "apiKeyEnv", "label": "API Key 环境变量", "type": "text",
      "role": "credential-ref", "default": "DEEPSEEK_API_KEY", "required": true },
    { "name": "baseURL", "label": "Base URL", "type": "text", "default": "https://api.deepseek.com" },
    { "name": "thinking", "label": "Thinking", "type": "select",
      "options": ["enabled", "disabled"], "default": "enabled" },
    { "name": "reasoningEffort", "label": "Reasoning Effort", "type": "select",
      "options": ["off", "low", "high", "max"], "default": "high" },
    { "name": "maxTokens", "label": "Max Tokens", "type": "number", "default": 256000, "min": 1 },
    { "name": "defaultContextWindow", "label": "Default Context Window", "type": "number",
      "default": 1000000, "min": 1 },
    { "name": "models", "label": "Models（目录）", "type": "list", "item": {
        "type": "object",
        "fields": [
          { "name": "id", "label": "Model ID", "type": "text", "required": true },
          { "name": "name", "label": "显示名", "type": "text" },
          { "name": "contextWindow", "label": "Context Window", "type": "number", "min": 1 }
        ] } }
  ],
  "actions": [
    { "name": "save", "label": "保存", "rpc": ["llm-deepseek", "save"], "primary": true },
    { "name": "discoverModels", "label": "发现模型", "rpc": ["llm-deepseek", "discoverModels"] }
  ]
}
```

### 2.1 渲染器契约（单一通用组件，只读声明）
- 输入：声明 JSON + 初始值（可空）。
- 输出：DOM 表单（text / number / select / list<object>）+ 按钮（每个 action 一个）。
- 动作调用：`POST /api/<ns>/<m>` body `{ args: <actionArgs> }`（对齐既有前端 gateway
  `payload.args` 解包，见 web.rs dispatch_wasm_remote）。save 的 args = `{ values: {...} }`。
- 坏声明 fail-loud（校验声明结构，不白屏）。
- **无任意 JS 执行**：渲染器只解释 JSON 字段 → 表单控件，插件不提供脚本。

## 3. wasm world 契约（复用 host-remote 接口，组件模型专）

`wasm-plugins/llm-deepseek/wit/llm-deepseek.wit`：

```wit
package dsh:llm-deepseek;
use dsh:host-remote/remote;
use dsh:host-remote/host-services;
world llm-deepseek {
  export remote;
  import host-services;
}
```

- 复用 `dsh:host-remote/remote.remove`, `dsh:host-remote/host-services` 同一接口身份 →
  宿主 `WasmRemoteEndpointPlugin`（已绑定 host-remote world）无需改绑定即可加载。
- `remote.handle(namespace, method, body)`，namespace = `llm-deepseek`。

## 4. 端点/动作面（RPC 动作白名单，宿主校验）

| namespace.method | 入参 | 行为 | 返回 |
|---|---|---|---|
| `llm-deepseek.describeUI` | `{}` | 返回 UI 声明（与 ui.json 一致） | `{ok:true, value:{...声明}}` |
| `llm-deepseek.currentValues` | `{}` | 读宿主 kv `llm-deepseek/settings` | `{ok:true, value:{values}}`（无则空） |
| `llm-deepseek.save` | `{values:{...}}` | 校验已知字段 → `host-services.set("kv",{key:"llm-deepseek/settings",value})` | `{ok:true, value:{saved:true}}` |
| `llm-deepseek.discoverModels` | `{}` | 返回 DEFAULT_MODELS（V4 Flash / V4 Pro / V4 Flash Vision Exp） | `{ok:true, value:{models:[...]}}` |

- 未知 method → 规范化 `{ok:false, error:{code:"internal", message:...}}`（fail-loud，不伪造成功）。
- 坏 JSON / 未知字段 → 同上（白名单校验，不落盘）。

## 5. 发现面（静态优先）
- 静态：GUI/渲染器从 `/plugins/llm-deepseek/ui.json` 拉声明（serve_package_asset，D-175 机制）。
- 动态：`describeUI` 经 `/api/llm-deepseek/describeUI`（增强路径）。
- 清单：试点阶段渲染器 demo 直连静态路径；pluginInventory 集成留后续（需 entry 化）。

## 6. 宿主接线（web serve，最小侵入）
1. `Boot` 增 `llm_deepseek_remote: Option<Rc<RefCell<WasmRemoteEndpointPlugin>>>`（镜像 host-remote）。
2. `llm_deepseek_component_bytes()` 读 `wasm-plugins/llm-deepseek/target/wasm32-wasip1/debug/llm_deepseek_plugin.wasm`。
3. `dispatch_wasm_remote`：namespace == `llm-deepseek` → 用该载体；否则 host-remote（不动既有路由）。
4. serve 装配时 `resolve_package(wasm_base,"llm-deepseek")` 追加进 `boot.packages` → `/plugins/llm-deepseek/**` 静态挂接。
5. 持久化复用既有 `kv` 后端（RemoteHost.set/get "kv"，无需改 remote_host.rs）。

## 7. 测试（TDD，红→绿）
- `crates/dsh-wasmrt/tests/m32_llm_deepseek.rs`（独立，仿 m31_host_remote）：
  - describeUI 返回有效声明（fields 含 apiKeyEnv/models、actions 含 save）；
  - save 写入测试 kv 投影器 + 坏入参 fail-loud；
  - discoverModels 返回 3 个默认模型。
- 全回归：workspace / clippy / verify-diff 26/26 / serve 冒烟不回归。

## 8. 交付物清单
- `wasm-plugins/llm-deepseek/`（Cargo.toml / wit / src/lib.rs + bindings / plugin.json / web/）
- `crates/dsh-cli/src/web.rs` + `lib.rs` 接线（llm-deepseek remote 载体 + 静态 ui.json）
- `crates/dsh-wasmrt/tests/m32_llm_deepseek.rs`
- 本目录 requirements/design/acceptance + DECISIONS 条目

## 9. 边界重申（延续 requirements §1.3）
- 不做真实 LLM 调用 / 不做 loader entry 依赖激活 / 不做渲染器产品化 / 不做 SSR。

## 10. v2 迁移指针（D-181，下一编码阶段 C1）

桌布架构（`.spec/service-assembly-ui-canvas/design.md`）把声明升为 `dsh/plugin-ui/v2`。
本试点的迁移**只动声明形态，不动任何机制**：

| 要素 | v1（本试点现状） | v2（迁移后） |
|---|---|---|
| 顶层 | `kind:"form"` | `kind:"card"` |
| 分类 | 无 | `type:"model"` |
| 尺寸 | 无 | `size:{w,h}`（封顶 w≤4/h≤8） |
| 标识 | 无 | `cardId:"llm-deepseek.settings"` |
| 内容 | `fields`/`actions` 平铺 | 原样搬进 `view:{kind:"form",…}` |

- **不动**：wasm world（`export remote` + `import host-services`）、四个端点、kv 落盘、
  白名单校验与 fail-loud、静态/动态**逐字段一致**约束、宿主路由与 `serve_package_asset`。
- **验证护栏**：m32 断言同步升级为 v2；新增「全仓无顶层 `kind:"form"` 残留」断言——
  这一条是**双模型防线**，确保 v1 形态被真正废止而非并存。
