# 需求结论：Rust dsh web 模型配置增删改查与 TS harness 对齐

日期：2026-08-26
阶段：需求分析（瀑布流阶段 1）——本文档为阶段关卡工件。
状态：**定稿（llm crate 调研完成并回填；genai 决策已定）**

## 1. 目标（Top-down）

让 `dsh web`（Rust，60886 类实例）的设置页「模型配置」在**增删改查**上与
TS harness（deepseek-harness `dsh-host-apiproxy`）**功能一致**：用户在浏览器能做与
3080 相同的模型配置操作，且配置真实生效、重启保留。

## 2. 非目标（明确不做）

- 不改前端（vendored 固定代码）；Rust 严格对齐其 wire/schema 期望。
- 不做 pi-ai 的**运行驱动**完整复刻（除非用户另行裁定；当前阶段聚焦 CRUD + 持久化 +
  discoverModels 真实探测 + selectModel 校验/持久化）。
- 不迁移 3080（TS harness）的既有配置数据（两实例独立持久化，用户已选定 Rust 自有文件）。
- 不改变 deepseek 现有真实调用路径（60886 已可真实对话）。

## 3. 假设（用户已确认）

- 持久化：**Rust 自有文件**（`settings.yaml` + `.credentials.yaml`），不碰 TS 的 C 盘
  `$DSH_HOME`（两实例配置独立，符合「先说清楚不一致、按自治实现」）。
- discoverModels：**真实网络探测**（对齐 TS，对自定义 baseURL 探测模型）。
- `session.selectModel`：**校验 + 持久化默认模型**（agent-default-model namespace）。
- 多 provider（llm-pi-ai）运行驱动：**引入 genai crate 承载**（用户选定）。

## 3b. 成熟库调研结论（用户指示调研 crates.io/crates/genai 与 crates.io/crates/llm）

