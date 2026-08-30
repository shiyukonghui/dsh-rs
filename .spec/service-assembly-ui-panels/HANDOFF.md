# 服务装配前端：交接总览（2026-09-05 · 第 61 轮同步，覆盖 … D-211 + S6d 预制）

一页入口：验证、切默认（S6d）与旧前端下线拍板所需的一切。细节各有专档，此处只做路由。

## 1. 终态一句话
13 张服务单元卡承载全部前端能力，且渲染器自身完成 **Rust 化双壳并存**：
`/canvas`（JS 旧壳，零改动保留）与 **`/canvas/rust`（Dioxus 壳，审计全绿且热插拔反超）**
同源于同一声明契约（声明=数据、单元=wasip1 组件、渲染器只是壳）。
剩两步 = **你的目检 + S6d/下线拍板**（材料已备齐，见 §3/§5）。

## 2. 生态账本（末次认证 = 第 55 轮·全工作区回归）
| 维度 | 数字 | 权威出处 |
|---|---|---|
| 装配单元 / 桌面卡 | **13 / 13**（契约未动，双壳共用） | `wasm-plugins/` + `ui_manifest.rs` 逐卡断言 |
| 决策日志 | D-180 … **D-210**（含 S1–S6a 进度补记、S6a 教训、体积优化） | `docs/DECISIONS.md` |
| Rust 壳（canvas-shell） | lib **22/22** + clippy **0**（wasm32 all-targets）+ 活体审计全绿 | `canvas-shell/`，s6-audit.md |
| JS 壳测试 | node **35/35** | `assets/canvas/tests/core.test.mjs` |
| 宿主 | dsh-cli lib **262/0**（+D-211 测）+ clippy **0** | cargo 第 58 轮 |
| 工作区 | **cargo test --workspace 全绿零失败** + verify-diff **ALL PASS** | 第 55 轮认证 |
| 内嵌面 | `/canvas/rust` assets = **build.rs include_bytes!** 表驱动，wasm-opt 件 **938KB**，gzip 传输 **367KB（-61%）** | `crates/dsh-cli/build.rs`、D-210 |

## 3. 晨间三步（更新版）
1. **起服**（D-211 后新配方）：
   `target\debug\dsh.exe web scenarios\web-smoke.cordis.yml --port <端口> --agent-loop`
   ——loop 开关是 `--agent-loop` 旗标（cordis 的 loop 条目对 web 路径惰性）。
   两壳同服：`http://<host>/canvas`（JS）与 `http://<host>/canvas/rust`（Rust）。
   无 LLM key 时 chat 发送=turn 真实驱动 + AUTH 诚实失败（设计行为；有 key 即全通）。
2. **照单打勾**：`e2e-offline-checklist.md` §1；交互面 `e2e-audit.mjs` 自动对打
   （T2–T11 含真卸载热插拔/持久化/几何/chat 活体折叠）——**Rust 壳全绿**。
3. **拍板两件**：① S6d 切默认已预制 = 分支 `s6d-switch-default`（单提交 3accaf8，
   守卫 7/7）：`git merge` + 重建重启即完成；revert 即回滚（runbook 已标注）；
   ② 旧 deepseek 前端（根 `/`）下线——需求分析稿已备
   （`.spec/frontend-final-retirement/requirements.md`，§6 四问待你答）。

## 4. 缺口速览（判定材料，更新）
- **JS 旧壳三件观感缺陷**（report 成功态 JSON 整坨 / ns 切换旧 stat 残留 / 热插拔 DOM 冻结）：
  **不在旧壳修**——Rust 壳对应实现全部正确且有审计证据；切默认即整体了结（省工且避免双维护）。
- **待你环境验证**：chat 真实回复回路（需 LLM key/agent loop 开启的正式 cordis）；
  审批动作闭环（需 agent loop 开）。冒烟环境下两者均为诚实错误态（已实证）。
- 既有条目（D-201 裁撤 locale 固定卡、D-202/203/204/205 系列）状态不变，见 DECISIONS。

## 5. 诚实账
1. **审计是拦截器不是橡皮图章**：S6 首轮审计拦下了 Rust 壳 panic=abort 级崩溃
   （dioxus-core runtime 223/280；根因=JS 回调内 `dioxus::spawn`），修复后复验全绿。
   教训固化：JS 回调只准 `spawn_local`（D-210 补记）。
2. rows 38 vs 46 = 观感差（status 行 markup），非契约违背，已记审计档。
3. 目标「不再使用 deepseek 前端」的最后一公里 = **你的两项拍板**（§3 第 3 步）；
   技术侧无阻塞项，回滚路径全部预置（双壳并存即回滚态）。

## 6. 若要继续自主开发
技术队列已空（wasm-opt/gzip/审计基建/T7 结案全部完成）。自主面仅剩巡检与文档。
决策面三件套全部备料到「一键」级：S6d=merge 预制分支；根收编=需求稿四问；
退役=runbook 步骤 3。
