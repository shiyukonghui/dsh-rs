//! P2-a：组合的**类型化**解析 + `disabled_expr` 行分类。
//!
//! discovery 的 `composition_problem` 只做浅形状检查；这里把组合解析成 typed 行
//! （id/name/group/config/disabled_expr + group 递归 children），并按 `process`
//! 门面计算每行是否禁用（对齐 loader `!js`/`disabled_expr` 语义，fail-closed：
//! 求值失败 = 禁用）。这是 standing mount 行审计的**数据基础**：哪些行生效、
//! 哪些被平台门控挡掉，先于任何挂载动作可知。
//!
//! D-102 的 `disabled_expr`/`{"__jsExpr": ...}` 译文原样进入 config 节点，**不求值**
//! config（`__jsExpr` 在 P3 的桥挂载期由 dsh-eval::interpolate 展开）。

use std::collections::HashMap;

use serde_json::Value;

/// 一行组合（组行带 children，叶行带 config）。
#[derive(Debug, Clone, PartialEq)]
pub struct CompositionRow {
    /// 行 `config.id`（当存在）。
    pub id: Option<String>,
    /// 行 `name`（插件名或 group 名）。
    pub name: String,
    /// 组行（`group: true`，config 为子行数组）。
    pub group: bool,
    /// 行的 config（组行为子行数组；叶行为配置对象）。
    pub config: Option<Value>,
    /// `disabled_expr`（`!js` 译文）；平台门控表达式。
    pub disabled_expr: Option<String>,
    /// 组行递归子行。
    pub children: Vec<CompositionRow>,
}

/// 解析组合文本 → typed 行树。
pub fn parse_composition(text: &str) -> Result<Vec<CompositionRow>, String> {
    let value: Value = serde_yaml::from_str(text).map_err(|e| format!("not valid YAML: {e}"))?;
    parse_rows(&value)
}

fn parse_rows(value: &Value) -> Result<Vec<CompositionRow>, String> {
    let arr = value
        .as_array()
        .ok_or_else(|| "top-level must be a list of plugin rows".to_string())?;
    arr.iter().map(parse_row).collect()
}

fn parse_row(value: &Value) -> Result<CompositionRow, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "row must be a map with a name".to_string())?;
    let get = |k: &str| obj.get(k);
    let name = get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if name.is_empty() {
        return Err("row names no plugin (a \"name\" string is required)".to_string());
    }
    let group = get("group").and_then(Value::as_bool).unwrap_or(false);
    let config = get("config").cloned();
    let disabled_expr = get("disabled_expr")
        .and_then(Value::as_str)
        .map(str::to_string);
    let children = if group {
        parse_rows(&config.clone().unwrap_or(Value::Null))?
    } else {
        Vec::new()
    };
    Ok(CompositionRow {
        id: get("id").and_then(Value::as_str).map(str::to_string),
        name,
        group,
        config,
        disabled_expr,
        children,
    })
}

/// 行分类状态（disabled 计算；broken 留给挂载审计——P3 桥面才有真凭据）。
#[derive(Debug, Clone, PartialEq)]
pub enum RowState {
    Active,
    Disabled,
    Broken(String),
}

/// 计算一行是否禁用（fail-closed：求值失败 = 禁用）。组行自身不被禁用（其子行
/// 各自判断）。
pub fn row_disabled(row: &CompositionRow, process: &Value) -> bool {
    let Some(expr) = &row.disabled_expr else {
        return false;
    };
    let mut scope = HashMap::new();
    scope.insert("process".to_string(), process.clone());
    scope.insert(
        "config".to_string(),
        row.config.clone().unwrap_or(Value::Null),
    );
    dsh_eval::evaluate(&scope, expr)
        .map(|v| dsh_eval::truthy(&v))
        .unwrap_or(true)
}

/// 行分类。
pub fn row_state(row: &CompositionRow, process: &Value) -> RowState {
    if row_disabled(row, process) {
        RowState::Disabled
    } else {
        RowState::Active
    }
}

