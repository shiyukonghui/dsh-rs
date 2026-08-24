//! P1-b/P5：preset 发现宿主 + **作者流**——web RPC `agentPreset.list/read/copy/remove`
//! 的 domain 侧。
//!
//! 语义镜像 `@deepseek-ai/dsh-agent-presets` 的**发现**部分（mount/guard 是 P2）：
//! 发现**不缓存**（TS 亦不缓存——每次 list/resolve 重读根，文件改动立即可见）；
//! 根序列 = 部署固定根（env `DSH_PRESET_ROOT` > cwd 相对仓库 `resources/agent-presets`）
//! + 用户根 `<dshHome>/.agent-presets`（无条件追加，align `includeUserRoot:true`；
//! 目录不存在 discover 天然跳过）。default 会话不隐式 join（D-103/C-04）：`isDefault`
//! 只标「新会话未选时的初始选择」，来自 settings `agent-presets.default`（base=工程默认）。
//!
//! **作者流（P5）**：`copy_preset` 任意源 → 新 user 预设（组合逐字 + 元数据显式
//! 覆盖 > 源 > 无）；`remove_preset` 仅删 user 预设（system 拒绝）；判定全部 fail-loud
//!（`AuthoringError`），fs 失败绝不当成功。

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

    /// P5：从任意源 preset 复制为新 **user** 预设（写 `<user_root>/<new_id>/`：
    /// 组合逐字 + preset.yml 元数据 = 显式覆盖 > 源元数据 > 无）。fail-loud 校验：
    /// 非法 id / 源不存在 / 目标 id 已存在（任一 root，首根胜出遮蔽）/ 用户根不可写。
    pub fn copy_preset(
        &self,
        from_id: &str,
        new_id: &str,
        with_name: Option<&str>,
        with_description: Option<&str>,
    ) -> Result<String, AuthoringError> {
        use std::fs;
        if !dsh_agent_presets::is_valid_preset_id(new_id) {
            return Err(AuthoringError::InvalidId(new_id.to_string()));
        }
        let roster = self.roster();
        let roster_ref = &roster;
        let src = roster_ref
            .iter()
            .find(|p| p.id == from_id)
            .ok_or_else(|| AuthoringError::NotFound(from_id.to_string()))?;
        if roster_ref.iter().any(|p| p.id == new_id) {
            return Err(AuthoringError::AlreadyExists(new_id.to_string()));
        }
        let user_root = self
            .user_root
            .as_ref()
            .filter(|p| p.is_dir())
            .ok_or(AuthoringError::NotAuthorable)?;
        let composition = fs::read_to_string(&src.path).map_err(|e| {
            AuthoringError::Io(format!(
                "cannot read source composition {}: {e}",
                src.path.display()
            ))
        })?;
        let target_dir = user_root.join(new_id);
        fs::create_dir_all(&target_dir)
            .map_err(|e| AuthoringError::Io(format!("create {}: {e}", target_dir.display())))?;
        let comp_path = target_dir.join(dsh_agent_presets::COMPOSITION_FILE);
        fs::write(&comp_path, composition)
            .map_err(|e| AuthoringError::Io(format!("write {}: {e}", comp_path.display())))?;
        let name = with_name.map(String::from).or_else(|| src.name.clone());
        let description = with_description
            .map(String::from)
            .or_else(|| src.description.clone());
        let mut meta = serde_json::Map::new();
        if let Some(n) = name {
            meta.insert("name".to_string(), serde_json::json!(n));
        }
        if let Some(d) = description {
            meta.insert("description".to_string(), serde_json::json!(d));
        }
        if !meta.is_empty() {
            let meta_yml = serde_yaml::to_string(&serde_json::Value::Object(meta))
                .map_err(|e| AuthoringError::Io(format!("serialize preset.yml: {e}")))?;
            let meta_path = target_dir.join("preset.yml");
            fs::write(&meta_path, meta_yml)
                .map_err(|e| AuthoringError::Io(format!("write {}: {e}", meta_path.display())))?;
        }
        Ok(new_id.to_string())
    }

    /// P5：删除 **user** 预设（仅用户根下的可由作者删除；system 资产拒绝删除）。
    /// fs 失败 fail-loud（绝不假装删除成功）。
    pub fn remove_preset(&self, id: &str) -> Result<String, AuthoringError> {
        let found = self
            .roster()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| AuthoringError::NotFound(id.to_string()))?;
        if found.trust != PresetTrust::User {
            return Err(AuthoringError::ReadOnly(id.to_string()));
        }
        let dir = found
            .path
            .parent()
            .ok_or_else(|| AuthoringError::Io(format!("preset \"{id}\" has no directory")))?;
        std::fs::remove_dir_all(dir)
            .map_err(|e| AuthoringError::Io(format!("remove {}: {e}", dir.display())))?;
        Ok(id.to_string())
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

/// P5 作者流错误：wire error envelope 用 `code()`（统一 `agent-preset-*` 前缀）。
#[derive(Debug, Clone, PartialEq)]
pub enum AuthoringError {
    /// 目标 id 非法（`PRESET_ID`）。
    InvalidId(String),
    /// 源/待删预设不存在。
    NotFound(String),
    /// 目标 id 已存在（任一 root——首根胜出会遮蔽）。
    AlreadyExists(String),
    /// 无用户根 / 不可写（authorable=false）。
    NotAuthorable,
    /// system 预设不可删除。
    ReadOnly(String),
    /// 文件系统失败（fail-loud，不假装成功）。
    Io(String),
}

