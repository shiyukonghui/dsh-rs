//! DSH 层启动器核心：从 cordis.yml 形态配置组装运行时并驱动 loop。
//!
//! 对应 deepseek-harness 的 app-boot（profile → bundle → cordis.patch → 挂载）：
//! 1. 读 YAML 入口列表（services + loop entries），叠加 profile overlays
//!    （同 id entry 后者覆盖 config——bundle/patch 语义）；
//! 2. 注册插件仓库：`dsh:services`（缝的承载）+ 插件包（**文件夹**：wasm 组件 +
//!    前端组件，文件夹名 = 注册名；`plugin.json` 清单或构建目录约定定位 wasm）；
//! 3. `Include` 挂载；
//! 4. 宿主经 `run_turn` 驱动 WASM loop（输入来自调用方）。

// 同 dsh-core：单线程运行时，`Arc` 仅共享所有权。
#![allow(clippy::arc_with_non_send_sync)]

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::*;
use dsh_loader::{EntryOptions, Include, Loader};
use dsh_wasmrt::{
    ComponentKind, DshServicesPlugin, WasmLoopPlugin, load_wasm_component_plugin,
};

/// `dsh web`——服务 DeepSeek Harness 前端 + `/api` RPC（M70）。
pub mod web;

/// M3a host 目录方法面（listDirectory/createDirectory 真实 fs 实现，可差分单测）。
pub mod host_dir;

/// M3a+（D-098）：`host.pickDirectory` 原生目录选择——纯时序层（可注入 bindings 单测）；
/// Windows 真交互在 `host_picker_windows`（进程内 IFileDialog，零子进程）。
pub mod host_picker;

/// D-099：`/plugins/events` HMR SSE 通道（客户端插件热重载，对齐 TS `client/hmr`）。
pub mod hmr_events;

/// D-100：工作区注册表（对齐 TS workspace 域本会话语义：create 幂等/新铸 id、list、
/// attach、归档；web RPC 用；持久化域另行立项）。
pub mod workspace_host;

/// P1-b：preset 发现宿主（roster/read/authorable 的 domain 侧；mount/guard 是 P2）。
pub mod preset_host;
pub mod standing;

/// D-098：Windows 原生目录选择绑定（IFileDialog/COM via 新版 windows crate）。
#[cfg(windows)]
pub mod host_picker_windows;

/// M1e SessionHost：把 WASM loop 的 SessionLog 事件 adopt 进 dsh-session store，
/// 并挂载持久化（dsh-persistence coordinator event 回调）。
pub mod session_host;

/// M4h 补实：subagent 真实进程内驱动（in-process spawn/fork + 只读 list/history +
/// prompt 经 AgentLoopHost 驱动 + interrupt 收据）。
pub mod subagent_runtime;
pub mod remote_host;

/// 插件包（文件夹）解析：插件 = 文件夹（wasm 组件 + 前端组件），文件夹名 = 注册名。
pub mod plugin_pkg;

/// D-183：桌布 C2——宿主实时清单聚合（`uiManifest/list`）：包扫描 + 校验归一 + 坏包 error
/// 条目 + sha256 内容哈希 rev。**每请求实时计算，禁缓存**（热插拔第一等要求）。
pub mod ui_manifest;

/// D-184：桌布 C3——`/canvas` 独立视图路由（壳资产编译进二进制；miss → 404 不落 SPA）。
pub mod canvas;

/// M6 step5b：真实 LLM 装配（deepseek 适配器 + dsh-core 流式 HTTP 桥 + 诚实 no-key
/// fail-loud；key 仅 `DEEPSEEK_API_KEY` 环境变量）。
pub mod m6_llm;
pub mod m6_env;

/// `host.pickDirectory` 宿主选择器：`Ok(Some(path))` 选中 / `Ok(None)` 取消 /
/// `Err(msg)` 不可用（wire `directory-picker-unavailable`）。
/// `Arc`（+ Send + Sync）：pickDirectory 是 user-paced 模态对话框，web serve 在
/// **独立线程**上驱动它，不能饿死单线程 accept 循环（D-098）。
pub type HostPicker = Arc<dyn Fn() -> Result<Option<String>, String> + Send + Sync>;

