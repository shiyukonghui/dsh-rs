# 验收结论：桌布 C3 —— 桌布壳最小集

日期：2026-09-05
阶段：测试验证 + 部署与维护（瀑布流阶段 4/5）。决策记录 **D-184**。
过关方式：用户休息中明示自主推进——需求/设计/验收三关**自主过闸**，默认值可回退清单
在 requirements §2（本文件 §5 再次声明）。
git 链：`08f7038`（C2 完成）→ `cc8283a`（C3 设计闸）→ 本验收提交。

---

## 1. 交付物

| 件 | 内容 |
|---|---|
| `crates/dsh-cli/assets/canvas/core.js` | 纯逻辑核心：TYPE_ORDER/buildModel/columnsForWidth/layoutGrid（可证无重叠 first-fit）/validateDeclaration（§7 九行）/collectValues/rpcEnvelope/pollDecision/focusKey |
| `crates/dsh-cli/assets/canvas/index.html` + `canvas.css` | 壳页面 + 样式（网格 CSS 变量：列宽 260/行高 100/**格距 10 契约值**） |
| `crates/dsh-cli/assets/canvas/app.js` | 薄粘合层：侧栏/工作台/焦点/动作/dataRpc/4s rev 轮询/resize 重排 |
| `crates/dsh-cli/assets/canvas/tests/core.test.mjs` | 12 测试（`node --test`）——本阶段规格源头 |
| `crates/dsh-cli/src/canvas.rs` | `/canvas` 路由纯函数（include_str! 闭环集；miss→None→404）+ 3 测试（含零 eval 哨兵 + 导出名齐哨兵） |
| `crates/dsh-cli/src/web.rs` | serve 闭包插入 `/canvas` 块（SPA fallback 之前，未识别 → 404 不回落） |
| `wasm-plugins/llm-deepseek/web/renderer.js` | 附带修复：`callRpc` 裸 `{args}` → 完整 client-request 信封（真实 HTTP 必 400 的实证缺陷，D-184） |

## 2. 逐条验收（requirements §3 判据 → 证据）

| # | 判据 | 证据 | 结果 |
|---|---|---|---|
| S1 | 入口 | `canvas_shell_served_with_asset_refs`（200 html + css/module 引用齐 + **零 `__DSH_BOOT__`** = 独立视图实证）；`canvas_assets_served_with_mimes`（3 资产 mime 齐 + ESM 可食）；`canvas_unknown_paths_are_none`（miss→404 不回落） | ✅ |
| S2 | 清单消费 | `buildModel groups…skips empty groups` + `…error entries as misc bad cards` | ✅ |
| S3 | 排布 | `deterministic first-fit positions` + `fills the gap column` + `clamps width` + `property: no overlap / in bounds`（seeded LCG × 40 卡 × C∈{1,3,6}）+ `columnsForWidth`（含保底 1 列） | ✅ |
| S4 | §7 九行 | `validateDeclaration covers all nine fail-loud rows`（含 status/list 本轮落 `renderer-unimplemented`——三档制诚实）+ canvas.rs `零 eval 哨兵`（core/app 都不引入执行面） | ✅ |
| S5 | form 数据面 | `collectValues converts…fails loud on bad list JSON`（指名字段、动作不发）+ `rpcEnvelope exact client-request wire shape` | ✅ |
| S6 | 焦点不改布局 | `focusKey stable identity; layout has no hidden state`（同输入两算 deepEqual）；app 层 focus 只加类名 + scrollIntoView（代码走查） | ✅ |
| S7 | rev 轮询 | `pollDecision keep on unchanged replace on changed rev` | ✅ |
| S8 | 空态 | app.js `.empty` 文案（分类清空自动回「全部」）——无自动化（DOM 层，见 §5-2） | ✅(代码走查) |
| S9 | 不回归 | dsh-cli **244/0**（241 + canvas 3）、dsh-wasmrt 全绿、clippy **0**、node --test **12/12** | ✅ |

## 3. TDD 纪律记录

1. **桩红**：先写 12 测试 + 空桩 `core.js` → `node --test` **12/12 全红**（行为性失败）→
   实现转绿 12/12。
2. **信封探针（红验证）**：桩的 `rpcEnvelope` **刻意复刻 demo 缺陷形**（裸 `{args}`）——
   测试当场抓住；修 wire 的意图由此钉死（真实 HTTP 下 `rpc_envelope_ok` 拒裸 body 的
   实证缺陷，同探针守护 app.js 与 demo renderer 两处）。
3. Rust 侧：canvas.rs 测试先行；首轮即绿由 `web.rs` 路由类型错暴露编译面（`Response`
   泛型不合型 → 改双分支），非恒真断言路径（shell 测试在 None 桩下必红）。

## 4. 回归数字

`cargo test -p dsh-cli -p dsh-wasmrt`：**244 + 23 + wasmrt 14 目标，0 失败**；
clippy `-D warnings` 0；`node --test crates/dsh-cli/assets/canvas/tests/core.test.mjs` 12/12。
（verify-diff 本轮未触碰装配语义——Rust 改动不涉 effect 引擎；上轮 26/26 结论仍有效，
下一轮全量批附跑。）

## 5. 诚实台账（回看清单）

1. **自主过闸的默认值**（requirements §2 表，均可回退）：默认「全部」视图 / 侧栏枚举序 /
   260×100px 列行单元 / 4s 轮询 / 资产编译进二进制 / first-fit 平手取最左。
2. **app.js 粘合层无自动化测试**（无浏览器基建）：其正确性依赖 core 纯函数全测 +
   路由冒烟 + 代码走查；DOM 行为（滚动高亮/空态文案/resize 重排）需人工浏览器验证，
   本环境未执行——**这是 C3 验收的已知边界，不是已验证事实**。
3. **真实 serve 进程冒烟未跑**（需完整 cordis.yml boot fixture）；`/canvas` 路由块是
   serve 闭包内 12 行镜像（同 `/plugins` 模式），行为由 `canvas_response` 纯函数测覆盖。
4. `status`/`list` 卡本轮**仍落回落**（C4 点亮）；C5 前实时性靠 4s 轮询。
5. demo `renderer.js` 仍是包内单卡 demo（C1 定位不变），本轮只修 wire 信封一行。

## 6. 回滚点

撤销本实现提交 = 回到 `cc8283a`（设计闸）；资产目录/`canvas.rs`/路由块/demo 一行全增量，
既有 wire/前端/清单端点零改动。
