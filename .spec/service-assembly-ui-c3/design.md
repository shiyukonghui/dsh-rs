# 设计结论：桌布 C3 —— 桌布壳最小集

日期：2026-09-05
阶段：系统设计（瀑布流阶段 2）——基于 requirements.md（自主过闸，默认值可回退）。
决策记录：`docs/DECISIONS.md` **D-184**。
上游契约：canvas design §4/§5/§6/§7；C2 清单 wire（D-183）。

---

## 1. 布局决策：资产宿主 + 路由

```
crates/dsh-cli/assets/canvas/          ← 桌布壳唯一前端源（随 dsh-cli 编译嵌入）
  index.html      壳页面（<script type="module" src="assets/app.js">）
  canvas.css      样式 + 网格几何 CSS 变量（--col:260px; --row:100px; --gap:10px）
  core.js         纯逻辑（零 DOM/零 fetch/零 eval）← TDD 主战场
  app.js          DOM/fetch/timer 粘合层（薄；手测 + 冒烟覆盖，诚实声明）
  package.json    {"type":"module"}（node --test 的 ESM 标记，浏览器忽略）
  tests/core.test.mjs                  ← `node --test crates/dsh-cli/assets/canvas/tests/`
crates/dsh-cli/src/canvas.rs
  canvas_response(path) -> Option<(u16, &'static str, &'static [u8])>   ← include_str! 闭环集
crates/dsh-cli/src/web.rs
  serve 闭包：/api、/plugins 之后、SPA fallback **之前**插 /canvas 块；
  未识别的 /canvas/* → 404（不回落 SPA——防「桌布失踪变前端」的诡异现场）
```

理由：web_root 是 harness dist（外部件），插件基建不混入；`include_str!` 零路径配置、
随二进制分发、可测。**独立视图**承诺兑现：harness 前端一行不动。

## 2. `core.js` 纯函数契约（全部经 `node --test` 钉死）

```js
TYPE_ORDER = ["model","config","capability","runtime","resource","session","misc"] // misc 恒末
buildModel(manifestValue) -> {
  rev, cards: [entry], groups: [{type, cards:[entry]}],   // 只含有卡分类；组内保声明序
}                                                          // error 条目 → type:"misc" 坏卡
layoutGrid(cards, C) -> { positions: [{key,col,row,w,h}], totalRows }
  // w=min(w,C) 收窄；卡顶 = max(heights[col..col+w-1])；平手取最左（严格 < 扫描）；
  // heights[span] = top+h。→ 可证：无重叠、col+w≤C、行推进。
validateDeclaration(decl) -> null | {code, message}
  // §7 链：非对象→declaration-unparseable；$schema≠v2→schema-version-unsupported；
  // kind≠card→card-kind-unknown；view/view.kind 缺→view-malformed；board→view-kind-rejected；
  // chat|chart|table→renderer-unimplemented；未知→view-kind-unknown；
  // form 缺 fields/actions 数组→view-malformed。（IMPLEMENTED=["form"]，status/list 本轮同落 unimplemented）
fieldsPlan(view) / collectValues(view, read)   // read(name)->raw；number 转换；list JSON.parse
                                               // 失败→抛 {field,message}（fail-loud，动作不发）
rpcEnvelope(method, args, rpcId) -> {type:"client-request", rpcId, method, payload:{args}}
pollDecision(current, value) -> {action:"keep"|"replace", rev, cards}  // unchanged→keep
focusKey(card) -> `${pluginName}/${cardId}`                             // 焦点不改布局：
                                               // app 层 focus 只加类名 + scrollIntoView
```

**双权威禁令**：壳**不重做** type/size 归一（信清单）；`validateDeclaration` 只回答
「这张声明画不画得出」。error 条目不发 fetch、直接坏卡（清单已判死刑）。

## 3. app.js 粘合层（薄胶水，明确职责边界）

- `POST /api/uiManifest/list`（信封）拉清单 → `buildModel` → 侧栏 + 工作台。
- 工作台：当前分类（或「全部」）的 cards 按 `layoutGrid(cards, columnsForWidth(el.clientWidth))`
  绝对定位；`columnsForWidth = floor((W+gap)/(col+gap))`（core.js 纯函数）。
- 卡体：正常条目 → `GET declPath` → `validateDeclaration` → `form` 渲染 / 回落元数据卡
  （**卡级动作仍渲染可用**，§4.2 明令）；error 条目 → 直接 fail-loud 卡。
- 动作/dataRpc：`rpcEnvelope(ns+"/"+m, args, rid)` → POST；状态行诚实（✗ code / ✓ value）。
- 焦点：点侧栏名 → `document.querySelector([data-focus-key])` → scrollIntoView + `.focus-hl`；
  无任何重排调用（S6 模型级断言：`layoutGrid` 输入不变）。
- 轮询：4s `POST list {args:{rev}}` → `pollDecision`（replace 才重绘）。
- 空态：`全部` 无卡 → 「还没有服务装配单元声明 UI」文案；分类被清空 → 自动回「全部」视图。

## 4. 附带修复（requirements §2-7）

`wasm-plugins/llm-deepseek/web/renderer.js::callRpc`：裸 `{args}` → 完整 client-request
信封（真实 HTTP 下 `rpc_envelope_ok` 必 400 的实证缺陷）。形态不动，仅 wire 一行。

## 5. 测试计划（TDD 红→绿；桩红验证 + 信封探针）

**node（`tests/core.test.mjs`）**
1. `buildModel groups counts and skips empty groups`
2. `buildModel keeps error entries as misc bad cards`
3. `layoutGrid deterministic first-fit positions`（手摆期望坐标）
4. `layoutGrid property no overlap / bounds`（seeded LCG，C∈{1,3,6}）
5. `layoutGrid clamps width to available columns`
6. `validateDeclaration covers all nine fail-loud rows`（§7 表逐行 + 好 form → null）
7. `form fieldsPlan and collectValues semantics`（number/list 失败 fail-loud）
8. `rpcEnvelope exact client-request wire shape`
9. `pollDecision keep on unchanged replace on changed`
10. `focus does not change layout input identity`（模型级）

**Rust（`canvas.rs` tests）**
11. `canvas_route_serves_shell_and_assets`（html/css/js 200 + mime；`/canvas/nope` → 404）
12. web.rs 冒烟：`/canvas` 不出现在 SPA fallback 路径（既有 fallback 测试不动 + 新 404 断言）

**红验证**：先以空桩 `core.js`（导出同名空函数）跑 node 测试全红 → 实现转绿；
`rpcEnvelope` 桩刻意返回 demo 的裸 `{args}` 形——确认测试真能抓住信封缺失（与 demo 缺陷同形）。

**回归**：`cargo test -p dsh-cli -p dsh-wasmrt` 0 新增失败；clippy 0；verify-diff 26/26。

## 6. 边界重申

无拖拽/持久化 · 无 board · status/list 渲染器与 SSE 属 C4/C5 · DOM 粘合层无自动化测试
（诚实台账声明）· harness 前端零改动。

## 7. 回滚点

新资产目录 + `canvas.rs` + serve 一插 + demo renderer 一行。撤销提交即回到 C2 完成态；
SPA/清单/试点零影响。