/// 启动结果：运行时上下文 + loop 插件句柄（供驱动）。
pub struct Boot {
    pub ctx: Cordis,
    /// M58：loop 插件句柄（`Rc<RefCell<>>`——HMR refresh 换 loop 组件时替换）。
    pub loop_plugin: Rc<std::cell::RefCell<Arc<WasmLoopPlugin>>>,
    /// 可用服务句柄（诊断）。
    pub sessions: dsh_core::SessionHandle,
    /// M1e：llm 服务句柄（`llm.providers`/`llm.models` 目录来源）。
    pub llm: dsh_core::LlmHandle,
    /// HMR refresh 回调：重读主配置 + overlays → 重新挂载（watch 模式用；
    /// 对应 Cordis Include 插件的 `internal/update → refresh` 路径）。
    pub refresh: Rc<dyn Fn() -> Result<(), CordisError>>,
    /// M2g：可选的 Rust AgentLoopHost（装配了真实 agent-loop 服务；Some 时
    /// `session.prompt`/`agent.run` 改驱 Rust loop，None 保留 M1 WASM loop 路径）。
    /// `Arc`：Phase 3 起 `AgentLoopHost` 为 Arc 句柄（Send+Sync）。
    pub agent_loop: Option<Arc<dsh_agent_loop::AgentLoopHost>>,
    /// M6（step8，D-087）：装配 loop 的真实 provider catalog 视图
    /// （`server_catalog_view`：models 目录 + 容量默认 + 重试策略）。`llm.models`
    /// 以此做 provider caps 列录；None（未启用 agent_loop）→ 回退既有 Boot.llm 目录。
    pub agent_catalog: Option<serde_json::Value>,
    /// M3b：settings 能力缝（namespace 注册 + describe/update/replace/mutate + 文件）。
    /// `Rc<RefCell>`——web RPC 只持 `&Boot`，跨请求共享可变状态。
    pub settings: Rc<std::cell::RefCell<dsh_settings::SettingsProvider>>,
    /// M3c：credentials 能力缝（env/file 分层 + set/unset + 文件）。
    pub credentials: Rc<std::cell::RefCell<dsh_credentials::CredentialProvider>>,
    /// M4h：goal 服务（goal.* RPC 的真实状态机；`Rc<RefCell>` 跨请求共享）。
    pub goal: Rc<std::cell::RefCell<dsh_goal::GoalService>>,
    /// M4h：会话投影注册表（当前挂 `todos` unit；goal/plan/subagent/jobs 投影
    /// 挂 dsh-session 事件流为 M4 后续接入，本子步仅注册 + 可选暴露）。
    pub projections: Rc<std::cell::RefCell<dsh_session_query::projection::ProjectionRegistry>>,
    /// M3a+（D-098）：`host.pickDirectory` 后端（None → wire `directory-picker-unavailable`，
    /// 诚实上报而非 `{path:null}` 冒充取消）。web serve 装配为进程内原生选择器；测试注入 stub。
    pub host_picker: Option<crate::HostPicker>,
    /// D-100：真实工作区注册表（`workspace.*` RPC 语义来源）。`Rc<RefCell>`——web RPC
    /// 只持 `&Boot`，跨请求共享可变状态（serve 单线程 accept 循环，无锁纪律）。
    pub workspaces: Rc<std::cell::RefCell<crate::workspace_host::WorkspaceRegistry>>,
    /// D-100：宿主事件日志（`host/*` 帧内层 payload 的 append-only 队列）。serve 装配
    /// `Arc<Mutex<Vec<Value>>>` 供 `events.host` SSE/WS 线程各自持游标增量下推；None
    /// （非 web boot/测试口）→ RPC 不推帧（注册表语义仍生效，事件面由 serve 级测试覆盖）。
    pub host_events: Option<Arc<std::sync::Mutex<Vec<serde_json::Value>>>>,
    /// P1-b：preset 发现宿主（roster/read/authorable + settings default 解析的 domain 侧；
    /// mount/guard 是 P2）。`Rc<RefCell>`——web RPC 只持 `&Boot`，跨请求共享（serve 单线程）。
    /// P1-b：复制自持 + 自定义 agent 预设发现宿主。
    pub presets: Rc<std::cell::RefCell<crate::preset_host::PresetHost>>,
    /// P4：standing 注册表（共享 SystemPrompt 的 scoped 贡献 + join 报告）。web
    /// serve 装配 agent-loop 后以 `host.prompt` 重建（否则为占位）。
    pub standings: Rc<std::cell::RefCell<crate::standing::StandingRegistry>>,
    /// L1（D-105）：plan-mode 折叠的「当前计划会话」（single-active GUI：最后一次
    /// agentPreset.select 的会话；standings 按 preset-id 挂载且单活跃，折叠取其事件
    /// 日志）。web serve 装配时 Some 并注入 standing 折叠源；None（未启 loop）→ 无源。
    /// `Arc<Mutex>`：被 Send+Sync 的 standing plan-mode 折叠源闭包捕获。
    pub plan_session: Option<Arc<std::sync::Mutex<String>>>,
    /// D-108/G：approval wire 注册表（前端 `approval/requested`/`resolved` 帧 +
    /// `POST /api/respond` 答复）。serve 装配 agent-loop 时 Some；None（未启 loop /
    /// 测试口）→ 不推 wire 帧、respond 一律 not-pending（无决可答，诚实）。
    pub approval_wire: Option<crate::web::approval_wire::ApprovalWireRef>,
    /// D-115-Web（D3）：wasm 组件承载 host 侧 remote 端点（`WasmRemoteEndpointPlugin`
    /// 加载 `host-remote` world 组件）。serve 装配 Some；None（测试口/未装配）→
    /// dispatch 回落 not-implemented（诚实）。`Rc<RefCell>`：web RPC 只持 `&Boot`，
    /// 组件懒实例化入 `RefCell`（单线程 accept，与 loop_plugin 同纪律）。
    pub remote_plugin: Option<Rc<std::cell::RefCell<dsh_wasmrt::WasmRemoteEndpointPlugin>>>,
    /// P2 试点（服务装配单元）：`llm-deepseek` wasm 远程载体（describeUI/save/
    /// discoverModels；复用 host-remote world 接口身份）。serve 装配 Some；None
    /// （测试口/未装配）→ `llm-deepseek.*` 路由回落 not-implemented（诚实）。
    pub llm_deepseek_remote: Option<Rc<std::cell::RefCell<dsh_wasmrt::WasmRemoteEndpointPlugin>>>,
    /// D2：真实宿主投影器（`RemoteServiceProjector`——loader/session/settings/持久 KV
    /// 等真实数据源面）。serve 装配 Some；None（测试口）→ wasm 端点反查宿主时诚实报错。
    pub remote_projector: Option<Rc<dyn dsh_wasmrt::RemoteServiceProjector>>,
    /// D-115-Web（阶段 A）：真实动态装配器 `dsh-loader`（create/update/dispose +
    /// register_plugin + fiber）——dynamicCordisRunner/pluginInventory 的真实数据源
    /// 与「动态装配」句柄。boot() 装配 Some；None（测试口）→ 投影回退空/诚实。
    pub loader: Option<dsh_loader::Loader>,
    /// 插件包（文件夹）装配结果（wasm + 前端）：web serve 据此挂 `/plugins/<name>/**`
    /// 静态资源（D2）。boot() 填充；非包入口不列入。
    pub packages: Vec<crate::plugin_pkg::PluginPackage>,
}

/// M56：转储生效配置（对齐生产 `dsh --dump-config`）——读主配置 + overlays
/// 合并（同 id 后者覆盖 config、新 id 追加），序列化为 YAML；**不 boot loop**
/// （纯配置查看）。
pub fn dump_config(config_path: &Path, overlays: &[PathBuf]) -> Result<String, CordisError> {
    let mut entries = read_entries(config_path)?;
    for overlay in overlays {
        let layer = read_entries(overlay)?;
        entries = merge_entries(entries, layer);
    }
    serde_yaml::to_string(&entries)
        .map_err(|e| CordisError::Internal(format!("dump-config serialize: {e}")))
}

/// 宿主可用服务插件登记面（服务装配单元 E1）：收敛「名称 → 实现」登记。
///
/// 现注册 `dsh:services`（`DshServicesPlugin::all()`：sessions/tools/llm）；未来
/// genai/llm-pi-ai 等适配器在此追加。cordis.yml 声明而仓库缺失的 name 由 loader
/// include.load() 报 `unknown plugin {name}`（fail-loud，诚实——不伪装可用）。
pub fn register_host_service_plugins(loader: &dsh_loader::Loader) {
    loader.register_plugin("dsh:services", Arc::new(DshServicesPlugin::all()));
}

