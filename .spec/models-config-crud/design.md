# 系统设计：Rust dsh web 模型配置增删改查对齐 TS harness

日期：2026-08-26
阶段：系统设计（瀑布流阶段 2）——阶段关卡工件。
状态：**定稿（genai 0.6.5 API 双源实证；全部决策点闭合）**

## 1. 设计目标

在 Rust `dsh web` 实现与 TS harness 一致的模型配置 CRUD：namespace 注册面、写路径、
discoverModels 真实探测、selectModel 校验+默认模型持久化、配置持久化（Rust 自有文件）、
llm-pi-ai 多 provider 运行驱动（genai 0.6.5）。前端 vendored 固定，Rust 严格对齐 wire。

## 2. 设计决策

### D-A. namespace 注册面（新增 3 个 llm 相关 namespace）

复用 `dsh_settings` 注册能力。在 `register_host_settings` 旁新增
`register_model_config_settings(sp)`，注册：

1. **`llm-deepseek`**（对齐 TS flat schema；Applies=live）：
   schema object 字段：`apiKeyEnv(string, credential-ref, default "DEEPSEEK_API_KEY")`、
   `baseURL(string)`、`thinking("enabled"|"disabled")`、`reasoningEffort("off"|"low"|"high"|"max")`、
   `maxTokens(number)`、`defaultContextWindow(number)`、`models(array of {id,name,...})`。
   校验：非 strict（允许未列字段），值类型校验。
2. **`llm-pi-ai`**（对齐 TS providers dict；Applies=live）：
   schema：`{providers: dict(route → Profile)}`。Profile 字段：
   `apiKeyEnv(credential-ref)`、`displayName`、`api(union openai-completions|openai-responses|anthropic-messages)`、
   `baseURL`、`models(array)`、`defaultContextWindow/defaultMaxTokens`、`transport`、
   `reasoning` 等（TDD 阶段按需实现核心字段，其余宽进——非 strict dict 放行未知键）。
   校验：写时不强制 serviceable（Rust 无 TS assertServiceable 编译期等价；运行期由 genai
   适配器真实探测失败时 fail-loud）。
3. **`agent-default-model`**（对齐 TS；Applies=live）：
   schema：`{provider(string required), model(string required), reasoningEffort?(string)}`。

### D-B. 持久化装配（Rust 自有文件）

- 当前 `boot()` 用 `SettingsProvider::memory()`（通用，headless/无文件也跑）。
- **serve 装配**：检测 `cfg.settings_path`（新增 WebConfig 项，默认
  `<workspace_root>/settings.yaml`）→ 用 `SettingsProvider::file(path)` 重建 settings，
  并把全部已注册 namespace 迁移（`register_model_config_settings` + `register_host_settings`
  在同一 provider 上重注册）。
- credentials 同理：serve 用 `CredentialProvider::file(path)`（默认
  `<workspace_root>/.credentials.yaml`），迁移 memory → file。
- 实现：`boot()` 保持 memory（通用装配）；serve 装配处若 cfg 指定路径，构造 file provider
  并替换 boot.settings/credentials 的 Rc 内容。**关键**：注册逻辑抽象为可复用函数
  （`register_all_settings(sp)`），boot 与 serve 共用，避免 drift。

### D-C. `llm.providers` 目录行对齐

TS 目录行权威：
- `deepseek-official` → `{provider:'deepseek-official', displayName:'DeepSeek',
  settingsNs:'llm-deepseek', settingsPath:[], active:<agent_loop 装配>}`（无 declared）。
- pi-ai 目录行：对 `llm-pi-ai` namespace 中已声明 providers 的 route +
  Rust 已知 catalog route（deepseek-v4-flash 等由 genai supported_providers）→
  `{provider:route, displayName, settingsNs:'llm-pi-ai', settingsPath:['providers',route],
  active:<genai 支持>, declared:...}`。
- 未声明仅注册路由（boot.llm 的 provider）追加 settingsNs=''。

Rust `llm_providers(boot)` 返回：deepseek-official 行（settingsNs='llm-deepseek'！）+ pi-ai
目录行 + boot.llm 注册路由。**这是当前实现的核心修正**（现返回 settingsNs='llm' 是错的）。

### D-D. `llm.discoverModels` 真实探测

- payload `{settingsNs, provider?, baseURL?, api?, apiKey?}`。
- 分派：
  - settingsNs='llm-deepseek' 或 provider='deepseek-official' → 返回装配 catalog 模型
    （现有 agent_catalog 真实值）+ deepseek DEFAULT_MODELS。
  - settingsNs='llm-pi-ai'（或自定义 baseURL）→ **genai 真实探测**（genai 的 ListModels/
    chat 探测该 baseURL/model），返回模型列表。探测失败 → `{ok:false,
    code:'model-discovery-failed'}`（对齐 TS）。
- 不做假：探测不了就诚实报失败。

### D-E. `session.selectModel` 校验 + 持久化

