# 设计结论 v1：桌布（Canvas）架构契约 —— 卡片壳 / 双枚举 / 网格排布 / 清单口子

日期：2026-08-28
阶段：系统设计（瀑布流阶段 2）——基于 `.spec/service-assembly-ui-canvas/requirements.md`（已过闸）。
决策记录：`docs/DECISIONS.md` **D-181**。
性质：**契约定稿，本轮零实现代码**。
不变式（承 D-179，本架构不推翻）：声明是数据不是代码 / 渲染器在浏览器 / Rust 只生声明不做渲染。

---

## 1. 架构总览（一条主线）

```
┌─ Rust 服务装配单元（wasm 插件包）──────────────────────────────┐
│  静态：web/ui.json（卡片声明，数据）                            │
│  动态：remote.handle("describeUI")（同一份声明；有状态增强）      │
│  动作：remote.handle(namespace, method, body) → host-services   │
└───────────────┬───────────────────────────────────────────────┘
                │ ①清单（元数据）      ②声明（内容）        ③动作
                ▼                      ▼                    ▼
┌─ 桌布 GUI 壳（浏览器，单一通用渲染器）────────────────────────┐
│ /api/uiManifest/list (args.rev)  →  GET /plugins/<n>/ui.json │
│   ├─ 左栏：按 type 分组 + 计数 + 插件名                        │
│   ├─ 右栏：分类内卡片按 size 自动流式排布（10px 网格）          │
│   ├─ 渲染器分派：view.kind → form / status / list / 回落       │
│   └─ 动作 → POST /api/<ns>/<m> {args} → Rust 执行（宿主校验）  │
└──────────────────────────────────────────────────────────────┘
                ▲ 变更事件（rev++）：/plugins/events SSE
```

三条通道的分工是**刻意的**：清单只给**排布所需元数据**（type/title/size），内容声明**按需拉取**
（选中分类才拉该组卡片），动作走既有 RPC 面。这样清单可以很轻，且热插只改清单。

---

## 2. 声明 schema v2（卡片壳 + 视图）

```jsonc
{
  "$schema": "dsh/plugin-ui/v2",
  "kind": "card",                         // 顶层唯一容器（v1 只有这一种）
  "cardId": "llm-deepseek.settings",      // 插件内稳定唯一；卡身份 = (插件名, cardId)
  "type": "model",                        // 分类枚举（§3）；未知 → misc 回落
  "title": "DeepSeek Provider",
  "description": "DeepSeek provider 连接与模型目录设置",
  "size": { "w": 2, "h": 3 },             // 格数；封顶 w≤4/h≤8；不含坐标
  "view": {                               // 渲染契约（§4）
    "kind": "form",
    "fields": [ /* 复用 D-180 字段集 */ ],
    "actions": [ /* 复用 D-180 动作集 */ ]
  }
}
```

### 2.1 与 D-180 的关系：**替换**，不是并列

| | D-180（v1） | 本架构（v2） |
|---|---|---|
| 顶层 | `kind:"form"` + fields/actions 平铺 | `kind:"card"` + `view` 嵌套 |
| 分类 | 无 | `type` |
| 尺寸 | 无（单页表单） | `size{w,h}` |

**迁移 = 把原平铺的 `fields`/`actions` 整体搬进 `view`，外面套 card 壳**。
旧顶层 `kind:"form"` **废止**，全仓**不留第二种顶层形态**（用户「一步到位、不要两套并行 schema」
的落点）。`$schema` 升 `dsh/plugin-ui/v2`；渲染器遇 `dsh/plugin-ui/v1` → **显式 fail-loud**
（`schema-version-unsupported`），**不做静默兼容**——静默兼容正是双模型崩塌的开始。

---

## 3. `type` 枚举（分类轴，闭集）

`model` | `config` | `capability` | `runtime` | `resource` | `session` | `misc`

| 值 | 语义 | 例 |
|---|---|---|
| `model` | 模型 / Provider | llm-deepseek |
| `config` | 偏好设置 | 主题/语言/onboarding |
| `capability` | 工具 / 技能 | 工具清单、技能面板 |
| `runtime` | 运行时编排 | jobs / schedule / goal / plan |
| `resource` | 资源 | fs、凭据、工作区 |
| `session` | 会话相关 | 会话列表、历史 |
| `misc` | 兜底 | 未知 type 落此 |

**回落语义**：`type` 缺失或不在枚举 → 卡片**照常渲染**，归入 `misc`，并把原始值留在
`declaredType` 供诊断与侧栏 tooltip（诚实：不隐藏用户的声明，也不白屏）。

