//! Service 基类语义（对应 PLAN §1.6，M1 子集）。

use std::any::Any;

/// 服务：持有名字与可选可用性谓词，统一作为 `Arc<dyn Any>` 注册进服务仓库。
///
/// 对应 Cordis `Service` 基类：`name`（`provide` 字段）、`check`（可用性谓词）。
/// 构造即注册（`provide_service`）、随 fiber 卸载，由门面方法提供。
pub trait Service: Any + Send + Sync {
    /// 服务名（Cordis `Service.provide` / 构造参数 name）。
    fn service_name(&self) -> &'static str;

    /// 可用性谓词：依赖方在 check 通过前保持 PENDING（Cordis `[Service.check]`）。
    fn check(&self) -> bool {
        true
    }
}