/// 服务装配单元 E3（A7）：把 loader 的持久化 seam 挂到主配置——
/// 运行时 loader.create/update/remove 后经 sink 把**权威入口列表**原子写回 cordis.yml
/// （`dsh_persistence::fs_atomic::atomic_write`：temp + sync + rename）。
/// 由宿主在 boot 完成后接线（避免启动期 include.load() 意外回写）。
pub fn attach_config_persist(loader: &dsh_loader::Loader, config_path: &std::path::Path) {
    let path = config_path.to_path_buf();
    loader.set_persist(Some(Rc::new(move |entries: &[dsh_loader::EntryOptions]| {
        let yaml = serde_yaml::to_string(entries)
            .map_err(|e| format!("persist serialize {}: {e}", path.display()))?;
        dsh_persistence::fs_atomic::atomic_write(&path, yaml.as_bytes())
            .map_err(|e| format!("persist write {}: {e}", path.display()))
    })));
}

/// 从 cordis.yml 形态的 YAML 配置启动（服务装配单元 Phase 1/E1：服务插件 entry 化）。
///
/// `boot` = [`boot_with_host_plugins`] 的便捷包装（无追加宿主插件）。
pub fn boot(
    config_path: &Path,
    overlays: &[PathBuf],
    wasm_base: &Path,
) -> Result<Boot, CordisError> {
    boot_with_host_plugins(config_path, overlays, wasm_base, &[])
}

/// boot + 追加宿主插件注册（服务装配单元 E1）：在 include.load() 前把宿主可用的
/// 服务插件按名注册进 loader 仓库——cordis.yml 声明的服务 entry（如未来的
/// llm-pi-ai 适配器 / 自定义服务）由此可按名解析 apply（服务插件 entry 化）。
pub fn boot_with_host_plugins(
    config_path: &Path,
    overlays: &[PathBuf],
    wasm_base: &Path,
    extra_host_plugins: &[(&str, Arc<dyn Plugin>)],
) -> Result<Boot, CordisError> {
    // 读主配置 + 叠加层，合并 entries（同 id 后者覆盖）
    let mut entries = read_entries(config_path)?;
    for overlay in overlays {
        let layer = read_entries(overlay)?;
        entries = merge_entries(entries, layer);
    }

    let cordis = Cordis::new();
    let loader = Loader::new(&cordis)?;

    // 宿主可用服务插件登记面（E1：消除 dsh:services 名称特判 → 统一按名解析）。
    register_host_service_plugins(&loader);
    // 额外宿主插件（服务装配单元：测试注入受控服务 / 未来 llm-pi-ai 等适配器打包）。
    for (name, plugin) in extra_host_plugins {
        loader.register_plugin(name, plugin.clone());
    }

    // 插件包装配（D4 重构）：`name` 未命中内置/宿主注册 → 解析为文件夹包
    // （wasm + 前端；plugin.json 清单或约定回退）。world 判别选适配器：
    // dsh-loop → WasmLoopPlugin（首个 = turn 句柄）；dsh-plugin → WasmComponentPlugin。
    // 移除 config.wasm 特判。
    let (loop_plugin_opt, packages) = assemble_plugin_packages(&loader, wasm_base, &entries)?;
    let loop_plugin = loop_plugin_opt.ok_or_else(|| {
        CordisError::Internal("boot: no loop entry (dsh-loop world plugin package) in cordis.yml".into())
    })?;
    // M58：可变 loop 句柄（HMR refresh 换组件时替换）
    let loop_cell: Rc<std::cell::RefCell<Arc<WasmLoopPlugin>>> =
        Rc::new(std::cell::RefCell::new(loop_plugin.clone()));

    // Include 挂载（用合并后的配置；临时写回供 loader.sync）
    let merged = merge_path_for_include(config_path, &entries)?;
    let include = Include::new(&loader, &merged, vec![]);
    include.load()?;

    let sessions: dsh_core::SessionHandle = cordis
        .get_typed::<dsh_core::SessionHandle>("sessions")
        .ok_or_else(|| CordisError::Internal("boot: sessions service missing".into()))?
        .as_ref()
        .clone();

    // M1e：llm 服务句柄（真实模型适配器注册处；web `llm.providers`/`llm.models`
    // 的目录来源）。sessions 服务必有，但 llm 可能未注册——缺省给空服务。
    let llm: dsh_core::LlmHandle = cordis
        .get_typed::<dsh_core::LlmHandle>("llm")
        .map(|a| a.as_ref().clone())
        .unwrap_or_else(dsh_core::new_llm);

    // HMR refresh：重读主配置 + overlays → 重新挂载（async 事务 `load_async` →
    // `sync_async` allSettled + 整事务回滚；经 current_thread runtime block_on 驱动）。
    let refresh_loader = loader.clone();
    let refresh_config = config_path.to_path_buf();
    let refresh_overlays = overlays.to_vec();
    let refresh_wasm_base = wasm_base.to_path_buf();
    let refresh_loop_cell = loop_cell.clone();
    let refresh: Rc<dyn Fn() -> Result<(), CordisError>> = Rc::new(move || {
        let entries = read_entries(&refresh_config)?;
        let mut merged = entries;
        for overlay in &refresh_overlays {
            let layer = read_entries(overlay)?;
            merged = merge_entries(merged, layer);
        }
        // 插件包装配（D4）：新出现/改名条目重新解析注册；首个 dsh-loop 包重建
        // loop 句柄（组件变化经 refresh 生效）。已注册名跳过（注册保持）。
        let (new_loop, _packages) =
            assemble_plugin_packages(&refresh_loader, &refresh_wasm_base, &merged)?;
        let tmp = merge_path_for_include(&refresh_config, &merged)?;
        let include = Include::new(&refresh_loader, &tmp, vec![]);
        // async 事务：全部入口都尝试 create/update（一个失败不阻断其他）、
        // 失败整事务回滚——对应 Cordis `EntryGroup.update(config)`。
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| CordisError::Internal(format!("hmr refresh runtime: {e}")))?;
        rt.block_on(include.load_async()).map_err(|agg| {
            // AggregateError → 首个失败（消息含数量）
            CordisError::Internal(format!(
                "hmr refresh failed ({} errors)",
                agg.errors.len()
            ))
        })?;
        // M58/（D4）：换 loop 句柄——refresh 后首个 dsh-loop 包的实例；无 loop 包则保持。
        if let Some(plugin) = new_loop {
            *refresh_loop_cell.borrow_mut() = plugin;
        }
        Ok(())
    });

    let settings = Rc::new(std::cell::RefCell::new(
        dsh_settings::SettingsProvider::memory(),
    ));
    // M3d：注册 LLM 连接 namespace（对齐 TS `llm` 插件注册集）。schema 覆盖
    // provider/model/baseURL/apiKey(secret)；用户写入即落到本地文档。
    {
        let mut sp = settings.borrow_mut();
        let mut dict = std::collections::HashMap::new();
        dict.insert(
            "provider".to_string(),
            dsh_schema::Schema::with_default(&dsh_schema::Schema::string(), serde_json::json!("dsh")),
        );
        dict.insert(
            "model".to_string(),
            dsh_schema::Schema::with_default(&dsh_schema::Schema::string(), serde_json::json!("echo")),
        );
        dict.insert("baseURL".to_string(), dsh_schema::Schema::string());
        dict.insert(
            "apiKey".to_string(),
            dsh_schema::Schema::secret(&dsh_schema::Schema::string()),
        );
        sp.register("llm", &dsh_schema::Schema::object(dict), None, dsh_settings::Applies::Restart);
        // M3d+（D-095）：Web 前端产品偏好 namespace 集（ui-onboarding/ui-theme/locale/
        // ui-conversation/shell/agent-loop/permission）——使用阶段发现前端必读写，
        // 缺注册即 settings-rejected。
        register_host_settings(&mut sp);
        // D-115-Web（模型配置 CRUD 对齐 TS）：llm-deepseek / llm-pi-ai / agent-default-model。
        register_model_config_settings(&mut sp);
    }

    Ok(Boot {
        ctx: cordis,
        loop_plugin: loop_cell,
        sessions,
        llm,
        refresh,
        agent_loop: None,
        agent_catalog: None,
        settings,
        credentials: Rc::new(std::cell::RefCell::new(
            dsh_credentials::CredentialProvider::memory(),
        )),
        goal: Rc::new(std::cell::RefCell::new(dsh_goal::GoalService::new(
            dsh_goal::ServiceOptions::default(),
        ))),
        projections: crate::web::assembled_projection_registry(),
        host_picker: None,
        workspaces: Rc::new(std::cell::RefCell::new(
            crate::workspace_host::WorkspaceRegistry::new(),
        )),
        host_events: None,
        presets: Rc::new(std::cell::RefCell::new(crate::preset_host::PresetHost::default())),
        standings: Rc::new(std::cell::RefCell::new(crate::standing::StandingRegistry::default())),
        plan_session: None,
        approval_wire: None,
        remote_plugin: None,
        llm_deepseek_remote: None,
        remote_projector: None,
        loader: Some(loader.clone()),
        packages,
    })
}