- **`genai`（crates.io/crates/genai）✅ 采纳，版本 `0.6.5`（用户指定，稳定版）**
  - 定位：Rust **多 provider 生成式 AI 客户端**（[rust-genai](https://github.com/diverger/rust-genai)）。
  - 支持 provider：Ollama / OpenAi / Anthropic / Gemini / DeepSeek / Groq / Cohere /
    xAI/Grok 等。
  - 版本：用户选定 `genai = "0.6.5"`（[docs.rs/crate/genai/0.6.5](https://docs.rs/crate/genai/0.6.5)
    区间；稳定版，非 beta）。
  - 匹配：pi-ai 的远程多 provider 需求（协议 openai-completions/anthropic-messages 等）。
  - 用途：作为 pi-ai 适配器的**运行时后端**——配置中的自定义 provider（route/api/baseURL/
    apiKey）→ genai client 真实请求；discoverModels 外部探测也可复用其模型列表能力。
- **`llm`（crates.io/crates/llm）❌ 未采纳（用户最终选定 genai；事实澄清如下）**
  - **名字被两次占用的澄清（docs.rs/crate/llm/1.3.8 Note 权威）**：
    - 0.1.x：rustformers/philpax 的**本地推理库**（llama.cpp/ggml，2023-05 归档）。
    - 1.0.0+：graniet 的「A Rust library unifying multiple LLM backends」——**纯远程多
      provider HTTP 客户端**，原生 OpenAI 兼容 /v1/chat/completions + 流式 SSE，后端覆盖
      openai/anthropic/ollama/openrouter/deepseek/google/groq/azure_openai/bedrock。
  - 未采纳理由（非能力不足）：用户已明确选定 `genai = "0.6.5"`（稳定性/多 provider 姿态愿
    景更贴合）；llm 1.3.8 单人维护、adoption 较小（~11.6 万下载）。功能上它本可胜任，
    记录此事实供后续对比，不推翻用户决策。

## 3c. 非目标（追加，对应 3b 决策）

- 不引入 `llm` crate（本地推理用途不符）。
- genai 引入范围：仅作 pi-ai 多 provider 的**运行驱动后端**；llm-deepseek 现有调用路径
  （llm_http）不动。

## 4. 硬约束（TS 权威契约，subagent 报告核实；文件路径相对 deepseek-harness/）

### 4.1 端点面
| 端点 | TS 语义 | 前端用途 |
|---|---|---|
| `llm.providers` | 目录 ∪ 注册路由（api-proxy.ts L3268-3296） | 设置页 provider 列表（store.ts L142-153） |
| `llm.models` | 宿主模型目录 | 模型下拉 |
| `llm.discoverModels` | 探 draft provider 的模型（api/llm.ts L67-89） | 拉取模型候选（ModelListEditor L234） |
| `settings.describe` | 全部 namespace（writable/hasDocument/namespaces） | 页面配置值 |
| `settings.update/replace/mutate` | 写 namespace（冲突→settings-conflict） | 保存/删除 |
| `credentials.describe/set/unset` | 凭据（ref 正则 `^[A-Za-z_][A-Za-z0-9_]*$`） | 密钥管理 |
| `session.selectModel` | 校验 resolveCallConfig + saveDefaultModelSelection（api-proxy L2194-2231） | 切换当前模型 |

### 4.2 关键 namespace schema（必须注册）
- **`llm-deepseek`**：**扁平** `{apiKeyEnv, baseURL, thinking, reasoningEffort, maxTokens,
  defaultContextWindow, models[], ...}`（index.ts L159-179），Applies=**live**。
  目录行：`{provider:'deepseek-official', displayName:'DeepSeek', settingsNs:'llm-deepseek',
  settingsPath:[]}`（L442-444）。前端 settingsPath=[] → 每字段一 op，mutate
  `{op:'set'|'unset', path:[key], value}`。
- **`llm-pi-ai`**：**providers dict** `{providers: Record<route, Profile>}`（config.ts
  L333-335），空 dict = dormant。目录行：`{settingsNs:'llm-pi-ai',
  settingsPath:['providers', route], declared:!catalog.has(route)}`（index.ts L118-138）。
  Profile 字段（L88-176）：apiKeyEnv/displayName/api/baseURL/models/.../retryPolicy。
  协议 union（provider.ts L47-63）：`openai-completions` / `openai-responses` /
  `anthropic-messages`。命名空间 Applies=live。
- **`agent-default-model`**：`{provider, model, reasoningEffort?}`（core/agent-default-model
  src/index.ts L24-38）。selectModel 成功 → `settings.replace(ns, {...})`（L98-104）。

### 4.3 前端写路径
- llm-deepseek（settingsPath=[]）：ProviderEditor applyOnce（L248-298）→ settings.mutate
  `{ns:'llm-deepseek', ops:[{op:'set',path:['baseURL'],...}, {op:'set',path:['models'],...}]}`
  + credentials.set（keyRef = namespace.value.apiKeyEnv 默认 DEEPSEEK_API_KEY）。
- llm-pi-ai（settingsPath=['providers',route]）：settings.mutate op path 含
  `['providers', ...]`；CustomProviderCard（L147-154, L267-271）settingsNs=NS=llm-pi-ai。
- selectModel：session.selectModel payload `{sessionId, provider, model, reasoningEffort?}`。

## 5. Rust 现状缺口（自下而上核实）

| 缺口 | 现状 | 需做 |
|---|---|---|
| namespace 注册 | 只注册 `llm`（lib.rs L292）+ host 偏好集 | 注册 llm-deepseek / llm-pi-ai / agent-default-model（含 schema） |
| llm.providers 目录行 | 已改返回 deepseek，但 settingsNs='llm'（错） | 对齐：settingsNs='llm-deepseek' settingsPath=[] + pi-ai 行 |
| llm.discoverModels | 仅 provider 匹配装配 catalog 时返回 | 真实网络探测（自定义 baseURL） |
| session.selectModel | 只 echo，不校验不持久化 | 校验（provider/model 可解析）+ 写 agent-default-model |
| 持久化 | SettingsProvider::memory + CredentialProvider::memory | 装配 file(path) |
| 多 provider 运行 | 只有 deepseek HTTP 适配器；dsh_llm 是 LlmAdapter 多 provider 抽象（runtime.rs L76） | 按调研方案决策 |

## 6. 已确认事实（关键澄清）

- Rust `dsh_llm` 是**真实多 provider 抽象**（`LlmAdapter` trait + `register_adapter(providers,
  adapter)` 任意路由 + `llm_http` OpenAI 兼容流式调用）——不是只有 deepseek 能力。
  当前只注册 deepseek 适配器是「实现面」问题，非「架构不支持多模型」。
- 用户疑点「Rust 有成熟 llm 库」→ **调研 `llm = "1.3.8"` 库中**（待 subagent 结论）。
  crates.io `llm` crate 初步判断为**本地推理库**（非远程 API 客户端）——待权威确认后
  更新本节并定夺多 provider 实现路径。

## 7. 测试与验收标准（阶段关卡）

- dsh-cli 全绿（含新增 namespace 注册/写路径/discoverModels 探测/selectModel 持久化测试）。
- clippy 0。
- 真实 key 实例（60886）：浏览器设置页能对 deepseek 增删改查（providers 行出现 + mutate
  生效 + 重启保留）；llm.providers 返回 settingsNs='llm-deepseek'；discoverModels 真实；
  selectModel 切换后 agent-default-model 落盘。
- DECISIONS.md 记录全部决策。

## 8. 决策收敛记录（全部已定）

- llm crate 1.3.8 调研（用户指示）：**本地推理库，不采纳**（与远程多 provider 需求不符）。
- genai 调研（用户指示）：**采纳 `genai = "0.6.5"` 承载 pi-ai 多 provider 运行驱动**。
- pi-ai 范围：**CRUD + 持久化 + genai 运行驱动** 齐做（用户选定包含 genai）。
- 持久化：Rust 自有文件（settings.yaml + .credentials.yaml）。
- discoverModels：真实网络探测。
- selectModel：校验 + 持久化 agent-default-model。

## 9. 遗留边界（如实记录，非本次目标）

- TS 的 `$DSH_HOME` C 盘既有配置不与 Rust 共享（两实例独立持久化；用户选定）。
- genai 与现有 llm_http 调用路径并存：deepseek 走现有 llm_http，pi-ai 自定义 provider
  走 genai——两套调用共存需在设计阶段明确边界与统一入口。
