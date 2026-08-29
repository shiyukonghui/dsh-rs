# 服务装配单元 · 桌布（Canvas）UI 工作流接手文档

日期：2026-08-28
作者：dsh-rs 项目组（P2 → 桌布 工作流）
定位：把「服务装配单元携带前端 UI」这条工作流的**当前状态、锁定契约、剩余缺口、环境地雷**
一次性交给接手 agent。目标读者 = 接手 C2 及之后阶段的新 agent。

前提阅读（按序）：
1. `docs/SERVICE-ASSEMBLY-HANDOFF.md`（**另一条**工作流：装配引擎契约 A1–A7/B1–B4，已闭环；
   本文不重复其内容，只讲 UI 侧）
2. `docs/DECISIONS.md` 的 **D-178 → D-179 → D-180 → D-181 → D-182**（本文每条结论都在此有原始记录）
3. `.spec/service-assembly-ui/`（P2 方向与架构模型）
4. `.spec/service-assembly-ui-pilot/`（试点，已落地并升 v2）
5. `.spec/service-assembly-ui-canvas/`（**桌布契约 = 接手的主工作对象**）

git 起点：`44f9618 D-180+D-181+D-182 服务装配单元试点落地 → 桌布契约定稿 → C1 声明迁移 v2`
（工作树干净；`**/target` 已 gitignore，`.wasm` 产物不入库，与仓库既有惯例一致——已跟踪的
`.wasm` 数量为 0）。

---

## 0. 为什么这是根本目标的延续

`SERVICE-ASSEMBLY-HANDOFF.md` 的裁定仍然成立：让 Rust 插件成为「配置驱动、依赖激活、可热更」
的**服务装配单元**是项目基石。装配引擎侧（A1–A7/B1–B4）已闭环；**本工作流补的是另一半**：
服务装配单元要能携带**自己的 UI**，且这套 UI 必须是**声明（数据）**——不是插件作者写进浏览器的
代码。这既是「一切能力皆插件」的延伸，也决定插件生态能否不依赖宿主发版就长出新界面。

不变式（D-179，任何阶段都不得推翻）：
- **声明是数据，不是代码**（一旦翻译产物成可执行 JS，就回到 P1，沙箱消失）
- **渲染器必须在浏览器**（Rust/wasm 到不了 layout/paint 与交互）
- **Rust 角色 = 生声明 + 数据面/权限/动作执行，独独不做渲染**

---

## 1. 已定稿且不可回退的契约（D-178 → D-182）

### 1.1 演进链
| 决策 | 内容 | 性质 |
|---|---|---|
| D-178 | 方向：P2 声明式数据驱动；否决 P1（JS bundle）/P3（iframe）/P4（浏览器内 wasm） | 方向 |
| D-179 | P2 架构模型：声明=数据 / 前端通用渲染器 / Rust 只生声明不渲染；SSR 首帧=可选非验收 | 设计 |
| D-180 | 试点落地：`llm-deepseek` wasm 装配单元 + 宿主接线 + m32 | 代码 |
| D-181 | **桌布契约定稿**（双枚举 / 卡片壳 / 网格排布 / 清单热插拔口） | 设计 |
| D-182 | **C1**：试点声明 `form → card.view.form` + 双模型防线 | 代码 |

### 1.2 锁定契约（改它 = 需要先开新 DECISIONS 决策，不要静默改）
1. **双正交枚举强制分离**：`type` = 侧边栏**分类**轴（面向用户，加值近乎免费）；
   `view.kind` = **渲染契约**轴（加值必须写真渲染器）。**合并成一条枚举已被明确否决**——
   会让侧栏显示 `form/list` 这种用户看不懂的值，且「加一个分类就得配一个渲染器」两条轴互锁。
2. **v2 顶层唯一容器 `kind:"card"`**：`{ $schema:"dsh/plugin-ui/v2", kind:"card", cardId, type,
   title, description, size{w,h}, view:{ kind, ... } }`。D-180 的顶层 `kind:"form"` **已废止**，
   不是并存；`$schema` 非 v2 → 显式 `schema-version-unsupported`，**不做静默兼容**。
3. **v1 三视图档位**：`form`/`status`/`list` = 契约「实现档」；`chat`/`chart`/`table` =
   「契约预留」（签名已定，渲染器未建，落 `renderer-unimplemented` 元数据回落）；
   **`board` 否决**（它就是画布本身，卡内嵌画布 = 无限递归）。