/// M3d+（D-095 使用验证发现）：注册宿主侧**产品偏好 namespace 集**——对齐 TS Host
/// apiproxy 注册面（`deepseek-harness/packages/host/apiproxy/tests/api-proxy-config.spec.ts`
/// 的 `serves product preference namespaces` 等用例）。web 前端会经 `settings.describe/
/// update/replace/mutate` 读写这些 namespace（onboarding/主题/语言/会话/agent-loop/
/// permission/shell）；此前只注册了 `llm`，前端一进页面就 `settings.mutate` 撞上
/// `namespace … is not registered`（settings-rejected，使用测试阶段实测发现）。
/// 全部 schema 照搬 TS Host；`register` 幂等（同名重复注册先行返回）。
pub fn register_host_settings(sp: &mut dsh_settings::SettingsProvider) {
    use dsh_schema::Schema;
    let mut on = std::collections::HashMap::new();
    on.insert("welcomeNoticeVersion".into(), Schema::string());
    sp.register("ui-onboarding", &Schema::object(on), None, dsh_settings::Applies::Live);

    let mut theme = std::collections::HashMap::new();
    theme.insert(
        "preference".into(),
        Schema::with_default(
            &Schema::union(vec![
                Schema::const_value(serde_json::json!("light")),
                Schema::const_value(serde_json::json!("dark")),
                Schema::const_value(serde_json::json!("system")),
            ]),
            serde_json::json!("system"),
        ),
    );
    sp.register("ui-theme", &Schema::object(theme), None, dsh_settings::Applies::Live);

    let mut locale = std::collections::HashMap::new();
    locale.insert(
        "preference".into(),
        Schema::union(vec![
            Schema::const_value(serde_json::json!("zh")),
            Schema::const_value(serde_json::json!("en")),
        ]),
    );
    sp.register("locale", &Schema::object(locale), None, dsh_settings::Applies::Live);

    let mut conversation = std::collections::HashMap::new();
    conversation.insert(
        "busyEnter".into(),
        Schema::with_default(
            &Schema::union(vec![
                Schema::const_value(serde_json::json!("queue")),
                Schema::const_value(serde_json::json!("steer")),
            ]),
            serde_json::json!("queue"),
        ),
    );
    sp.register(
        "ui-conversation",
        &Schema::object(conversation),
        None,
        dsh_settings::Applies::Live,
    );

    let mut shell = std::collections::HashMap::new();
    shell.insert(
        "timeoutMs".into(),
        Schema::with_default(&Schema::number(), serde_json::json!(120_000)),
    );
    sp.register("shell", &Schema::object(shell), None, dsh_settings::Applies::Live);

    let mut agent_loop = std::collections::HashMap::new();
    agent_loop.insert(
        "maxParallelToolCalls".into(),
        Schema::with_default(&Schema::number(), serde_json::json!(10)),
    );
    sp.register("agent-loop", &Schema::object(agent_loop), None, dsh_settings::Applies::Live);

    let mut permission = std::collections::HashMap::new();
    permission.insert(
        "defaultPreset".into(),
        Schema::required(&Schema::union(vec![
            Schema::const_value(serde_json::json!("read-only")),
            Schema::const_value(serde_json::json!("workspace-write")),
        ])),
    );
    sp.register(
        "permission",
        &Schema::object(permission),
        Some(serde_json::json!({"defaultPreset": "read-only"})),
        dsh_settings::Applies::Live,
    );

    // P1-b（D-103/C-04）：agent-presets settings namespace {default}——新会话未选时的
    // 初始预设（base=工程默认；default 会话不隐式 join）。
    crate::preset_host::register_agent_presets_settings(sp);
}

