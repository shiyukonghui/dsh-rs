# 需求结论（试点选定）：llm-deepseek 服务装配单元 —— rust + ui 声明 + wasm 转换

日期：2026-08-28
阶段：需求分析（瀑布流阶段 1）——本文档为阶段关卡工件（试点选定 + 转换边界）。
依据：`docs/SERVICE-ASSEMBLY-HANDOFF.md`（§7 Sprint 0/第一阶段「服务插件 entry 化」）+
`.spec/service-assembly-ui/requirements.md`（P2 方向已过闸）+
`.spec/service-assembly-ui/design.md`（P2 架构模型 v1，D-179）。
状态：**试点选定 + 转换边界定稿（用户确认方向）**；编码走 TDD（下一关卡 design.md 落契约）。

## 1. 试点选定：`llm-deepseek`

从 deepseek-harness（TS）中选 **`packages/llm/llm-deepseek`** 为首个服务装配单元试点。

### 1.1 为什么是它
- **HANDOFF 点名的 canonical 一行插件**：`cordis.yml 里声明一行插件（如 llm-pi-ai 或
  自定义服务）` —— llm-deepseek 正是「一行插件 = 一个 provider 服务」的典型。
- **UI 是纯声明式表单**：Config schema 全为标量/枚举/模型列表（apiKeyEnv / baseURL /
  thinking / reasoningEffort / maxTokens / defaultContextWindow / models[]），天然可表达为
  P2 声明（数据），不需要 P1 的 JS bundle。
- **契约平行**：TS 的 `name='llm-deepseek'`、`inject=['llm']`、settings namespace
  `llm-deepseek`、`credentials` seam、`discoverModels` —— 与 Rust 的 dsh-settings /
  dsh-credentials / dsh-wasmrt host-services 面一一对应，能走「配置驱动、依赖激活」主线。
- **验收可做 demo**：设置表单渲染 + 保存落盘 + 发现模型，闭环且不需要真实 LLM 网络。

### 1.2 试点范围（本任务做）
1. 把 llm-deepseek 插件**转换**为 Rust wasm 组件服务装配单元（文件夹形态插件包）：
   - `plugin.json` 清单（wasm + web + caps + world）——文件夹名 = 插件注册名 `llm-deepseek`
   - wasm 组件经 remote world 暴露 UI 声明面 + 动作面（describeUI / save / discoverModels）
   - `web/ui.json` 静态 UI 声明 + 最小通用渲染器 demo（读声明渲染表单 → 动作 RPC）
2. 宿主接线（web serve）：
   - 装配 llm-deepseek wasm remote 载体（namespace 路由）
   - `/plugins/llm-deepseek/**` 静态挂接（复用既有 serve_package_asset，D-175）
3. TDD：wasm 声明面 + 动作面红→绿（m 系列/独立集成测试）。

### 1.3 明确不做（边界，后续阶段）
- **不做** 真实 DeepSeek HTTP 调用 / LLM adapter（只保留 discoverModels 目录 + 保存设置；
  运行期 provider 行为在试点后再沿 genai 决策落地，见 models-config-crud §3b）。
- **不做** 把它作为 `dsh-loader` Plugin trait 的 entry 装配（含依赖激活 `inject=['llm']`）——
  那是「服务插件 entry 化」的下一阶段（handoff §4-2）。本试点聚焦 P2 设计文档预告的
  **「先 wasm 插件声明面 + 壳渲染器最小集」**。
- **不做** 前端通用渲染器产品化（仅包内最小 demo 壳，验证声明契约可渲染可交互）。
- **不做** SSR 首帧（P2 design §1.2 明确可选加速，非验收项）。

## 2. 转换映射（TS llm-deepseek → 本试点）

| TS（llm-deepseek） | 试点转换 |
|---|---|
| `name = 'llm-deepseek'` | 插件包文件夹名 `llm-deepseek` + remote namespace `llm-deepseek` |
| `Config` schema（apiKeyEnv/baseURL/thinking/reasoningEffort/maxTokens/defaultContextWindow/models[]） | UI 声明字段子集（web/ui.json + describeUI） |
| settings namespace `llm-deepseek` | `values` 经 host-services kv 落盘（key `llm-deepseek/settings`） |
| `discoverModels`（deferred 模型目录） | 动作 RPC `llm-deepseek/discoverModels` 返回 DEFAULT_MODELS |
| 前端 ProviderEditor（applyOnce + mutate + credentials.set） | 通用渲染器 demo：渲染表单 → `POST /api/llm-deepseek/save` `{args:{values}}` |

## 3. 约束与验收基线
- 新语义落独立测试（m32_llm_deepseek）：红→绿；不破坏既有全回归基线
  （workspace 0 / clippy 0 / verify-diff 26/26 / serve 200/13270）。
- 声明必是**数据（JSON 文本）**，无任意 JS 进浏览器（渲染器只读声明）。
- 坏输入 fail-loud（规范化 `{ok:false,error:{code,message}}`），绝不伪造成功。
- 决策记录追加 DECISIONS 条目；改动 → git 提交 → 决策条目互查。

## 4. 验收（阶段关卡待测）
1. wasm 组件 `describeUI` 返回有效声明（含 fields/actions 子集）；未知端点/坏入参 fail-loud。
2. `save` 写宿主 kv 并读回一致；`discoverModels` 返回 models 目录。
3. 静态 `/plugins/llm-deepseek/ui.json` 与 `describeUI` 声名一致（声明=数据，只读渲染）。
4. 包内最小渲染器可从 ui.json 渲染表单并触发 save RPC（demo 冒烟）。
