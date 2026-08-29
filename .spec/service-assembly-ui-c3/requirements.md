# 需求结论：桌布 C3 —— 桌布壳最小集（侧栏 + 网格工作台 + form 渲染器 + fail-loud 表）

日期：2026-09-05
阶段：需求分析（瀑布流阶段 1）
**过关方式特别说明**：用户明示「我准备休息了，你继续工作即可，不用停下来等我同意」——
本轮需求关**自主过闸**：所有需用户裁定的开放点按下表默认值推进，逐条标注**可回退**
（改动面局限在展示层常量/交互，不动契约）。决策记录 D-184。
上游：`.spec/service-assembly-ui-c2/`（清单端点已落地）、canvas design §4/§5/§6/§7/§11/§13、
D-178~D-183。

---

## 1. 目标（第一性原理拆解）

**根本目的**：清单（C2）回答了「有哪些卡」；C3 要回答「用户如何看见并用它们」。
不可再分的三件事：
1. **发现**：侧栏按 `type` 分组 + 计数 + 插件名（未知落 `misc`）；
2. **呈现**：右侧工作台 10px 网格自动排布（声明序、列数自适应、无重叠/出界）；
3. **使用**：`form` 渲染器端到端（dataRpc 预填 → 编辑 → 动作 RPC 回宿主）+ §7 fail-loud
   表逐行落地（**绝不白屏、绝不伪造、坏的一侧显式可见**）。

不变式（D-179/D-181，不得推翻）：声明=数据（**渲染器零 eval、零插件代码执行**）；
渲染器在浏览器；坐标由桌布算、永不问插件；不推翻 harness 前端（**独立视图**）。

## 2. 决策回执（自主过闸；默认值均可回退）

| # | 开放点 | 默认值（本轮采用） | 回退成本 |
|---|---|---|---|
| 1 | 默认工作台视图 | **全部**（未选分类时显示所有卡）；点分类=只看该类；再点同一分类=回「全部」 | 纯展示层，改 `app.js` 一处初始态 |
| 2 | 侧栏分组序 | 闭集枚举序（model→…→misc，misc 恒末），**不出空组** | 常量表 |
| 3 | 网格几何 | 列宽 260px / 行高单元 100px / **格距 10px（契约锁定）**，CSS 变量可调 | CSS 变量 |
| 4 | 排布算法 | 瀑布流 first-fit：卡顶 = 跨列当前高的最大值，平手取最左（声明序直觉）| 纯函数 `layoutGrid`，测试钉死行为 |
| 5 | rev 轮询间隔 | 4s（C5 SSE 落地后仅作为兜底保留） | 常量 |
| 6 | 资产宿主 | 桌布壳静态资产**编译进 dsh-cli**（`include_str!`），路由 `/canvas`；不依赖 harness dist | 路由+模块级替换 |
| 7 | 附带修复 | 试点 demo `renderer.js` 的 `callRpc` 发裸 `{args}`，**缺 client-request 信封**，经真实 HTTP 必 400（`rpc_envelope_ok` web.rs:1898 实证）——本轮按同一 wire 修正 | 一行修复 + 决策日志 |

## 3. 自上而下：成功标准分解