4. **坐标永不外泄给插件**：插件只声明 `size{w,h}`；**封顶 `w≤4`/`h≤8`，超出由宿主裁剪 +
   记录（降级不是失败）**；排布按**声明顺序**（不引入 `priority`）；**10px 网格格距**（不是
   打印 pt）；列数随容器宽度自适应。
5. **`type` 闭集**：`model | config | capability | runtime | resource | session | misc`；
   未知/缺失 → 落 `misc`，保留原值可诊断，卡片照常渲染（不白屏、不隐藏用户声明）。
6. **发现面**：宿主聚合清单必须**每次从实时状态计算**（禁止启动快照缓存）+ 带 `rev`；
   卡身份 = `(pluginName, cardId)`；变更复用 `/plugins/events` SSE。**热插拔是第一等要求**。
7. **v1 单卡 + `cardId` 预留**：一个装配单元出一张卡；将来出多卡是**清单层面**的加法。
8. **一份契约**：静态 `web/ui.json` 与 wasm `describeUI` 输出**逐字段一致**（m32 有断言守护）。

### 1.3 三条概念纠偏（历史弯路，勿再走）
- 「XML/JSON 翻译成 TS/CSS 交给浏览器」= **有害**（翻译产物必为可执行 JS → 沙箱消失）。
- 「通用渲染器放 Rust、由 Rust 渲染更快」= **错**。渲染前半（声明→HTML 串）Rust 能做 = SSR
  首帧加速（**可选、非验收**）；后半（layout/paint + 事件 + 交互）浏览器独占。
- 「桌布上放 board 卡片，卡里再排卡片」= **递归陷阱**，`board` 已否决；「一卡里显示一行行条目」
  的需求由 `list` 覆盖。

---

## 2. 现状：代码里真实有什么（自读实证，非推测）

### 2.1 试点插件包 `wasm-plugins/llm-deepseek/`（文件夹名 = 注册名，D-175 形态）
| 文件 | 作用 |
|---|---|
| `wit/llm-deepseek.wit` | world：**复用 `dsh:host-remote/remote` + `host-services` 接口身份** → 宿主 `WasmRemoteEndpointPlugin` **零改动**即可加载 |
| `src/lib.rs` | 端点 `handle(namespace, method, body)`：`describeUI` / `currentValues` / `save` / `discoverModels`；`ui_declaration()` 产 **v2 卡片**（Rust 只生声明） |
| `plugin.json` | `{ wasm, web:"web", caps:["remote"], world:"remote" }` |
| `web/ui.json` | **静态 v2 卡片声明**（与 `describeUI` 逐字段一致）：`type:"model"`、`size 2×3`、`cardId:"llm-deepseek.settings"`、`view.dataRpc` |
| `web/renderer.js` | **包内 demo 渲染器**：校验 v2 → 按 `view.kind` 分派 → `dataRpc` 预填 → 动作 RPC；含契约 §7 的 fail-loud 分派 |
| `web/index.html` | demo 容器 |
| `Cargo.lock` | **手工从 host-remote 复制并改根包名**（无网时必需的离线锁，见 §4） |

动作面白名单：`save` 只接受 `apiKeyEnv/baseURL/thinking/reasoningEffort/maxTokens/
defaultContextWindow/models`，未知字段 → `internal` fail-loud **且不落盘**；持久化走既有
`host-services.set("kv", {key:"llm-deepseek/settings"})`（**未新增宿主后端**）。

### 2.2 宿主接线（`crates/dsh-cli`）
- `Boot.llm_deepseek_remote: Option<Rc<RefCell<WasmRemoteEndpointPlugin>>>`（默认 `None`）
- `WebConfig.wasm_base: PathBuf`（main.rs 传入）——供 serve 解析插件包
- `web.rs` serve 装配：构建试点载体 + `resolve_package(wasm_base,"llm-deepseek")` 追加进
  `boot.packages` → 走既有 `/plugins/<name>/**` 静态挂接（`serve_package_asset`，D-175）
- `dispatch_wasm_remote`：`namespace == "llm-deepseek"` → 试点载体；其余仍走 host-remote
  （**既有路由未动**）；未装配载体 → `not-implemented`（诚实回落）

> 关键判断已被 C1 验证：卡片化**只影响声明形态与呈现**，`world`/端点/kv 落盘/路由/静态挂接
> 在迁移中一行未动。后续加插件按此型扩展（每装配单元一载体，namespace 分流）。

