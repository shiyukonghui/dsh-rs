//! 插件包（文件夹）解析：一个插件 = 一个文件夹（wasm 组件 + 前端组件），
//! **文件夹名 = 插件注册名**（对齐 cordis `Tree.import(name)`：`name` 未命中
//! 内置/宿主注册 → 解析为该文件夹包）。
//!
//! 包布局（D1，用户确认）：`plugin.json` 清单声明 + 约定回退：
//! - `plugin.json`：`wasm`（相对包目录的路径或绝对路径）/ `web`（前端资源目录）/
//!   `caps`（能力数组）/ `world`（可选 world 提示）——全部可选；
//! - 缺省回退：wasm = `<name>/target/wasm32-wasip1/debug/<name>_plugin.wasm`（既有构建约定，
//!   连字符转下划线）；web = `<name>/web/` 存在则取。
//!
//! world 判别：预检组件导出接口（`detect_component_kind`）——ABI 事实；
//! `world` 提示可作显式覆盖/快路径，仍以字节探测兜底。

use std::path::{Path, PathBuf};

use serde::Deserialize;

use dsh_core::{CordisError, Value};
use dsh_wasmrt::Capabilities;

/// 插件包清单（serde；所有字段可选）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PackageManifest {
    /// wasm 组件路径（相对包目录；或绝对路径）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wasm: Option<String>,
    /// 前端资源目录（相对包目录）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<String>,
    /// 包级能力数组（entry `config.caps` 缺席时生效）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caps: Option<Vec<String>>,
    /// world 提示（"loop" / "plugin"）；缺省 = 字节探测。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world: Option<String>,
}

/// 解析完成的插件包。
#[derive(Debug, Clone)]
pub struct PluginPackage {
    /// 文件夹名 = 插件注册名。
    pub name: String,
    /// 包目录。
    pub dir: PathBuf,
    /// wasm 组件文件路径。
    pub wasm: PathBuf,
    /// 前端资源目录（清单 `web` 或 `web/` 存在时）。
    pub web: Option<PathBuf>,
    /// 包级能力数组（manifest）。
    pub caps: Option<Vec<String>>,
    /// world 提示（manifest 覆盖；None = 字节探测）。
    pub world: Option<String>,
}

/// 缺省 wasm 路径（既有构建约定：`<name>/target/wasm32-wasip1/debug/<name>_plugin.wasm`）。
fn default_wasm_path(dir: &Path, name: &str) -> PathBuf {
    let wasm_name = format!("{}_plugin.wasm", name.replace('-', "_"));
    dir.join("target")
        .join("wasm32-wasip1")
        .join("debug")
        .join(wasm_name)
}

/// 读包清单：`plugin.json` 缺失 → 缺省（全 None）；JSON 非法 → fail-loud。
fn read_manifest(dir: &Path, name: &str) -> Result<PackageManifest, CordisError> {
    let path = dir.join("plugin.json");
    if !path.is_file() {
        return Ok(PackageManifest::default());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| {
        CordisError::Internal(format!("plugin package {name}: read plugin.json: {e}"))
    })?;
    serde_json::from_str(&text).map_err(|e| {
        CordisError::Internal(format!("plugin package {name}: invalid plugin.json: {e}"))
    })
}

/// 解析 `<wasm_base>/<name>` 为插件包。
/// - 目录不存在 → `Ok(None)`（装配层留给 loader 报「未知插件」）；
/// - 目录存在但清单非法 / wasm 缺失 / 声明的 web 目录缺失 → `Err`（fail-loud）。
pub fn resolve_package(wasm_base: &Path, name: &str) -> Result<Option<PluginPackage>, CordisError> {
    let dir = wasm_base.join(name);
    if !dir.is_dir() {
        return Ok(None);
    }
    let manifest = read_manifest(&dir, name)?;
    let wasm = match &manifest.wasm {
        Some(p) => {
            let path = Path::new(p);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                dir.join(path)
            }
        }
        None => default_wasm_path(&dir, name),
    };
    if !wasm.is_file() {
        return Err(CordisError::Internal(format!(
            "plugin package {name}: wasm component not found at {}",
            wasm.display()
        )));
    }
    let web = match &manifest.web {
        Some(p) => {
            let w = dir.join(p);
            if !w.is_dir() {
                return Err(CordisError::Internal(format!(
                    "plugin package {name}: web dir not found at {}",
                    w.display()
                )));
            }
            Some(w)
        }
        None => {
            let w = dir.join("web");
            if w.is_dir() {
                Some(w)
            } else {
                None
            }
        }
    };
    Ok(Some(PluginPackage {
        name: name.to_string(),
        dir,
        wasm,
        web,
        caps: manifest.caps,
        world: manifest.world,
    }))
}