- 校验：provider 必须是「可解析」（deepseek-official 装配的 catalog 中，或 genai 支持的
  pi-ai 路由 + 该 route 的模型）；未注册 provider → `{ok:false, code:'model-unavailable'}`。
- 持久化：成功后 `settings.replace('agent-default-model', {provider, model, reasoningEffort?})`
  （对齐 TS saveDefaultModelSelection）。
- 返回 `{selected: {provider, model, reasoningEffort?}}`。

### D-F. llm-pi-ai 多 provider 运行驱动（genai 0.6.5）

**genai 0.6.5 API 实证（本地 spike 编译 + subagent 源码核对双源印证）**：
- `Client`：默认多 provider；`Client::builder()` 可配 `with_adapter_kind(AdapterKind)`
  （bound-adapter——裸 model 名直路由指定适配器，**避免名嗅探落入 Ollama**，专为网关/自定义
  model 名设计）、`with_auth_resolver_fn(...)`（返回 `AuthData::from_env(apiKeyEnv)`）、
  `with_service_target_resolver(...)`（自定义 endpoint）。
- `exec_chat(req, opts)` / `exec_chat_stream(...)`：chat 补全（流式）。
- `all_model_names(AdapterKind, ProviderConfig {endpoint, auth})`：模型列表（discoverModels）。
- `AdapterKind` 27 变体，含 `OpenAI`(completions) / `OpenAIResp`(responses) / `Anthropic`。
- **pi-ai 协议映射**：openai-completions→OpenAI、openai-responses→OpenAIResp、
  anthropic-messages→Anthropic；自定义 baseURL 经 `ServiceTarget`/endpoint 设置；认证头适配器
  决定（OpenAI 系 Bearer，Anthropic x-api-key）。
- **异步约束**：genai 是 tokio async（edition 2024，需求 Rust 1.85+；dsh 用 1.94 ✓）；
  dsh-core **已依赖 tokio rt+macros**，但现有 llm_http 是**同步**面。

**集成设计**：
- 新增 `crates/dsh-cli/src/genai_llm.rs`：一个 genai 适配器，实现 `dsh_llm::LlmAdapter`，
  注册为 pi-ai 的 provider 路由。内部持有**共享 tokio runtime**（Arc<Runtime>，
  serve 装配一个；genai 调用经 `runtime.block_on` 桥接同步 LlmAdapter 面）。
- pi-ai profile → genai：
  - route 解析（pi-ai 的 `api` 协议 → `AdapterKind`）；
  - keyRef（profile.apiKeyEnv 或 derive）→ 从 credentials resolve → `AuthData::from_env`/
    `from_single`；
  - baseURL → `ServiceTarget`/endpoint；
  - **bound-adapter client**（`with_adapter_kind`）避免 model 名推断错误。
- 模型列表（discoverModels pi-ai）：`all_model_names(AdapterKind, ProviderConfig)`。
- discoverModels 对 llm-deepseek：返回装配 catalog 模型（现有 agent_catalog 真实值）。
- 失败路径：探测/调用失败 → `{ok:false, code:'model-discovery-failed'/'model-unavailable'}`
  （诚实，不伪造）。

**运行时边界**：genai 只在 pi-ai 路由使用；deepseek 仍走现有 llm_http 同步路径（不动）。
两者统一在 `dsh_llm::LlmRuntime` 的 adapter 注册表下，`provider` 路由区分。

## 3. 组件/模块职责

| 模块 | 职责 |
|---|---|
| `dsh-cli::settings_register` | register_model_config_settings（llm-deepseek/pi-ai/agent-default-model schema） |
| `dsh-cli::web` | llm.providers / discoverModels / selectModel / settings / credentials dispatch 对齐 |
| `dsh-cli::serve` | file 持久化装配（settings_path/credentials_path） |
| `dsh-cli::genai_llm`（新） | genai 集成的 LlmAdapter（pi-ai 多 provider 真实调用 + 探测） |
| `dsh-llm` | 既有 LlmAdapter 抽象；不新增（genai 适配器在其外实现或注册） |

## 4. 与非目标的关系

- 不迁移 TS C 盘配置（用户选定 Rust 自有文件）。
- deepseek 现有调用路径不动（llm_http 保留）。
- 前端不改。

## 5. 测试策略（阶段关卡）

- 单测（TDD）：namespace 注册 describe/mutate 生效；llm.providers 目录行对齐；discoverModels
  对 catalog 返回 + 对 pi-ai 探测；selectModel 校验通过/拒绝 + agent-default-model 落盘。
- 集成：真实 key 实例（60886）浏览器「设置→模型配置」增删改查 + 重启保留。
- genai 集成：spike 验证自定义 baseURL/auth chat；失败路径诚实报错。

## 6. 风险与回滚

- genai 0.6.5 API 与假设不符 → 用 subagent 报告校准设计（D-F 细节）。
- file 持久化迁移 memory→file 出错 → 回退 memory（web serve 默认仍可用）。
- genai 引入重（tokio 等）→ 评估与现有 async 面协同。
