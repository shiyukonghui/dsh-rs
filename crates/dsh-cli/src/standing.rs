//! P2-b：standing 注册表——每个 preset 一个 standing scope，贡献挂在共享
//! dsh-scope / dsh-system-prompt 注册面内；agent 经 **scope 父链** join；守卫报告
//! 行审计（bridged/disabled/guarded）。
//!
//! 路径 B 组合权威（D-103/A-01/P2）：
//! - 行审计 = `dsh-agent-presets::parse`（typed 行 + `disabled_expr` × process 门面）；
//! - 贡献 = `dsh-system-prompt` 的 **scoped section**（P2 桥：`@deepseek-ai/dsh-persona`
//!   行 → standing scope 的 complete/persona section + `includeRuntimeContext:false`
//!   抑制）——joined agent 的 `assemble(scope)` 经 `scope_chain_of` 看到它；
//! - join = `dsh-scope::bind_scope_parent`（换 preset 用绑定 `.rebind`）。
//!
//! **诚实边界**：P2 只桥 persona；其余生效叶行一律列 `guarded`（P3/P5 缩小，D-103
//! 「先 broken」）。standing 是注册面里的一个作用域子树；真实 isolate 服务隔离是
//! C 段收敛目标——P2 不伪装 release。

use std::collections::HashMap;
use std::rc::Rc;

use dsh_agent_presets::parse::{row_disabled, CompositionRow};
use dsh_scope::{bind_scope_parent, store::Undo, ScopeKey, ScopeParentBinding};
use dsh_system_prompt::{PromptSection, PromptSectionText, SystemPrompt};
use serde_json::Value;

/// persona 行名（P2 唯一实现的桥）。
pub const PERSONA_ROW: &str = "@deepseek-ai/dsh-persona";

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
    standings: HashMap<String, Standing>,
}

impl StandingRegistry {
    pub fn new(system_prompt: Rc<SystemPrompt>) -> Self {
        StandingRegistry {
            system_prompt,
            standings: HashMap::new(),
        }
    }

    /// 挂载 preset：行审计 + 铸 standing scope + persona 桥贡献。同 id 换代：
    /// 先 unmount（撤销 scoped 贡献）再建新。
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
                if row_disabled(row, process) {
                    report.disabled.push(row.name.clone());
                    continue;
                }
                leaves.push(row);
            }
            leaves
        }
        let leaves = walk(rows, process, &mut report);

        // 桥（P2：persona 行 → standing scope section + runtime-context 抑制）。
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

        // 守卫：其余活化叶行未桥 → guarded（P3/P5 缩小；D-103「先 broken」）。
        for row in leaves.iter().filter(|r| r.name != PERSONA_ROW) {
            report
                .guarded
                .push((row.name.clone(), "no Rust bridge yet (P3/P5)".to_string()));
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
        let mut reg = StandingRegistry::new(sp.clone());
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
        let mut reg = StandingRegistry::new(sp.clone());
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
        assert!(
            r.disabled
                .iter()
                .any(|s| s == "@deepseek-ai/dsh-terminal-bash")
                && r.disabled
                    .iter()
                    .any(|s| s == "@deepseek-ai/dsh-tool-bash-persistent"),
            "win32-gated bash rows disabled: {:?}",
            r.disabled
        );
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

    /// 换 preset = 原有绑定 rebind 到另一 standing scope；agent 视图随之切换。
    #[test]
    fn rejoin_rebinds_parent_and_switches_view() {
        let sp = new_sp();
        let mut reg = StandingRegistry::new(sp.clone());
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
        let mut reg = StandingRegistry::new(sp.clone());
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
}