/// 有效能力：entry `config.caps` > 包清单 `caps` > 缺省（abi_only）。
pub fn effective_caps(entry_config: &Value, pkg: &PluginPackage) -> Capabilities {
    let caps: Option<Value> = entry_config
        .get("caps")
        .cloned()
        .or_else(|| {
            pkg.caps.as_ref().map(|list| {
                Value::Array(list.iter().map(|s| Value::String(s.clone())).collect())
            })
        });
    Capabilities::from_json(caps.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dsh-plugin-pkg-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 约定回退：无 plugin.json → wasm = 构建目录约定；web 缺省。
    #[test]
    fn resolve_falls_back_to_build_convention() {
        let base = tmp("fallback");
        let pkg_dir = base.join("mypkg");
        let wasm = pkg_dir
            .join("target/wasm32-wasip1/debug")
            .join("mypkg_plugin.wasm");
        fs::create_dir_all(wasm.parent().unwrap()).unwrap();
        fs::write(&wasm, b"wasm").unwrap();

        let pkg = resolve_package(&base, "mypkg").unwrap().expect("package");
        assert_eq!(pkg.name, "mypkg");
        assert_eq!(pkg.wasm, wasm);
        assert!(pkg.web.is_none());
        assert!(pkg.caps.is_none());
        assert!(pkg.world.is_none());
        fs::remove_dir_all(&base).ok();
    }

    /// plugin.json 清单：显式 wasm（相对包目录）+ web + caps + world。
    #[test]
    fn resolve_reads_manifest() {
        let base = tmp("manifest");
        let pkg_dir = base.join("mypkg");
        let wasm = pkg_dir.join("custom/plugin.wasm");
        let web = pkg_dir.join("ui");
        fs::create_dir_all(wasm.parent().unwrap()).unwrap();
        fs::create_dir_all(&web).unwrap();
        fs::write(&wasm, b"wasm").unwrap();
        fs::write(web.join("panel.html"), "<html>ui</html>").unwrap();
        fs::write(
            pkg_dir.join("plugin.json"),
            r#"{"wasm":"custom/plugin.wasm","web":"ui","caps":["all"],"world":"loop"}"#,
        )
        .unwrap();

        let pkg = resolve_package(&base, "mypkg").unwrap().expect("package");
        assert_eq!(pkg.wasm, wasm);
        assert_eq!(pkg.web.as_deref(), Some(web.as_path()));
        assert_eq!(pkg.caps, Some(vec!["all".to_string()]));
        assert_eq!(pkg.world.as_deref(), Some("loop"));
        fs::remove_dir_all(&base).ok();
    }

    /// 非包目录 → Ok(None)（装配层留给 loader 报未知插件）。
    #[test]
    fn resolve_non_package_dir_is_none() {
        let base = tmp("none");
        assert!(resolve_package(&base, "nope").unwrap().is_none());
        fs::remove_dir_all(&base).ok();
    }

    /// 包目录存在但 wasm 缺失 / manifest JSON 非法 / 声明的 web 缺失 → Err（fail-loud）。
    #[test]
    fn resolve_fail_loud_on_bad_package() {
        let base = tmp("fail");
        let missing_wasm = base.join("nowasm");
        fs::create_dir_all(&missing_wasm).unwrap();
        assert!(resolve_package(&base, "nowasm").is_err(), "missing wasm -> Err");

        let bad_json = base.join("badjson");
        fs::create_dir_all(&bad_json).unwrap();
        fs::write(bad_json.join("plugin.json"), "not json{").unwrap();
        assert!(resolve_package(&base, "badjson").is_err(), "invalid json -> Err");

        let bad_web = base.join("badweb");
        fs::create_dir_all(&bad_web).unwrap();
        fs::write(
            bad_web.join("plugin.json"),
            r#"{"wasm":"x.wasm","web":"nope"}"#,
        )
        .unwrap();
        fs::write(bad_web.join("x.wasm"), b"wasm").unwrap();
        assert!(resolve_package(&base, "badweb").is_err(), "missing web dir -> Err");
        fs::remove_dir_all(&base).ok();
    }

    /// 有效能力优先级：entry config.caps > 包 caps > 缺省 abi_only。
    #[test]
    fn effective_caps_precedence() {
        use dsh_wasmrt::{CAPS_GET, CAPS_PROVIDE, CAPS_WASI_NET};
        let base = PluginPackage {
            name: "p".into(),
            dir: PathBuf::from("."),
            wasm: PathBuf::from("p.wasm"),
            web: None,
            caps: Some(vec!["all".to_string()]),
            world: None,
        };
        // entry caps 优先（["get"] → GET 有、WASI 无）
        let entry = serde_json::json!({"caps": ["get"]});
        assert!(effective_caps(&entry, &base).allows(CAPS_GET));
        assert!(!effective_caps(&entry, &base).allows(CAPS_WASI_NET));
        // 无 entry caps → 包 caps（all）
        let empty = serde_json::json!({});
        assert!(effective_caps(&empty, &base).allows(CAPS_PROVIDE));
        assert!(effective_caps(&empty, &base).allows(CAPS_WASI_NET));
        // 无任何 caps → abi_only（provide/emit/get，无 WASI）
        let none_pkg = PluginPackage { caps: None, ..base };
        let e = effective_caps(&empty, &none_pkg);
        assert!(e.allows(CAPS_GET));
        assert!(!e.allows(CAPS_WASI_NET));
    }
}