impl AuthoringError {
    pub fn code(&self) -> &'static str {
        match self {
            AuthoringError::InvalidId(_) => "agent-preset-invalid-id",
            AuthoringError::NotFound(_) => "agent-preset-not-found",
            AuthoringError::AlreadyExists(_) => "agent-preset-exists",
            AuthoringError::NotAuthorable => "agent-presets-not-authorable",
            AuthoringError::ReadOnly(_) => "agent-preset-readonly",
            AuthoringError::Io(_) => "agent-preset-io",
        }
    }

    pub fn message(&self) -> String {
        match self {
            AuthoringError::InvalidId(id) => {
                format!("preset id \"{id}\" must match /^[a-z0-9][a-z0-9-]*$/")
            }
            AuthoringError::NotFound(id) => format!("no preset \"{id}\" in the roster"),
            AuthoringError::AlreadyExists(id) => {
                format!("preset \"{id}\" already exists — remove it first or pick another id")
            }
            AuthoringError::NotAuthorable => {
                "no authorable user preset root (user root missing)".to_string()
            }
            AuthoringError::ReadOnly(id) => {
                format!("preset \"{id}\" is a system preset and cannot be removed")
            }
            AuthoringError::Io(m) => m.clone(),
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

    // —— P5：作者流（copy/remove 写 user root，fail-loud 校验）——

    #[test]
    fn copy_preset_creates_user_preset_with_composition_and_meta() {
        let sys = tmp_dir("cpysys");
        let usr = tmp_dir("cpyusr");
        let src = sys.join("standard");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("agent.cordis.yml"),
            "- id: p\n  name: 'plugin-x'\n",
        )
        .unwrap();
        fs::write(src.join("preset.yml"), "name: 标准\n").unwrap();
        let mut roots = roots_of(&sys, PresetTrust::System);
        roots.push(PresetRoot {
            path: usr.clone(),
            trust: PresetTrust::User,
        });
        let host = PresetHost::with_user_root(roots, Some(usr.clone()));

        let id = host
            .copy_preset(
                "standard",
                "my-standard",
                Some("我的标准"),
                Some("from standard"),
            )
            .expect("copy");
        assert_eq!(id, "my-standard");
        // 文件落地：组合逐字 + preset.yml 用显式覆盖元数据。
        assert_eq!(
            fs::read_to_string(usr.join("my-standard/agent.cordis.yml")).unwrap(),
            "- id: p\n  name: 'plugin-x'\n"
        );
        let meta = fs::read_to_string(usr.join("my-standard/preset.yml")).unwrap();
        assert!(meta.contains("我的标准") && meta.contains("from standard"));
        // roster 即见（发现不缓存）：trust=user。
        let found = host.find("my-standard").expect("discovered after copy");
        assert_eq!(found.trust, PresetTrust::User);
        assert_eq!(found.name.as_deref(), Some("我的标准"));
        let _ = fs::remove_dir_all(&sys);
        let _ = fs::remove_dir_all(&usr);
    }

    #[test]
    fn copy_preset_fails_loud_on_bad_ids_and_collisions() {
        let sys = tmp_dir("cpybad");
        let usr = tmp_dir("cpybadusr");
        make_preset(&sys, "standard", Some(1), None);
        let mut roots = roots_of(&sys, PresetTrust::System);
        roots.push(PresetRoot {
            path: usr.clone(),
            trust: PresetTrust::User,
        });
        let host = PresetHost::with_user_root(roots.clone(), Some(usr.clone()));

        assert_eq!(
            host.copy_preset("standard", "Bad_Id", None, None),
            Err(AuthoringError::InvalidId("Bad_Id".into()))
        );
        assert_eq!(
            host.copy_preset("nope", "fresh", None, None),
            Err(AuthoringError::NotFound("nope".into()))
        );
        // 目标 id 与既有（system）撞 → 拒绝（首根胜出遮蔽）。
        assert_eq!(
            host.copy_preset("standard", "standard", None, None),
            Err(AuthoringError::AlreadyExists("standard".into()))
        );
        // 用户根缺失 → NotAuthorable。
        let missing = std::env::temp_dir().join(format!("dsh-cpy-nousr-{}", std::process::id()));
        let host2 = PresetHost::with_user_root(roots.clone(), Some(missing));
        assert_eq!(
            host2.copy_preset("standard", "fresh", None, None),
            Err(AuthoringError::NotAuthorable),
            "no user root dir => cannot author"
        );
        let _ = fs::remove_dir_all(&sys);
        let _ = fs::remove_dir_all(&usr);
    }

    #[test]
    fn remove_preset_deletes_user_only_and_refuses_system() {
        let sys = tmp_dir("rmsys");
        let usr = tmp_dir("rmusr");
        make_preset(&sys, "standard", Some(1), None);
        make_preset(&usr, "mine", None, Some("我的"));
        let mut roots = roots_of(&sys, PresetTrust::System);
        roots.push(PresetRoot {
            path: usr.clone(),
            trust: PresetTrust::User,
        });
        let host = PresetHost::with_user_root(roots, Some(usr.clone()));

        // 未知 → NotFound。
        assert_eq!(
            host.remove_preset("nope"),
            Err(AuthoringError::NotFound("nope".into()))
        );
        // system → ReadOnly（不删部署资产）。
        assert_eq!(
            host.remove_preset("standard"),
            Err(AuthoringError::ReadOnly("standard".into()))
        );
        assert!(
            sys.join("standard/agent.cordis.yml").exists(),
            "system preset intact"
        );
        // user → 删除目录、roster 即去。
        assert_eq!(host.remove_preset("mine"), Ok("mine".to_string()));
        assert!(!usr.join("mine").exists(), "user preset dir removed");
        assert!(host.find("mine").is_none(), "roster reflects removal");
        let _ = fs::remove_dir_all(&sys);
        let _ = fs::remove_dir_all(&usr);
    }
}