### 2.3 测试锁点
- `crates/dsh-wasmrt/tests/m32_llm_deepseek.rs`（8 个，独立于 web boot，仿 m31）：
  `describe_ui_returns_valid_declaration` · `declaration_satisfies_v2_card_contract`
  （cardId 非空 / type 落闭集 / size 封顶且**声明里无坐标** / `dataRpc` 显式）
  · **`no_legacy_v1_top_level_declaration_anywhere`**（双模型防线）
  · `static_ui_json_matches_describe_ui`（一份契约）· `save_writes_kv_and_rejects_unknown_field_fail_loud`
  · `current_values_roundtrips_saved_settings` · `discover_models_returns_default_catalog`
  · `unknown_endpoint_fail_loud`
- `crates/dsh-cli/src/web.rs`：`llm_deepseek_remote_routes_and_serves_static`
  （路由 + 未装配回落 + `/plugins/llm-deepseek/ui.json` 静态 200 且为 v2）
- 双模型防线**做过红验证**：临时放入 `kind:"form" + $schema:v1` 探针 → 该测试 FAILED 并精确
  报出违规路径 → 移除后复绿。**它不是恒真断言**。（注意：必须解析后看顶层，不能 grep 文本——
  `view.kind:"form"` 是合法内容视图，grep 必假阳性。）

### 2.4 诚实台账：契约已定但**尚未实现**的（新 agent 最易踩的坑）
| 项 | 真实状态 |
|---|---|
| `/api/ui-manifest` 清单端点 | ⬜ **完全未实现**（端点形状还有开放问题，见 §3-A） |
| 桌布壳（左侧分类栏 + 右侧网格工作台 + 排布算法） | ⬜ **不存在**。`web/renderer.js` 只是**单卡 demo，不是桌布** |
| `status` / `list` 渲染器 | ⬜ **未实现**。契约里属「实现档」，但当前 demo 只认 `form`，其余落 `renderer-unimplemented` 回落（这是三档制的**设计意图**，不是 bug，也不代表已完成） |
| `ui-manifest-changed` SSE 事件 | ⬜ 未加（通道 `/plugins/events` 本身已存在，D-099） |
| 试点 entry 化（作为 `dsh-loader` Plugin 按名解析 + `inject=['llm']` 依赖激活） | ⬜ 未做（**试点明确边界外**，属装配引擎侧的后续） |
| 真实 DeepSeek 网络调用 / LLM adapter | ⬜ 未做（`discoverModels` 只返回默认目录；运行期行为归 genai 决策） |
| SSR 首帧 | ⬜ 未做（D-179 定为可选、非验收项） |

---

## 3. 剩余缺口 C2 → C5（按优先级；每项标出**必须先决策**的点）

### A. C2：`/api/ui-manifest` 实时清单 + `rev`（**下一步就做**）
契约见 `.spec/service-assembly-ui-canvas/design.md` §6。要交付：
- 聚合 `boot.packages`（含 `web/ui.json` 的包）→ `cards:[{pluginName, cardId, type, title,
  size, declPath}]` + `rev`
- **实时计算**（禁缓存）；`rev` 建议用**内容哈希**而非单调计数（重启后客户端缓存的 rev 仍有效）
- **坏声明不静默丢**：`ui.json` 存在但坏（非 JSON / 非 v2 / 顶层非 card）→ 以 **error 条目**出现
  （画布据此画 fail-loud 卡）；**没有** `web/ui.json` 的包 = 无 UI，正常跳过（两者要区分）
- 建议在清单层就完成 **size 裁剪**与 **type 未知 → misc** 归一（宿主是单一权威，渲染器只信清单）
- 与 loader 生效状态交叉：**disabled 的 entry 不应出卡片**

**⚠ 开工前必须先决策：端点形状（我上一轮查到一半，证据如下）**
- `web.rs:971-972`：`if path.starts_with("/api") { let method = path.trim_start_matches("/api/") }`
  → `/api/ui-manifest` 会得到 `method = "ui-manifest"`（**不含 `/`**）
- 而 `dispatch_wasm_remote` 要求 `namespace/method`，**无 `/` 即 `not-implemented`**
- 两个选项：
  - **(a) 服从既有网关约定**：`/api/uiManifest/list`（args 可带 `{rev}`，未变则回
    `{unchanged:true}`），在原生 `match method` 里加一臂 —— **我的倾向**（与
    `pluginInventory/list`、`settings/describe` 同形，复用 trust fence / 前端 rpc 通道，不造野路由）
  - (b) 保留 `/api/ui-manifest` 裸 method：需在路由前特判，属为单个端点开例外
