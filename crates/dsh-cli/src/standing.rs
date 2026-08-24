//! P2-b/P3-a：standing 注册表——每个 preset 一个 standing scope，贡献挂在共享
//! dsh-scope / dsh-system-prompt / dsh-tools 注册面内；agent 经 **scope 父链** join；
//! 守卫报告行审计（bridged/disabled/guarded）。
//!
//! 路径 B 组合权威（D-103/A-01/P2/P3-a）：
//! - 行审计 = `dsh-agent-presets::parse`（typed 行 + `disabled_expr` × process 门面）；
//! - 内容桥 = `dsh-system-prompt` scoped section：`@deepseek-ai/dsh-persona`
//!   （complete/persona + `includeRuntimeContext:false` 抑制）与
//!   `@deepseek-ai/dsh-agent-instructions`（`<cwd>/AGENTS.md`，maxBytes cap）——
//!   joined agent 的 `assemble(scope)` 经 `scope_chain_of` 看到它们；
//! - 工具行桥 = `dsh-tools` **scoped register**：宿主全局已有工具按行 config 重呈现
//!   （description/timeoutMs）注册进 standing scope——joined agent 的 `schemas(scope)`
//!   走链即见（未 join 不可见），这正是组合的 presentAs 机制；
//! - D-103 win32-B：Rust 无 pwsh 执行器（A 并行未落地）→ **win32 上 bash 系强制可用、
//!   pwsh 系判禁**（取代忠实门控的「bash 禁用/pwsh 可用」），对照 A 落地回滚；
//! - join = `dsh-scope::bind_scope_parent`（换 preset 用绑定 `.rebind`）。
//!
//! **诚实边界**：P3-a 已桥 persona + agent-instructions + 单工具行（bash /
//! str_replace_editor）；多工具行（fs-local/terminal）与 pwsh 执行器留 P3-b/A。
//! standing 是注册面里的一个作用域子树；真实 isolate 服务隔离是 C 段收敛目标。

use std::collections::HashMap;
use std::rc::Rc;

use dsh_agent_presets::parse::{row_disabled, CompositionRow};
use dsh_scope::{bind_scope_parent, store::Undo, ScopeKey, ScopeParentBinding};
use dsh_system_prompt::{PromptSection, PromptSectionText, SystemPrompt};
use dsh_tools::ToolRegistry;
use serde_json::Value;

/// persona 行名（P2-b 桥）。
pub const PERSONA_ROW: &str = "@deepseek-ai/dsh-persona";
/// agent-instructions 行名（P3-a 内容桥）。
pub const INSTRUCTIONS_ROW: &str = "@deepseek-ai/dsh-agent-instructions";

/// 挂载行审计报告（守卫面；诚实）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StandingReport {
    pub preset: String,
    /// 已桥并贡献的行。
    pub bridged: Vec<String>,
    /// `disabled_expr` 判禁的行（平台门控挡掉）。
    pub disabled: Vec<String>,
    /// 未桥叶子行（name, 原因）——P3/P5 缩小；组容器自身不列。
    pub guarded: Vec<(String, String)>,
}

/// 一个 standing 挂载。
pub struct Standing {
    pub id: String,
    /// standing scope：贡献（persona section 等）挂这里；agent join 时成为其父。
    pub scope: ScopeKey,
    pub report: StandingReport,
    undos: Vec<Undo>,
}

/// standing 注册表（per boot Runtime；调用方以 `Rc<RefCell<>>` 共享，web 单线程
/// accept，无锁纪律）。
pub struct StandingRegistry {
    /// 共享的 SystemPrompt 注册面（**同一实例** —— join 后 agent 的 assemble 走它）。
    system_prompt: Rc<SystemPrompt>,
    /// 共享的 ToolRegistry（P3-a 工具行桥用；None = 该 host 未装配工具注册面 →
    /// 工具行一律 guarded，诚实）。
    tools: Option<Rc<ToolRegistry>>,
    standings: HashMap<String, Standing>,
}

impl Default for StandingRegistry {
    /// 占位注册表（Bootstrap 期/无真实 loop 时）：持有独立占位 SystemPrompt、无
    /// 工具注册面。web serve 装配 agent-loop 后以 `host.prompt` + `host.tools` 重建
    /// （见 web.rs `boot.standings`），保证 standing 贡献落进 loop 实际组装的注册面。
    fn default() -> Self {
        let placeholder = Rc::new(
            SystemPrompt::new(&dsh_system_prompt::Config::default(), Rc::new(|| {}))
                .expect("standing placeholder system prompt"),
        );
        StandingRegistry::new(placeholder, None)
    }
}

