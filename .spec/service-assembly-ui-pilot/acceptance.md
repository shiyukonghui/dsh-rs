# 验收报告：llm-deepseek 服务装配单元试点（rust + ui 声明 + wasm）

日期：2026-08-28
阶段：测试验证（阶段 4）+ 部署维护（阶段 5）——本文档为阶段关卡工件（验收收口）。
依据：`.spec/service-assembly-ui-pilot/requirements.md` + `design.md`（定稿）。

## 1. 交付范围（对需求/设计逐条核）

| 项 | 需求 | 交付 | 证据 |
|---|---|---|---|
| wasm 声明面 | 组件暴露 describeUI（声明=数据） | ✅ `wasm-plugins/llm-deepseek`（export remote + import host-services） | m32 describe_ui |
| 一份契约 | 静态 ui.json 与 describeUI **逐字段一致** | ✅ 断言 | m32 static_ui_json_matches_describe_ui |
| 动作面 | save（白名单校验+落宿主 kv）/ discoverModels / currentValues | ✅ 全部实现 | m32 save / discover / currentValues |
| fail-loud | 坏入参/未知端点/未知字段绝不伪造成功 | ✅ | m32 save(未知字段) + unknown_endpoint |
| 宿主接线 | serve 装配试点载体 + namespace 路由 + `/plugins/<name>/**` 静态 | ✅ `Boot.llm_deepseek_remote` + dispatch 路由 + serve_package_asset | dsh-cli llm_deepseek_remote_routes_and_serves_static |
| 最小渲染器 | 读声明渲染表单 + 动作 RPC（壳 demo） | ✅ `web/index.html`+`renderer.js`+`ui.json` | 包内 demo（人工冒烟） |
| 试点边界 | 不做真实 LLM / 不做 loader entry 依赖激活 (inject) / 不做 SSR | ✅ 未引入 | scope 说明见 requirements §1.3 |

## 2. 测试验证（红→绿记录）

- **`crates/dsh-wasmrt/tests/m32_llm_deepseek.rs`：6 测试全绿**
  - describe_ui_returns_valid_declaration
  - static_ui_json_matches_describe_ui（声明=数据，一份契约）
  - save_writes_kv_and_rejects_unknown_field_fail_loud
  - current_values_roundtrips_saved_settings
  - discover_models_returns_default_catalog
  - unknown_endpoint_fail_loud
- **`crates/dsh-cli` web 集成测试**：`llm_deepseek_remote_routes_and_serves_static` 绿
  （路由到试点载体 + 未装配回落 not-implemented + `/plugins/llm-deepseek/ui.json` 静态分发 200）。
- 首次红（default）记录：无（机制沿用 host-remote 已验证模式；m32 新增断言即红转绿）。

## 3. 部署冒烟与维护

- wasm 组件经 `cargo component build`（本环境离线需 `CARGO_NET_OFFLINE=true`；锁文件已固化）
  产出 `target/wasm32-wasip1/debug/llm_deepseek_plugin.wasm`；serve 缺构建设为自动构建。
- `web/ui.json` 经既有 `/plugins/<name>/**` 静态挂接（D-175 serve_package_asset）对外分发；
  渲染器 demo 可直接打开（读静态声明 → 表单 → 动作 RPC）。
- 回滚：移除 `llm-deepseek` 载体装配 + 测试即恢复（新增字段默认 None，路由回落 not-implemented）。

## 4. 回归基线

- `cargo test -p dsh-cli -p dsh-wasmrt`：**225 通过**；5 个 M5 bash/schedule/job 失败为
  **基线既有环境性失败**（git stash 验证：无本改动时同样 FAILED，与试点无关）。
- `cargo clippy -p dsh-cli -p dsh-wasmrt --all-targets -- -D warnings`：**0 告警**。
- m32 新增 6 断言 + dsh-cli 新增 1 断言；既有断言零改动。

## 5. 诚实边界（未做 / 延后）

- 真实 DeepSeek 网络调用与 LLM adapter（genai 决策后续）。
- 作为 `dsh-loader` Plugin trait 的 entry 装配 + `inject=['llm']` 依赖激活（服务插件 entry 化
  下一阶段）。
- 前端通用渲染器产品化（当前为包内最小 demo 壳）。
- SSR 首帧（P2 design §1.2 明确可选、非验收）。

## 6. 决策链

`D-178 方向 → D-179 P2 架构模型 → 本试点（D-180，待提交）`；改动 → git 提交 → DECISIONS 条目互查。