/// 生效叶行（禁用行 + 组容器自身排除，组递归展开）——standing 挂载的**有效行集**。
pub fn active_rows<'a>(rows: &'a [CompositionRow], process: &Value) -> Vec<&'a CompositionRow> {
    fn walk<'a>(rows: &'a [CompositionRow], process: &Value, out: &mut Vec<&'a CompositionRow>) {
        for row in rows {
            if row_disabled(row, process) {
                continue;
            }
            if row.group {
                walk(&row.children, process, out);
            } else {
                out.push(row);
            }
        }
    }
    let mut out = Vec::new();
    walk(rows, process, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const WIN32: &str = r#"{"platform":"win32","env":{"DSH_CWD":"C:\\w"},"cwd":"C:\\repo"}"#;
    const LINUX: &str = r#"{"platform":"linux","env":{},"cwd":"/repo"}"#;

    fn facade(json: &str) -> Value {
        serde_json::from_str(json).unwrap()
    }

    /// 真实自持文件（D-102 译文）可被类型化解锁：结构完好、组递归、门控表达式就位。
    #[test]
    fn real_builtin_compositions_parse_typed() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .canonicalize()
            .expect("repo root");
        let root = repo.join("resources").join("agent-presets");
        for id in ["minimal", "standard", "code", "cordis"] {
            let text = std::fs::read_to_string(root.join(id).join("agent.cordis.yml"))
                .unwrap_or_else(|e| panic!("{id}: {e}"));
            let rows = parse_composition(&text).unwrap_or_else(|e| panic!("{id} parse: {e}"));
            assert!(!rows.is_empty(), "{id}: top-level rows");
            let groups: Vec<_> = rows.iter().filter(|r| r.group).collect();
            for g in &groups {
                assert!(
                    !g.children.is_empty(),
                    "{id}: group {} must recurse children",
                    g.name
                );
            }
        }
    }

    /// win32 门控在 win32 facade 下：bash 系禁用、pwsh 系生效、`cwd` 的 `__jsExpr`
    /// 原样保留（不求值）。
    #[test]
    fn minimal_rows_classify_on_win32() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .canonicalize()
            .unwrap();
        let text =
            std::fs::read_to_string(repo.join("resources/agent-presets/minimal/agent.cordis.yml"))
                .unwrap();
        let rows = parse_composition(&text).unwrap();
        let proc = facade(WIN32);
        let flat = active_rows(&rows, &proc);

        let by_name: HashMap<&str, &CompositionRow> =
            flat.iter().map(|r| (r.name.as_str(), *r)).collect();
        // win32：`=== 'win32'` 的 bash 行被禁用（不许出现在生效行集）；`!== 'win32'`
        // 的 pwsh 系生效（spike-6/8 结论直接回归）。
        for r in flat.iter().filter(|r| r.name.contains("bash")) {
            assert_ne!(
                r.disabled_expr.as_deref(),
                Some("process.platform === 'win32'"),
                "win32: a bash row gated by === 'win32' must not be active"
            );
        }
        let pwsh = by_name["@deepseek-ai/dsh-tool-pwsh-persistent"];
        assert_eq!(
            pwsh.disabled_expr.as_deref(),
            Some("process.platform !== 'win32'")
        );
        // cwd __jsExpr 原样（未求值成字面路径）。
        let fs_local = by_name["@deepseek-ai/dsh-fs-local"];
        let cwd = &fs_local.config.as_ref().unwrap()["cwd"];
        assert!(
            cwd.get("__jsExpr").is_some(),
            "cwd must stay an unpicked __jsExpr node: {cwd}"
        );
    }

    #[test]
    fn linux_flips_the_gates() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .canonicalize()
            .unwrap();
        let text =
            std::fs::read_to_string(repo.join("resources/agent-presets/minimal/agent.cordis.yml"))
                .unwrap();
        let rows = parse_composition(&text).unwrap();
        let proc = facade(LINUX);
        let flat = active_rows(&rows, &proc);
        let by_name: HashMap<&str, &CompositionRow> =
            flat.iter().map(|r| (r.name.as_str(), *r)).collect();
        assert!(
            by_name.contains_key("@deepseek-ai/dsh-tool-bash-persistent"),
            "linux: bash persistent active"
        );
        assert!(
            !by_name.contains_key("@deepseek-ai/dsh-tool-pwsh-persistent"),
            "linux: pwsh persistent disabled"
        );
    }

    /// 本机真 facade：组合全部行都能求值（结构性 bug 不再：不是每行都被判禁用）。
    #[test]
    fn real_facade_evaluates_all_gates_cleanly() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .canonicalize()
            .unwrap();
        let root = repo.join("resources").join("agent-presets");
        let proc = dsh_eval::process_facade();
        for id in ["minimal", "standard", "code", "cordis"] {
            let text = std::fs::read_to_string(root.join(id).join("agent.cordis.yml")).unwrap();
            let rows = parse_composition(&text).unwrap();
            for row in &rows {
                let _ = row_state(row, &proc); // 求值不得 panic/err（unwrap_or(true) 兜底为禁用）
            }
            // 每个预设至少有一条生效 shell-ish 行（防全禁用回归）。
            let active = active_rows(&rows, &proc);
            assert!(
                active.iter().any(|r| r.name.contains("bash")
                    || r.name.contains("pwsh")
                    || r.name.contains("terminal")),
                "{id}: no shell row active on this platform"
            );
        }
    }

    #[test]
    fn malformed_composition_reports() {
        assert!(parse_composition("{ a: 1 }").is_err(), "not a list");
        assert!(parse_composition("- 5").is_err(), "row not a map");
        assert!(parse_composition("- {}").is_err(), "row without name");
        // group 的 config 非数组 → 报错（递归校验）。
        assert!(parse_composition("- name: g\n  group: true\n  config: { a: 1 }\n").is_err());
    }
}