/// D-115-Web（模型配置 CRUD 对齐 TS）：注册模型配置相关 settings namespace——
/// `llm-deepseek`（扁平，对齐 TS llm-deepseek Config）、`llm-pi-ai`（providers dict，
/// 对齐 TS llm-pi-ai Config）、`agent-default-model`（selectModel 默认模型持久化）。
/// 全部 Applies=Live（对齐 TS installSettingsSection 默认 live）。
///
/// schema 为**宽进**（非 strict）：TS 前端 editor 可能写未列字段（models 子字段、
/// profile 的各类容量），我们以类型级校验 + 真实语义验证（genai/适配器）为准，不因
/// schema 漏列拒绝前端可写配置。
pub fn register_model_config_settings(sp: &mut dsh_settings::SettingsProvider) {
    use dsh_schema::Schema;
    let mut profile_fields = std::collections::HashMap::new();
    // llm-pi-ai provider profile：对齐 TS PiAiProviderProfile 核心字段（宽进）。
    profile_fields.insert(
        "apiKeyEnv".into(),
        Schema::role(&Schema::string(), "credential-ref"),
    );
    profile_fields.insert("displayName".into(), Schema::string());
    profile_fields.insert(
        "api".into(),
        Schema::union(vec![
            Schema::const_value(serde_json::json!("openai-completions")),
            Schema::const_value(serde_json::json!("openai-responses")),
            Schema::const_value(serde_json::json!("anthropic-messages")),
        ]),
    );
    profile_fields.insert("baseURL".into(), Schema::string());
    profile_fields.insert("transport".into(), Schema::string());
    profile_fields.insert("defaultContextWindow".into(), Schema::number());
    profile_fields.insert("defaultMaxTokens".into(), Schema::number());

    // models 数组：id 必填 + 可选 name/contextWindow/maxTokens；数组项对象构造成 lazy-ref
    // 免循环（models → profile → models 的交叉应用在 settings 层不需要，宽进即可）。
    let mut model_fields = std::collections::HashMap::new();
    model_fields.insert("id".into(), Schema::required(&Schema::string()));
    model_fields.insert("name".into(), Schema::string());
    model_fields.insert("contextWindow".into(), Schema::number());
    model_fields.insert("maxTokens".into(), Schema::number());
    profile_fields.insert(
        "models".into(),
        Schema::array(Schema::object(model_fields.clone())),
    );

    // llm-pi-ai：`{ providers: dict(route → Profile) }`；空 dict = dormant（对齐 TS）。
    let mut pi_ai = std::collections::HashMap::new();
    pi_ai.insert(
        "providers".into(),
        Schema::dict(Schema::object(profile_fields.clone()), Schema::string()),
    );
    sp.register("llm-pi-ai", &Schema::object(pi_ai), None, dsh_settings::Applies::Live);

    // llm-deepseek：扁平 {apiKeyEnv, baseURL, thinking, reasoningEffort, maxTokens,
    // defaultContextWindow, models[]}（对齐 TS Config；apiKeyEnv 是 credential-ref）。
    let mut deepseek = std::collections::HashMap::new();
    deepseek.insert(
        "apiKeyEnv".into(),
        Schema::with_default(
            &Schema::role(&Schema::string(), "credential-ref"),
            serde_json::json!("DEEPSEEK_API_KEY"),
        ),
    );
    deepseek.insert("baseURL".into(), Schema::string());
    deepseek.insert(
        "thinking".into(),
        Schema::union(vec![
            Schema::const_value(serde_json::json!("enabled")),
            Schema::const_value(serde_json::json!("disabled")),
        ]),
    );
    deepseek.insert(
        "reasoningEffort".into(),
        Schema::union(vec![
            Schema::const_value(serde_json::json!("off")),
            Schema::const_value(serde_json::json!("low")),
            Schema::const_value(serde_json::json!("high")),
            Schema::const_value(serde_json::json!("max")),
        ]),
    );
    deepseek.insert("maxTokens".into(), Schema::number());
    deepseek.insert("defaultContextWindow".into(), Schema::number());
    deepseek.insert("models".into(), Schema::array(Schema::object(model_fields)));
    sp.register("llm-deepseek", &Schema::object(deepseek), None, dsh_settings::Applies::Live);

    // agent-default-model：{provider, model, reasoningEffort?}（对齐 TS 语义——selectModel
    // 持久化默认模型）。provider/model 用 optional（非 required）：boot 泛用无 compose base，
    // 空 section（未设置默认）必须能 resolve；selectModel 成功时写成完整对象。
    let mut adm = std::collections::HashMap::new();
    adm.insert("provider".into(), Schema::string());
    adm.insert("model".into(), Schema::string());
    adm.insert("reasoningEffort".into(), Schema::string());
    sp.register(
        "agent-default-model",
        &Schema::object(adm),
        None,
        dsh_settings::Applies::Live,
    );
}

/// 读取 YAML 入口列表。
fn read_entries(path: &Path) -> Result<Vec<EntryOptions>, CordisError> {    let text = std::fs::read_to_string(path).map_err(|e| {
        CordisError::Internal(format!("boot read {}: {e}", path.display()))
    })?;
    let value: Value = serde_yaml::from_str(&text)
        .map_err(|e| CordisError::Internal(format!("boot parse yaml: {e}")))?;
    match value {
        Value::Array(items) => items
            .iter()
            .map(|v| serde_json::from_value(v.clone()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| CordisError::Internal(format!("boot entries invalid: {e}"))),
        _ => Err(CordisError::Internal(
            "cordis.yml must be a top-level array".into(),
        )),
    }
}

/// 合并两层 entries（同 id 后者覆盖 config/name；base 顺序保留，新 id 追加）。
fn merge_entries(base: Vec<EntryOptions>, overlay: Vec<EntryOptions>) -> Vec<EntryOptions> {
    let mut out = base;
    for layer in overlay {
        match out.iter_mut().find(|e| e.id == layer.id) {
            Some(existing) => {
                existing.name = layer.name;
                if !layer.config.is_null() && layer.config.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                    existing.config = layer.config;
                }
                existing.disabled = layer.disabled;
            }
            None => out.push(layer),
        }
    }
    out
}

/// 把合并后的 entries 写为临时 YAML 供 Include 挂载（loader.sync 读文件）。
fn merge_path_for_include(config_path: &Path, entries: &[EntryOptions]) -> Result<PathBuf, CordisError> {
    // 文件名含地址唯一性（多 boot 并行时避免覆盖）
    let unique = format!("{:p}", entries.as_ptr()).replace("0x", "");
    let tmp = std::env::temp_dir().join(format!(
        "dsh-boot-{}-{}-{}",
        config_path.file_name().and_then(|s| s.to_str()).unwrap_or("cfg"),
        std::process::id(),
        unique
    ));
    let yaml = serde_yaml::to_string(entries)
        .map_err(|e| CordisError::Internal(format!("boot serialize merged: {e}")))?;
    std::fs::write(&tmp, yaml)
        .map_err(|e| CordisError::Internal(format!("boot write merged: {e}")))?;
    Ok(tmp)
}