impl StandingRegistry {
    pub fn new(system_prompt: Rc<SystemPrompt>, tools: Option<Rc<ToolRegistry>>) -> Self {
        StandingRegistry {
            system_prompt,
            tools,
            standings: HashMap::new(),
        }
    }
    /// 挂载 preset：行审计 + 铸 standing scope + 桥贡献（persona / instructions /
    /// 单工具行重呈现）。同 id 换代：先 unmount（撤销 scoped 贡献）再建新。
    pub fn mount(
        &mut self,
        id: &str,
        rows: &[CompositionRow],
        process: &Value,
    ) -> Result<(), String> {
        if self.standings.contains_key(id) {
            self.unmount(id);
        }
        let scope = ScopeKey::new();
        let mut report = StandingReport {
            preset: id.to_string(),
            ..Default::default()
        };
        let mut undos: Vec<Undo> = Vec::new();

        // 行审计：走 tree（组递归；组容器自身不列），叶子 → disabled/活化。
        // D-103 win32-B：禁用判定先过平台策略（bash 系 win32 强制可用、pwsh 系
        // win32 判禁——Rust 暂无 pwsh 执行器，见 A 并行），再回落忠实 `row_disabled`。
        fn walk<'a>(
            rows: &'a [CompositionRow],
            process: &Value,
            report: &mut StandingReport,
        ) -> Vec<&'a CompositionRow> {
            let mut leaves = Vec::new();
            for row in rows {
                if row.group {
                    leaves.extend(walk(&row.children, process, report));
                    continue;
                }
                if row_disabled_for_platform(row, process) {
                    report.disabled.push(row.name.clone());
                    continue;
                }
                leaves.push(row);
            }
            leaves
        }
        let leaves = walk(rows, process, &mut report);

        // —— 内容桥：persona 行 → standing scope section + runtime-context 抑制。
        for (i, row) in leaves.iter().filter(|r| r.name == PERSONA_ROW).enumerate() {
            let cfg = row.config.clone().unwrap_or(Value::Null);
            let text = cfg
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let complete = cfg
                .get("complete")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let include_rt = cfg
                .get("includeRuntimeContext")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if !include_rt {
                undos.push(self.system_prompt.suppress_runtime_context(Some(&scope)));
            }
            let section = PromptSection {
                name: format!("preset:{id}:persona:{i}"),
                order: 0.0,
                text: PromptSectionText::Static(text),
                complete,
            };
            let undo = self
                .system_prompt
                .section(Some(&scope), &section)
                .map_err(|e| format!("preset {id}: persona section: {e}"))?;
            undos.push(undo);
            report
                .bridged
                .push(format!("@deepseek-ai/dsh-persona (complete={complete})"));
        }

        // —— 内容桥：agent-instructions 行 → `<facade.cwd>/AGENTS.md` scoped section
        //    （maxBytes cap；文件缺失 = 桥解析但因无文件不贡献，仍诚实标 bridged）。
        for row in leaves.iter().filter(|r| r.name == INSTRUCTIONS_ROW) {
            let cfg = row.config.clone().unwrap_or(Value::Null);
            let max_bytes = cfg.get("maxBytes").and_then(Value::as_u64).unwrap_or(65536) as usize;
            let cwd = process.get("cwd").and_then(Value::as_str).unwrap_or("");
            let path = std::path::Path::new(cwd).join("AGENTS.md");
            let marker = match std::fs::read(&path) {
                Ok(raw) => {
                    let capped = &raw[..raw.len().min(max_bytes)];
                    let text = String::from_utf8_lossy(capped).into_owned();
                    let section = PromptSection {
                        name: format!("preset:{id}:agent-instructions"),
                        order: 40.0,
                        text: PromptSectionText::Static(text),
                        complete: false,
                    };
                    let undo = self
                        .system_prompt
                        .section(Some(&scope), &section)
                        .map_err(|e| format!("preset {id}: instructions section: {e}"))?;
                    undos.push(undo);
                    "AGENTS.md"
                }
                Err(_) => "no AGENTS.md",
            };
            report
                .bridged
                .push(format!("@deepseek-ai/dsh-agent-instructions ({marker})"));
        }

        // —— 工具行桥：单工具行按行 config 重呈现（description/timeoutMs）注册进
        //    standing scope（joined agent 的 schemas 走链即见）；**多工具组行**解析
        //    宿主工具集——单工作区宿主下 standing 链本就从全局基继承工具，故组行为
        //    「解析确认」而非逐工具重呈现（诚实标注 single-workspace）。
        for row in leaves
            .iter()
            .filter(|r| r.name != PERSONA_ROW && r.name != INSTRUCTIONS_ROW)
        {
            let Some(home) = self.tools.as_ref() else {
                report.guarded.push((
                    row.name.clone(),
                    "no shared tool registry in this host".to_string(),
                ));
                continue;
            };
            // 多工具组行解析（fs-local / terminal）。
            if let Some(group) = host_tool_group_for_row(row) {
                let missing: Vec<&str> = group
                    .iter()
                    .copied()
                    .filter(|t| home.get(t, None).is_none())
                    .collect();
                if missing.is_empty() {
                    report.bridged.push(format!(
                        "{} (host toolset: {}; chain-visible, single-workspace)",
                        row.name,
                        group.join("/")
                    ));
                    continue;
                }
                report.guarded.push((
                    row.name.clone(),
                    format!(
                        "host tool group missing {} of {} ({} missing: {})",
                        missing.len(),
                        group.len(),
                        row.name,
                        missing.join(", ")
                    ),
                ));
                continue;
            }
            // terminal 后端行（dsh-terminal-bash/-pwsh）：由宿主默认 shell 充当
            // （win32 = Git Bash，满足 win32-B）。组行已解析 → 标 bridged 后端。
            if row.name.contains("dsh-terminal") {
                let terminal_resolved = leaves.iter().any(|r| {
                    r.name != row.name
                        && host_tool_group_for_row(r).is_some()
                        && home.get("terminal_open", None).is_some()
                });
                if terminal_resolved {
                    report.bridged.push(format!(
                        "{} (terminal backend; host default shell)",
                        row.name
                    ));
                } else {
                    report.guarded.push((
                        row.name.clone(),
                        "terminal backend without a resolved terminal group (P3-b)".to_string(),
                    ));
                }
                continue;
            }
            match host_tool_for_row(row) {
                Some(tool) => match home.get(tool, None) {
                    Some(base) => {
                        let mut def = (*base).clone();
                        let cfg = row.config.clone().unwrap_or(Value::Null);
                        if let Some(desc) = cfg.get("description").and_then(Value::as_str) {
                            def.description = desc.to_string();
                        }
                        if let Some(t) = cfg.get("timeoutMs").and_then(Value::as_f64) {
                            def.timeout_ms = Some(t);
                        }
                        let undo = home
                            .register(Rc::new(def), Some(&scope))
                            .map_err(|e| format!("preset {id}: tool {}: {e}", row.name))?;
                        undos.push(undo);
                        report.bridged.push(format!("{} (tool {tool})", row.name));
                    }
                    None => report.guarded.push((
                        row.name.clone(),
                        format!("no host tool \"{tool}\" in the shared registry"),
                    )),
                },
                None => report
                    .guarded
                    .push((row.name.clone(), tool_guard_reason(row))),
            }
        }

        self.standings.insert(
            id.to_string(),
            Standing {
                id: id.to_string(),
                scope,
                report,
                undos,
            },
        );
        Ok(())
    }

    /// join：把 agent 的 scope 链到 standing scope。绑定句柄由调用方持有；
    /// 换 preset 用绑定 `.rebind(standing_scope)`（`scope_of` 可取到）。
    pub fn join(
        &self,
        preset_id: &str,
        agent_scope: &ScopeKey,
    ) -> Result<ScopeParentBinding, String> {
        let standing = self
            .standings
            .get(preset_id)
            .ok_or_else(|| format!("dsh-cli standing: no preset {preset_id} mounted"))?;
        bind_scope_parent(agent_scope.clone(), standing.scope.clone())
    }

    /// standing 的 scope key（join/rebind 用）。
    pub fn scope_of(&self, preset_id: &str) -> Option<&ScopeKey> {
        self.standings.get(preset_id).map(|s| &s.scope)
    }

    pub fn report(&self, preset_id: &str) -> Option<&StandingReport> {
        self.standings.get(preset_id).map(|s| &s.report)
    }

    /// 换代/销毁：撤销 scoped 贡献（undo 精确幂等）；scope 随注册表释放。
    pub fn unmount(&mut self, id: &str) {
        if let Some(standing) = self.standings.remove(id) {
            for undo in standing.undos {
                undo();
            }
        }
    }

    pub fn len(&self) -> usize {
        self.standings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.standings.is_empty()
    }
}

