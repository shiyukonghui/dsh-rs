//! P1-b：preset 发现宿主——web RPC `agentPreset.list/read` 的 domain 侧。
//!
//! 语义镜像 `@deepseek-ai/dsh-agent-presets` 的**发现**部分（mount/guard 是 P2）：
//! 发现**不缓存**（TS 亦不缓存——每次 list/resolve 重读根，文件改动立即可见）；
//! 根序列 = 部署固定根（env `DSH_PRESET_ROOT` > cwd 相对仓库 `resources/agent-presets`）
//! + 用户根 `<dshHome>/.agent-presets`（无条件追加，align `includeUserRoot:true`；
//! 目录不存在 discover 天然跳过）。default 会话不隐式 join（D-103/C-04）：`isDefault`
//! 只标「新会话未选时的初始选择」，来自 settings `agent-presets.default`（base=工程默认）。

use std::path::{Path, PathBuf};

use dsh_agent_presets::{
    discovery::discover_presets, home::*, AgentPreset, PresetRoot, PresetTrust,
};
use serde_json::Value;

/// 部署固定内置根的环境变量覆盖（正式部署/独立装配注入；缺省回退 cwd 相对仓库根）。
pub const SHIPPED_PRESET_ROOT_ENV: &str = "DSH_PRESET_ROOT";

/// 工程默认预设 id（settings `agent-presets.default` 的 base 值；部署固定值）。
pub const DEPLOYMENT_DEFAULT_PRESET: &str = "standard";

/// 发现宿主：持有根序列 + 用户根（authorable 探测）。
pub struct PresetHost {
    roots: Vec<PresetRoot>,
    user_root: Option<PathBuf>,
}

impl PresetHost {
    /// 显式根 + 用户根（测试注入用；authorable = 用户根目录存在）。
    pub fn with_user_root(roots: Vec<PresetRoot>, user_root: Option<PathBuf>) -> Self {
        PresetHost { roots, user_root }
    }

    /// 全部预设（发现不缓存；顺序 = order-else-id、每 id 首根胜出）。
    pub fn roster(&self) -> Vec<AgentPreset> {
        discover_presets(&self.roots)
    }

    /// 按 id 查找；无 → None（wire「agent-preset-not-found」）。
    pub fn find(&self, id: &str) -> Option<AgentPreset> {
        self.roster().into_iter().find(|p| p.id == id)
    }

    /// authorable = 用户根目录存在（存在即真；D-103/B-04）。
    pub fn authorable(&self) -> bool {
        match &self.user_root {
            Some(p) => p.is_dir(),
            None => false,
        }
    }
}

impl Default for PresetHost {
    /// 部署默认：env/cwd 固定根 + 用户根。
    fn default() -> Self {
        PresetHost {
            roots: default_roots(),
            user_root: user_root_path(),
        }
    }
}

/// 部署默认根序列：env 固定根 > cwd 相对仓库根，然后无条件追加用户根。
pub fn default_roots() -> Vec<PresetRoot> {
    let cwd = std::env::current_dir().ok();
    let shipped = cwd.as_deref().and_then(|c| {
        resolve_shipped_root(c, std::env::var(SHIPPED_PRESET_ROOT_ENV).ok().as_deref())
    });
    let mut roots = Vec::new();
    if let Some(s) = shipped {
        roots.push(PresetRoot {
            path: s,
            trust: PresetTrust::System,
        });
    }
    if let Some(ur) = user_root_path() {
        roots.push(PresetRoot {
            path: ur,
            trust: PresetTrust::User,
        });
    }
    roots
}

/// 固定（system）根解析：env 非空白且为目录 → 用它；否则 cwd 相对 `resources/agent-presets`。
pub fn resolve_shipped_root(cwd: &Path, dshenv: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = dshenv.map(str::trim).filter(|s| !s.is_empty()) {
        let pb = PathBuf::from(p);
        if pb.is_dir() {
            return Some(pb);
        }
    }
    let probe = cwd.join("resources").join("agent-presets");
    if probe.is_dir() {
        Some(probe)
    } else {
        None
    }
}

/// 用户根 = `<dshHome>/.agent-presets`（home 取 `$DSH_HOME` → `~/.dsh`）。
pub fn user_root_path() -> Option<PathBuf> {
    let home = std::env::var("USERPROFILE")
        .ok()
        .or_else(|| std::env::var("HOME").ok())?;
    let dsh_home = dsh_home_from(
        &PathBuf::from(home),
        std::env::var("DSH_HOME").ok().as_deref(),
    );
    Some(user_preset_root_path(&dsh_home))
}

/// 注册 settings `agent-presets` namespace（`{default}`，base=工程默认）。
pub fn register_agent_presets_settings(sp: &mut dsh_settings::SettingsProvider) {
    let mut dict = std::collections::HashMap::new();
    dict.insert("default".to_string(), dsh_schema::Schema::string());
    sp.register(
        "agent-presets",
        &dsh_schema::Schema::object(dict),
        Some(serde_json::json!({ "default": DEPLOYMENT_DEFAULT_PRESET })),
        dsh_settings::Applies::Live,
    );
}