/// 插件包装配（D4）：扫描入口，`name` 未命中内置/宿主注册 → 解析为文件夹包
/// （wasm + 前端；plugin.json 清单或约定回退）；**world 判别**选适配器并注册：
/// - dsh-loop → `WasmLoopPlugin`（**首个** = turn 句柄，`run_turn` 具体类型）；
/// - dsh-plugin → `WasmComponentPlugin`（通用 apply）；
/// - Unknown → fail-loud（非 dsh world）。
///
/// boot 与 HMR refresh 共用。`world` 清单提示可显式覆盖字节探测快路径。
fn assemble_plugin_packages(
    loader: &Loader,
    wasm_base: &Path,
    entries: &[EntryOptions],
) -> Result<(Option<Arc<WasmLoopPlugin>>, Vec<crate::plugin_pkg::PluginPackage>), CordisError> {
    let mut loop_handle: Option<Arc<WasmLoopPlugin>> = None;
    let mut packages = Vec::new();
    for entry in entries {
        let Some(pkg) = crate::plugin_pkg::resolve_package(wasm_base, &entry.name)? else {
            continue;
        };
        let bytes = std::fs::read(&pkg.wasm).map_err(|e| {
            CordisError::Internal(format!("boot: read wasm component {}: {e}", pkg.wasm.display()))
        })?;
        let caps = crate::plugin_pkg::effective_caps(&entry.config, &pkg);
        let world = match pkg.world.as_deref() {
            Some("loop") => ComponentKind::Loop,
            Some("plugin") => ComponentKind::Plugin,
            _ => dsh_wasmrt::detect_component_kind(&bytes),
        };
        let already = loader.has_plugin(&entry.name);
        match world {
            ComponentKind::Loop => {
                let plugin = Arc::new(WasmLoopPlugin::new_owned(&entry.name, &bytes, caps)?);
                if loop_handle.is_none() {
                    loop_handle = Some(plugin.clone());
                }
                if !already {
                    let dyn_plugin: Arc<dyn Plugin> = plugin.clone();
                    loader.register_plugin(&entry.name, dyn_plugin);
                }
            }
            ComponentKind::Plugin => {
                if !already {
                    let plugin = load_wasm_component_plugin(
                        Box::leak(entry.name.clone().into_boxed_str()),
                        &bytes,
                        caps,
                    )?;
                    loader.register_plugin(&entry.name, plugin);
                }
            }
            ComponentKind::Unknown => {
                return Err(CordisError::Internal(format!(
                    "boot: plugin package {} is not a dsh-plugin or dsh-loop component",
                    entry.name
                )));
            }
        }
        packages.push(pkg);
    }
    Ok((loop_handle, packages))
}

/// 驱动一个 turn（宿主侧：注入 ctx → run_turn）。
/// M58：经 `Rc<RefCell<>>` 读当前 loop 插件（HMR refresh 换组件后生效）。
pub fn run_turn(boot: &Boot, input: &Value) -> Result<Value, CordisError> {
    let plugin = boot.loop_plugin.borrow().clone();
    plugin.run_turn(&boot.ctx, input)
}

/// M2g：把一条 user 文本驱动进 Rust AgentLoopHost 的配置 agent。
/// - 目标 agent：`configured_for_session` 解析——精确 `sessionId`（含 D-101 运行时
///   注册的 per-session agent）▸ `resumeSessionId` ▸ 约定 `agent-{id}`；
/// - agent 懒装配（ensure_agent 幂等）；事件直接写 AgentLoopHost 持有的共享 store
///   （web 侧与 SessionHost 同店 → 前端读模型/下链/持久化同一事实源）；
/// - 无 host 或无可路由 agent → Err（fail loud）。
///
/// 返回（D-106）：该 turn 后**仍待审批**的调用 id 列表（空 = 无审批挂起）；GUI 以此
/// 感知弹窗并把 `session.approval.decide` 作为回执。
pub fn run_rust_loop(boot: &Boot, session_id: &str, content: &str) -> Result<Vec<String>, CordisError> {
    let host = boot.agent_loop.clone().ok_or_else(|| {
        CordisError::Internal("no Rust AgentLoopHost assembled in this boot".into())
    })?;
    run_rust_loop_on_host(&host, session_id, content)
}

/// D-115（Phase 4 serve worker 化）：与 [`run_rust_loop`] 同语义，但只依赖
/// `Arc<AgentLoopHost>`（Send+Sync）而非整个 `&Boot`（含 Rc/RefCell 非 Send
/// 字段）——供 serve 的 worker 线程以 owned `Arc` 驱动整轮 turn（长 RPC 不占
/// accept 循环；HTTP 同步契约不变）。worker 线程内调用即真·生成中可被并发的
/// `session.cancel` 中断（共享取消令牌经 transport signal 直达阻塞读）。
pub fn run_rust_loop_on_host(
    host: &Arc<dsh_agent_loop::AgentLoopHost>,
    session_id: &str,
    content: &str,
) -> Result<Vec<String>, CordisError> {
    let configured = match host.configured_for_session(session_id) {
        Some(c) => c,
        None => {
            // D-101 续接路径：重启后恢复的持久化会话（存在于共享 store，但运行时
            // agent 按进程登记）→ 首次 prompt 时挂接 agent 再路由；**未知**会话仍
            // fail loud（修复不放行任意 id）。
            let sid = dsh_session::types::SessionId::from_raw(session_id.to_string());
            if host.store.get(&sid).is_some() {
                ensure_session_agent_on_host(host, session_id, None)?;
                host.configured_for_session(session_id)
                    .expect("just-registered session agent must resolve")
            } else {
                return Err(CordisError::Internal(format!(
                    "no configured agent maps to session \"{session_id}\""
                )));
            }
        }
    };
    host.ensure_agent(&configured)
        .map_err(|e| CordisError::Internal(format!("agent-loop host: {e}")))?;
    let message = dsh_llm::Message::user(
        // 消息 id 必须**会话内唯一**（前端按消息 id 建 conversation context；旧格式
        // `prompt-{session_id}` 同会话每轮重名 → 前端报「received more than one start
        // Match」。seq = 会话当前事件数（每次 prompt 递增，恢复会话续接也单调）。
        dsh_llm::MessageId::from_raw(format!(
            "prompt-{session_id}-{}",
            host.events(session_id).len()
        )),
        vec![dsh_llm::ContentBlock::text(content)],
    );
    host.followup(&configured.id, message)
        .map_err(|e| CordisError::Internal(format!("agent-loop host: {e}")))?;
    // D-106：驱动后仍待审批的调用（GUI 感知；空 = 无挂起）。
    let pending = host
        .pending_calls(&configured.id)
        .map_err(|e| CordisError::Internal(format!("agent-loop host: {e}")))?;
    Ok(pending
        .into_iter()
        .map(|p| p.block.id.raw().to_string())
        .collect())
}