- **选定后请同步修 `.spec/service-assembly-ui-canvas/design.md` §6.1**（现文本写的是
  `/api/ui-manifest?rev=`，与既有 wire 不一致）。

**待查**：disabled entry 的取数接口——复用点候选是 `crates/dsh-cli/src/remote_host.rs`
的 `"loader"` 投影（它已在读 loader 条目并映射 `disabled`/`fiber.state`），**我未确认 API 细节**。

### B. C3：桌布壳最小集
侧栏（按 `type` 分组 + 计数 + 每类下插件名，未知落 `misc`）+ 右侧工作台（10px 网格自动流式、
按声明顺序、列数自适应、裁剪、点名字滚动聚焦且**不改布局**）+ `form` 渲染器 + §7 fail-loud 表
逐行落地。§7 表共 9 行（含 C1 补的 `card-kind-unknown`：区分「版本错」与「顶层容器种类错」）。

### C. C4：`status` / `list` 渲染器 + 真实数据面
首个 `list` 试点建议 = **插件清单**（harness 里大量面板本质是列表，`form` 表达会别扭）。
数据来源走 `view.dataRpc` / 卡级 `refresh`；拉不到显示诚实空态/错误态，**绝不伪造**。

### D. C5：热插拔最小验证
`/plugins/events` 新增 `ui-manifest-changed {rev}`；验证「装/卸一包 → 卡片增删可见」。

### E. 明确不做（勿顺手实现）
用户拖拽 / 自由摆放 / 布局持久化 · 卡内嵌画布（`board`）· `chat`/`chart`/`table` 渲染器 ·
SSR 首帧 · 推翻 harness 前端（迁移是**增量**的）。

---

## 4. 环境地雷（本会话踩实，**开工前先读**）

1. **无外网**，且 `cargo component build --offline` **不可用**（报
   `lock file must be provided when offline mode is enabled`，对既有插件同样报）。
   ✅ 正确姿势：`$env:CARGO_NET_OFFLINE="true"` 后用**不带 `--offline`** 的
   `cargo component build`。
2. **`sccache` 包装器会超时**（`sccache: Timed out waiting for server startup`）。
   ✅ 每条命令先 `Remove-Item Env:RUSTC_WRAPPER`。`term` 工具**不跨调用保留环境变量**，
   所以每条命令都要重新设 `$env:CARGO_NET_OFFLINE` 与清理 wrapper。
3. **PowerShell 会破坏中文 UTF-8**（读 `Get-Content -Raw` + `Set-Content` 双重编码；
   写 here-string 也会）**本会话曾因此损坏 `crates/dsh-cli/src/main.rs`，靠 `git checkout`
   救回**。✅ 规则：
   - **含中文的文件一律用编辑器工具创建/编辑**，绝不用 pwsh 写
   - 需要「重写整个文件」而编辑器拒绝时：先用 pwsh 写**纯 ASCII 占位**，再用编辑器
     `view`（同步缓存）→ `str_replace` 填中文
   - 提交信息含中文 → 写到 `.git/` 下的临时文件，`git commit -F <file>`（`.git/` 不入库），
     完事删除；字节级校验中文可用「按 Latin-1 读字节 + 匹配 UTF-8 序列」，**不要信控制台显示**
4. **新 wasm 插件必须有 `Cargo.lock`**（无网无法解析索引）：从 `wasm-plugins/host-remote/Cargo.lock`
   复制、只改根包名即可（依赖 serde_json/wit-bindgen-rt 0.44 已在本地缓存）。
5. **行号不可信**：`Get-Content` 数组下标 与 `Select-String` 的 `LineNumber`/编辑器行号
   在含中文的 UTF-8 文件上会**不一致**。定位用 `Select-String`/编辑器 `view`，**不要用
   数组下标做精确编辑**。
6. `cargo component build` 会往 stderr 打进度，PowerShell 会把整段当错误（`exit code 1`
   + `NativeCommandError`）——**这是假失败**。以 `Finished` / `Creating component` 为准。

---

## 5. 验收与回归基线

