//! dsh-sandbox — M5 沙箱策略缝（设计见 M5-DESIGN.md §3）。
//!
//! 由红→绿测试驱动（tests/modes.rs 先行）：SandboxMode 词汇与更宽阶梯已落地；
//! escalation 校验、writableRoots、策略解析、`sandbox/mode` 会话事件随各自红测陆续加入。

use std::collections::HashMap;

mod policy;

pub use policy::{
    canonical_path, escalation_hint_marker, sandbox_denial_marker, validate_escalation_args,
    writable_roots,
};

/// 参考 `SandboxMode`：三个真实模式（kebab 序列化）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl SandboxMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            SandboxMode::ReadOnly => "read-only",
            SandboxMode::WorkspaceWrite => "workspace-write",
            SandboxMode::DangerFullAccess => "danger-full-access",
        }
    }
}

impl std::fmt::Display for SandboxMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SandboxMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read-only" => Ok(SandboxMode::ReadOnly),
            "workspace-write" => Ok(SandboxMode::WorkspaceWrite),
            "danger-full-access" => Ok(SandboxMode::DangerFullAccess),
            other => Err(format!("unknown sandbox mode: {other}")),
        }
    }
}

/// 升级 target 只允许这两个更宽模式（read-only 永不可为 target）。
pub const ESCALATION_TARGETS: [SandboxMode; 2] =
    [SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess];

/// 每个模式的「严格更宽」集合（单向阶梯）。
pub fn wider_modes(mode: SandboxMode) -> &'static [SandboxMode] {
    match mode {
        SandboxMode::ReadOnly => &[SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess],
        SandboxMode::WorkspaceWrite => &[SandboxMode::DangerFullAccess],
        SandboxMode::DangerFullAccess => &[],
    }
}

/// 阶梯映射（查询用；与 `wider_modes` 同义，测试友好形态）。
pub fn wider_modes_map() -> HashMap<SandboxMode, Vec<SandboxMode>> {
    let mut m = HashMap::new();
    m.insert(
        SandboxMode::ReadOnly,
        vec![SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess],
    );
    m.insert(SandboxMode::WorkspaceWrite, vec![SandboxMode::DangerFullAccess]);
    m.insert(SandboxMode::DangerFullAccess, vec![]);
    m
}
