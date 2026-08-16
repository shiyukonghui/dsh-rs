//! 错误模型：CordisError 代码 + AggregateError（对应 PLAN §2.4）。

use std::fmt;

/// 框架错误，消息风格与 Cordis 保持一致（差分测试比对消息）。
#[derive(Debug, Clone, PartialEq)]
pub enum CordisError {
    /// `INACTIVE_EFFECT`：在已 dispose / 卸载中的 fiber 上创建 effect。
    InactiveEffect,
    /// invalid plugin：插件形状不支持。
    InvalidPlugin(String),
    /// cannot get required service "<name>" in inactive context。
    MissingService(String),
    /// cannot set property "<name>" without provide。
    NotProvided(String),
    /// cannot set property "<name>" in multiple fibers。
    MultipleFibers(String),
    /// service "<name>" has been registered at <owner>。
    AlreadyRegistered(String),
    /// 配置校验失败（schemastery 消息）。
    Validation(String),
    /// 引用的 fiber 不存在（内部句柄失效）。
    FiberNotFound(u64),
    /// 内部错误（带消息）。
    Internal(String),
}

impl fmt::Display for CordisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CordisError::InactiveEffect => write!(f, "cannot create effect on inactive context"),
            CordisError::InvalidPlugin(shape) => {
                write!(f, "invalid plugin, expect function or object with an \"apply\" method, received {shape}")
            }
            CordisError::MissingService(name) => {
                write!(f, "cannot get required service \"{name}\" in inactive context")
            }
            CordisError::NotProvided(name) => write!(f, "cannot set property \"{name}\" without provide"),
            CordisError::MultipleFibers(name) => {
                write!(f, "cannot set property \"{name}\" in multiple fibers")
            }
            CordisError::AlreadyRegistered(name) => {
                write!(f, "service \"{name}\" has been registered at a previous fiber")
            }
            CordisError::Validation(msg) => write!(f, "{msg}"),
            CordisError::FiberNotFound(id) => write!(f, "fiber {id} not found"),
            CordisError::Internal(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for CordisError {}

/// 多失败聚合（parallel 的 allSettled、loader 事务回滚）。
#[derive(Debug)]
pub struct AggregateError {
    pub errors: Vec<CordisError>,
}

impl fmt::Display for AggregateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "aggregate error ({} failures)", self.errors.len())
    }
}

impl std::error::Error for AggregateError {}
