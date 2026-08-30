# S6d 切默认运行手册（备而未用——拍板即执行，每步单提交可回滚）

前提认证（第 52 轮）：`/canvas/rust` 交互审计全绿（含热插拔完整闭环）+ 零 panic；
JS 壳热插拔缺陷三次复现坐实。**推荐序**：切默认 → 观察期 → 退役 JS 渲染器 → （远期）根 `/` 收编。

## 步骤 1 · 切默认（一个提交）
> **已预制（第 60 轮）**：分支 `s6d-switch-default`（提交 `3accaf8`，守卫 7/7 绿）
> 就是本步骤的成品提交。拍板 = `git merge s6d-switch-default` + 重建重启 serve；
> 回滚 = `git revert 3accaf8`。以下原文为此提交的设计说明。

`crates/dsh-cli/src/canvas.rs` 路由对调——入口给 Rust 壳，JS 壳降居 legacy：

- `"/canvas" | "/canvas/"` → 返回 SHELL_ASSETS 里的 `rust.html`
  （其 import 已是绝对路径 `/canvas/rust/assets/...`，无需改 html 与资产路由）；
- 旧 JS 壳整组移到 `"/canvas/legacy"` + `"/canvas/legacy/assets/*"`（原样保留=回滚素材）；
- 测试同步：`canvas_shell_served_with_asset_refs` 改断言 Rust 入口（import 绝对路径），
  新增 legacy 面命中；`rust_shell_embedded_served` 不动。
- 验证：真路由目检 + `e2e-audit.mjs` 对 `/canvas` 跑绿。
- **回滚**：revert 此提交（JS 壳文件从未删除）。

## 步骤 2 · 观察期（建议 1–3 天真实使用）
观察面：chat 真实回路、审批动作、保存流、关卡/布局手感。
旧面常伴：`/canvas/legacy` 全程可用。

## 步骤 3 · JS 渲染器退役（观察通过后，独立提交）
- `crates/dsh-cli/assets/canvas/{index.html,core.js,app.js}` + `tests/` 移入
  `.spec/archive/canvas-js/`（node 35 测随迁或退役，记 DECISIONS）；
- canvas.rs 删 legacy 路由 + 导出守卫测试退役；
- **canvas.css 保留**（Rust 壳在用——唯一幸存者）。
- 回滚：revert + 资产还原。

## 步骤 4 ·（远期，另立需求）根 `/` 收编 = 「不再使用 deepseek 前端」终态
把根路由指向 canvas（重定向或直服），harness SPA dist 退役。
**另立需求分析**（涉及旧前端插件面/书签/使用习惯），不在本手册内擅自执行。

## 红线（不变式）
- 任何一步不碰 wasm 单元与声明契约（双壳同源）；
- `/canvas/rust/**` miss→404 不回落 SPA 的铁律保持；
- 每步提交信息对应 DECISIONS 补记条目。