**加值规则**：新增值 = **向枚举加成员**（侧栏多一组），**不需要**任何渲染器改动，走一次 DECISIONS。
这正是它与 `view.kind` 必须分离的原因（见 D-181 概念纠偏）。

---

## 4. `view.kind` 契约（渲染轴，三档制）

### 4.1 ✅ v1 实现

**`form`**（复用 D-180，字段/动作语义不变）
```jsonc
"view": { "kind": "form",
  "fields": [ {name,label,type:"text|number|select|list",default?,required?,options?,min?,item?} ],
  "actions": [ {name,label,rpc:[ns,method],primary?} ] }
```

**`status`**（只读状态卡；无输入，可有刷新动作）
```jsonc
"view": { "kind": "status",
  "items": [ {label, value, kind?: "text|number|badge|bool", tone?: "ok|warn|err|idle"} ],
  "actions": [ {name,label,rpc:[ns,method]} ] }   // 典型：刷新
```

**`list`**（行列表；行可带行动作）
```jsonc
"view": { "kind": "list",
  "columns": [ {key,label,type?} ],
  "rowsPath": "items",                    // 数据里行数组的位置
  "rowActions": [ {name,label,rpc:[ns,method],scope:"row",confirm?:true} ],  // C6（D-189）：confirm:true → 渲染器执行前必须用户确认（只认严格 true）；参数线形状 args = { row: <该行完整对象> }——单元自校验身份（渲染器不是安全边界）
  "actions": [ {name,label,rpc:[ns,method]} ],   // 卡级动作
  "emptyText": "暂无条目" }
```

> `status`/`list` 的**数据来源**：与 D-180 一致——渲染器启动时调 `view.dataRpc`（或卡级 `refresh`
> 动作）拉宿主真实数据；`view.items`/`view.rows` 可作静态兜底。诚实规则沿用：拉不到就显示
> 诚实空态/错误态，**绝不伪造**。

### 4.2 🔒 契约预留（签名先定，渲染器未建 → fail-loud 回落）

```jsonc
"view": { "kind": "chat",  "sessionIdRef"?: string, "actions":[...] }   // 消息流 + 输入
    // ↑ C8 契约已定稿（D-193，实现排期 C8-1..4）：sessionSource/historyRpc/sendRpc
    //   三个 [ns,method] 面 + stream:"session-events"（闭集）；详见
    //   .spec/service-assembly-ui-c8-chat/design.md（会话协议归宿主原生臂，单元只拥有声明）
"view": { "kind": "chart", "seriesPath": string, "xKey": string, "actions":[...] }
"view": { "kind": "table", "columnsPath"?: string, "rowsPath": string }
```

**回落渲染**（三档共用，不白屏）：画一张元数据卡——`type` 徽章 + `title` + `view.kind` +
一行说明「该视图渲染器未实现（契约已预留）」，并让**卡级动作仍可用**（能用的功能不因渲染器
缺失而消失）。RPC 返回 `error.code = "renderer-unimplemented"`。

### 4.3 ❌ 否决：`board`

「board」= 桌布/画布本身。卡片内再嵌画布 = **无限递归**（画布套画布），v1 无解且无价值。
「一卡里显示一行行条目」的真实需求已由 `list` 覆盖。渲染器遇 `kind:"board"` →
`error.code = "view-kind-rejected"`（显式拒绝，**不静默降级成 list**——那会让契约说谎）。

---

## 5. `size` 与排布规则（坐标不外泄）

### 5.1 声明与裁剪
- `size{w,h}` = 网格格数；**契约封顶 `w≤4`、`h≤8`**。
- 超出 → **画布裁剪到上限 + 记录**（诊断面可见 `size-clamped`）。这是**降级不是失败**：
  卡片照常渲染，布局不崩（用户点 5c 裁定）。
- `size` 缺失 → 取该 `type` 的默认尺寸（`model/config→2×3`，`status→2×2`，`list→4×4`）。

### 5.2 排布算法（画布职责，插件不感知）
1. 可用列数 `C = floor((容器宽 + 格距) / (格宽 + 格距))`，**窄屏缩列、宽屏多列**；
   并保证可用列数不小于最小阈值（否则单卡 `w` 按可用列数收）。
2. 卡片按**声明顺序**（= 清单序，**无 `priority` 字段**）依次放入：行内首行优先找能容纳该 `w`
   的空隙，占 `h` 行；不足宽度换行（瀑布流式推进）。
3. **格距 10px**（不用打印 pt——依赖 DPI 缩放，屏幕单位不稳）。
4. 焦点（点侧栏名）= 滚动到该卡 + 高亮描边，**不改布局**（多卡共存是工作台本义）。
5. 空分类 = 诚实空态（不是白屏）。