/// D-115（Phase 4）：`ensure_session_agent` 的 host 参数化版（worker 线程同语义，
/// 只依赖 `Arc<AgentLoopHost>`，见 [`run_rust_loop_on_host`]）。
pub fn ensure_session_agent_on_host(
    host: &Arc<dsh_agent_loop::AgentLoopHost>,
    session_id: &str,
    cwd: Option<&str>,
) -> Result<(), CordisError> {
    if host.configured_for_session(session_id).is_some() {
        return Ok(());
    }
    let template = host
        .configured_for_session("default")
        .or_else(|| host.config.agents.first().cloned())
        .ok_or_else(|| {
            CordisError::Internal("agent-loop host: no base agent to clone for new session".into())
        })?;
    let configured = dsh_agent_loop::ConfiguredAgent {
        id: format!("session-{session_id}"),
        provider: template.provider.clone(),
        model: template.model.clone(),
        session_id: Some(session_id.to_string()),
        max_tokens: template.max_tokens,
        cwd: cwd.map(str::to_string).or_else(|| template.cwd.clone()),
        resume_session_id: None,
    };
    host.register_session_agent(configured)
        .map_err(|e| CordisError::Internal(format!("agent-loop host: {e}")))?;
    Ok(())
}

/// D-101：给一个**运行时铸出**的会话（web `session.create`/`fork`）挂接一个真实
/// agent——否则 `session.prompt` 对非配置会话报 `no configured agent maps to session`。
/// - 模板 = 装配期 `default` agent（provider/model/cwd 继承部署默认）；
/// - `cwd`：调用方（web 侧已知工作区路径时）优先，否则模板 cwd；
/// - 未装配 agent-loop（M1 WASM 路径）→ no-op（该路径不依赖 per-session agent）；
/// - 幂等（`register_session_agent`：会话已有身份则复用，不重复装配）。
pub fn ensure_session_agent(boot: &Boot, session_id: &str, cwd: Option<&str>) -> Result<(), CordisError> {
    let host = match &boot.agent_loop {
        Some(host) => host.clone(),
        None => return Ok(()),
    };
    ensure_session_agent_on_host(&host, session_id, cwd)
}

/// headless 单发任务的结果（对齐 DSH `dsh --profile headless "job"`：
/// 从 session 事件推导最终答案与 turn 结束原因）。
#[derive(Debug, Clone)]
pub struct HeadlessResult {
    /// 最后一个非空 assistant 文本（`data.message.content[0].text`）。
    pub answer: String,
    /// 最终 turn/end 的 `data.reason`（completed/blocked/max-tokens/...）。
    pub reason: String,
}

/// M45：headless 单发模式——提交一个任务（user 消息），驱动 loop，从
/// **session 事件流**推导最终答案（而非 loop 返回值——任何 loop 都可用）。
/// - 取最后一条 `assistant/message` 的 `data.message.content[0].text`
///   （M34 生产 Message 形状；空 content 的助手消息被 `derive_messages`
///   跳过，此处同样跳过空文本）；
/// - 取最后 `turn/end` 的 `data.reason`；
/// - 无 assistant 消息 → Err（fail loud）。
pub fn run_headless(boot: &Boot, task: &str) -> Result<HeadlessResult, CordisError> {
    run_turn(boot, &json!({"content": task}))?;
    let log = boot.sessions.lock().unwrap();
    derive_headless(log.events())
}

/// M48：恢复会话（`--session-in`）——从 JSONL 加载历史事件并导入
/// `boot.sessions`（append 重放 events + surface；`session_history()` 投影
/// 含前轮消息 → 多轮共享上下文，对齐 DSH resume 语义）。
pub fn restore_session(boot: &Boot, path: &std::path::Path) -> Result<(), CordisError> {
    let loaded = dsh_core::SessionLog::load_from(path)?;
    let mut log = boot.sessions.lock().unwrap();
    for e in loaded.events() {
        log.append(&e.kind, e.payload.clone());
    }
    Ok(())
}

