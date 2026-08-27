# 验收：harness FIXME —— 插件包（文件夹：wasm + 前端）装配闭环

日期：2026-08-27
阶段：测试验证（4）→ 验收（5）——本文档为两阶段关卡工件。
依据：`.spec/plugin-file-resolve/{requirements,design}.md`（D-174，Q1-Q4 用户确认）+ 实现/锁点
（D-175）+ 全回归（本档）。

## 1. 验收结论：**PASS** —— beyond 目标全部闭环

beyond 目标三项（M27/M28 时序、A2 收口复查、harness FIXME）至此全部完成。
本任务 = 真实 harness `Tree.import(name)` 的 Rust 对应物：插件 = 文件夹包（wasm 组件 + 前端
组件），**文件夹名 = 注册名**；world 判别选适配器；前端静态挂接。

## 2. 决策回执（用户确认，2026-08-27）

| # | 决策 | 实现 |
|---|---|---|
| D1 布局 | plugin.json 清单 + 约定回退 | `resolve_package`：manifest（wasm/web/caps/world 可选）→ 回退 `<name>/target/wasm32-wasip1/debug/<name>_plugin.wasm` + `web/` |
| D2 前端 | 静态资源目录挂接 | web.rs `/plugins/<name>/**` → 包 web 目录（根/子目录 → index.html；在 client.js 分支前，`@scope` id 不冲突） |
| D3 范围 | Rust 侧 | folder→wasm 注册 + serve 挂接 + 测试；GUI 消费侧留后续 |
| D4 loop | name=folder + world 判别，移除 config.wasm | `assemble_plugin_packages`（boot/refresh 共用）；`config.wasm` 键废除，web-cordis.yml 纯 folder 形态 |

## 3. S1-S4 验收证据

- **S1 文件夹解析**：`resolve_package` 5/5（回退/清单/非包=None/缺 wasm·坏 JSON·缺 web=Err/
  caps 优先级）；boot 组件包装配测试绿（`boot_assembles_wasm_component_package_sibling_to_loop`）。
- **S2 loop 统一**：world 判别 m30 3/3（echo-loop→Loop、hello-component→Plugin、非法→Unknown）；
  turn 句柄 = 首个 dsh-loop 包；无 config.wasm 键。
- **S3 前端挂接**：web `serve_package_asset_reads_package_web_dir` 绿（资源/目录索引/miss/无 web）。
- **S4 回归**：workspace **0 失败**；clippy **0**；verify-diff **26/26**（golden 数据面未触）；
  serve 冒烟 **HTTP 200/13270**（web-cordis.yml 纯 folder 迁移后基线一致）。

## 4. TDD 红→绿

`boot_assembles_wasm_component_package_sibling_to_loop` 红验证：stash 实现 → **0/1 FAILED**；
恢复 → 绿。swap 迁移：旧 config.wasm 换组件测试在新语义下先红（`name:loop` 无文件夹）→ 迁移
为 name 换包后绿。新面测试（detect/parse/serve）本身即锁点。

## 5. 迁移与兼容

- web-cordis.yml / web-smoke*.yml / m9_boot / m9_yaml_assemble 全部去 `config.wasm`（D4）。
- 兼容降级：旧 config.wasm 键成死键（name=folder 优先生效），故旧配置仍可解析（非破坏）。

## 6. 诚实边界

- 前端组件 = 静态目录挂接（D2=a）；GUI 消费侧 + 配置编辑后续。
- 不做插件文件 watch（HMR 显式 refresh 重解析）。
- `plugin.json` 包级清单；packages 仅含 web 目录的包参与 `/plugins/**` 挂接。

## 7. 决策链互查 / 部署 / 回滚

D-174（需求+设计：源码对照 + Q1-Q4 确认）→ D-175（编码 TDD 红→绿）→ D-176（本验收）。
部署：插件分发 = 目录包（wasm 组件 + 前端 + 可选 plugin.json）放 `wasm-base/`；`name` 即包名。
回滚：revert D-175（特征级）；`config.wasm` 旧配置兼容降级（name=folder 解析）无需紧急回滚。