---

## 6. 发现面与热插拔口子

### 6.1 `uiManifest/list`（D-183 wire 形状修订：与 `pluginInventory/list` 同形，不开裸路由特判）

`POST /api/uiManifest/list`（client-request 信封；`args.rev?` = 客户端已持有的清单哈希）：
```jsonc
{ "ok": true, "value": {
  "rev": "3f9a…64-hex sha256",          // 内容哈希（D-183：非单调计数，重启后仍稳定）
  "cards": [ {
    "pluginName": "llm-deepseek",
    "cardId": "llm-deepseek.settings",
    "type": "model",
    "title": "DeepSeek Provider",
    "size": { "w": 2, "h": 3 },
    "declPath": "/plugins/llm-deepseek/ui.json"   // 或 describeUI 标记
  } ] } }
```
`args.rev` == 当前值 → `{ "rev": …, "unchanged": true }`（省带宽，无 cards）。

**硬约束（热插拔的关键）**：
- 清单**每次请求从实时状态计算**（`boot.packages` + loader 生效 entries + 各包 `ui.json` 元数据），
  **禁止启动期快照缓存**——否则热插新插件不出卡片，D-175/D-177 的热更语义就断了。
- `rev` = 清单内容的**代数**（内容哈希或单调计数）。桌布携 `rev` 轮询/收 SSE，只在变化时重取。
- 清单**只含元数据**，不含 view 内容（内容按分类选中才拉，保持清单轻）。
- **失败面**：某包 `ui.json` 缺/坏 → 该卡**以坏卡条目出现**（`declPath` + `error`），画布画
  fail-loud 元数据卡。**不静默丢弃**（诚实：装了但坏了，要让用户看见）。

### 6.2 变更通知
复用既有 `/plugins/events` SSE（D-099/D-175 通道）新增事件类型 `ui-manifest-changed {rev}`；
桌布收到即重取清单并按 `rev` 差量增删卡片。热拔 = 条目消失 → 网格位回收；
热插 = 新条目 → 自动入网格。

---

## 7. 渲染器分派与 fail-loud 总表

| 情形 | 行为 | code |
|---|---|---|
| 未知 `type` | 归 `misc`，正常渲染，保留原值 | — |
| `size` 越上限 | 裁剪 + 记录，正常渲染 | —（诊断 `size-clamped`） |
| `view.kind` 未定义 | fail-loud 元数据卡 | `view-kind-unknown` |
| `view.kind` 已否决（`board`） | fail-loud 元数据卡，显式拒绝 | `view-kind-rejected` |
| `view.kind` 契约预留（`chat`/`chart`/`table`） | fail-loud 元数据卡，**卡级动作仍可用** | `renderer-unimplemented` |
| `view.kind` ∈ v1 但 view 体不合契约 | fail-loud 元数据卡（列具体缺陷） | `view-malformed` |
| `$schema` 非 v2 | fail-loud 元数据卡，**不静默兼容** | `schema-version-unsupported` |
| 顶层 `kind` ≠ `"card"` | fail-loud 元数据卡（v2 唯一顶层容器） | `card-kind-unknown` |
| 声明整体非 JSON / 非对象 | 画布画坏卡（清单已带 error） | `declaration-unparseable` |

统一原则（沿用 D-179/D-180）：**绝不伪造成功、绝不白屏、坏的一侧显式可见**。

> `card-kind-unknown` 是 C1 编码时**补进契约的一行**：`$schema` 对但顶层容器写错（例如仍写
> `kind:"form"`）时，只报「版本不对」会指错方向；显式区分「版本错」与「容器种类错」，
> 坏声明的诊断才落到真正的问题上。这类细分属于契约的正常收敛，不改变 v1 形态已废止的事实。

---

## 8. 组件职责矩阵（承 D-179 §2，增列桌布项）

| 组件 | 归属端 | 职责 |
|---|---|---|
| 卡片声明生产者 | Rust 插件（wasm） | 出 `kind:"card"` 声明（含 type/size/view）；产物=数据 |
| 卡片元数据聚合 | **Rust 宿主** | `uiManifest/list`（实时计算 + rev）；坏声明带 error |
| 变更广播 | Rust 宿主 | `/plugins/events` 加 `ui-manifest-changed` |
| 静态分发 | Rust 宿主 | `/plugins/<name>/**`（复用 `serve_package_asset`，D-175） |
| 动作执行 / 权限 | Rust 插件（wasm） | 白名单校验 + `host-services` 落盘；fail-loud |
| 侧栏分类聚合 | 浏览器桌布壳 | 按 `type` 分组 + 计数；未知落 `misc` |
| 网格排布 | 浏览器桌布壳 | 10px 格距自动流式；裁剪；焦点滚动 |
| 通用渲染器 | 浏览器桌布壳 | 按 `view.kind` 分派；只读数据 → 天然沙箱 |