/// 从 session 事件流推导 headless 结果（独立函数：可单测错误路径）。
pub(crate) fn derive_headless(events: &[dsh_core::SessionEvent]) -> Result<HeadlessResult, CordisError> {
    let mut answer: Option<String> = None;
    let mut reason: Option<String> = None;
    for e in events {
        let v = e.payload_value();
        match e.kind.as_str() {
            "assistant/message" => {
                // data = {turn, step, message: {id, role, content: [...], source}}
                let text = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                    .and_then(|blocks| {
                        blocks
                            .iter()
                            .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                            .and_then(|b| b.get("text").and_then(|t| t.as_str()))
                    })
                    .unwrap_or("");
                if !text.is_empty() {
                    answer = Some(text.to_string());
                }
            }
            "turn/end" => {
                if let Some(r) = v.get("reason").and_then(|r| r.as_str()) {
                    reason = Some(r.to_string());
                }
            }
            _ => {}
        }
    }
    let answer = answer.ok_or_else(|| {
        CordisError::Internal("headless: no assistant answer in session".into())
    })?;
    let reason = reason.unwrap_or_else(|| "completed".to_string());
    Ok(HeadlessResult { answer, reason })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M45：derive_headless 错误路径——无 assistant 文本 → Err。
    #[test]
    fn derive_headless_no_answer_fails() {
        let events = vec![
            dsh_core::SessionEvent {
                seq: 0,
                kind: "turn/start".into(),
                payload: serde_json::to_vec(&json!({"turn": 1})).unwrap(),
            },
            dsh_core::SessionEvent {
                seq: 1,
                kind: "turn/end".into(),
                payload: serde_json::to_vec(&json!({"turn": 1, "reason": "completed"})).unwrap(),
            },
        ];
        let err = derive_headless(&events).unwrap_err();
        assert!(err.to_string().contains("no assistant answer"), "{err}");
    }

    /// M45：derive_headless 跳过空文本助手消息（对齐 derive_messages）。
    #[test]
    fn derive_headless_skips_empty_assistant() {
        let empty = dsh_core::SessionEvent {
            seq: 0,
            kind: "assistant/message".into(),
            payload: serde_json::to_vec(&json!({
                "turn": 1, "step": 1,
                "message": {
                    "id": "a1", "role": "assistant",
                    "content": [],
                    "source": {"kind": "model", "provider": "mock", "model": "mock"},
                },
            }))
            .unwrap(),
        };
        let real = dsh_core::SessionEvent {
            seq: 1,
            kind: "assistant/message".into(),
            payload: serde_json::to_vec(&json!({
                "turn": 1, "step": 1,
                "message": {
                    "id": "a2", "role": "assistant",
                    "content": [{"type": "text", "text": "real answer"}],
                    "source": {"kind": "model", "provider": "mock", "model": "mock"},
                },
            }))
            .unwrap(),
        };
        let end = dsh_core::SessionEvent {
            seq: 2,
            kind: "turn/end".into(),
            payload: serde_json::to_vec(&json!({"turn": 1, "reason": "completed"})).unwrap(),
        };
        let r = derive_headless(&[empty, real, end]).expect("real answer");
        assert_eq!(r.answer, "real answer");
        assert_eq!(r.reason, "completed");
    }

    /// M52：`merge_entries`（`--overlay`/`--patch` 的合并语义）——
    /// 同 id 完整 config 替换（对齐生产 patch「替换整行 config」）+ 新 id
    /// 追加插入。
    #[test]
    fn merge_entries_replaces_config_and_inserts() {
        use dsh_loader::EntryOptions;
        let mut base = vec![
            EntryOptions::new("services", "dsh:services"),
            EntryOptions::new("loop", "echo-loop"),
        ];
        base[0].config = json!({"services": ["sessions"]});
        base[1].config = json!({"wasm": "echo-loop"});

        // patch 层 1：替换 loop 的完整 config（换 tool-loop）+ 插入新 entry
        let mut p1 = EntryOptions::new("loop", "tool-loop");
        p1.config = json!({"wasm": "tool-loop"});
        let p2 = EntryOptions::new("extra", "dsh:extra");
        let merged = merge_entries(base, vec![p1, p2]);

        assert_eq!(merged.len(), 3, "inserted new entry");
        let loop_entry = merged.iter().find(|e| e.id == "loop").unwrap();
        assert_eq!(loop_entry.name, "tool-loop");
        assert_eq!(loop_entry.config, json!({"wasm": "tool-loop"}), "config fully replaced");
        assert!(merged.iter().any(|e| e.id == "extra"), "new id appended");
        // 未命中的 services 保留
        assert!(merged.iter().any(|e| e.id == "services" && e.config == json!({"services": ["sessions"]})));
    }

    /// D-095（使用验证发现）：`register_host_settings` 注册 Web 前端必读写产品偏好
    /// namespace——此前只注册 `llm`，前端一进页面 `settings.mutate` 撞
    /// `namespace "ui-onboarding" is not registered`（settings-rejected）。红 = 缺
    /// 注册（NotRegistered）；绿 = 全套可读写（对齐 TS Host apiproxy 注册面）。
    #[test]
    fn register_host_settings_exposes_product_preference_namespaces() {
        let mut sp = dsh_settings::SettingsProvider::memory();
        crate::register_host_settings(&mut sp);

        let names = sp
            .describe_all()
            .into_iter()
            .map(|d| d.ns)
            .collect::<Vec<_>>();
        for want in ["ui-onboarding", "ui-theme", "locale", "ui-conversation", "shell", "agent-loop", "permission"] {
            assert!(names.contains(&want.to_string()), "namespace {want} registered (got {names:?})");
        }

        // 前端 onboarding 的缺省读写路径：settings.mutate 写 welcomeNoticeVersion。
        let v = sp
            .mutate(
                "ui-onboarding",
                &serde_json::json!([{"op": "set", "path": ["welcomeNoticeVersion"], "value": "v1"}]),
                None,
            )
            .expect("ui-onboarding mutate accepted (not settings-rejected)");
        assert_eq!(
            v.value["welcomeNoticeVersion"],
            serde_json::json!("v1"),
            "onboarding version persisted"
        );
        // ui-theme 同样可写（前端主题切换）。
        let theme = sp
            .mutate(
                "ui-theme",
                &serde_json::json!([{"op": "set", "path": ["preference"], "value": "dark"}]),
                None,
            )
            .expect("ui-theme mutate accepted");
        assert_eq!(theme.value["preference"], serde_json::json!("dark"));
        // 未注册 namespace（如持久化侵入探测）仍被拒（守卫不被弱化）。
        let err = sp
            .mutate("ui-evil", &serde_json::json!([{"op":"set","path":["x"],"value":1}]), None)
            .unwrap_err();
        assert!(matches!(err, dsh_settings::SettingsError::NotRegistered(_)), "guard holds: {err:?}");
    }

    /// D-115-Web（模型配置 CRUD 对齐 TS）：`register_model_config_settings` 注册
    /// llm-deepseek（扁平）/ llm-pi-ai（providers dict）/ agent-default-model，且可
    /// write/mutate（前端增删改查路径落到真实 settings）。
    #[test]
    fn register_model_config_settings_exposes_and_writes() {
        let mut sp = dsh_settings::SettingsProvider::memory();
        crate::register_model_config_settings(&mut sp);

        let names = sp
            .describe_all()
            .into_iter()
            .map(|d| d.ns)
            .collect::<Vec<_>>();
        for want in ["llm-deepseek", "llm-pi-ai", "agent-default-model"] {
            assert!(names.contains(&want.to_string()), "namespace {want} registered (got {names:?})");
        }

        // llm-deepseek：前端扁平写（settingsPath=[] 每字段一 op）→ baseURL + models set。
        let deepseek = sp
            .mutate(
                "llm-deepseek",
                &serde_json::json!([
                    {"op":"set","path":["baseURL"],"value":"http://100.105.152.101:18080/v1"},
                    {"op":"set","path":["models"],"value":[{"id":"deepseek-v4-flash-0731-ext","name":"V4"}]},
                ]),
                None,
            )
            .expect("llm-deepseek mutate accepted");
        assert_eq!(deepseek.value["baseURL"], serde_json::json!("http://100.105.152.101:18080/v1"));
        assert_eq!(deepseek.value["models"][0]["id"], serde_json::json!("deepseek-v4-flash-0731-ext"));

        // llm-pi-ai：dict 写 {providers:{openai:{...}}}（settingsPath=['providers',route]）。
        let pi = sp
            .mutate(
                "llm-pi-ai",
                &serde_json::json!([
                    {"op":"set","path":["providers","openai"],"value":{
                        "displayName":"OpenAI","api":"openai-completions",
                        "baseURL":"https://api.openai.com/v1","apiKeyEnv":"OPENAI_API_KEY"}},
                ]),
                None,
            )
            .expect("llm-pi-ai mutate accepted");
        assert_eq!(pi.value["providers"]["openai"]["api"], serde_json::json!("openai-completions"));
        assert_eq!(pi.value["providers"]["openai"]["apiKeyEnv"], serde_json::json!("OPENAI_API_KEY"));

        // agent-default-model：selectModel 持久化路径 {provider, model, reasoningEffort?}。
        let adm = sp
            .replace(
                "agent-default-model",
                &serde_json::json!({"provider":"deepseek-official","model":"deepseek-v4-flash-0731-ext"}),
                None,
            )
            .expect("agent-default-model replace accepted");
        assert_eq!(adm.value["provider"], serde_json::json!("deepseek-official"));
        assert_eq!(adm.value["model"], serde_json::json!("deepseek-v4-flash-0731-ext"));
    }
}
