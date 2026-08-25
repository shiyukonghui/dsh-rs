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

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use dsh_agent_presets::parse::{row_disabled_with, CompositionRow};
use dsh_core::{Cordis, CordisError, EffectOutcome, FiberHandle, Plugin, ScopeId};
use dsh_scope::{bind_scope_parent, store::Undo, ScopeKey, ScopeParentBinding};
use dsh_system_prompt::{AssembleContext, PromptSection, PromptSectionText, SystemPrompt};
use dsh_tools::ToolRegistry;
use dsh_wasmrt::{ComboEvaluator, FallbackEval, NativeComboEvaluator, WasmComboEvaluator};
use serde_json::Value;

/// persona 行名（P2-b 桥）。
pub const PERSONA_ROW: &str = "@deepseek-ai/dsh-persona";
/// agent-instructions 行名（P3-a 内容桥）。
pub const INSTRUCTIONS_ROW: &str = "@deepseek-ai/dsh-agent-instructions";
/// skill-filesystem 行名（P3-c 最小只读目录桥）。
pub const SKILLS_ROW: &str = "@deepseek-ai/dsh-skill-filesystem";

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

impl StandingReport {
    /// K2/C：unusable-rows 挂载否决（对齐 harness `mount.ts` 的 `inactiveRows`）。
    /// 鉴别「桥依赖不可满足」的守卫行 = 挂载失败（fail-loud）；桥未实现 / 刻意
    /// broken 的诚实降级不计（D-103 guard 报告设计，报告而不否决）。调用方（web
    /// `agentPreset.select`）见非空即拒绝挂载且不留残留。
    pub fn unusable_rows(&self) -> Vec<(String, String)> {
        self.guarded
            .iter()
            .filter(|(_, why)| matches!(guard_kind(why), GuardKind::Stuck))
            .cloned()
            .collect()
    }
}

/// K2/C：守卫原因分类——「桥依赖不可满足」（挂载否决）vs「桥未实现 / 刻意 broken」
/// （诚实降级）。对齐 harness `inactiveRows`：只有行在等一个组合永远不满足的依赖
/// 才否决；未实现面（P3/P5 收窄）、D-103 broken 集、A-03 只读 skill 是刻意取舍，
/// 不算 unusable。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardKind {
    Stuck,
    Honest,
}

fn guard_kind(why: &str) -> GuardKind {
    if why.starts_with("no host tool \"")
        || why.starts_with("host tool group missing")
        || why == "terminal backend without a resolved terminal group (P3-b)"
        || why == "no shared tool registry in this host"
        || why.starts_with("no base dir")
    {
        GuardKind::Stuck
    } else {
        GuardKind::Honest
    }
}

/// K3/C：挂载记录插件——把「本 preset 已挂载」落为一个真实 dsh-core agent-scope
/// 子树（组合权威的生存期/泄漏审计本体；`Cordis` 挂载点随 standing 收薄）。
/// apply（record fiber 已在挂载作用域）：正常 → `isolate("preset.mount", scope)`
/// → 记录服务落 agent realm（`audit_subtree` 判定干净）；fault 注入
/// （`leakToRoot: true`，**仅测试**经 `set_fault_root_leak` 设定）→ 不 isolate →
/// 落 root realm → 审计判定泄漏（复刻 harness `leakedServices`，验证守卫正路与
/// 拒绝路径）。
struct PresetRecordPlugin(Value);

impl Plugin for PresetRecordPlugin {
    fn name(&self) -> &'static str {
        "dsh.preset.mount"
    }
    fn apply(&self, ctx: &Cordis, config: Value) -> Result<EffectOutcome, CordisError> {
        let scope = ctx.current_scope();
        let leak = config
            .get("leakToRoot")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !leak {
            ctx.isolate("preset.mount", scope)?;
        }
        ctx.provide("preset.mount", Arc::new(self.0.clone()))?;
        Ok(EffectOutcome::None)
    }
}

/// L1（D-105）/S3（D-107）：plan-mode 折叠 Fn——组装会话身份 `Some(sid)` 按该会话
/// 折叠（per-agent 保真），`None` 回退全局/上次 select 会话。
type PlanModeProbe = Rc<dyn Fn(Option<&str>) -> bool>;

/// L1（D-105）/S3（D-107）：plan-mode 折叠源句柄——`Rc<RefCell<Option<…>>>` 便于宿主在
/// standings 重建后注入/替换；Fn 段组装期经 `active(session_id)` 判定（单一权威态 =
/// 会话 `plan/mode` 事件折叠；**per-agent**：携带组装会话身份则按该会话折叠，`None`
/// 回退全局/上次 select 会话）。`None` = 永不注入（无 loop/未接）。
type PlanModeSource = Rc<RefCell<Option<PlanModeProbe>>>;