---

## 9. 与既有面的衔接 + 迁移清单

| 既有点 | 处理 |
|---|---|
| `wasm-plugins/llm-deepseek/web/ui.json`（`kind:"form"`） | **迁移**为 `kind:"card"` + `view.form`；`type:"model"` + 定 `size` |
| `src/lib.rs::ui_declaration()`（wasm 生声明） | 同步迁移（m32 的「静态与 describeUI 逐字段一致」断言继续守护） |
| `m32_llm_deepseek.rs` | 红→绿迁移：断言改 v2 形态；新增「顶层无 form 残留」断言 |
| 桌布渲染器 demo（`web/renderer.js`） | 从「单页表单」升级为「清单 → 侧栏 → 网格 → 分派渲染器」最小桌布 |
| `dispatch_wasm_remote` namespace 路由 | **不动**（卡片化只影响声明形态与呈现，不影响 RPC 面） |
| `Boot.llm_deepseek_remote` | 不动；后续多插件时按同型扩展为「每装配单元一载体」 |

---

## 10. 测试计划（编码阶段 TDD 红→绿；不破坏基线）

1. **契约层**（Rust）：v2 声明解析/校验单测——type 枚举 + 未知落 misc；size 裁剪；
   `$schema` v1 显式拒绝（红→绿）。
2. **迁移层**（Rust）：llm-deepseek 迁移后 m32 全绿 + 新增断言：全仓无顶层 `kind:"form"` 残留。
3. **清单层**（Rust）：`uiManifest/list` 集成测试——多包聚合、`rev` 随增删变化、
   坏包以 error 条目出现不静默丢。
4. **桌布壳**（前端）：fail-loud 表逐行断言（含 `renderer-unimplemented` /
   `view-kind-rejected` / `schema-version-unsupported`）；排布无重叠/无出界（窄 + 宽两档视口）。
5. **全回归**：workspace / clippy `-D warnings` / verify-diff / serve 冒烟不回归。

---

## 11. 编码切分建议（下一轮起，走 TDD）

- **C1**：v2 契约 + llm-deepseek 迁移（`form` → `card.view.form`）+ m32 红→绿 ← *最小可验证第一步*
- **C2**：`uiManifest/list`（实时 + rev）+ 坏声明 error 条目
- **C3**：桌布壳最小集（侧栏分类 + 网格排布 + `form` 渲染器 + fail-loud 表）
- **C4**：`status` / `list` 渲染器 + 真实数据面（首个 `list` 试点建议 = 插件清单）
- **C5**：`ui-manifest-changed` SSE 接热插拔（最小验证：装/卸一包，卡片增删可见）

---

## 12. 边界重申（不做）

无拖拽 / 无自由摆放 / 无布局持久化 · 无卡内嵌画布（`board` 否决）· 不实现 `chat`/`chart`/`table` ·
不推翻 harness 前端（增量迁移）· 沿用试点边界（无真实 LLM 调用 / 无 loader entry 依赖激活 / 无 SSR）。

---

## 13. 实施状态

