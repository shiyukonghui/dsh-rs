//! 预设发现：`scan_root` / `discover_presets` + 组合形状健康检查。
//!
//! 语义镜像 `@deepseek-ai/dsh-agent-presets` discovery：
//! - absent 根 → 无预设（不抛错）；目录名匹配 `PRESET_ID` 才是行。
//! - broken = 组合文件缺失 → 原因说明；存在但不可装载 → 形状/解析原因。
//! - 排序：`order`（缺省 Infinity）升序，并列按 id；`discover` 每 id 首根胜出。

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::{
    is_valid_preset_id, metadata::read_preset_metadata, AgentPreset, PresetRoot, COMPOSITION_FILE,
};

/// 扫描一个根目录 → 预设 roster（有序）。
///
/// absent 根 → 无预设（不抛错）；仅目录名匹配 `PRESET_ID` 的是行（`.DS_Store` 级
/// 残留跳过——既不能 claim 为 id，报了反而让人学会忽略 broken 标记）；每行 broken =
/// 组合缺失（原因）或不可装载（解析/形状原因）；`order`（缺省 Infinity）升序、并列按 id
/// （**确定性差异**：TS 对无 order 同层用 readdir 稳定序（`Infinity-Infinity=NaN`），
/// 文件名相关；Rust 用 id 字典序，跨文件系统可复现）。
pub fn scan_root(root: &PresetRoot) -> Vec<AgentPreset> {
    let read = match fs::read_dir(&root.path) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut found = Vec::new();
    for entry in read.flatten() {
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if !ft.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_valid_preset_id(&name) {
            continue;
        }
        let preset_dir = entry.path();
        let path = preset_dir.join(COMPOSITION_FILE);
        let broken = if path.is_file() {
            composition_problem(&path)
        } else {
            Some(format!(
                "the composition file {COMPOSITION_FILE} is missing — the directory still occupies the id; delete it or restore the file"
            ))
        };
        let meta = read_preset_metadata(&preset_dir);
        found.push(AgentPreset {
            id: name,
            trust: root.trust,
            path,
            name: meta.name,
            description: meta.description,
            order: meta.order,
            broken,
        });
    }
    found.sort_by(|a, b| match (a.order, b.order) {
        (Some(x), Some(y)) => x
            .partial_cmp(&y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.id.cmp(&b.id),
    });
    found
}

/// 扫描所有根（按传入顺序），每 id **首根胜出**，保持首见顺序（对齐 TS Map 插入序）。
pub fn discover_presets(roots: &[PresetRoot]) -> Vec<AgentPreset> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for root in roots {
        for preset in scan_root(root) {
            if seen.insert(preset.id.clone()) {
                out.push(preset);
            }
        }
    }
    out
}

/// 组合文件为何不可装载（解析/形状），`None` = 可装载。已知 D-102 忠实转译产物
/// （`disabled_expr` string / `{__jsExpr}` map / skills 数组）天然通过：**不求值**。
pub fn composition_problem(path: &Path) -> Option<String> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Some("the composition file agent.cordis.yml cannot be read".to_string()),
    };
    let value: Value = match serde_yaml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            let first = e.to_string().lines().next().unwrap_or_default().to_string();
            return Some(format!("the composition is not valid YAML: {first}"));
        }
    };
    entry_list_problem(&value, "")
}