/// 一个 standing 挂载。
pub struct Standing {
    pub id: String,
    /// standing scope：贡献（persona section 等）挂这里；agent join 时成为其父。
    pub scope: ScopeKey,
    pub report: StandingReport,
    /// K3/C：dsh-core 侧该 standing 的 agent-scope 子树（`mount_scope` 铸造）——
    /// 组合权威的生存期本体：`unmount` 走 `unmount_scope` 整树卸载；挂载记录
    /// fiber（`record_fiber`）在该子树内 provide 记录服务，是 `audit_subtree`
    /// （leakedServices 守卫）的审计目标。
    pub core_scope: ScopeId,
    pub record_fiber: FiberHandle,
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
    /// K3/C：组合运行时（dsh-core）——每个挂载铸造真实 agent-scope 子树，生存期/
    /// 泄漏审计本体（组合权威归位 dsh-core 的收敛载体）。
    core: Cordis,
    /// K3/C 故障注入（root-realm 泄漏）：生产恒 false；仅测试
    /// （`set_fault_root_leak`，cf(g test)-only）置真以推泄漏拒绝路径。
    fault_root_leak: bool,
    /// K4/F-05：组合求值引擎（`disabled_expr` 门控）。生产默认 = **WASM 面为主、
    /// native 兜底**（`FallbackEval`）；测试/宿主可注入替身（`with_combo`）。
    combo: Rc<dyn ComboEvaluator>,
    /// combo 是否为 WASM 主面（`from_default_build` 成功即 wasm；否则 native-only）。
    combo_wasm: bool,
    /// L1/D-105：plan-mode 折叠源（单一权威态 = 会话 `plan/mode` 事件折叠；宿主注入）。
    /// Fn 段组装期经此判定 active → 注入 `dsh-plan-mode` 行 config.section。
    plan_mode_source: PlanModeSource,
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
        let (combo, combo_wasm) = match WasmComboEvaluator::from_default_build() {
            Ok(w) => (
                Rc::new(FallbackEval::new(
                    Rc::new(w),
                    Rc::new(NativeComboEvaluator),
                )) as Rc<dyn ComboEvaluator>,
                true,
            ),
            Err(_) => (Rc::new(NativeComboEvaluator) as Rc<dyn ComboEvaluator>, false),
        };
        StandingRegistry {
            system_prompt,
            tools,
            standings: HashMap::new(),
            core: Cordis::new(),
            fault_root_leak: false,
            combo,
            combo_wasm,
            plan_mode_source: Rc::new(RefCell::new(None)),
        }
    }

    /// L1/S3（D-107）：注入 plan-mode 折叠源（`None` = 永不注入）。经
    /// `Rc<RefCell<Option<_>>>` 承载——宿主在 standings 重建后注入亦可；Fn 段组装期
    /// 读取。**组装者会话身份**参数：`Some(sid)` = 按该会话折叠（per-agent 保真），
    /// `None` = 无身份组装（回退全局/上次 select 会话）。
    pub fn set_plan_mode_source(&self, source: Option<PlanModeProbe>) {
        *self.plan_mode_source.borrow_mut() = source;
    }

    /// 注入组合求值引擎（测试替身 / 宿主自选）。
    pub fn with_combo(
        system_prompt: Rc<SystemPrompt>,
        tools: Option<Rc<ToolRegistry>>,
        combo: Rc<dyn ComboEvaluator>,
        combo_wasm: bool,
    ) -> Self {
        StandingRegistry {
            system_prompt,
            tools,
            standings: HashMap::new(),
            core: Cordis::new(),
            fault_root_leak: false,
            combo,
            combo_wasm,
            plan_mode_source: Rc::new(RefCell::new(None)),
        }
    }

    /// 组合求值引擎是否 WASM 主面（诊断/测试）。
    pub fn combo_is_wasm(&self) -> bool {
        self.combo_wasm
    }
    /// 挂载 preset：行审计 + 铸 standing scope + 桥贡献（persona / instructions /
    /// 单工具行重呈现 / skill 目录清单）。无 base_dir（占位/测试口）。
    pub fn mount(
        &mut self,
        id: &str,
        rows: &[CompositionRow],
        process: &Value,
    ) -> Result<(), String> {
        self.mount_at(id, rows, None, process)
    }

    /// 挂载（带 base_dir）：skill 目录解析等需要「组合所在目录」的桥用此口。同 id
    /// 换代：先 unmount（撤销 scoped 贡献）再建新。
    pub fn mount_at(
        &mut self,
        id: &str,
        rows: &[CompositionRow],
        base_dir: Option<&std::path::Path>,
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
        // L1/D-105：plan-mode 折叠源（注册表注入，单一权威态 = 会话 `plan/mode` 事件
        // 折叠）。Fn 段每组装期判定 active → 注入 config.section、否则空串。
        let plan_mode_source = self.plan_mode_source.clone();

        // 行审计：走 tree（组递归；组容器自身不列），叶子 → disabled/活化。
        // 禁用判定 = `row_disabled_with`（fail-closed + truthy 权威在 dsh-agent-presets），
        // 求值引擎 = 注入的 `self.combo`（K4/F-05：WASM 面为主、native 兜底；
        // wasm 缺失自动回落 native-only）。
        let combo = self.combo.clone();
        let gate = move |row: &CompositionRow, process: &Value| {
            row_disabled_with(row, process, &|scope: &Value, expr: &str| combo.eval(scope, expr))
        };
        fn walk<'a>(
            rows: &'a [CompositionRow],
            process: &Value,
            report: &mut StandingReport,
            gate: &dyn Fn(&CompositionRow, &Value) -> bool,
        ) -> Vec<&'a CompositionRow> {
            let mut leaves = Vec::new();
            for row in rows {
                if row.group {
                    leaves.extend(walk(&row.children, process, report, gate));
                    continue;
                }
                if gate(row, process) {
                    report.disabled.push(row.name.clone());
                    continue;
                }
                leaves.push(row);
            }
            leaves
        }
        let leaves = walk(rows, process, &mut report, &gate);

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

        // —— 内容桥：skill 最小只读（D-103/A-03：复用 directives 装载）——
        //    `@deepseek-ai/dsh-skill-filesystem` 行解析 preset 自带 `skills/` 目录
        //    （baseUrl = 组合所在目录，composition 注释即此语义），扫 SKILL.md 子目录，
        //    落一个 scoped **目录段**（directives 风格：各 skill 名 + 摘要 + 绝对路径，
        //    模型用 fs read 工具打开 SKILL.md 即用——无写、无加载器工具，诚实）。
        for row in leaves.iter().filter(|r| r.name == SKILLS_ROW) {
            let Some(dir) = base_dir else {
                report.guarded.push((
                    row.name.clone(),
                    "no base dir — skill catalog cannot resolve (P3-c)".to_string(),
                ));
                continue;
            };
            let skills_dir = dir.join("skills");
            let mut catalog: Vec<(String, String)> = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&skills_dir) {
                for entry in rd.flatten() {
                    let Ok(ft) = entry.file_type() else {
                        continue;
                    };
                    if !ft.is_dir() {
                        continue;
                    }
                    let id = entry.file_name().to_string_lossy().into_owned();
                    let md = entry.path().join("SKILL.md");
                    let summary = std::fs::read_to_string(&md).ok().map(|text| {
                        text.lines()
                            .find(|l| {
                                let t = l.trim();
                                !t.is_empty() && !t.starts_with('#') && !t.starts_with("---")
                            })
                            .map(|l| l.trim().chars().take(120).collect::<String>())
                            .unwrap_or_default()
                    });
                    catalog.push((id, summary.unwrap_or_default()));
                }
            }
            catalog.sort_by(|a, b| a.0.cmp(&b.0));
            let mut catalog_text = String::new();
            if catalog.is_empty() {
                catalog_text = format!("(no SKILL.md files under {})", skills_dir.display());
            } else {
                catalog_text.push_str("## Skills in this agent preset\n");
                for (id, summary) in &catalog {
                    let when = if summary.is_empty() {
                        String::new()
                    } else {
                        format!(" — {summary}")
                    };
                    catalog_text.push_str(&format!(
                        "- `{id}`{when} — read `{}/skills/{id}/SKILL.md` with the read tool to load it\n",
                        dir.display()
                    ));
                }
            }
            let section = PromptSection {
                name: format!("preset:{id}:skills"),
                order: 30.0,
                text: PromptSectionText::Static(catalog_text),
                complete: false,
            };
            let undo = self
                .system_prompt
                .section(Some(&scope), &section)
                .map_err(|e| format!("preset {id}: skills section: {e}"))?;
            undos.push(undo);
            let ids: Vec<&str> = catalog.iter().map(|(i, _)| i.as_str()).collect();
            report.bridged.push(format!(
                "@deepseek-ai/dsh-skill-filesystem (skill catalog: {}; minimal read-only per A-03)",
                if ids.is_empty() {
                    "none found".to_string()
                } else {
                    ids.join(", ")
                }
            ));
        }

        // —— 工具行桥：单工具行按行 config 重呈现（description/timeoutMs）注册进
        //    standing scope（joined agent 的 schemas 走链即见）；**多工具组行**解析
        //    宿主工具集——单工作区宿主下 standing 链本就从全局基继承工具，故组行为
        //    「解析确认」而非逐工具重呈现（诚实标注 single-workspace）。
        for row in leaves
            .iter()
            .filter(|r| r.name != PERSONA_ROW && r.name != INSTRUCTIONS_ROW && r.name != SKILLS_ROW)
        {
            // L1/D-105：plan-mode 状态驱动段桥（组合 `dsh-plan-mode` 行）——不依赖工具
            // 注册面；config.section 经 Fn 段随 standing 的 plan-mode 状态注入
            // （active → 文本、否则空串）。per-agent 本性（预设注释：entry-local realm
            // 是正确生存期）。section 缺失 → 诚实 guard（非泛化）。
            if row.name == "@deepseek-ai/dsh-plan-mode" {
                let section = row
                    .config
                    .as_ref()
                    .and_then(|c| c.get("section"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                match section {
                    Some(text) => {
                        let source = plan_mode_source.clone();
                        let ftext = text.clone();
                        let pc = PromptSection {
                            name: format!("preset:{id}:plan-mode"),
                            order: PLAN_MODE_ORDER,
                            text: PromptSectionText::Fn(Rc::new(move |ctx: &AssembleContext| {
                                // S3（D-107）：折叠源按**组装者自身会话**判定——身份在场
                                // 时 per-agent 保真（多会话共享 standing 各看各的
                                // plan/mode），无身份（None）回退全局源。
                                let active = source
                                    .borrow()
                                    .as_ref()
                                    .is_some_and(|f| f(ctx.session_id.as_deref()));
                                if active {
                                    ftext.clone()
                                } else {
                                    String::new()
                                }
                            })),
                            complete: false,
                        };
                        let undo = self
                            .system_prompt
                            .section(Some(&scope), &pc)
                            .map_err(|e| format!("preset {id}: plan-mode section: {e}"))?;
                        undos.push(undo);
                        report.bridged.push(
                            "@deepseek-ai/dsh-plan-mode (plan-mode section bridge; state-driven)"
                                .to_string(),
                        );
                    }
                    None => report.guarded.push((
                        "@deepseek-ai/dsh-plan-mode".to_string(),
                        "plan-mode row without config.section (L1, deliberate)".to_string(),
                    )),
                }
                continue;
            }
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

        // K3/C：铸造组合权威本体——dsh-core agent-scope 子树 + 挂载记录 fiber。
        // 放在所有可失败桥之后：桥出错时**不**留悬空 pending_scope / 幽灵 fiber。
        let (core_scope, _) = self
            .core
            .mount_scope()
            .map_err(|e| format!("preset {id}: core mount_scope: {e}"))?;
        let record_fiber = self
            .core
            .plugin(
                PresetRecordPlugin(serde_json::json!({ "preset": id })),
                serde_json::json!({ "leakToRoot": self.fault_root_leak }),
            )
            .map_err(|e| {
                self.core.unmount_scope(core_scope);
                format!("preset {id}: core record plugin: {e}")
            })?;

        self.standings.insert(
            id.to_string(),
            Standing {
                id: id.to_string(),
                scope,
                report,
                core_scope,
                record_fiber,
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

    /// 换代/销毁：撤销 scoped 贡献（undo 精确幂等）+ dsh-core 子树整树卸载
    /// （`unmount_scope`，随 fiber 展开）；scope 随注册表释放。
    pub fn unmount(&mut self, id: &str) {
        if let Some(standing) = self.standings.remove(id) {
            for undo in standing.undos {
                undo();
            }
            self.core.unmount_scope(standing.core_scope);
        }
    }

    pub fn len(&self) -> usize {
        self.standings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.standings.is_empty()
    }

    /// K3/C：该 standing 的 dsh-core 挂载子树作用域（join/report 等的核心对应物）。
    pub fn core_scope_of(&self, preset_id: &str) -> Option<ScopeId> {
        self.standings.get(preset_id).map(|s| s.core_scope)
    }

    /// K3/C：该 standing 的挂载记录 fiber（生存期断言：Active ↔ 挂载存续）。
    pub fn record_fiber(&self, preset_id: &str) -> Option<FiberHandle> {
        self.standings.get(preset_id).map(|s| s.record_fiber)
    }

    /// K3/C：组合运行时（dsh-core）句柄——审计/测试/宿主自检用。
    pub fn core(&self) -> &Cordis {
        &self.core
    }

    /// K3/C：root-realm 泄漏审计（harness `leakedServices`）。对**每个**已挂载
    /// standing 的 agent-scope 子树跑 `audit_subtree`；空 = 全部干净。宿主
    /// （web select）见非空即拒绝该挂载并 unmount（fail-loud，同 K2）。
    pub fn audit(&self) -> Vec<String> {
        let mut out = Vec::new();
        for st in self.standings.values() {
            out.extend(self.core.audit_subtree(st.core_scope));
        }
        out
    }

    /// K3/C 故障注入（root-realm 泄漏；**测试专用**）：下一次挂载的记录服务不
    /// isolate → 落 root realm → `audit` 判定泄漏（验证泄漏拒绝路径端到端）。
    #[cfg(test)]
    pub(crate) fn set_fault_root_leak(&mut self) {
        self.fault_root_leak = true;
    }
}

// 禁用判定走**忠实门控**（P3-e 起，A 并行收口）：pwsh 执行器已在册（P3-d）且有
// pwsh 工具/终端后端可桥（P3-e），win32 不再需要 win32-B 强制覆盖——各平台一律由
// 组合自身的 `disabled_expr` 决定（win32 上 bash 系 `=== 'win32'` 判禁、pwsh 系
// 活化）。求值走注入引擎（K4/F-05 WASM 面 + native 兜底；见 mount_at 的 `gate`）。

/// 单工具行 → 宿主工具名（P3-a 桥表；P3-e 加 pwsh；U1/D-105 加 todo；U2 加 workflow）。
/// 多工具行（fs-local/terminal/…）见 `host_tool_group_for_row`。
fn host_tool_for_row(row: &CompositionRow) -> Option<&'static str> {
    match row.name.as_str() {
        "@deepseek-ai/dsh-tool-bash" => Some("bash"),
        "@deepseek-ai/dsh-tool-bash-persistent" => Some("bash"),
        "@deepseek-ai/dsh-tool-pwsh" => Some("pwsh"),
        "@deepseek-ai/dsh-tool-pwsh-persistent" => Some("pwsh"),
        "@deepseek-ai/dsh-tool-str-replace-editor" => Some("str_replace_editor"),
        "@deepseek-ai/dsh-tool-todo" => Some("todo_write"),
        // U2（D-105）：dsh-tool-workflow → 宿主已注册的 M4 `workflow`（恒注册；执行
        // 为 UNSUPPORTED_OPTION 桩 → fail-loud。工具在目录中可见、调用即诚实报错，
        // 故为桥非守卫——守卫的「no bridge」说法反而不实）。
        "@deepseek-ai/dsh-tool-workflow" => Some("workflow"),
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

/// L1/D-105：plan-mode 段 order——预设文本要求「plan-mode rules override 任何更晚的
/// 工具指引」，故置于工具指引带（100–199）之前、persona（0）之后；与 skills（30）
/// 区分以免同阶排序模糊。
pub const PLAN_MODE_ORDER: f64 = 55.0;

/// 多工具组行 → 宿主工具集（P3-b 解析确认；单工作区宿主 = standing 链继承全局基）。
/// U1（D-105）加：dsh-tool-fs（compound fs）→ 宿主 fs 六件套（同 fs-local 语义）；
/// dsh-tool-fs-search → 宿主搜索面（glob 路径 + grep 内容）；dsh-tool-jobs → 宿主
/// job_* 工具集（M4 宿主 bind）。
fn host_tool_group_for_row(row: &CompositionRow) -> Option<&'static [&'static str]> {
    match row.name.as_str() {
        "@deepseek-ai/dsh-fs-local" => {
            Some(&["read", "write", "edit", "read_image", "glob", "grep"])
        }
        "@deepseek-ai/dsh-tool-fs" => {
            Some(&["read", "write", "edit", "read_image", "glob", "grep"])
        }
        "@deepseek-ai/dsh-tool-fs-search" => Some(&["glob", "grep"]),
        "@deepseek-ai/dsh-tool-jobs" => Some(&["job_output", "job_list", "job_kill"]),
        "@deepseek-ai/dsh-terminal" => Some(TERMINAL_TOOLS),
        _ => None,
    }
}

/// 未桥工具行的守卫原因（D-103 broken 集显式标记，不伪装）。fs-local/terminal
/// 组与后端行在桥内已被前置解析，不落此函数。
fn tool_guard_reason(row: &CompositionRow) -> String {
    let n = row.name.as_str();
    if n.contains("pwsh") {
        // P3-e 后 dsh-tool-pwsh/-persistent 已入桥表；仅未映射的 pwsh 系变体到此。
        "unmapped pwsh-family row (A-parallel: only dsh-tool-pwsh/-persistent bridge to host \"pwsh\")"
            .to_string()
    } else if n.contains("tool-skill") {
        "minimal read-only per A-03: SKILL.md files are readable via fs tools; a skill loader tool needs the host skill service (C)"
            .to_string()
    } else if n.contains("tool-cordis") || n.contains("command-compact") || n.contains("web") {
        "broken per D-103 (unbridged surface)".to_string()
    } else if n == "@deepseek-ai/dsh-tool-goal" {
        // U1/D-105 自下而上核对：本 build 的 goal 是 RPC/会话投影面（web goal_dispatch +
        // dsh-session-query goal_projection），**没有**注册为 agent 可调用工具；预设注释
        // 亦声明「model-facing tool，service 在 host 面」——故不桥，诚实 guard。
        "host goal is an RPC/session-projection surface, not an agent tool in this build (U1)"
            .to_string()
    } else if n.contains("subagent") {
        // U2/D-105 自下而上核对：dsh-subagent + subagent_runtime + 会话 subagent 投影 +
        // jobs kind "subagent" 都在，但**没有**注册为 agent 可调用模型工具（模型无法
        // 发 subagent 调用）——故不桥，诚实 guard。
        if n.contains("control") {
            "subagent control surfaces are internal RPC/projections, not agent-callable tools in this build (U2)"
                .to_string()
        } else {
            "host exposes subagent delegation via an internal runtime/RPC (dsh-subagent + subagent_runtime + session projections); no agent-callable tool in this build (U2)"
                .to_string()
        }
    } else if n == "@deepseek-ai/dsh-workflow-worker-thread" {
        // U2/D-105：M4 workflow 是桩（UNSUPPORTED_OPTION），无 worker-thread 后端。
        "host workflow is the registered M4 stub (UNSUPPORTED_OPTION); no worker-thread backend in this build (U2)"
            .to_string()
    } else if n.contains("ralph") {
        "no host ralph loop tool in this build (U2)".to_string()
    } else if n.contains("ask-user") {
        "host ask-user is a UI/approval RPC, not an agent-callable tool in this build (U2)"
            .to_string()
    } else if n == "@deepseek-ai/dsh-compaction-basic"
        || n == "@deepseek-ai/dsh-compaction-tool-result-pruner"
    {
        // L3（D-105 compaction 档位 3，待桥）：守卫段 + 接口预留；tool-result-pruner 的
        // thresholdChars/headChars/tailChars 语义留接口、不实现行为（不接 append_tool_result）。
        "L3 (D-105 compaction tier-3): guard section + reserved interface; no summarization/pruning behavior yet"
            .to_string()
    } else if n == "@deepseek-ai/dsh-agent-tool-presentation" {
        // U3：tool-presentation 是装配期呈现变换（把工具 B 以 A 呈现）；standing 桥已对
        // 单工具行按 config 的 description/timeoutMs 逐行呈现，即此变换的宿主落地。
        "tool-presentation is an assembly-time presentation transform; standing already re-presents tools per row config (U3)"
            .to_string()
    } else {
        "no Rust bridge yet (P3/P5)".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_scope::scope_chain_of;
    use dsh_system_prompt::{AssembleContext, Config};
    use std::cell::RefCell;
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
        sect_texts_with(sp, scope, None)
    }

    /// S3（D-107）：带组装会话身份的组装视图（None = 无身份组装）。
    fn sect_texts_with(
        sp: &SystemPrompt,
        scope: &ScopeKey,
        session_id: Option<&str>,
    ) -> Vec<String> {
        sp.assemble(&AssembleContext{
            scope: Some(scope.clone()),
            session_id: session_id.map(str::to_string),
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
        // P3-e（A 并行收口）：win32 忠实门控——bash 系 `=== 'win32'` 判禁，pwsh 系
        // `!== 'win32'` 活化（pwsh 执行器已在册）。
        assert!(
            r.disabled
                .iter()
                .any(|s| s == "@deepseek-ai/dsh-tool-bash-persistent"),
            "win32 faithful disables bash: {:?}",
            r.disabled
        );
        assert!(
            !r.disabled
                .iter()
                .any(|s| s == "@deepseek-ai/dsh-tool-pwsh-persistent"),
            "win32 faithful keeps pwsh active: {:?}",
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
- id: tskill
  name: '@deepseek-ai/dsh-tool-skill'
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
        assert!(
            reason_of("@deepseek-ai/dsh-tool-skill").contains("minimal read-only per A-03"),
            "tool-skill minimal-readonly reason"
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
            "pwsh",
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
        // P3-e 忠实门控（win32）：bash 系判禁（组合 `=== 'win32'`）、pwsh 系活化已桥。
        assert!(
            r.disabled
                .iter()
                .any(|s| s == "@deepseek-ai/dsh-tool-bash-persistent"),
            "win32 bash-persistent disabled: {:?}",
            r.disabled
        );
        assert!(
            bridged("@deepseek-ai/dsh-tool-pwsh-persistent"),
            "pwsh single-tool bridged: {:?}",
            r.bridged
        );
        assert!(
            bridged("@deepseek-ai/dsh-tool-str-replace-editor"),
            "editor bridged"
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
            "pwsh",
            "str_replace_editor",
        ] {
            assert!(names.iter().any(|n| n == t), "joined model sees {t}");
        }
    }

    // —— K2/C —— unusable-rows 挂载否决（对齐 harness `inactiveRows`）。
    // 「桥依赖不可满足」（缺宿主工具/组/注册面/后端/base-dir）→ 挂载否决；
    // 刻意 broken / 未实现面的诚实降级 → 仅报告、不否决（D-103 兼容）。
    /// 生产等同宿主工具集（web_m5 注册面：fs 六件套 + terminal 六件套 + bash/pwsh
    /// + str_replace_editor）。
    fn realistic_tools() -> Rc<ToolRegistry> {
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
            "pwsh",
            "str_replace_editor",
            // U1（D-105）：生产等同工具集补齐 M4 宿主面（web.rs 恒注册）。
            "todo_write",
            "job_output",
            "job_list",
            "job_kill",
            // U2（D-105）：M4 workflow 恒注册（桩 → UNSUPPORTED_OPTION）。
            "workflow",
        ] {
            tools.register_global(tool_def(name, name, 1000.0)).unwrap();
        }
        tools
    }

    fn linux_facade() -> Value {
        serde_json::json!({
            "platform": "linux",
            "env": {},
            "cwd": "/repo",
        })
    }

    /// 安全网：真实 shipped 预设（minimal/standard/code/cordis）+ 生产等同工具集
    /// → 无 unusable 行（K2 对四个真实预设零回归；unmapped 行是诚实降级不计）。
    /// 用 `mount_at(Some(base_dir))` 复刻 web select 的真实装配（base_dir = 组合目录，
    /// skill 目录桥可解析）。
    #[test]
    fn real_presets_have_no_unusable_rows_on_production_host() {
        let sp = new_sp();
        let root = repo_root()
            .join("resources")
            .join("agent-presets");
        for id in ["minimal", "standard", "code", "cordis"] {
            let mut reg = StandingRegistry::new(sp.clone(), Some(realistic_tools()));
            reg.mount_at(id, &preset_rows(id), Some(&root.join(id)), &win32_facade())
                .unwrap_or_else(|e| panic!("{id}: mount failed: {e}"));
            let r = reg.report(id).unwrap();
            let u = r.unusable_rows();
            assert!(
                u.is_empty(),
                "{id}: shipped preset must be usable on production host: {u:?}"
            );
        }
    }

    /// —— U1（D-105）—— fs/搜索/jobs 组行 + todo 单工具行在宿主工具在场时真桥接
    /// （组解析确认 = 宿主工具集 / 单工具重呈现）；goal 行无宿主 goal 模型工具 →
    /// 诚实 guard（专用原因，与预设法定义「service 在 host 面、此工具为 model-facing」
    /// 的意图一致）。TDD 红→绿。
    #[test]
    fn tool_family_fs_search_jobs_todo_bridge_when_host_tools_present() {
        let sp = new_sp();
        // realistic_tools 含 fs-six + terminal-six + bash/pwsh/edit + job_* + todo_write。
        let mut reg = StandingRegistry::new(sp.clone(), Some(realistic_tools()));
        let comp = "
- id: fs
  name: '@deepseek-ai/dsh-tool-fs'
- id: fss
  name: '@deepseek-ai/dsh-tool-fs-search'
- id: jobs
  name: '@deepseek-ai/dsh-tool-jobs'
- id: todo
  name: '@deepseek-ai/dsh-tool-todo'
- id: goal
  name: '@deepseek-ai/dsh-tool-goal'
";
        let rows = dsh_agent_presets::parse::parse_composition(comp).unwrap();
        reg.mount("u1", &rows, &win32_facade()).unwrap();
        let r = reg.report("u1").unwrap();
        let bridged_of = |n: &str| {
            r.bridged
                .iter()
                .find(|s| s.starts_with(n))
                .map(|s| s.as_str())
                .unwrap_or("<not bridged>")
        };
        let guarded_of = |n: &str| {
            r.guarded
                .iter()
                .find(|(name, _)| name == n)
                .map(|(_, w)| w.as_str())
                .unwrap_or("<not guarded>")
        };
        assert!(
            bridged_of("@deepseek-ai/dsh-tool-fs").contains("read/write/edit/read_image/glob/grep"),
            "fs row bridged to host fs toolset: {}",
            bridged_of("@deepseek-ai/dsh-tool-fs")
        );
        assert!(
            bridged_of("@deepseek-ai/dsh-tool-fs-search").contains("glob/grep"),
            "fs-search bridged to host search surface: {}",
            bridged_of("@deepseek-ai/dsh-tool-fs-search")
        );
        assert!(
            bridged_of("@deepseek-ai/dsh-tool-jobs").contains("job_output/job_list/job_kill"),
            "jobs bridged to host jobs toolset: {}",
            bridged_of("@deepseek-ai/dsh-tool-jobs")
        );
        assert!(
            bridged_of("@deepseek-ai/dsh-tool-todo").contains("(tool todo_write)"),
            "todo bridged to host todo_write: {}",
            bridged_of("@deepseek-ai/dsh-tool-todo")
        );
        assert!(
            guarded_of("@deepseek-ai/dsh-tool-goal").contains("not an agent tool"),
            "goal stays honest guard with specific reason: {}",
            guarded_of("@deepseek-ai/dsh-tool-goal")
        );
        assert!(
            r.unusable_rows().is_empty(),
            "all bridged/guarded rows usable: {:?}",
            r.unusable_rows()
        );
    }

    /// U1 在真实 shipped 预设上的呈现：standard/code/cordis 的 fs/family、jobs、todo
    /// 行 → bridged；goal/skill/web 行 → 诚实 guard（不为终点能力而误桥）。
    #[test]
    fn real_presets_bridge_fs_jobs_todo_and_guard_goal_skill_web() {
        let sp = new_sp();
        let root = repo_root().join("resources").join("agent-presets");
        for id in ["standard", "code", "cordis"] {
            let mut reg = StandingRegistry::new(sp.clone(), Some(realistic_tools()));
            reg.mount_at(id, &preset_rows(id), Some(&root.join(id)), &win32_facade())
                .unwrap_or_else(|e| panic!("{id}: mount failed: {e}"));
            let r = reg.report(id).unwrap();
            for want in [
                "@deepseek-ai/dsh-tool-fs",
                "@deepseek-ai/dsh-tool-fs-search",
                "@deepseek-ai/dsh-tool-jobs",
                "@deepseek-ai/dsh-tool-todo",
            ] {
                assert!(
                    r.bridged.iter().any(|s| s.starts_with(want)),
                    "{id}: {want} bridged: {:?}",
                    r.bridged
                );
                assert!(
                    !r.guarded.iter().any(|(n, _)| n == want),
                    "{id}: {want} must not stay guarded"
                );
            }
            for want in [
                "@deepseek-ai/dsh-tool-goal",
                "@deepseek-ai/dsh-tool-skill",
                "@deepseek-ai/dsh-tool-web",
            ] {
                assert!(
                    r.guarded.iter().any(|(n, _)| n == want),
                    "{id}: {want} honestly guarded: {:?}",
                    r.guarded
                );
            }
        }
    }

    /// —— U2（D-105）—— 下伸面：subagent 家 / workflow-worker-thread / ralph /
    /// ask-user 宿主**无对应模型工具**（dsh-subagent 是内部运行时/RPC + 会话投影，
    /// 非 agent 可调用工具；M4 workflow 是桩）→ 全部诚实 guard（专用原因，非泛化
    /// "no Rust bridge yet"）；`dsh-tool-workflow` → 单工具桥到宿主已注册 `workflow`
    /// （M4 桩 → 执行 fail-loud UNSUPPORTED_OPTION，注册即见）。TDD 红→绿。
    #[test]
    fn delegation_rows_guard_specifically_workflow_bridges() {
        let sp = new_sp();
        let mut reg = StandingRegistry::new(sp.clone(), Some(realistic_tools()));
        let comp = "
- id: wf
  name: '@deepseek-ai/dsh-tool-workflow'
- id: wfwt
  name: '@deepseek-ai/dsh-workflow-worker-thread'
- id: sub
  name: '@deepseek-ai/dsh-tool-subagent'
- id: subc
  name: '@deepseek-ai/dsh-tool-subagent-control'
- id: subl
  name: '@deepseek-ai/dsh-tool-subagent-control/list-agents'
- id: ral
  name: '@deepseek-ai/dsh-tool-ralph'
- id: ask
  name: '@deepseek-ai/dsh-tool-ask-user'
- id: web
  name: '@deepseek-ai/dsh-tool-web'
";
        let rows = dsh_agent_presets::parse::parse_composition(comp).unwrap();
        reg.mount("u2", &rows, &win32_facade()).unwrap();
        let r = reg.report("u2").unwrap();
        assert!(
            r.bridged
                .iter()
                .any(|s| s.starts_with("@deepseek-ai/dsh-tool-workflow")),
            "workflow row bridged to host workflow: {:?}",
            r.bridged
        );
        let guarded_of = |n: &str| {
            r.guarded
                .iter()
                .find(|(name, _)| name == n)
                .map(|(_, w)| w.as_str())
                .unwrap_or("<not guarded>")
        };
        assert!(
            guarded_of("@deepseek-ai/dsh-tool-subagent").contains("internal runtime"),
            "subagent guarded: {}",
            guarded_of("@deepseek-ai/dsh-tool-subagent")
        );
        assert!(
            guarded_of("@deepseek-ai/dsh-tool-subagent-control").contains("control"),
            "subagent-control guarded: {}",
            guarded_of("@deepseek-ai/dsh-tool-subagent-control")
        );
        assert!(
            guarded_of("@deepseek-ai/dsh-tool-subagent-control/list-agents").contains("control"),
            "list-agents guarded: {}",
            guarded_of("@deepseek-ai/dsh-tool-subagent-control/list-agents")
        );
        assert!(
            guarded_of("@deepseek-ai/dsh-workflow-worker-thread").contains("worker-thread"),
            "workflow-worker-thread guarded: {}",
            guarded_of("@deepseek-ai/dsh-workflow-worker-thread")
        );
        assert!(
            guarded_of("@deepseek-ai/dsh-tool-ralph").contains("ralph"),
            "ralph guarded: {}",
            guarded_of("@deepseek-ai/dsh-tool-ralph")
        );
        assert!(
            guarded_of("@deepseek-ai/dsh-tool-ask-user").contains("ask-user"),
            "ask-user guarded: {}",
            guarded_of("@deepseek-ai/dsh-tool-ask-user")
        );
        assert!(
            guarded_of("@deepseek-ai/dsh-tool-web").contains("broken per D-103"),
            "web stays broken-D-103 (unchanged): {}",
            guarded_of("@deepseek-ai/dsh-tool-web")
        );
        assert!(
            r.unusable_rows().is_empty(),
            "none of these are stuck: {:?}",
            r.unusable_rows()
        );
    }

    /// U2 在真实 shipped 预设上的呈现：静态 `disabled: true`（subagent codex/
    /// claude-code）→ disabled（不进守卫）；workflow 行 → bridged；subagent 家 /
    /// workflow-worker-thread / ralph / ask-user → 诚实 guard（专用原因）。
    #[test]
    fn real_presets_present_delegation_rows_static_disabled_and_specific_guard() {
        let sp = new_sp();
        let root = repo_root().join("resources").join("agent-presets");
        for id in ["standard", "code", "cordis"] {
            let mut reg = StandingRegistry::new(sp.clone(), Some(realistic_tools()));
            reg.mount_at(id, &preset_rows(id), Some(&root.join(id)), &win32_facade())
                .unwrap_or_else(|e| panic!("{id}: mount failed: {e}"));
            let r = reg.report(id).unwrap();
            assert!(
                r.disabled.iter().any(|n| n == "@deepseek-ai/dsh-tool-subagent"),
                "{id}: static-disabled subagent (codex/claude-code) held in disabled: {:?}",
                r.disabled
            );
            assert!(
                r.bridged.iter().any(|s| s.starts_with("@deepseek-ai/dsh-tool-workflow")),
                "{id}: workflow row bridged: {:?}",
                r.bridged
            );
            for want in [
                "@deepseek-ai/dsh-tool-subagent-control",
                "@deepseek-ai/dsh-tool-subagent-control/list-agents",
                "@deepseek-ai/dsh-tool-subagent",
                "@deepseek-ai/dsh-workflow-worker-thread",
                "@deepseek-ai/dsh-tool-ralph",
                "@deepseek-ai/dsh-tool-ask-user",
            ] {
                assert!(
                    r.guarded.iter().any(|(n, _)| n == want),
                    "{id}: {want} must stay honestly guarded: {:?}",
                    r.guarded
                );
            }
        }
    }

    /// —— U3（D-105）—— 安全网：真实 shipped 预设在生产宿主下，**每一行守卫原因都
    /// 必须来自经过决策的专用集**（broken-D-103 / A-03 skill / U1 goal / U2 delegation /
    /// L1 plan-mode pending / L3 compaction tier-3 pending / 工具呈现），不得落入泛化
    /// 「no Rust bridge yet」；且无 stuck 行。TDD 红→绿。
    #[test]
    fn real_presets_guarded_rows_all_have_deliberate_reasons() {
        let sp = new_sp();
        let root = repo_root().join("resources").join("agent-presets");
        for id in ["minimal", "standard", "code", "cordis"] {
            let mut reg = StandingRegistry::new(sp.clone(), Some(realistic_tools()));
            reg.mount_at(id, &preset_rows(id), Some(&root.join(id)), &win32_facade())
                .unwrap_or_else(|e| panic!("{id}: mount failed: {e}"));
            let r = reg.report(id).unwrap();
            for (name, why) in &r.guarded {
                assert!(
                    !why.contains("no Rust bridge yet"),
                    "{id}: {name} fell through to a generic unbridged reason — needs a deliberate one: {why}"
                );
                assert!(
                    !["", "no shared tool registry in this host"]
                        .contains(&why.as_str()),
                    "{id}: {name} has a degenerate reason: {why}"
                );
            }
            assert!(
                r.unusable_rows().is_empty(),
                "{id}: no stuck rows on production host: {:?}",
                r.unusable_rows()
            );
        }
    }

    /// —— L1（D-105）—— plan-mode 状态驱动段：组合 `dsh-plan-mode` 行的
    /// config.section 随**折叠源**（会话 `plan/mode` 事件 fold 的注入替身）状态注入
    /// SystemPrompt——active → 段出现；非 active → 消失；无源 → 永不注入。该行从
    /// 守卫转 bridged（section bridge）。TDD 红→绿（先红：行仍在守卫、无段）。
    #[test]
    fn plan_mode_section_injected_when_fold_source_active_else_absent() {
        let sp = new_sp();
        let mut reg = StandingRegistry::new(sp.clone(), Some(realistic_tools()));
        // 折叠源替身：单一权威态应从 `dsh_plan::fold_plan_mode(events)` 折叠；这里
        // 注入可控闭包验证段对 folding 结果的响应（真实 fold 由 dsh-plan 测试与
        // PlanModeHost 测试覆盖）。
        let state = Rc::new(RefCell::new(false));
        let src = state.clone();
        reg.set_plan_mode_source(Some(Rc::new(move |_sid| *src.borrow())));
        let comp = "
- id: plan-mode
  name: '@deepseek-ai/dsh-plan-mode'
  config:
    section: |
      YOU-ARE-IN-PLAN-MODE-MARKER-{tag}
";
        let rows = dsh_agent_presets::parse::parse_composition(comp).unwrap();
        reg.mount("l1", &rows, &win32_facade()).unwrap();
        let r = reg.report("l1").unwrap();
        assert!(
            r.bridged.iter().any(|s| s.starts_with("@deepseek-ai/dsh-plan-mode")),
            "plan-mode row bridged (section bridge): {:?}",
            r.bridged
        );
        let scope = ScopeKey::new();
        reg.join("l1", &scope).unwrap();
        let has_marker = |sp: &Rc<SystemPrompt>, sc: &ScopeKey| {
            sect_texts(sp, sc)
                .iter()
                .any(|t| t.contains("YOU-ARE-IN-PLAN-MODE-MARKER"))
        };
        assert!(!has_marker(&sp, &scope), "inactive: no plan-mode section");
        *state.borrow_mut() = true; // 会话进入 plan mode（fold active）
        assert!(has_marker(&sp, &scope), "active: plan-mode section injected");
        *state.borrow_mut() = false; // 会话退出 plan mode（fold inactive）
        assert!(!has_marker(&sp, &scope), "left: section removed again");

        // 无源 → 永不注入（未接 loop 的诚实面）。
        let sp2 = new_sp();
        let mut reg2 = StandingRegistry::new(sp2.clone(), Some(realistic_tools()));
        reg2.mount("l1", &rows, &win32_facade()).unwrap();
        let scope2 = ScopeKey::new();
        reg2.join("l1", &scope2).unwrap();
        assert!(
            !sect_texts(&sp2, &scope2)
                .iter()
                .any(|t| t.contains("YOU-ARE-IN-PLAN-MODE-MARKER")),
            "no source: never injected"
        );
    }

    /// —— S3（D-107）—— per-agent plan-mode 保真：**同一 standing（多会话共享）**下，
    /// 折叠源按**组装者自身会话身份**判定——A 进 plan → A 的组装含段、B 不含；翻转
    /// 亦然；无身份组装（None）回退全局判定。这正是此前「单活跃全局源」被修的缺陷。
    #[test]
    fn plan_mode_section_folds_per_assembled_session() {
        let sp = new_sp();
        let mut reg = StandingRegistry::new(sp.clone(), Some(realistic_tools()));
        // 折叠源 = 按会话判定（模拟宿主解析器：会话 "alice" 处于 plan，其它否）。
        reg.set_plan_mode_source(Some(Rc::new(|sid| sid == Some("alice"))));
        let comp = "
- id: plan-mode
  name: '@deepseek-ai/dsh-plan-mode'
  config:
    section: |
      YOU-ARE-IN-PLAN-MODE-MARKER-{tag}
";
        let rows = dsh_agent_presets::parse::parse_composition(comp).unwrap();
        reg.mount("s3", &rows, &win32_facade()).unwrap();
        let scope = ScopeKey::new();
        reg.join("s3", &scope).unwrap();
        let has_marker = |t: &Vec<String>| {
            t.iter().any(|x| x.contains("YOU-ARE-IN-PLAN-MODE-MARKER"))
        };
        // alice 进 plan → alice 的组装含段。
        assert!(
            has_marker(&sect_texts_with(&sp, &scope, Some("alice"))),
            "per-agent: alice (plan active) sees the plan-mode section"
        );
        // bob 未进 plan → 同 standing、同 scope，bob 的组装不含段。
        assert!(
            !has_marker(&sect_texts_with(&sp, &scope, Some("bob"))),
            "per-agent: bob (not in plan) must NOT see alice's plan-mode section"
        );
        // 无身份组装（None）→ 回退全局判定（源对 None 返回 false）。
        assert!(
            !has_marker(&sect_texts(&sp, &scope)),
            "no identity: falls back to global source verdict"
        );
    }

    /// 映射行缺宿主工具（桥依赖不可满足）→ stuck → 挂载否决。
    #[test]
    fn unusable_rows_flags_mapped_tool_missing_from_host() {
        let sp = new_sp();
        // linux 门面：bash 行不被平台判禁 → 活化 → 需宿主 "bash"（缺失）。
        let tools = Rc::new(ToolRegistry::new(dsh_tools::ToolExecutionMode::Native));
        let comp = "- id: t\n  name: '@deepseek-ai/dsh-tool-bash'\n";
        let rows = dsh_agent_presets::parse::parse_composition(comp).unwrap();
        let mut reg = StandingRegistry::new(sp.clone(), Some(tools));
        reg.mount("x", &rows, &linux_facade()).unwrap();
        let u = reg.report("x").unwrap().unusable_rows();
        assert!(
            u.iter().any(|(n, w)| {
                n == "@deepseek-ai/dsh-tool-bash" && w.contains("no host tool \"bash\"")
            }),
            "stuck mapped row detected: {u:?}"
        );
    }

    /// 组行缺宿主工具组 / terminal 后端行无已解析组 → stuck → 挂载否决。
    #[test]
    fn unusable_rows_flags_group_and_backend_stuck_dependencies() {
        let sp = new_sp();
        // 宿主只有 bash：fs/terminal 组缺失，且 terminal-bash 后端行无解析组。
        let tools = Rc::new(ToolRegistry::new(dsh_tools::ToolExecutionMode::Native));
        tools.register_global(tool_def("bash", "bash", 1000.0)).unwrap();
        let comp = "
- id: fs
  name: '@deepseek-ai/dsh-fs-local'
- id: term
  name: '@deepseek-ai/dsh-terminal'
- id: term-bash
  name: '@deepseek-ai/dsh-terminal-bash'
";
        let rows = dsh_agent_presets::parse::parse_composition(comp).unwrap();
        let mut reg = StandingRegistry::new(sp.clone(), Some(tools.clone()));
        reg.mount("x", &rows, &win32_facade()).unwrap();
        let u = reg.report("x").unwrap().unusable_rows();
        assert!(
            u.iter().any(|(n, _)| n == "@deepseek-ai/dsh-fs-local"),
            "fs group missing is stuck: {u:?}"
        );
        assert!(
            u.iter().any(|(n, _)| n == "@deepseek-ai/dsh-terminal"),
            "terminal group missing is stuck: {u:?}"
        );
        assert!(
            u.iter().any(|(n, w)| {
                n == "@deepseek-ai/dsh-terminal-bash"
                    && w.contains("terminal backend without a resolved terminal group")
            }),
            "terminal backend without resolved group is stuck: {u:?}"
        );
    }

    /// D-103/A-03 诚实降级（broken 集、只读 skill、未桥面）→ 不否决（仅报告）。
    #[test]
    fn unusable_rows_ignores_declared_broken_and_unbridged_degrade() {
        let sp = new_sp();
        let comp = "
- id: a
  name: '@deepseek-ai/dsh-tool-cordis'
- id: b
  name: '@deepseek-ai/dsh-command-compact'
- id: c
  name: '@deepseek-ai/dsh-tool-skill'
- id: d
  name: '@deepseek-ai/dsh-tool-web'
- id: e
  name: '@deepseek-ai/dsh-tool-goal'
";
        let rows = dsh_agent_presets::parse::parse_composition(comp).unwrap();
        let mut reg = StandingRegistry::new(sp.clone(), Some(realistic_tools()));
        reg.mount("y", &rows, &win32_facade()).unwrap();
        let r = reg.report("y").unwrap();
        assert!(
            !r.guarded.is_empty(),
            "all five honest-degrade rows are guarded: {:?}",
            r.guarded
        );
        assert!(
            r.unusable_rows().is_empty(),
            "declared-broken / minimal-skill / unbridged rows must not reject: {:?}",
            r.unusable_rows()
        );
    }

    // —— K3/C —— dsh-core agent-scope 子树承载每个 standing 的挂载本体：挂载记录
    // fiber 提供记录服务（isolate 于 agent realm → 审计干净）；unmount 走
    // `unmount_scope` 整树卸载；fault 注入（root-realm 泄漏）→ `audit` 捕获。
    /// 真实预设（minimal/standard/code/cordis）+ 生产等同工具集 → 每个挂载都铸造
    /// dsh-core 子树（记录 fiber Active）且泄漏审计干净（K3 对四个真实预设零回归）。
    #[test]
    fn real_presets_mount_core_subtrees_and_audit_clean() {
        use dsh_core::FiberState;
        let sp = new_sp();
        let root = repo_root().join("resources").join("agent-presets");
        for id in ["minimal", "standard", "code", "cordis"] {
            let mut reg = StandingRegistry::new(sp.clone(), Some(realistic_tools()));
            reg.mount_at(id, &preset_rows(id), Some(&root.join(id)), &win32_facade())
                .unwrap_or_else(|e| panic!("{id}: mount failed: {e}"));
            let fid = reg
                .record_fiber(id)
                .unwrap_or_else(|| panic!("{id}: record fiber must exist"));
            assert_eq!(
                reg.core().fiber_state(fid),
                Some(FiberState::Active),
                "{id}: record fiber Active"
            );
            assert_eq!(
                reg.core().audit_subtree(reg.core_scope_of(id).unwrap()).len(),
                0,
                "{id}: isolated record must not leak"
            );
            let leaks = reg.audit();
            assert!(leaks.is_empty(), "{id}: registry audit clean: {leaks:?}");
            // unmount → dsh-core 整树卸载（fiber 转 Disposed）+ 注册面无残留。
            reg.unmount(id);
            assert!(reg.record_fiber(id).is_none(), "{id}: fiber gone after unmount");
            assert!(reg.core_scope_of(id).is_none(), "{id}: scope gone after unmount");
            assert_eq!(
                reg.core().fiber_state(fid),
                Some(FiberState::Disposed),
                "{id}: fiber disposed"
            );
        }
    }

    /// —— K4/F-05 —— 组合 `disabled_expr` 门控走注入求值引擎（生产 = WASM 面 +
    /// native 兜底）；组合权威（fail-closed + truthy）仍在 dsh-agent-presets。
    /// 记录每次求值调用：证明 standing 行审计真的消费了注入引擎。
    struct RecordingCombo(Rc<RefCell<Vec<String>>>);

    impl dsh_wasmrt::ComboEvaluator for RecordingCombo {
        fn eval(&self, scope: &Value, expr: &str) -> Result<Value, String> {
            self.0.borrow_mut().push(expr.to_string());
            let win = scope
                .get("process")
                .and_then(|p| p.get("platform"))
                .and_then(Value::as_str)
                == Some("win32");
            Ok(Value::Bool(win))
        }
    }

    #[test]
    fn standing_disabled_gate_consumes_injected_combo_evaluator() {
        let sp = new_sp();
        let calls: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let combos = calls.clone();
        let stub = RecordingCombo(calls);
        let mut reg = StandingRegistry::with_combo(
            sp.clone(),
            Some(realistic_tools()),
            Rc::new(stub),
            false,
        );
        let comp = "
- id: a
  name: '@deepseek-ai/dsh-tool-bash'
  disabled_expr: \"process.platform === 'x'\"
- id: b
  name: '@deepseek-ai/dsh-tool-pwsh'
";
        let rows = dsh_agent_presets::parse::parse_composition(comp).unwrap();
        reg.mount("t", &rows, &win32_facade()).unwrap();
        let r = reg.report("t").unwrap();
        assert!(
            combos.borrow().iter().any(|e| e == "process.platform === 'x'"),
            "enabled-with-expr row went through the injected evaluator: {:?}",
            combos.borrow()
        );
        assert!(
            r.disabled.iter().any(|s| s == "@deepseek-ai/dsh-tool-bash"),
            "stub verdict (win32 == true) disabled bash row: {:?}",
            r.disabled
        );
        assert!(
            r.bridged.iter().any(|s| s.starts_with("@deepseek-ai/dsh-tool-pwsh")),
            "no-expr row stays active: {:?}",
            r.bridged
        );
    }

    /// 默认构造：combo_eval.wasm 已构建 → 组合求值 = WASM 主面 + native 兜底
    /// （F-05 语义；blob 缺失回落 native-only，仍正确——本次构建后应命中 wasm 面）。
    #[test]
    fn default_combo_is_wasm_faced_when_blob_present() {
        let sp = new_sp();
        if WasmComboEvaluator::from_default_build().is_ok() {
            let reg = StandingRegistry::new(sp.clone(), None);
            assert!(
                reg.combo_is_wasm(),
                "default combo is wasm-faced (native fallback underneath)"
            );
        } else {
            eprintln!("combo_eval.wasm not built — skipping wasm-default assertion");
        }
    }

    /// fault 注入：记录服务不 isolate → 落 root realm → `audit` 判定泄漏（验证
    /// leakedServices 守卫的真实拒绝输入）；unmount 后干净。
    #[test]
    fn fault_root_leak_is_caught_by_audit_and_unmount_cleans() {
        let sp = new_sp();
        let comp = "- id: persona\n  name: '@deepseek-ai/dsh-persona'\n  config:\n    text: LEAK\n";
        let rows = dsh_agent_presets::parse::parse_composition(comp).unwrap();
        let mut reg = StandingRegistry::new(sp.clone(), None);
        reg.set_fault_root_leak();
        reg.mount("leaky", &rows, &win32_facade()).unwrap();
        let leaks = reg.audit();
        assert!(
            !leaks.is_empty(),
            "root-leaking record must be flagged by audit"
        );
        assert!(
            leaks.iter().any(|l| l.contains("preset.mount")),
            "leak names the record service: {leaks:?}"
        );
        reg.unmount("leaky");
        assert!(reg.audit().is_empty(), "unmount cleans the leak");
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

    /// —— P3-c —— skill 最小只读目录桥（D-103/A-03）：`@deepseek-ai/dsh-skill-filesystem`
    /// 行经 base_dir 解析 `<dir>/skills/*/SKILL.md` → scoped 目录段（joined 视图见
    /// 摘要 + 绝对路径）；无 base_dir → guarded；空目录 → 仍 bridged（诚实 none found）。
    /// 修复前该行落 tool_guard_reason「no Rust bridge yet」= 红。
    #[test]
    fn skill_catalog_bridge_via_preset_base_dir() {
        let sp = new_sp();
        let mut reg = StandingRegistry::new(sp.clone(), None);
        let base = std::env::temp_dir().join(format!("dsh-standing-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("skills/editing-cordis-compositions")).unwrap();
        std::fs::create_dir_all(base.join("skills/cordis-plugin-development")).unwrap();
        std::fs::write(
            base.join("skills/editing-cordis-compositions/SKILL.md"),
            "# Editing Cordis\nLoad this before authoring.\n",
        )
        .unwrap();
        std::fs::write(
            base.join("skills/cordis-plugin-development/SKILL.md"),
            "# Cordis Plugin Dev\n",
        )
        .unwrap();
        let proc: Value =
            serde_json::from_str(r#"{"platform":"linux","env":{},"cwd":"/repo"}"#).unwrap();
        let comp = "- id: skillfs\n  name: '@deepseek-ai/dsh-skill-filesystem'\n";
        let rows = dsh_agent_presets::parse::parse_composition(comp).unwrap();

        // base_dir 存在 → 目录桥 + joined 视图见摘要与路径。
        reg.mount_at("p7", &rows, Some(&base), &proc).unwrap();
        let r = reg.report("p7").unwrap();
        assert!(
            r.bridged.iter().any(|s| s.starts_with(
                "@deepseek-ai/dsh-skill-filesystem (skill catalog: cordis-plugin-development, editing-cordis-compositions"
            )),
            "skill catalog bridged (sorted): {:?}",
            r.bridged
        );
        let joined = ScopeKey::new();
        reg.join("p7", &joined).unwrap();
        let texts = sect_texts(&sp, &joined);
        assert!(
            texts
                .iter()
                .any(|t| t.contains("editing-cordis-compositions")
                    && t.contains("Load this before authoring")
                    && t.contains("SKILL.md")),
            "catalog summary + read path inline: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|t| t.contains("cordis-plugin-development")),
            "second skill listed"
        );

        // 空 skills 目录 → 仍 bridged（none found，诚实不假装有目录）。
        let empty =
            std::env::temp_dir().join(format!("dsh-standing-skills-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&empty);
        std::fs::create_dir_all(&empty).unwrap();
        reg.mount_at("p8", &rows, Some(&empty), &proc).unwrap();
        let r8 = reg.report("p8").unwrap();
        assert!(
            r8.bridged
                .iter()
                .any(|s| s
                    .starts_with("@deepseek-ai/dsh-skill-filesystem (skill catalog: none found")),
            "empty catalog still bridged: {:?}",
            r8.bridged
        );

        // 无 base_dir（占位口）→ guarded（诚实，不假装解析）。
        reg.mount("p9", &rows, &proc).unwrap();
        let r9 = reg.report("p9").unwrap();
        assert!(
            r9.guarded.iter().any(|(n, why)| {
                n == "@deepseek-ai/dsh-skill-filesystem" && why.contains("no base dir")
            }),
            "no-base-dir guarded: {:?}",
            r9.guarded
        );
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&empty);
    }
}