| 切分 | 状态 | 证据 / 备注 |
|---|---|---|
| **C1** v2 契约 + 试点迁移 | ✅ **已落地**（2026-08-28，D-182） | `web/ui.json` + wasm `ui_declaration()` 均升 `card→view.form`（`type:"model"`、`size 2×3`、`cardId`、`view.dataRpc`）；m32 **8/8 绿**，含 `declaration_satisfies_v2_card_contract` 与双模型防线 `no_legacy_v1_top_level_declaration_anywhere`；dsh-cli `llm_deepseek_remote_routes_and_serves_static` 绿；clippy `-D warnings` 0；全量 225 通过 / 5 失败均为**基线既有** M5 bash 环境性失败（与 C1 无关，git stash 已验证） |
| **C2** `uiManifest/list` | ✅ **已落地**（2026-09-04，D-183） | `crates/dsh-cli/src/ui_manifest.rs`（实时聚合纯函数 + sha256 内容哈希 rev + 坏声明 error 条目 + type/size/title 归一 + disabled 交叉）+ `dispatch()` 原生臂；11 新测试（桩红 10/11 + **缓存探针红验证**：注入 OnceLock 快照缓存 → 实时性测试必红）；dsh-cli **241/0**（基线 230 + 11 新增），m32 8/8，clippy **0**，verify-diff **26/26**；详见 `.spec/service-assembly-ui-c2/acceptance.md` |
| **C3** 桌布壳最小集 | ✅ **已落地**（2026-09-05，D-184） | `/canvas` 独立视图（资产编译进 dsh-cli，SPA 前拦截，miss→404）；`core.js` 纯函数（buildModel/layoutGrid 可证无重叠/validateDeclaration §7 九行/rpcEnvelope/pollDecision）12 测试 + canvas.rs 3 测试；侧栏分类 + 10px 瀑布工作台 + form 渲染器 + dataRpc + 4s rev 轮询；附带修复 demo renderer 裸 `{args}` 信封缺陷；dsh-cli **244/0**、clippy **0**、`node --test` **12/12**；`status/list` 落回落待 C4；详见 `.spec/service-assembly-ui-c3/acceptance.md` |
| **C4** `status`/`list` 渲染器 + 首个面板改写 | ✅ **已落地**（2026-09-05，D-185） | 渲染器实现档齐（form/status/list；list 加 rowsPath 必备校验）；首个 harness 面板改写 = `wasm-plugins/panel-plugin-inventory`（list 卡，wasm 自持 loader 行投影，服务失败不伪造空表）；宿主泛化 `Boot.remote_carriers` + serve `scan_remote_units` 发现挂载（关死「每面板一次宿主提交」）；node 16/16、m33 5/5、dsh-cli **246/0**、clippy **0**；详见 `.spec/service-assembly-ui-c4/acceptance.md` |
| **C5** 热插拔 SSE | ✅ **已落地**（2026-09-05，D-186） | serve 主循环 tick（2s 节流）同步 scan 挂载的单元装/卸（运行时**不构建**、失败不炸、只卸 mounted 登记）→ rev 变经 `/plugins/events` 广播 `ui-manifest-changed {rev}`；桌布 EventSource 消费 + 10s 轮询兜底；watch 4 测 + 帧形状测（桩红→绿；clippy 复活一条被吞 `#[test]`）；dsh-cli **251/0**、clippy **0**；详见 `.spec/service-assembly-ui-c5/acceptance.md` |
| **C6** 行动作 + 确认 | ✅ **已落地**（2026-09-05，D-189） | §4.1 rowActions 渲染 + `confirm` 契约字段（只认严格 true；v1 = window.confirm）；线形状 `args={row}` 单元自校验；首张写能力卡 = panel-dynamic-plugins stop/undefine（宿主 dynamicStop/Undefine 透传）；node 19/19 + m35 10/10（探针红×3 生效）；详见 `.spec/service-assembly-ui-c6-row-actions/acceptance.md` |
| **C7** 面板改写 ×N | ✅ **持续进行**（D-187–D-192，6/N） | 改写型六卡批量落地（inventory/runtime-status/dynamic-plugins(+写动作)/workspace-files/sessions/settings 概览）；D-181 五语义位全有真实卡；台账 `.spec/service-assembly-ui-panels/progress.md` |
| **C8** chat 视图 | ✅ **已落地**（2026-09-05，D-193，切片 C8-1..4） | chat 校验 + `chatFoldFrame`/`chatOptions`（node 26/26）；宿主面复用回正（session.history 既表面 + list/prompt 别名）；`renderChat` 四档点亮；`panel-chat` 声明单元（第八卡，零自有数据端点）；SSE 直订待帧形状取证（轮询同事实源）；`.spec/service-assembly-ui-c8-chat/acceptance.md` |

**C1 的三点诚实交代**：

1. **双模型防线经过红验证**：临时放入一个 `$schema:v1 / kind:"form"` 的探针声明 →
   `no_legacy_v1_top_level_declaration_anywhere` **FAILED** 并精确报出违规文件路径；
   移除探针后复绿。即该护栏不是「恒真的装饰断言」。
2. **`renderer.js` 是包内 demo，不是桌布壳**。C1 只把它升到「正确消费 v2 单张卡片 +
   按 §7 表 fail-loud 分派」；真正的侧栏分类 + 网格工作台在 C3。因此 `status`/`list` 在
   **当前** demo 里落 `renderer-unimplemented` 回落——这正是三档制回落语义的用途（契约已定、
   实现未点亮），不是缺陷，也不是虚报。
3. **契约补了一行**（§7 `card-kind-unknown`）：实现时发现「版本对但顶层容器写错」若只报
   `schema-version-unsupported` 会指错诊断方向，故把两种坏法分开。属正常收敛，
   不改变「v1 顶层形态已废止」的结论。