命令（每条都要先清 wrapper、设 offline 变量）：
```
$env:CARGO_NET_OFFLINE="true"                       # 不要加 --offline
cargo component build --manifest-path wasm-plugins/llm-deepseek/Cargo.toml
cargo test -p dsh-wasmrt --test m32_llm_deepseek    # 期望 8 passed
cargo test -p dsh-cli  llm_deepseek_remote_routes_and_serves_static
cargo test -p dsh-cli -p dsh-wasmrt
cargo clippy -p dsh-cli -p dsh-wasmrt --all-targets -- -D warnings   # 期望 0
node diff/ts-host/verify-diff.mjs                   # 26/26（本工作流未碰装配语义，应不变）
```

**当前基线（必须对齐，别追幽灵）**：
- m32 **8/8 绿**；`dsh-wasmrt` **14 个测试目标全绿**（含 m31 host-remote）
- `dsh-cli` **225 通过 / 5 失败**；clippy **0**；verify-diff **26/26**；serve 冒烟 **200/13270**
- **那 5 个失败是基线既有的环境性失败**（需真实 bash/后台进程），已用 `git stash` 验证与
  UI 工作流无关，**不要试图修它们**，但**新增失败一定是你引入的**：
  ```
  web::tests::m5_host_assemble_drives_real_tools
  web::tests::m5g_tick_auto_settles_bash_background_job
  web::tests::register_m5_tools_with_shell_host_binds_bash_really
  web::tests::register_m5_tools_with_bash_jobs_bridge_background_really
  web::tests::server_tick_once_advances_schedule_and_settles_jobs
  ```
- 流程纪律：需求 → 设计 → 编码（TDD 红→绿，**新断言必须做红验证**）→ 验收；每步落
  `.spec/<phase>/{requirements,design,acceptance}.md` + `docs/DECISIONS.md` 条目 + git 提交互查。

---

## 6. 文件索引

**规格**
- `.spec/service-assembly-ui/` P2 方向 + 架构模型（D-178/D-179）
- `.spec/service-assembly-ui-pilot/` 试点 req/design/acceptance（design 顶部横幅已标 v1→v2 已迁移；
  §10 是 v1→v2 逐项对照 + 「什么没动」）
- `.spec/service-assembly-ui-canvas/` **桌布契约**（requirements 决策回执表 / design §4 视图契约、
  §5 排布、§6 清单、§7 fail-loud 表、§9 迁移清单、§11 C1–C5 切分、§13 实施状态）

**代码**
- `wasm-plugins/llm-deepseek/{wit,src/lib.rs,plugin.json,web/*}`
- `crates/dsh-cli/src/web.rs`：serve 装配（搜 `llm-deepseek`）、`llm_deepseek_component_bytes()`、
  `dispatch_wasm_remote`（namespace 分流）、`serve_package_asset`、`/api` 路由（~971 行）
- `crates/dsh-cli/src/lib.rs`：`Boot.llm_deepseek_remote` / `Boot.packages`
- `crates/dsh-cli/src/main.rs`：`WebConfig { ..., wasm_base }`
- `crates/dsh-cli/src/plugin_pkg.rs`：文件夹包解析（`plugin.json` + 约定回退）
- `crates/dsh-cli/src/remote_host.rs`：宿主投影器（`kv` get/set 后端；`"loader"` 投影 = C2
  disabled 取数候选）
- `crates/dsh-wasmrt/src/remote.rs`：`WasmRemoteEndpointPlugin` + `RemoteServiceProjector`
- 测试：`crates/dsh-wasmrt/tests/m32_llm_deepseek.rs`（+ `m31_host_remote.rs` 为同型参考模板）

---

## 7. 接手第一步（Sprint 0，建议顺序）

1. 读 §4 环境地雷（能省你半天）。
2. 读 `.spec/service-assembly-ui-canvas/design.md` §4/§5/§6/§7/§13 —— §13 是「已做/未做」的权威表。
3. 跑一遍 §5 回归，确认自己拿到的基线数字与 §5 一致（不一致先查环境，别急着改代码）。
4. **决策 §3-A 的端点形状**（(a) 我的倾向：`uiManifest/list`），定完立刻回写 design §6.1。
5. 建 `.spec/service-assembly-ui-c2/{requirements,design}.md`（把 design §6 具体化到函数签名/
   装配点/rev 语义/坏包语义/测试清单），过闸后再写代码。
6. **TDD 红→绿**：清单聚合、`rev` 稳定性与变化、坏包 error 条目、**实时性**（装卸包后清单变）、
   size 裁剪与 type 归一在清单层生效；然后才接 C3 桌布壳。

**三条不要**：不要把 `type` 与 `view.kind` 合并；不要让插件提交坐标；不要把 `status/list`
当成「已完成」（它们现在只会走回落）。
