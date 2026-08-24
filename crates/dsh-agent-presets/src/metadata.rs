//! preset 元数据读取（display text only，非致命；缺失/损坏显示 id 即可）。

use std::fs;
use std::path::Path;

use serde::Deserialize;

/// display 元数据（缺失全为 None）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PresetMetadata {
    pub name: Option<String>,
    pub description: Option<String>,
    pub order: Option<f64>,
}

#[derive(Deserialize, Default)]
struct Raw {
    name: Option<String>,
    description: Option<String>,
    #[serde(default)]
    order: Option<f64>,
}

/// 读 `preset.yml`/`preset.yaml`；任何失败 → 默认元数据（行仍可挂载，只显示 id）。
pub fn read_preset_metadata(dir: &Path) -> PresetMetadata {
    for file in ["preset.yml", "preset.yaml"] {
        let text = match fs::read_to_string(dir.join(file)) {
            Ok(t) => t,
            Err(_) => continue,
        };
        if let Ok(raw) = serde_yaml::from_str::<Raw>(&text) {
            return PresetMetadata {
                name: raw.name,
                description: raw.description,
                order: raw.order,
            };
        }
    }
    PresetMetadata::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dsh-presets-meta-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reads_yaml_metadata() {
        let dir = tmp_dir("ok");
        fs::write(dir.join("preset.yml"), "name: 标准模式\norder: 1\n").unwrap();
        let m = read_preset_metadata(&dir);
        assert_eq!(m.name.as_deref(), Some("标准模式"));
        assert_eq!(m.order, Some(1.0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_or_broken_defaults() {
        let dir = tmp_dir("miss");
        let m = read_preset_metadata(&dir);
        assert_eq!(m, PresetMetadata::default());
        let _ = fs::remove_dir_all(&dir);
    }
}
