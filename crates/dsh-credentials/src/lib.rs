//! dsh-credentials：凭据引用能力缝（M3c，见 M3-REQUIREMENTS.md）。
//!
//! 权威参考：`@deepseek-ai/dsh-credentials` + `credentials-local`。
//! M3c 交付：REF 语法校验、CredentialView、分层 resolve（env→file）、set/unset
//! 文件持久化（shadowed + 空值拒绝）。records half（grant/api-key）留 M5。
//!
//! 分层（对齐 credentials-local）：
//! ```text
//! 进程 env（只读，wins） > 本地文件（provider-managed，可写）
//! ```
//! 空值 seam-wide 规则：一个空存储值到处都等于「未配置」——resolve 跳过它、
//! describe 报 unconfigured，空白永不伪装成已配置的 secret。

use std::collections::HashMap;
use std::path::PathBuf;

/// POSIX 环境变量名（seam 的 REF 语法）。
const REF_PATTERN: &str = "^[A-Za-z_][A-Za-z0-9_]*$";
static REF_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
fn ref_re() -> &'static regex::Regex {
    REF_RE.get_or_init(|| regex::Regex::new(REF_PATTERN).unwrap())
}

/// 能否作为凭据引用名（对齐 `isCredentialRefName`——非语法名读作「未配置」而非抛错）。
pub fn is_credential_ref_name(value: &str) -> bool {
    ref_re().is_match(value)
}

/// 文件布局版本（对齐 `DOCUMENT_VERSION = 1`）。
pub const DOCUMENT_VERSION: u64 = 1;

/// 凭据提供的错误。
#[derive(Debug)]
pub enum CredentialsError {
    /// env 层遮蔽写（set/unset 拒绝——写会看起来成功而 resolve 仍返回遮蔽值）。
    Shadowed(String),
    /// 空值不能存储（set 需非空；用 unset 删）。
    Empty(String),
    /// 文件解析拒绝（损坏文档 boot-invalid）。
    Invalid(String),
    /// 其它 IO 失败。
    Other(String),
}

impl std::fmt::Display for CredentialsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CredentialsError::Shadowed(ref_name) => write!(
                f,
                "credentials: \"{ref_name}\" is supplied read-only by the launching environment, so set/unset would be shadowed; unset it in the shell you start dsh from instead"
            ),
            CredentialsError::Empty(ref_name) => write!(
                f,
                "credentials: an empty value cannot be stored for \"{ref_name}\"; use unset"
            ),
            CredentialsError::Invalid(msg) => write!(f, "credentials: {msg}"),
            CredentialsError::Other(msg) => write!(f, "credentials: {msg}"),
        }
    }
}

/// 一次已解析的凭据值与供给它的源层。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCredential {
    /// 非空 secret 值。
    pub value: String,
    /// 源层 id（`env`/`file`）。
    pub source: String,
}

/// 一个引用的源与可写事实（供配置 UI——不含值）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialView {
    pub configured: bool,
    /// 当 configured 时的供给层；未配置时缺席。
    pub source: Option<String>,
    pub writable: bool,
}

/// 已解析的文档：refs 映射（records 留 M5）。
#[derive(Debug, Default)]
struct CredentialsDocument {
    refs: HashMap<String, String>,
}

/// 解析文档文本（对齐 parseCredentialsDocument 的 refs half + version 规则）。
fn parse_document(text: &str, filename: &str) -> Result<CredentialsDocument, CredentialsError> {
    let root: serde_yaml::Value = serde_yaml::from_str(text)
        .map_err(|e| CredentialsError::Invalid(format!("invalid document at {filename}: {e}")))?;
    let Some(mapping) = root.as_mapping() else {
        return Err(CredentialsError::Invalid(format!(
            "credentials: {filename} must be a mapping"
        )));
    };
    if mapping.is_empty() {
        // 空（或仅注释）文档 = 空 store，无需 version。
        return Ok(CredentialsDocument::default());
    }
    let version = mapping
        .get(serde_yaml::Value::String("version".to_string()))
        .and_then(|v| v.as_u64());
    match version {
        Some(v) if v == DOCUMENT_VERSION => {}
        _ => {
            return Err(CredentialsError::Invalid(format!(
                "credentials: {filename} declares version {:?}; this build reads version {DOCUMENT_VERSION}",
                mapping.get(serde_yaml::Value::String("version".to_string()))
            )));
        }
    }
    for key in mapping.keys() {
        let name = key.as_str().unwrap_or("");
        if name != "version" && name != "refs" && name != "records" {
            return Err(CredentialsError::Invalid(format!(
                "credentials: unknown top-level key \"{name}\" in {filename}"
            )));
        }
    }
    let mut refs = HashMap::new();
    if let Some(refs_section) = mapping.get(serde_yaml::Value::String("refs".to_string())) {
        let Some(refs_map) = refs_section.as_mapping() else {
            return Err(CredentialsError::Invalid(format!(
                "credentials: \"refs\" in {filename} must be a mapping"
            )));
        };
        for (key, value) in refs_map {
            let name = key
                .as_str()
                .ok_or_else(|| CredentialsError::Invalid(format!("credentials: non-string ref key in {filename}")))?;
            if !is_credential_ref_name(name) {
                return Err(CredentialsError::Invalid(format!(
                    "credentials: ref \"{name}\" in {filename} is not a valid reference"
                )));
            }
            let value = value
                .as_str()
                .ok_or_else(|| CredentialsError::Invalid(format!("credentials: the value for \"{name}\" in {filename} must be a string")))?;
            if value.is_empty() {
                return Err(CredentialsError::Invalid(format!(
                    "credentials: the value for \"{name}\" in {filename} is empty; remove the key instead"
                )));
            }
            refs.insert(name.to_string(), value.to_string());
        }
    }
    Ok(CredentialsDocument { refs })
}

