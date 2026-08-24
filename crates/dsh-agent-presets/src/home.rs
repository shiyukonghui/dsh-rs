//! dshHome 解析与用户根路径（对齐 util/home-paths `resolveDshHome`）。

use std::path::{Path, PathBuf};

use crate::DSH_HOME_DIR_NAME;

/// 用户预设根目录段（对齐 TS `USER_PRESET_DIR`）。
pub const USER_PRESET_DIR: &str = ".agent-presets";

/// 解析 dsh home：显式配置路径占位（本函数仅覆盖 env + 默认两级）> `$DSH_HOME`
/// （空/纯空白视为未设）> `~/.dsh`。纯函数，便于 TDD。
pub fn dsh_home_from(home_dir: &Path, dsh_home_env: Option<&str>) -> PathBuf {
    match dsh_home_env.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => home_dir.join(DSH_HOME_DIR_NAME),
    }
}

/// 用户根 = `<dshHome>/.agent-presets`（对齐 TS `dshHomePath(USER_PRESET_DIR)`）。
pub fn user_preset_root_path(dsh_home: &Path) -> PathBuf {
    dsh_home.join(USER_PRESET_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn dsh_home_from_env_overrides_default() {
        let home = Path::new("/home/u");
        assert_eq!(dsh_home_from(home, Some("/tmp/dsh")), Path::new("/tmp/dsh"));
    }

    #[test]
    fn dsh_home_ignores_blank_env() {
        let home = Path::new("/home/u");
        assert_eq!(dsh_home_from(home, Some("   ")), home.join(".dsh"));
        assert_eq!(dsh_home_from(home, Some("")), home.join(".dsh"));
    }

    #[test]
    fn dsh_home_defaults_under_home() {
        let home = Path::new("C:\\Users\\u");
        assert_eq!(dsh_home_from(home, None), home.join(".dsh"));
    }

    #[test]
    fn user_root_appends_segment() {
        let dsh_home = Path::new("/home/u/.dsh");
        assert_eq!(
            user_preset_root_path(dsh_home),
            PathBuf::from("/home/u/.dsh/.agent-presets")
        );
    }
}