/// D-103 win32-B 平台策略：bash 系行在 win32 **强制可用**（Rust 经 Git Bash 可跑
/// bash），pwsh 系行在 win32 **判禁**（无 pwsh 执行器，A 并行落地后移除此覆盖）。
/// 非 win32 回落忠实 `disabled_expr` 求值。
fn row_disabled_for_platform(row: &CompositionRow, process: &Value) -> bool {
    let platform = process.get("platform").and_then(Value::as_str);
    match platform {
        Some("win32") => {
            if is_bash_family(row) {
                return false;
            }
            if is_pwsh_family(row) {
                return true;
            }
            row_disabled(row, process)
        }
        _ => row_disabled(row, process),
    }
}

/// bash 系：`dsh-tool-bash(-persistent)`；或 `dsh-terminal-bash` 的 **bash 方言**
/// 变体（`shellDialect` 非 pwsh、或未标注 = 默认 bash）。
fn is_bash_family(row: &CompositionRow) -> bool {
    let n = row.name.as_str();
    let dialect = row
        .config
        .as_ref()
        .and_then(|c| c.get("shellDialect"))
        .and_then(Value::as_str);
    n.contains("dsh-tool-bash") || (n.contains("dsh-terminal-bash") && dialect != Some("pwsh"))
}

/// pwsh 系：`dsh-tool-pwsh(-persistent)`；或 `dsh-terminal-bash` 的 pwsh 方言变体。
fn is_pwsh_family(row: &CompositionRow) -> bool {
    let n = row.name.as_str();
    let dialect = row
        .config
        .as_ref()
        .and_then(|c| c.get("shellDialect"))
        .and_then(Value::as_str);
    n.contains("dsh-tool-pwsh") || (n.contains("dsh-terminal-bash") && dialect == Some("pwsh"))
}

