//! 插件形状与注册（对应 PLAN §1.1）。

use std::sync::Arc;

use crate::context::Cordis;
use crate::error::CordisError;
use crate::fiber::EffectOutcome;
use crate::types::{FiberId, Value};

/// 插件入口：`(ctx, config) -> EffectOutcome`。类插件（Service 基类语义）在 M1 引入。
///
/// `name` / `inject` 为实例方法（对应 Cordis `Plugin.Base` 的实例元数据），
/// 使同一类型的不同实例可以不同名、不同依赖。
pub trait Plugin {
    /// 显示名（注册键；Cordis `Plugin.Base.name`）。
    fn name(&self) -> &'static str;

    /// 依赖的服务名（Cordis `inject`）。
    fn inject(&self) -> &'static [&'static str] {
        &[]
    }

    /// 配置校验 schema（Cordis `Plugin.Base.Config`；M4 dsh-schema）。
    /// 加载/更新时校验，失败 → `CordisError::Validation` → fiber FAILED；
    /// 通过后按 default 填充再传给 [`Plugin::apply`]。
    fn config_schema(&self) -> Option<dsh_schema::SchemaRef> {
        None
    }

    /// 插件主体：在 fiber 加载/重载时运行一次，安装可逆副作用。
    fn apply(&self, ctx: &Cordis, config: Value) -> Result<EffectOutcome, CordisError>;
}

/// 插件句柄（M0 = 名称键；M2 起扩展 manifest hash）。
pub type PluginHandle = String;

/// 插件运行时记录（Cordis `Plugin.Runtime`）。
pub struct RuntimeRecord {
    pub key: String,
    pub name: Option<String>,
    /// 该插件的存活 fiber。
    pub fibers: Vec<FiberId>,
    pub plugin: Arc<dyn Plugin>,
}