/// roster 行 → wire `AgentPresetEntry`（id/trust/isDefault + 可选 name/description/broken）。
pub fn to_entry(p: &AgentPreset, is_default: bool) -> Value {
    let mut v = serde_json::Map::new();
    v.insert("id".to_string(), serde_json::json!(p.id));
    v.insert(
        "trust".to_string(),
        serde_json::json!(match p.trust {
            PresetTrust::System => "system",
            PresetTrust::User => "user",
        }),
    );
    v.insert("isDefault".to_string(), serde_json::json!(is_default));
    if let Some(n) = &p.name {
        v.insert("name".to_string(), serde_json::json!(n));
    }
    if let Some(d) = &p.description {
        v.insert("description".to_string(), serde_json::json!(d));
    }
    if let Some(b) = &p.broken {
        v.insert("broken".to_string(), serde_json::json!(b));
    }
    Value::Object(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dsh-cli-presets-{tag}-{}", std::process::id()));
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

    fn make_preset(root: &Path, id: &str, order: Option<u32>, name: Option<&str>) {
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        write(
            &dir.join("agent.cordis.yml"),
            "- id: p\n  name: 'plugin-x'\n",
        );
        let mut meta = String::new();
        if let Some(o) = order {
            meta.push_str(&format!("order: {o}\n"));
        }
        if let Some(n) = name {
            meta.push_str(&format!("name: {n}\n"));
        }
        if !meta.is_empty() {
            write(&dir.join("preset.yml"), &meta);
        }
    }

    fn roots_of(dir: &Path, trust: PresetTrust) -> Vec<PresetRoot> {
        vec![PresetRoot {
            path: dir.to_path_buf(),
            trust,
        }]
    }

    #[test]
    fn roster_dispatches_roots_and_orders() {
        let sys = tmp_dir("sys");
        let usr = tmp_dir("usr");
        make_preset(&sys, "standard", Some(1), Some("标准"));
        make_preset(&sys, "code", Some(2), None);
        make_preset(&usr, "mine", None, Some("我的"));

        let mut roots = roots_of(&sys, PresetTrust::System);
        roots.push(PresetRoot {
            path: usr.clone(),
            trust: PresetTrust::User,
        });
        let host = PresetHost::with_user_root(roots, Some(usr.clone()));

        let roster = host.roster();
        let ids: Vec<&str> = roster.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["standard", "code", "mine"]);
        assert_eq!(roster[0].trust, PresetTrust::System);
        assert_eq!(roster[2].trust, PresetTrust::User);
        assert_eq!(roster[0].name.as_deref(), Some("标准"));
        let _ = fs::remove_dir_all(&sys);
        let _ = fs::remove_dir_all(&usr);
    }

    #[test]
    fn find_returns_preset_or_none() {
        let sys = tmp_dir("find");
        make_preset(&sys, "abc", Some(1), None);
        let host = PresetHost::with_user_root(roots_of(&sys, PresetTrust::System), None);
        let found = host.find("abc").expect("present");
        assert_eq!(found.id, "abc");
        assert!(found.path.ends_with("abc/agent.cordis.yml"));
        assert!(host.find("nope").is_none());
        let _ = fs::remove_dir_all(&sys);
    }

    #[test]
    fn authorable_reflects_user_root_existence() {
        let sys = tmp_dir("authsys");
        let usr = tmp_dir("authusr");
        let missing =
            std::env::temp_dir().join(format!("dsh-cli-presets-missing-{}", std::process::id()));
        let mut roots = roots_of(&sys, PresetTrust::System);
        roots.push(PresetRoot {
            path: usr.clone(),
            trust: PresetTrust::User,
        });

        let real = PresetHost::with_user_root(roots.clone(), Some(usr.clone()));
        assert!(real.authorable(), "existing user root => authorable");
        let absent = PresetHost::with_user_root(roots.clone(), Some(missing));
        assert!(!absent.authorable(), "absent user root => not authorable");
        let none = PresetHost::with_user_root(roots, None);
        assert!(!none.authorable());
        let _ = fs::remove_dir_all(&sys);
        let _ = fs::remove_dir_all(&usr);
    }

    #[test]
    fn resolve_shipped_root_prefers_env_then_cwd_probe() {
        let base = tmp_dir("shipped");
        let env_dir = base.join("env-root");
        fs::create_dir_all(&env_dir).unwrap();
        let cwd = base.join("cwd");
        fs::create_dir_all(cwd.join("resources/agent-presets")).unwrap();

        // env 非空白 → env 优先（目录须存在）
        assert_eq!(
            resolve_shipped_root(&cwd, Some(env_dir.to_str().unwrap())),
            Some(env_dir.clone())
        );
        // env 空白 → 忽略 → cwd 相对探测
        assert_eq!(
            resolve_shipped_root(&cwd, Some("   ")),
            Some(cwd.join("resources/agent-presets"))
        );
        assert_eq!(
            resolve_shipped_root(&cwd, Some("")),
            Some(cwd.join("resources/agent-presets"))
        );
        // env 指向不存在目录 → 回退 cwd 探测
        assert_eq!(
            resolve_shipped_root(&cwd, Some(base.join("nope").to_str().unwrap())),
            Some(cwd.join("resources/agent-presets"))
        );
        // 两者皆无 → None
        let bare = base.join("bare");
        fs::create_dir_all(&bare).unwrap();
        assert_eq!(resolve_shipped_root(&bare, None), None);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn entry_wire_shape() {
        let p = AgentPreset {
            id: "code".into(),
            trust: PresetTrust::System,
            path: PathBuf::from("x/agent.cordis.yml"),
            name: Some("代码".into()),
            description: None,
            order: Some(2.0),
            broken: None,
        };
        let v = to_entry(&p, true);
        assert_eq!(v["id"], "code");
        assert_eq!(v["trust"], "system");
        assert_eq!(v["isDefault"], true);
        assert_eq!(v["name"], "代码");
        assert!(
            v.get("description").is_none(),
            "absent field omitted, not null"
        );
        assert!(v.get("broken").is_none());
        assert!(
            v.get("order").is_none(),
            "order is internal, not on the wire"
        );
    }
}