/// 单工具行 → 宿主工具名（P3-a 桥表）。多工具行（fs-local/terminal/…）见
/// `host_tool_group_for_row`。
fn host_tool_for_row(row: &CompositionRow) -> Option<&'static str> {
    match row.name.as_str() {
        "@deepseek-ai/dsh-tool-bash" => Some("bash"),
        "@deepseek-ai/dsh-tool-bash-persistent" => Some("bash"),
        "@deepseek-ai/dsh-tool-str-replace-editor" => Some("str_replace_editor"),
        _ => None,
    }
}

/// M5h 宿主注册的终端工具组（web_m5）。
const TERMINAL_TOOLS: &[&str] = &[
    "terminal_open",
    "terminal_send",
    "terminal_read",
    "terminal_signal",
    "terminal_close",
    "terminal_list",
];

/// 多工具组行 → 宿主工具集（P3-b 解析确认；单工作区宿主 = standing 链继承全局基）。
fn host_tool_group_for_row(row: &CompositionRow) -> Option<&'static [&'static str]> {
    match row.name.as_str() {
        "@deepseek-ai/dsh-fs-local" => {
            Some(&["read", "write", "edit", "read_image", "glob", "grep"])
        }
        "@deepseek-ai/dsh-terminal" => Some(TERMINAL_TOOLS),
        _ => None,
    }
}

