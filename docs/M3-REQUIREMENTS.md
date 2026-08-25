# M3 宿主方法面 + settings/credentials/guard：需求结论文档 + 系统设计

> 本文件是 `PLAN-rust-full-harness-migration.md` §6「M3 范围」的实现工件：
> **阶段一（需求分析）** 产出目标/非目标/假设/约束/边界/验收标准；
> **阶段二（系统设计）** 产出 crate 划分、依赖序、模块结构与关键设计决策。
> 需求分析的契约事实直接由逐行阅读参考源码核出（packages/host/apiproxy/src/api/*.schema.ts
> + packages/settings/* + packages/credentials/* + packages/host/directory-picker-browse +
> packages/guard/*），错误消息/字段名/wire 形状逐字对齐并记录差异。
> 决策编号追加记入 `DECISIONS.md`，git 提交可互查。

---

## 第一部分：需求分析（第一性原理 + 双视角）

### 1. 根本目标

M3 的目标：把 **web.rs 的空桩方法面全部做实**——让前端（复用现有 harness UI）能真实地：
浏览/创建宿主目录、持久化并编辑用户配置（settings）、持久化并编辑凭据（credentials）、
并在 loop 侧获得工具超时与重复调用提醒两个 guard。配置与凭据最终落在 `$DSH_HOME` 下的
真实文件（settings.yaml/.json 与 .credentials.yaml），重启后恢复。

| 交付 | 对应 TS 包 | 一句话职责 |
|---|---|---|
| `host.*` 方法面 | host/directory-picker-browse + host/webserver + host/frontend-static | listDirectory/createDirectory/pickDirectory/openPath/describe 全做实（真实 fs 扫目录 + home/provider/model） |
| `dsh-settings` | settings/settings + settings/settings-file | 用户配置能力缝：namespace 注册、分层 resolve（defaults→base→user）、redact、revision/conflict、文件持久化 |
| `dsh-credentials` | credentials/credentials + credentials/credentials-local | 凭据能力缝：resolve/describe/set/unset + env 分层 + shadowed 拒绝 + 文件持久化 |
| `settings.*`/`credentials.*` 方法面 | host/apiproxy/src/api/{settings,credentials} | web.rs 接线：describe/update/replace/mutate/openDocument、describe/set/unset |
| `guard` | guard/timeout-policy + guard/repeat-tool-reminder | 工具执行 timeout 结构化结果 + 重复调用提醒（最小语义切片） |

### 2. 第一性原理分解

1. **「配置」的本质 = 一个分层合并纯函数**：`resolve(defaults, base, userSection)` ——
   schema 默认值垫底、registrant 组合层居中、用户文档盖顶；再经 schema 校验。
   settings 的一切（registration/describe/update/replace/mutate/watch/revision/redact）
   都是对这个纯函数的外围协议。→ 核心是纯函数 + 确定性提交序，可单测穷举。
2. **「配置 surface」只配读到脱敏视图**：`role('secret')` 字段从不跨 wire；`secrets`
   槽位列表只报 path + set 标志。任何 wire 响应携带 secret 即破坏设计。→
   describe/update/replace/mutate 的**每个**返回都经 `redactSecrets(schema, value)`。
3. **「凭据」的读取是 per-operation 的**：consumers 每次操作重新 resolve，绝不过期缓存；
   空值 = 未配置（一条 seam-wide 规则绑定 resolve/describe）。→ Cast resolve 是每次实时查。
4. **「写不遮蔽」是凭据缝的正确性规则**：一个层的写若被更高优先层遮蔽（process env 遮蔽
   文件），写会「看似成功却永不生效」。→ set/unset 在环境遮蔽时拒绝（shadowed 文案逐字）。
5. **「配置/凭据」是缝不是表**：底层 format（YAML/JSON/CRC）由提供者接管，Service
   Definition 只定义 register/describe/write seamen；文件提供者可热重载。→ 与新配置类似
   M1 持久化缝（dsh-persistence）同形态，但其文档语义（leaf-diff 保注释）是 M5 范围。
6. **前端驱动的目录浏览必须满足两件事**：`fully-qualified` 围栏（绝不 up-path / 相对路径
   重基）与 `bounded` 窗口（任意大目录内存/传输有界，truncated 诚实标记）。→
   服务端纯数据函数（fullyQualified + boundedInsert + ancestryCrumbs）可差分测试。
7. **guard 是 loop 卫生不改变行为**：timeout 只把超时换成分类错误（TOOL_TIMEOUT）不擅自杀
   掉工具；repeat-reminder 只附加模型上下文不改写调用。→ 事件/上下文 enrichment 语义，
   不 new 状态机。

### 3. 自顶向下（Top-down）：M3 交付物分解

```
M3a host 方法面        <- 依赖：std fs 仅（无新 crate；web.rs 内实现 + 独立模块可测）
M3b dsh-settings       <- 依赖：dsh-schema（SchemaRef/resolve/to_json）、dsh-brand（无）、
                          dsh-persistence（原子写缝复用：write_tmp_then_publish 形态）
M3c dsh-credentials    <- 依赖：dsh-brand（无新依赖）；读写本地文件需原子写形态
M3d web.rs 接线        <- 依赖：dsh-settings + dsh-credentials + SessionHost（装配 + RPC 分派）
M3e guard              <- 依赖：dsh-tools（M2f execute 通道）/ dsh-agent（PreStepDecision）
M3f M3-ACCEPTANCE      <- 依赖：上面全部（契约面 + 持久化恢复 + 全绿 + clippy）
```

### 4. 自底向上（Bottom-up）：现有资产核实

- `dsh-schema`：`Schema`/`SchemaKind`/`Meta`（含 `role`）/`resolve()`/`ResolveOptions`
  （M4 移植 Schemastery）。**可用**；但缺 `to_json()`（wire `schema` 字段需
  `{uid, refs:{uid:{$meta...}}}` 形状）→ M3b 补。
- `dsh-persistence::jsonl`：`write_tmp_then_publish`（temp 写 + rename 原子发布）——settings/
  credentials 文件写直接复用该形态（可抽公共小函数，避免 O(fs) 重复）。
- `web.rs`（dsh-cli）：`host.*` 四方法、`settings.*` 五方法、`credentials.*` 三方法当前为
  空桩/硬编码返回；`llm.providers/models` 已由注册表驱动（M1e）。**字段缺口**：
  `host.describe` 缺 `home`（M3 schema 必填）、`provider`/`model`（可选）。
- `dsh-tools`：M2f 已建 `execute_inner` pre-phase（pre→ask→deny→guards→dispatch）。
  guard 的 timeout wrapper 可挂为 execute 通道的一个 wrapper；repeat-reminder 读
  `post` 决策路径。
- 环境：无浏览器 / 无 API key / out 网络阻断（D-027）——M3 的浏览器 E2E 依旧以
  `handle_rpc_host` 集成测试代偿；真浏览器阶段验收收口于 M4+（延续 D-022/D-036 声明）。

→ M3 大部分逻辑落在 **dsh-cli/web.rs + 两个新 crate（dsh-settings、dsh-credentials）**，
dsh-schema 补一个方法，dsh-persistence 可抽一个原子写工具。不新增重型依赖。

### 5. 需求结论（目标 / 非目标 / 假设 / 约束 / 边界 / 验收）

**目标（M3 内）**

- `host.listDirectory`：fully-qualified 围栏；window=1000(+1) 有界排序；符号链接 stat 探针
  （可进入才 row，broken 静默跳）；`{path, home, crumbs, entries, truncated}` 逐字段对齐；
  `directory-unreadable` 文案对齐 browse 实现。
- `host.createDirectory`：父路径 fully-qualified 围栏 + 段名校验（空白/`.`/`..`/含 `/\` 拒）；
  `mkdir` 非递归；`EEXIST → directory-exists`、其余 → `directory-create-failed`。
- `host.pickDirectory` / `host.openPath`：无 native dialog 环境下 pickDirectory 返回
  `{path:null}`（对齐「取消」语义）；openPath 记录 + `{opened:true}`（无桌面 opener 的
  诚实降级，文案记录差异）。
- `host.describe`：补 `home`（用户主目录）与可选 `provider`/`model`（来自 Boot.llm 当前
  选择），cwd/attachedSessions/canOpenPath 保持。
- `dsh-settings`：`SettingsNamespace`（kebab-case 校验，pattern 逐字）/ `register`
  （重复注册 fail loud）/ `SettingsScope`（get/watch/update/replace）/
  `resolve(defaults→base→user)`（复用 dsh-schema）+ `SettingsConflictError`
  （`SETTINGS_CONFLICT`，expected/actual 消息逐字）/ `update|replace|mutate`（mergeLayers /
  wholesale / path-op reduce，expectedRevision 冲突检测）/ `describe`（Registration 序 +
  redact + secrets）/ watchers 串行提交 / `settings/updated`+`settings/document-updated`。
- `dsh-credentials`：`CredentialRef`（POSIX shell 标识符 pattern 逐字）/ `resolve`
  （env→file→project-env→user-env；空值跳过）/ `describe`（configured/source/writable）/
  `set`/`unset`（空值拒、shadowed 拒、unset 幂等）。
- settings/credentials 文件提供者：`settings.yaml|json`（`$DSH_HOME/settings.yaml` 默认）/
  `.credentials.yaml`；锁 + 原子写（复用 write_tmp_then_publish 形态）；hot-reload 观察——
  Rust 侧不做 chokidar，改为写后自读 + LF 计算（差异记录：无 OS 级 watch，M5 可选轮询）。
- web.rs 接线：settings.describe/update/replace/mutate/openDocument、credentials.describe/
  set/unset 变为真实服务驱动。
- guard：`TOOL_TIMEOUT` 结构化替换结果（`tool call timed out after {ms}ms` 逐字）+ 超时
  wrapper；repeat-tool-reminder 阈值检测（默认 `[3,5,8]`）pure 逻辑 + 提醒消息逐字
  （gentle/detailed）。**接线范围**：guard 的完整 agent-loop 接线若非纯函数（依赖 fs/shell
  M5 通道），则在 M3 交付 seam + 最小 executor 路径，完整 E2E 留 M5。

**非目标（明确排除，防扩散）**

- 不做 settings 的 YAML 注释保留 leaf-diff（TS `patchNode`/`renderYaml` 保留注释的语义）；
  Rust 侧以「leaf 差异写 + 其余键原样重写」等价值得差异记录（D-037），注释保真留后续。
- 不做 chokidar 等价 OS 级文件 watch（无跨平台依赖引入）；hot-reload 只做写路径自一致。
- 不做 `llm.discoverModels` 的真实模型发现（需真实 provider/凭据——M4+真实 provider 一起）；
  保持 `{models:[]}` 但 shape 已对。
- 不做 `dynamicCordisRunner` 系列非空实现——宿主无动态 JS 插件（Q2），`inventory:[]` 即正确。
- 不做 M4/M5 边界：goal/subagent/schedule/jobs/workflow、fs/shell/subprocess/sandbox、
  mcp/acp/hooks/skill 宿主化。skills/agentPreset/goal/subagent 保持现有桩（shape 已对）。
- 不引入异步运行时/多线程于核心（继承 D-004/D-006）：settings/credentials 读写单线程；
  文件 IO 用 std fs（原子写已单线程安全）。

**假设**

- 前端只经 /api 与 WS 感知宿主；settings.describe 读到的 namespace 集合 = Rust 宿主注册的
  集合（浏览器不假设特定 namespace 存在）。
- `$DSH_HOME` 缺省 `~/.dsh`（对齐 DSH）；`DSH_HOME` env 可覆盖。
- host provider/model：无显式默认时省略字段（前端 adapter 内部回退），对齐 TS host schema
  的可选性。
- 浏览器 E2E 本环境不可跑（无浏览器/无 key/网络阻断），以 handle_rpc_host 集成代偿。

**约束**

- `cargo test --workspace` 全绿 + clippy 零告警（-D warnings）为每子步门禁。
- 错误消息/模型可见文本/wire 字段名/可选字段缺省即省略，逐字对齐（见各参考文件）。
- 中文写文件只用 write/edit 工具（PowerShell 字符串替换已两次损坏文件，禁）。
- cargo/clippy 一律 `--offline` + `$env:RUSTC_WRAPPER=''`（D-027）。

**边界（不变量）**

- 任何 wire 上的 settings 值必须已 redact（无 `role('secret')` 字段残留）。
- `credentials.resolve()` 永不返回空串（空 = 未配置）。
- `set`/`unset` 在 env 遮蔽时必拒绝（shadowed）。
- settings 写 revision 冲突必 `SETTINGS_CONFLICT`，绝不静默覆盖。
- host 目录操作绝不相对路径重基（fully-qualified 围栏）。

**验收标准**

1. `cargo test --workspace` 全绿；clippy `-D warnings` 零告警。
2. `host.listDirectory/createDirectory`：真实 fs 场景（temp 目录含子目录/点文件/符号链接/
   大目录）断言 entries/crumbs/hidden/truncated 与错误文案。
3. settings 走一遍注册→describe(redact)→update(merge)→mutate(path-op)→replace(reset)→
   revision/conflict；文件落盘→重启恢复→依旧可读可写。
4. credentials 走 resolve/describe/set/unset + env 遮蔽拒绝 + 空值拒 + 幂等 unset；
   `.credentials.yaml` 落盘恢复。
5. web.rs 12 个方法（settings 5 + credentials 3 + host 4 目录类）经 `handle_rpc_host` 集成
   测试全部真实服务驱动（不再空桩）。
6. guard：TOOL_TIMEOUT 消息逐字 + 阈值提醒消息逐字（gentle/detailed）。
7. 每子步 DECISIONS 对应条目 + git 提交可互查。

---

## 第二部分：系统设计（决策 + 模块结构）

### 6. 关键设计决策（对应 DECISIONS D-037…）

| 决策点 | 结论 | 理由 / 差异记录 |
|---|---|---|
| 分层合并语义 | `mergeLayers(defaults?, base?, userSection)` 复用 dsh-schema `resolve`（M4 已验 Schemastery） | 避免重写 schemastery；defaults 由 schema default 求值给出 |
| `schema.toJSON()` | 给 dsh-schema 补 `to_json()`：`{uid, refs}` 形状（浅 uid 序言 + 深拷贝 refs 表） | wire 契约必须对齐（settingsNamespaceViewSchema.schema 是未知载荷，前端 `new Schema(json)` 消费） |
| 文件格式 | settings 默认 YAML（`serde_yaml`），`.json` 扩展名也支持；credentials 只 YAML | 对齐 TS 默认路径；避免 YAML 注释语法的额外 parser |
| 原子写 | 复用写临时件+rename 发布（dsh-persistence 内可抽公共 `atomic_write`） | D-019 已有此形态；settings/credentials 文件同语义 |
| hot-reload | 写路径自一致 + 启动读；**不引入 OS watch** | 无新增跨平台依赖；外部编辑热更新留后续（差异记录） |
| 凭据分层 | `env → file → project-env(.env) → user-env(~/.dsh/.env)`；只实现 env + file 两层（dotenv 留后续） | .env 解析是 M5 服务层；web gui 主要用 file 层 |
| host.describe home | `std::env::var("USERPROFILE"/"HOME")` 兜底 | 无 dirs crate 依赖 |
| pickDirectory | 无 native dialog → 恒 `{path:null}`（= 用户取消） | 诚实降级；canOpenPath 语义保留 |

### 7. 模块结构

```
crates/dsh-settings/
  src/lib.rs            # 类型 + Re-exports：SettingsNamespace/SettingScope/SettingsProvider
  src/types.rs          # SettingsNamespace brand / SettingsApplies / SettingsDescriptor / SettingsPathOp
  src/provider.rs       # SettingsProvider（register/describe/get/update/replace/mutate/watch/conflict）
  src/resolve.rs        # mergeLayers + resolve（包 dsh-schema）+ deepEqualJson + cloneJsonShaped
  src/redact.rs         # redactSecrets 纯函数（object/dict/array walk）
  src/file.rs           # FileSettingsProvider（load/persist/原子写；YAML/JSON）
  tests/m3_settings.rs  # 语义 + 文件 + 冲突 + redact + 重启恢复
crates/dsh-credentials/
  src/lib.rs            # Re-exports + types + provider + local provider
  tests/m3_credentials.rs
crates/dsh-cli/src/web.rs
  settings_*: describe/update/replace/mutate/openDocument → dsh-settings 接线
  credentials_*: describe/set/unset → dsh-credentials 接线
  host_*: listDirectory/createDirectory/pickDirectory/openPath/describe(+home) 真实实现
crates/dsh-schema/src/lib.rs / crates/dsh-persistence/src/atomic.rs
  补 to_json() / 抽 atomic_write 公共写
crates/dsh-tools/src/runtime.rs + guard 切片（timeout wrapper + repeat-reminder pure）
```

### 8. 依赖序与验证策略

- **先** M3a host 方法面（web.rs 独立可测模块，无新 crate）→ 绿；
- **再** dsh-schema `to_json()`（单测锚定形状）→ dsh-persistence `atomic_write` 抽取（回归）；
- **再** dsh-settings / dsh-credentials（各自语义单测 + 文件恢复测试）；
- **再** web.rs 接线（handle_rpc_host 集成）；
- **再** guard 切片（dsh-tools/agent 边缘单测）；
- **最后** M3-ACCEPTANCE：workspace 全绿 + clippy + D-038 收口报告。

---

*依据：deepseek-harness packages/host/apiproxy/src/api/{settings,credentials,host}.schema.ts +
settings/settings{+file} + credentials/credentials{+local} + host/directory-picker-browse +
guard/{timeout-policy,repeat-tool-reminder} + vendor/schemastery toJSON（本轮逐行阅读）。
浏览器 E2E 代偿声明延续 D-022 / D-036。*
