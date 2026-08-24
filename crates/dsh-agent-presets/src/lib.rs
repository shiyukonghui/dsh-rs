//! dsh-agent-presets——per-session agent preset 组合（路径 B：组合权威归位
//! dsh-core/loader + 窄服务桥）。
//!
//! P1（本阶段）：解析 / 发现 / 根。镜像 `@deepseek-ai/dsh-agent-presets` 的发现语义：
//! `scanRoot`/`discoverPresets`（absent 根 → 无；目录名 = preset id（`PRESET_ID` /
//! `^[a-z0-9][a-z0-9-]*$/`）；broken = 组合缺失或不可装载；`order`-else-`id` 排序；每 id
//! **首根胜出**）+ `dshHome` 用户根约定（`$DSH_HOME`→空白忽略→`~/.dsh`；用户根
//! `<dshHome>/.agent-presets` trust=user，authorable=目录存在即真，D-103/B-04）。
//!
//! 组合文件默认只做**浅形状检查**（顶层数组 + 每行 map 带 string `name`，group 递归
//! `config` 数组）——与 loader 接受度同向，但**不求值** `{__jsExpr}`/`disabled_expr`
//! （求值在挂载期，P2+）。D-102 的忠实转译产物天然通过此检查。

pub mod discovery;
pub mod home;
pub mod metadata;

use std::path::PathBuf;

/// 组合文件名（容器目录内唯一入口）。
pub const COMPOSITION_FILE: &str = "agent.cordis.yml";
/// dsh 用户数据根目录名（默认 `~/.dsh`）。
pub const DSH_HOME_DIR_NAME: &str = ".dsh";
/// dsh home 的环境变量名（最高优先，空/纯空白视为未设）。
pub const DSH_HOME_ENV: &str = "DSH_HOME";

/// preset id 合法字符（对齐 TS `PRESET_ID`）。
pub fn is_valid_preset_id(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// 预设根信任级（对齐 TS `trust: 'system' | 'user'`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetTrust {
    System,
    User,
}

/// 预设根：目录 + 信任级。
#[derive(Debug, Clone)]
pub struct PresetRoot {
    pub path: PathBuf,
    pub trust: PresetTrust,
}

/// 一个已发现的预设（roster 行）。
#[derive(Debug, Clone, PartialEq)]
pub struct AgentPreset {
    pub id: String,
    pub trust: PresetTrust,
    /// 组合文件绝对路径。
    pub path: PathBuf,
    pub name: Option<String>,
    pub description: Option<String>,
    pub order: Option<f64>,
    /// `Some(原因)` = broken（组合缺失/不可装载）。显示文本，非致命。
    pub broken: Option<String>,
}