/// 未桥工具行的守卫原因（D-103 broken 集显式标记，不伪装）。fs-local/terminal
/// 组与后端行在桥内已被前置解析，不落此函数。
fn tool_guard_reason(row: &CompositionRow) -> String {
    let n = row.name.as_str();
    if is_pwsh_family(row) || n.contains("pwsh") {
        "no host pwsh executor (D-103 A-parallel lands in P3)".to_string()
    } else if n.contains("tool-cordis") || n.contains("command-compact") || n.contains("web") {
        "broken per D-103 (unbridged surface)".to_string()
    } else {
        "no Rust bridge yet (P3/P5)".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_scope::scope_chain_of;
    use dsh_system_prompt::{AssembleContext, Config};
    use std::path::{Path, PathBuf};

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .canonicalize()
            .unwrap()
    }

    fn win32_facade() -> Value {
        serde_json::json!({
            "platform": "win32",
            "env": {},
            "cwd": "C:\\repo",
        })
    }

    fn preset_rows(id: &str) -> Vec<CompositionRow> {
        let text = std::fs::read_to_string(
            repo_root()
                .join("resources")
                .join("agent-presets")
                .join(id)
                .join("agent.cordis.yml"),
        )
        .unwrap();
        dsh_agent_presets::parse::parse_composition(&text).unwrap()
    }

    /// 测试用最小工具定义（object-rooted schema，register 的 schema 断言可过）。
    fn tool_def(name: &str, description: &str, timeout: f64) -> Rc<dsh_tools::ToolDefinition> {
        Rc::new(dsh_tools::ToolDefinition {
            name: name.to_string(),
            description: description.to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            output: dsh_tools::ToolOutputDefinition {
                schema: dsh_tools::json_schema::JsonSchemaNode {
                    r#type: Some(dsh_tools::json_schema::JsonSchemaType::Object),
                    ..Default::default()
                },
                render: Rc::new(|_: &Value, _: &Value| Vec::new()),
                presentation_meta: None,
            },
            timeout_ms: Some(timeout),
            execute: Rc::new(|_: &Value, _: &dsh_tools::ToolRunContext| {
                Err(dsh_tools::ToolFailureData::new("stub", "stub", "stub"))
            }),
            finalize_content: None,
            is_concurrency_safe: None,
            present_call: None,
            present_result: None,
        })
    }

    fn new_sp() -> Rc<SystemPrompt> {
        Rc::new(
            SystemPrompt::new(
                &Config {
                    include_harness_identity: false,
                    include_runtime_context: true,
                    persona: String::new(),
                    tool_order: None,
                },
                Rc::new(|| {}),
            )
            .unwrap(),
        )
    }

    fn sect_texts(sp: &SystemPrompt, scope: &ScopeKey) -> Vec<String> {
        sp.assemble(&AssembleContext {
            scope: Some(scope.clone()),
        })
        .unwrap()
        .sections
        .into_iter()
        .map(|s| s.text)
        .collect()
    }

    /// 两 standing 隔离 + join 可见性：X∝minimal 只见最小 persona，Y∝standard 只见
    /// 标准 persona，未 join 的 Z 两者皆不可见。
    #[test]
    fn two_standings_isolated_and_join_visible() {
        let sp = new_sp();
        let mut reg = StandingRegistry::new(sp.clone(), None);
        let proc = win32_facade();
        reg.mount("minimal", &preset_rows("minimal"), &proc)
            .unwrap();
        reg.mount("standard", &preset_rows("standard"), &proc)
            .unwrap();

        let x = ScopeKey::new();
        let y = ScopeKey::new();
        let z = ScopeKey::new();
        reg.join("minimal", &x).unwrap();
        reg.join("standard", &y).unwrap();
        // z 不 join。

        let x_texts = sect_texts(&sp, &x);
        assert!(
            x_texts
                .iter()
                .any(|t| t.contains("helpful software engineer")),
            "minimal composite visible to joined X: {x_texts:?}"
        );
        assert!(
            !x_texts
                .iter()
                .any(|t| t.contains("coding agent powered by")),
            "standard composite hidden from X: {x_texts:?}"
        );
        let y_texts = sect_texts(&sp, &y);
        assert!(
            y_texts
                .iter()
                .any(|t| t.contains("coding agent powered by")),
            "standard composite visible to joined Y: {y_texts:?}"
        );
        assert!(
            !y_texts
                .iter()
                .any(|t| t.contains("helpful software engineer")),
            "minimal composite hidden from Y: {y_texts:?}"
        );
        let z_texts = sect_texts(&sp, &z);
        assert!(
            !z_texts
                .iter()
                .any(|t| t.contains("software engineer") || t.contains("coding agent powered")),
            "unjoined Z sees neither standing: {z_texts:?}"
        );
        // 父链：X 的链含 minimal standing scope、不含 standard 的。
        let x_chain = scope_chain_of(Some(&x));
        assert!(x_chain.contains(reg.scope_of("minimal").unwrap()));
        assert!(!x_chain.contains(reg.scope_of("standard").unwrap()));
    }

    /// 守卫报告：minimal@win32 —— persona bridged、win32 门控行 disabled、
    /// 其余叶行 guarded。
    #[test]
    fn guard_report_splits_bridged_disabled_guarded() {
        let sp = new_sp();
        let mut reg = StandingRegistry::new(sp.clone(), None);
        reg.mount("minimal", &preset_rows("minimal"), &win32_facade())
            .unwrap();
        let r = reg.report("minimal").unwrap();
        assert!(
            r.bridged
                .iter()
                .any(|s| s.starts_with("@deepseek-ai/dsh-persona")),
            "bridged: {:?}",
            r.bridged
        );
        // D-103 win32-B：win32 上 bash 系**保持可用**（Rust 可跑 bash），pwsh 系判禁
        // （无 pwsh 执行器）。
        assert!(
            !r.disabled
                .iter()
                .any(|s| s == "@deepseek-ai/dsh-tool-bash-persistent"),
            "win32-B keeps bash active: {:?}",
            r.disabled
        );
        assert!(
            r.disabled
                .iter()
                .any(|s| s == "@deepseek-ai/dsh-tool-pwsh-persistent"),
            "win32-B blocks pwsh: {:?}",
            r.disabled
        );
        // 无工具注册面（boot_with_sessions 占位）→ 工具行 guarded（诚实）。
        assert!(
            r.guarded
                .iter()
                .any(|(n, _)| n == "@deepseek-ai/dsh-fs-local"),
            "unbridged rows guarded: {:?}",
            r.guarded
        );
        assert!(
            r.guarded
                .iter()
                .any(|(n, _)| n == "@deepseek-ai/dsh-tool-str-replace-editor"),
            "unbridged rows guarded: {:?}",
            r.guarded
        );
    }

    /// Linux 对照：忠实门控（win32-B 只在 win32 生效）——bash 可用、pwsh 禁用。
    #[test]
    fn guard_report_linux_uses_faithful_gates() {
        let sp = new_sp();
        let mut reg = StandingRegistry::new(sp.clone(), None);
        let proc = serde_json::from_str(r#"{"platform":"linux","env":{},"cwd":"/repo"}"#).unwrap();
        reg.mount("minimal", &preset_rows("minimal"), &proc)
            .unwrap();
        let r = reg.report("minimal").unwrap();
        assert!(
            !r.disabled
                .iter()
                .any(|s| s == "@deepseek-ai/dsh-tool-bash-persistent"),
            "linux: bash faithful-active = not disabled: {:?}",
            r.disabled
        );
        assert!(
            r.disabled
                .iter()
                .any(|s| s == "@deepseek-ai/dsh-tool-pwsh-persistent"),
            "linux: pwsh disabled: {:?}",
            r.disabled
        );
    }

    /// 换 preset = 原有绑定 rebind 到另一 standing scope；agent 视图随之切换。
    #[test]
    fn rejoin_rebinds_parent_and_switches_view() {
        let sp = new_sp();
        let mut reg = StandingRegistry::new(sp.clone(), None);
        let proc = win32_facade();
        reg.mount("minimal", &preset_rows("minimal"), &proc)
            .unwrap();
        reg.mount("standard", &preset_rows("standard"), &proc)
            .unwrap();
        let x = ScopeKey::new();
        let binding = reg.join("minimal", &x).unwrap();
        assert!(sect_texts(&sp, &x)
            .iter()
            .any(|t| t.contains("helpful software engineer")));
        // 换 preset：rebind 到 standard standing scope。
        binding
            .rebind(reg.scope_of("standard").unwrap().clone())
            .unwrap();
        assert!(sect_texts(&sp, &x)
            .iter()
            .any(|t| t.contains("coding agent powered")));
        assert!(
            !sect_texts(&sp, &x)
                .iter()
                .any(|t| t.contains("helpful software engineer")),
            "view switched away from minimal"
        );
    }

    /// 换代：re-mount 同 id 撤销旧贡献（undo 幂等），不残留旧 section。
    #[test]
    fn remount_same_id_replaces_cleanly() {
        let sp = new_sp();
        let mut reg = StandingRegistry::new(sp.clone(), None);
        let proc = win32_facade();
        let rows = preset_rows("minimal");
        reg.mount("minimal", &rows, &proc).unwrap();
        let first_scope = reg.scope_of("minimal").unwrap().clone();
        let x = ScopeKey::new();
        reg.join("minimal", &x).unwrap();
        // remount（换代）：旧 standing 撤销、新 scope 独立。
        reg.mount("minimal", &rows, &proc).unwrap();
        assert_ne!(
            reg.scope_of("minimal").unwrap(),
            &first_scope,
            "new standing scope"
        );
        // 旧绑定指向已撤销的旧 scope；链仍含旧 scope，但已无贡献（undo 已跑）——
        // 这恰是换代语义：join 须在换代后用新 scope（P4 同步 select 后重绑）。
        let x_texts = sect_texts(&sp, &x);
        assert!(
            !x_texts
                .iter()
                .any(|t| t.contains("helpful software engineer")),
            "old standing contribution undone after remount: {x_texts:?}"
        );
    }

    /// —— P3-a：工具行桥 —— 宿主全局工具按行 config 重呈现注册进 standing scope：**
    /// joined** agent 的 schemas/get 见覆盖后 presentation（description/timeoutMs）；
    /// **未 join** agent 仍是全局原值（组合呈现隔离）。
    #[test]
    fn tool_rows_re_present_into_standing_scope_for_joined_only() {
        let sp = new_sp();
        let tools = Rc::new(ToolRegistry::new(dsh_tools::ToolExecutionMode::Native));
        tools
            .register_global(tool_def("bash", "GLOBAL-BASH-DESC", 111.0))
            .unwrap();
        tools
            .register_global(tool_def("str_replace_editor", "GLOBAL-EDIT-DESC", 5000.0))
            .unwrap();
        let mut reg = StandingRegistry::new(sp.clone(), Some(tools.clone()));
        let comp = "
- id: sh
  name: '@deepseek-ai/dsh-tool-bash'
  config:
    description: |-
      ROW-BASH-LONG-DESC
    timeoutMs: 222
- id: ed
  name: '@deepseek-ai/dsh-tool-str-replace-editor'
  config:
    maxOutputChars: 16000
";
        let proc = win32_facade();
        reg.mount(
            "p1",
            &dsh_agent_presets::parse::parse_composition(comp).unwrap(),
            &proc,
        )
        .unwrap();
        let r = reg.report("p1").unwrap();
        assert!(
            r.bridged
                .iter()
                .any(|s| s.starts_with("@deepseek-ai/dsh-tool-bash ")),
            "bash row bridged: {:?}",
            r.bridged
        );
        assert!(
            r.bridged
                .iter()
                .any(|s| s.starts_with("@deepseek-ai/dsh-tool-str-replace-editor ")),
            "editor row bridged: {:?}",
            r.bridged
        );
        // joined agent 见重呈现。
        let joined = ScopeKey::new();
        reg.join("p1", &joined).unwrap();
        let bash_def = tools.get("bash", Some(&joined)).unwrap();
        assert_eq!(bash_def.description.trim_end(), "ROW-BASH-LONG-DESC");
        assert_eq!(bash_def.timeout_ms, Some(222.0));
        // 未 join agent scope：全局原值（无 parent → 不走 standing）。
        let alone = ScopeKey::new();
        let alone_def = tools.get("bash", Some(&alone)).unwrap();
        assert_eq!(alone_def.description, "GLOBAL-BASH-DESC");
        assert_eq!(alone_def.timeout_ms, Some(111.0));
    }

    /// 工具行守卫原因：无宿主工具 / pwsh 执行器 / fs 多工具展开 / D-103 broken 集。
    #[test]
    fn unbridged_tool_rows_guarded_with_specific_reasons() {
        let sp = new_sp();
        let tools = Rc::new(ToolRegistry::new(dsh_tools::ToolExecutionMode::Native));
        // 只注册 str_replace_editor（无 bash）。
        tools
            .register_global(tool_def("str_replace_editor", "EDIT", 1.0))
            .unwrap();
        let mut reg = StandingRegistry::new(sp.clone(), Some(tools.clone()));
        let comp = "
- id: sh
  name: '@deepseek-ai/dsh-tool-bash'
- id: psh
  name: '@deepseek-ai/dsh-tool-pwsh-persistent'
- id: fs
  name: '@deepseek-ai/dsh-fs-local'
- id: web
  name: '@deepseek-ai/dsh-tool-web-fetch'
";
        // 无门控行 + linux 门面（win32-B 会让 pwsh 进 disabled，故用 linux 让所有
        // 无门控行都活化 → 走守卫）。
        let proc: Value =
            serde_json::from_str(r#"{"platform":"linux","env":{},"cwd":"/repo"}"#).unwrap();
        reg.mount(
            "p2",
            &dsh_agent_presets::parse::parse_composition(comp).unwrap(),
            &proc,
        )
        .unwrap();
        let r = reg.report("p2").unwrap();
        let reason_of = |n: &str| {
            r.guarded
                .iter()
                .find(|(name, _)| name == n)
                .map(|(_, why)| why.as_str())
                .unwrap_or("<missing>")
        };
        assert!(
            reason_of("@deepseek-ai/dsh-tool-bash").contains("no host tool \"bash\""),
            "missing host tool reason"
        );
        assert!(
            reason_of("@deepseek-ai/dsh-tool-pwsh-persistent").contains("pwsh"),
            "pwsh A-parallel reason"
        );
        assert!(
            reason_of("@deepseek-ai/dsh-fs-local").contains("host tool group missing"),
            "fs group missing-host-tools reason"
        );
        assert!(
            reason_of("@deepseek-ai/dsh-tool-web-fetch").contains("broken per D-103"),
            "web broken reason"
        );
    }

    /// —— P3-b —— fs-local / terminal **组行**解析宿主工具集（单工作区宿主：standing
    /// 链继承全局基）；minimal 在 win32-B + 全工具集下 fs/terminal/bash/editor 全
    /// bridged、pwsh 系 disabled，joined agent 模型面见整组工具。
    #[test]
    fn fs_and_terminal_groups_resolve_when_host_toolset_present() {
        let sp = new_sp();
        let tools = Rc::new(ToolRegistry::new(dsh_tools::ToolExecutionMode::Native));
        for name in [
            "read",
            "write",
            "edit",
            "read_image",
            "glob",
            "grep",
            "terminal_open",
            "terminal_send",
            "terminal_read",
            "terminal_signal",
            "terminal_close",
            "terminal_list",
            "bash",
            "str_replace_editor",
        ] {
            tools.register_global(tool_def(name, name, 1000.0)).unwrap();
        }
        let mut reg = StandingRegistry::new(sp.clone(), Some(tools.clone()));
        reg.mount("minimal", &preset_rows("minimal"), &win32_facade())
            .unwrap();
        let r = reg.report("minimal").unwrap();
        let bridged = |s: &str| r.bridged.iter().any(|b| b.starts_with(s));
        assert!(
            bridged("@deepseek-ai/dsh-fs-local"),
            "fs group bridged: {:?}",
            r.bridged
        );
        assert!(
            bridged("@deepseek-ai/dsh-terminal"),
            "terminal group bridged: {:?}",
            r.bridged
        );
        assert!(
            bridged("@deepseek-ai/dsh-tool-bash-persistent"),
            "bash single-tool bridged"
        );
        assert!(
            bridged("@deepseek-ai/dsh-tool-str-replace-editor"),
            "editor bridged"
        );
        // win32-B：pwsh 系 → disabled（不进 guarded、不 bridged）。
        assert!(
            r.disabled
                .iter()
                .any(|s| s == "@deepseek-ai/dsh-tool-pwsh-persistent"),
            "pwsh-persistent disabled"
        );
        assert!(
            !r.bridged
                .iter()
                .any(|b| b.starts_with("@deepseek-ai/dsh-tool-pwsh-persistent")),
            "pwsh not bridged"
        );
        // joined agent 模型面整组工具可见（全局基 + standing 链）。
        let joined = ScopeKey::new();
        reg.join("minimal", &joined).unwrap();
        let names = tools.known_names(Some(&joined));
        for t in [
            "read",
            "write",
            "edit",
            "glob",
            "grep",
            "terminal_open",
            "terminal_send",
            "bash",
            "str_replace_editor",
        ] {
            assert!(names.iter().any(|n| n == t), "joined model sees {t}");
        }
    }

    /// —— P3-a：agent-instructions 内容桥 —— `<cwd>/AGENTS.md` → joined 视图 scoped
    /// section；maxBytes cap；缺失 = 不贡献但仍标 bridged（桥已解析）。
    #[test]
    fn agent_instructions_content_bridge_reads_agents_md() {
        let sp = new_sp();
        let mut reg = StandingRegistry::new(sp.clone(), None);
        let cwd = std::env::temp_dir().join(format!("dsh-standing-agents-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cwd);
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(cwd.join("AGENTS.md"), "AGENTS-MARKER-0123456789").unwrap();
        let proc = serde_json::json!({
            "platform": "linux",
            "env": {},
            "cwd": cwd.to_string_lossy().into_owned(),
        });
        let comp = "
- id: ins
  name: '@deepseek-ai/dsh-agent-instructions'
  config:
    maxBytes: 12
";
        let rows = dsh_agent_presets::parse::parse_composition(comp).unwrap();
        reg.mount("p3", &rows, &proc).unwrap();
        let r = reg.report("p3").unwrap();
        assert!(
            r.bridged
                .iter()
                .any(|s| s == "@deepseek-ai/dsh-agent-instructions (AGENTS.md)"),
            "instructions bridged: {:?}",
            r.bridged
        );
        let joined = ScopeKey::new();
        reg.join("p3", &joined).unwrap();
        // maxBytes=12，AGENTS.md 内容 22 字节 → 只取前 12 字节（片段仍含 "AGENTS-M"）。
        let texts = sect_texts(&sp, &joined);
        let marker = texts
            .iter()
            .find(|t| t.contains("AGENTS-M"))
            .expect("instructions section visible to joined agent");
        assert_eq!(marker.len(), 12, "capped at maxBytes");
        // 缺失文件：不贡献但仍 bridged（标 no AGENTS.md）。
        let cwd2 = std::env::temp_dir().join(format!("dsh-standing-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cwd2);
        std::fs::create_dir_all(&cwd2).unwrap();
        let proc2 = serde_json::json!({
            "platform": "linux", "env": {}, "cwd": cwd2.to_string_lossy().into_owned(),
        });
        reg.mount("p4", &rows, &proc2).unwrap();
        let r4 = reg.report("p4").unwrap();
        assert!(
            r4.bridged
                .iter()
                .any(|s| s == "@deepseek-ai/dsh-agent-instructions (no AGENTS.md)"),
            "absent file still bridged: {:?}",
            r4.bridged
        );
        let joined2 = ScopeKey::new();
        reg.join("p4", &joined2).unwrap();
        assert!(
            !sect_texts(&sp, &joined2)
                .iter()
                .any(|t| t.contains("AGENTS-MARKER")),
            "no instructions section without a file"
        );
        let _ = std::fs::remove_dir_all(&cwd);
        let _ = std::fs::remove_dir_all(&cwd2);
    }
}