/// 渲染文档文本（version + refs）。
fn render_document(refs: &HashMap<String, String>) -> String {
    let mut out = String::from("version: 1\nrefs:\n");
    if refs.is_empty() {
        return out;
    }
    let mut keys: Vec<&String> = refs.keys().collect();
    keys.sort();
    for key in keys {
        out.push_str(&format!("  {}: \"{}\"\n", key, refs[key]));
    }
    out
}

/// 凭据提供者（M3c：env + file 两层，refs only）。
pub struct CredentialProvider {
    /// 进程环境（注入，可测）。
    env: HashMap<String, String>,
    /// 本地文档路径（None = 纯内存模式）。
    document_path: Option<PathBuf>,
    /// 内存 refs 快照（file 模式加载后维护；memory 模式直接当 store）。
    refs: HashMap<String, String>,
}

impl CredentialProvider {
    /// 纯内存提供者（测试）。
    pub fn memory() -> Self {
        CredentialProvider {
            env: std::env::vars().collect(),
            document_path: None,
            refs: HashMap::new(),
        }
    }

    /// 显式 env 注入（测试可控）。
    pub fn with_env(env: HashMap<String, String>) -> Self {
        CredentialProvider {
            env,
            document_path: None,
            refs: HashMap::new(),
        }
    }

    /// 文件提供者：缺失文件 = 空 store；损坏文件 fail loud（但 `file` 作为便捷
    /// 构造——boot 场景由 `try_file` 严格）。此处 `file` 采用宽松语义：读不到
    /// 就空（对齐 boot 缺失 → 空）；损坏也在构造时报告（无法表达 → 用 try_file）。
    pub fn file(path: PathBuf) -> Self {
        let mut p = CredentialProvider {
            env: std::env::vars().collect(),
            document_path: Some(path.clone()),
            refs: HashMap::new(),
        };
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(doc) = parse_document(&text, &path.to_string_lossy()) {
                p.refs = doc.refs;
            }
        }
        p
    }

    /// 严格文件构造：文件损坏 → Err（不静默当空）。boot 用这个。
    pub fn try_file(path: PathBuf) -> Result<Self, CredentialsError> {
        let mut p = CredentialProvider {
            env: std::env::vars().collect(),
            document_path: Some(path.clone()),
            refs: HashMap::new(),
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let doc = parse_document(&text, &path.to_string_lossy())?;
                p.refs = doc.refs;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(CredentialsError::Other(format!(
                    "read {}: {e}",
                    path.display()
                )));
            }
        }
        Ok(p)
    }

    fn inherited(&self, ref_name: &str) -> Option<String> {
        match self.env.get(ref_name) {
            Some(v) if !v.is_empty() => Some(v.clone()),
            _ => None,
        }
    }

    /// 解析一次（每次调用重新读；文件模式读内存快照——M3 无 OS watch，写路径
    /// 自一致）。
    pub fn resolve(&self, ref_name: &str) -> Result<Option<ResolvedCredential>, CredentialsError> {
        if let Some(v) = self.inherited(ref_name) {
            return Ok(Some(ResolvedCredential { value: v, source: "env".to_string() }));
        }
        if let Some(v) = self.refs.get(ref_name) {
            return Ok(Some(ResolvedCredential { value: v.clone(), source: "file".to_string() }));
        }
        Ok(None)
    }

    /// 描述：configured/source/writable（不含值）。
    pub fn describe(&self, ref_name: &str) -> Result<CredentialView, CredentialsError> {
        // 只有继承的环境不可写：它无法从内编辑。
        if self.inherited(ref_name).is_some() {
            return Ok(CredentialView { configured: true, source: Some("env".into()), writable: false });
        }
        if self.refs.contains_key(ref_name) {
            return Ok(CredentialView { configured: true, source: Some("file".into()), writable: true });
        }
        Ok(CredentialView { configured: false, source: None, writable: true })
    }

    /// set：非空值写入文件层；env 遮蔽 → Shadowed。
    pub fn set(&mut self, ref_name: &str, value: &str) -> Result<(), CredentialsError> {
        if value.is_empty() {
            return Err(CredentialsError::Empty(ref_name.to_string()));
        }
        self.write(ref_name, Some(value))
    }

    /// unset：删除（无记录则是 no-op 成功）；env 遮蔽 → Shadowed。
    pub fn unset(&mut self, ref_name: &str) -> Result<(), CredentialsError> {
        self.write(ref_name, None)
    }

    /// 统一写路径（内核）。
    fn write(&mut self, ref_name: &str, value: Option<&str>) -> Result<(), CredentialsError> {
        if self.inherited(ref_name).is_some() {
            return Err(CredentialsError::Shadowed(ref_name.to_string()));
        }
        if value.is_none() && !self.refs.contains_key(ref_name) {
            return Ok(()); // unset absent → no-op 成功。
        }
        match value {
            Some(v) => {
                self.refs.insert(ref_name.to_string(), v.to_string());
            }
            None => {
                self.refs.remove(ref_name);
            }
        }
        self.persist()
    }

    /// 把当前 refs 快照持久化到文档（memory 模式 no-op）。
    fn persist(&self) -> Result<(), CredentialsError> {
        let Some(path) = &self.document_path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    CredentialsError::Other(format!("mkdir {}: {e}", parent.display()))
                })?;
            }
        }
        let text = render_document(&self.refs);
        dsh_persistence::fs_atomic::atomic_write(path, text.as_bytes())
            .map_err(|e| CredentialsError::Other(format!("persist {}: {e}", path.display())))?;
        Ok(())
    }
}