| # | 子目标 | 验收判据 |
|---|---|---|
| S1 | 入口 | `GET /canvas` → 200 text/html（模块引用齐）；`/canvas/assets/<file>` → 200 + 正确 mime；不劫持 SPA fallback（其他路径不变） |
| S2 | 清单消费 | 模型层：cards → 按 type 分组 + 计数；error 条目保留为坏卡；空组不出现在侧栏 |
| S3 | 排布 | 纯函数：声明序、无重叠、不出界（窄 C=1 / 宽 C=6 两档）、`w>C` 收为 C、卡片跨度=声明 size |
| S4 | §7 九行逐行 | 校验/分派纯函数逐行断言：未知 type→misc（清单层已归一，壳再防御）、size-clamped 仅诊断、`view-kind-unknown`、`view-kind-rejected`(board)、`renderer-unimplemented`(chat/chart/table，**卡级动作仍可用**)、`view-malformed`、`schema-version-unsupported`、`card-kind-unknown`、`declaration-unparseable`（清单 error 条目直接坏卡，不发请求） |
| S5 | form 渲染器 | 字段映射 text/number/select/list；`collectValues` 语义（number 转数、list JSON 解析失败 fail-loud 不落动作）；`dataRpc` 预填、拉不到用声明默认值（诚实）；动作 wire = **client-request 信封** `{type,rpcId,method:"ns/m",payload:{args}}` |
| S6 | 焦点 | 点侧栏插件名 → 该卡滚动 + 高亮；**布局不重算**（模型断言：focus 不改 layout 结果） |
| S7 | 实时 | `rev` 轮询：`unchanged` → 保留现状；变化 → 重建模型（fetch 注入假替身可测） |
| S8 | 空态 | 无卡 / 分类被清空 → 诚实空态文案（非白屏） |
| S9 | 不回归 | dsh-cli 241/0 基础上零劣化；m32 8/8；clippy 0；verify-diff 26/26；新 JS 测试 `node --test` 全绿 |

## 4. 自下而上：现有事实（已读实证）

| 事实 | 证据 | 含义 |
|---|---|---|
| SPA fallback：未知路径 → harness index.html | `web.rs:1115-1136` | `/canvas` 必须**先于** static_response 路由，否则返回前端壳 |
| `/api` 前 POST → `handle_rpc_host`（信封校验） | `web.rs:971/1959/1966` | 壳必须发完整信封；demo renderer 裸 `{args}` 是缺陷 |
| demo renderer §7 校验链 + form 渲染已有可移植实现 | `web/renderer.js:38-68/81-148` | 校验/字段映射逻辑移植为**纯函数**（DOM 零依赖）再进壳 |
| node v24 可用（verify-diff 先例） | 环境实测 | JS 核心逻辑以 `node --test` 做 TDD；DOM 胶水层保持薄并诚实声明 |
| `serve_package_asset` 提供 `/plugins/<n>/ui.json` | `web.rs:1392-1417` | 声明拉取路径零新增 |
| C2 清单已带归一（type/size） | `ui_manifest.rs` | 壳**信任清单归一**，只对拉回的声明做防御性校验 |

## 5. 非目标（越界即违规）

拖拽/自由摆放/布局持久化 · `board` · `chat`/`chart`/`table` 渲染器 · `status`/`list` 渲染器
（C4；本轮它们落 `renderer-unimplemented` 回落）· SSE `ui-manifest-changed`（C5）·
推翻 harness 前端 · SSR · 真实 LLM 调用 · 试点 entry 化。

## 6. 假设与常见错误

**假设**（用户休息期，按默认推进，回看时逐条可翻）：
- A1：§2 表 7 项默认值。
- A2：JS 核心以纯函数呈现、`node --test` 断言；**DOM 粘合层不做自动化测试**（无浏览器
  基建），以路由冒烟 + 代码评审 + 手测路径补偿，并在验收文档诚实声明。
- A3：demo `renderer.js`（包内演示）继续存在不动形态，只修 wire 信封一行——它是历史
  demo 不是桌布壳；两者并存是 C1 已声明的现状。

**最易犯错误**：
1. 把排布算法写进 DOM 副作用里（不可测、不可证无重叠）——必须先纯函数出坐标/跨度，
   CSS 只消费。
2. fail-loud 回落卡把卡级动作一起丢掉（契约 §4.2 明令「卡级动作仍可用」）。
3. 在壳里重复实现清单归一（type/size）造成**双权威漂移**——归一只信清单，声明校验
   只管「能不能画」。

## 7. 验收（阶段关卡）

1. S1-S9 全部有测试名对应；JS 测试为 TDD 红→绿，护栏断言做红验证。
2. 工件：`.spec/service-assembly-ui-c3/{requirements,design,acceptance}.md` + DECISIONS D-184 +
   canvas design §13 点亮 + git 提交互查。
3. 全回归：Rust 套件 0 新增失败 / clippy 0 / verify-diff 26/26 / `node --test` 全绿。