/// 浅形状检查（TS `entryListProblem`）：顶层数组 + 每行 map 带 string `name`，
/// group 递归 `config` 数组。必须接受 loader 的一切：不求值表达式、不校验插件存在。
fn entry_list_problem(rows: &Value, at: &str) -> Option<String> {
    let arr = match rows.as_array() {
        Some(a) => a,
        None => {
            return Some(if at.is_empty() {
                "the composition must be a top-level list of plugin rows".to_string()
            } else {
                format!("group {at} must hold a list of plugin rows")
            })
        }
    };
    for (index, row) in arr.iter().enumerate() {
        let label = if at.is_empty() {
            format!("row {}", index + 1)
        } else {
            format!("{at} row {}", index + 1)
        };
        let obj = match row.as_object() {
            Some(o) => o,
            None => {
                return Some(format!(
                    "{label} is not a plugin row (expected a map with a \"name\")"
                ))
            }
        };
        let name = obj.get("name").and_then(Value::as_str).unwrap_or_default();
        if name.is_empty() {
            return Some(format!(
                "{label} names no plugin (a \"name\" string is required)"
            ));
        }
        let is_group = obj.get("group").and_then(Value::as_bool).unwrap_or(false);
        if is_group {
            let nested = obj.get("config").unwrap_or(&Value::Null);
            if let Some(problem) = entry_list_problem(nested, &label) {
                return Some(problem);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PresetTrust;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dsh-presets-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn make_preset(root: &Path, id: &str, comp: Option<&str>, meta: Option<&str>) {
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        if let Some(c) = comp {
            write(&dir.join(COMPOSITION_FILE), c);
        }
        if let Some(m) = meta {
            write(&dir.join("preset.yml"), m);
        }
    }

    fn root_of(dir: PathBuf, trust: PresetTrust) -> PresetRoot {
        PresetRoot { path: dir, trust }
    }

    #[test]
    fn scan_absent_root_yields_none() {
        let dir = tmp_dir("absent").join("nope");
        assert_eq!(
            scan_root(&root_of(dir, PresetTrust::User)),
            Vec::<AgentPreset>::new()
        );
    }

    #[test]
    fn scan_rosters_ordered_and_broken() {
        let dir = tmp_dir("roster");
        let comp = "- id: p\n  name: 'plugin-x'\n";
        make_preset(&dir, "alpha", Some(comp), Some("order: 2\n"));
        make_preset(&dir, "zebra", Some(comp), None); // 无 metadata → order None（Infinity）
        make_preset(&dir, "bad-1", None, Some("name: broken\n")); // 组合缺失 → broken
        make_preset(&dir, ".DS_Store", Some(comp), None); // 非法 id → 跳过
        make_preset(&dir, "UPPER", Some(comp), None); // 非法 id → 跳过

        let roster = scan_root(&root_of(dir.clone(), PresetTrust::System));
        let ids: Vec<&str> = roster.iter().map(|p| p.id.as_str()).collect();
        // order: alpha(2) → 无 order 层（Infinity）：bad-1 与 zebra 均无 order，按 id 字典序
        // （deterministic 差异：TS 对同层用 readdir 稳定序，文件系统相关——见 scan_root doc）
        assert_eq!(ids, vec!["alpha", "bad-1", "zebra"]);
        assert_eq!(roster[0].trust, PresetTrust::System);
        assert_eq!(roster[0].order, Some(2.0));
        assert_eq!(roster[1].broken.as_deref(), Some(
            "the composition file agent.cordis.yml is missing — the directory still occupies the id; delete it or restore the file"
        ));
        assert_eq!(
            roster[2].broken, None,
            "zebra (valid composition) must not be broken"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_first_root_wins_per_id() {
        let sys = tmp_dir("sys");
        let usr = tmp_dir("usr");
        let comp = "- id: p\n  name: 'plugin-x'\n";
        make_preset(&sys, "standard", Some(comp), Some("order: 9\n"));
        make_preset(&sys, "minimal", Some(comp), Some("order: 3\n"));
        make_preset(&usr, "standard", Some(comp), Some("order: 1\n")); // 用户覆盖内置
        make_preset(&usr, "mine", Some(comp), Some("name: 我的\n"));

        let roster = discover_presets(&[
            root_of(sys.clone(), PresetTrust::System),
            root_of(usr.clone(), PresetTrust::User),
        ]);
        let ids: Vec<&str> = roster.iter().map(|p| p.id.as_str()).collect();
        // 系统根 scan_root 已按 order 排序：minimal(3) < standard(9)；然后用户根的 mine。
        // 用户根的 standard 被跳过（首根胜出）。
        assert_eq!(ids, vec!["minimal", "standard", "mine"]);
        let std = roster.iter().find(|p| p.id == "standard").unwrap();
        assert_eq!(std.trust, PresetTrust::System);
        assert_eq!(
            std.order,
            Some(9.0),
            "first-root wins: system order 9 must survive"
        );
        let _ = fs::remove_dir_all(&sys);
        let _ = fs::remove_dir_all(&usr);
    }

    #[test]
    fn composition_problem_reports_shapes() {
        let dir = tmp_dir("shape");
        let good = dir.join("good.yml");
        write(&good, "- id: p\n  name: 'plugin-x'\n  config: { a: 1 }\n");
        assert_eq!(composition_problem(&good), None);

        let not_list = dir.join("not-list.yml");
        write(&not_list, "{ a: 1 }\n");
        assert!(composition_problem(&not_list)
            .unwrap()
            .contains("must be a top-level list"));

        let no_name = dir.join("no-name.yml");
        write(&no_name, "- id: p\n  config: {}\n");
        assert!(composition_problem(&no_name)
            .unwrap()
            .contains("names no plugin"));

        let bad_group = dir.join("bad-group.yml");
        write(
            &bad_group,
            "- id: g\n  name: 'c:group'\n  group: true\n  config: [1, 2]\n",
        );
        assert!(composition_problem(&bad_group)
            .unwrap()
            .contains("not a plugin row"));

        let garbage = dir.join("garbage.yml");
        write(&garbage, "]: not yaml\n");
        assert!(composition_problem(&garbage)
            .unwrap()
            .contains("not valid YAML"));

        let missing = dir.join("missing.yml");
        assert!(composition_problem(&missing)
            .unwrap()
            .contains("cannot be read")); // 不可读 = broken 原因
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn preset_id_validation_alignment() {
        assert!(is_valid_preset_id("standard"));
        assert!(is_valid_preset_id("a1-b2"));
        assert!(is_valid_preset_id("0x"));
        assert!(!is_valid_preset_id("Standard"));
        assert!(!is_valid_preset_id("s_"));
        assert!(!is_valid_preset_id("-x"));
        assert!(!is_valid_preset_id(""));
        assert!(!is_valid_preset_id(".DS_Store"));
    }

    #[test]
    fn real_builtin_root_discoverable() {
        // P1 验收：D-102 自持的 4 个内置预设全部发现且**可装载**（broken=None）。
        // 同时证明 D-102 忠实转译产物（12 个 disabled_expr/__jsExpr 节点）通过 P1 形状检查。
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .canonicalize()
            .expect("repo root");
        let builtin = repo.join("resources").join("agent-presets");
        assert!(
            builtin.is_dir(),
            "self-hosted preset root missing: {builtin:?}"
        );
        let roster = scan_root(&root_of(builtin, PresetTrust::System));
        let mut ids: Vec<&str> = roster.iter().map(|p| p.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["code", "cordis", "minimal", "standard"]);
        for preset in &roster {
            assert!(
                preset.broken.is_none(),
                "preset {} must be loadable: {:?}",
                preset.id,
                preset.broken
            );
            assert!(preset.path.is_file());
        }
    }

    #[test]
    fn real_builtin_plus_custom_user_root() {
        // P1 验收：4 内置 + 1 自定义（用户根）发现；用户根同 id 覆盖内置（首根胜出）。
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .canonicalize()
            .expect("repo root");
        let builtin = repo.join("resources").join("agent-presets");
        let usr = tmp_dir("custom");
        let comp = "- id: mine-tool\n  name: 'plugin-x'\n";
        make_preset(
            &usr,
            "minimal",
            Some(comp),
            Some("name: 我的极简\norder: 1\n"),
        );
        make_preset(&usr, "mine", Some(comp), None);

        let roster = discover_presets(&[
            root_of(builtin, PresetTrust::System),
            root_of(usr.clone(), PresetTrust::User),
        ]);
        let by_id: std::collections::HashMap<&str, &AgentPreset> =
            roster.iter().map(|p| (p.id.as_str(), p)).collect();
        assert!(by_id.contains_key("code"));
        assert!(by_id.contains_key("standard"));
        assert!(by_id.contains_key("mine"));
        let minimal = by_id.get("minimal").expect("minimal present");
        assert_eq!(
            minimal.trust,
            PresetTrust::System,
            "built-in minimal wins over user override by first-root"
        );
        assert!(minimal.broken.is_none());
        let _ = fs::remove_dir_all(&usr);
    }
}
