//! Service 基类语义（对应 PLAN §1.6，M1 子集）。
//! B1：服务作者通用原语——`extend`（派生作用域实例；`None` = 恒等）+ `invoke`（可调用
//! 服务，默认不可调）。对齐 fork `Service[extend]`（service.ts:65-73）与 `createCallable`
//! （utils.ts:226）。

use std::any::Any;
use std::sync::Arc;

use crate::context::Cordis;
use crate::error::CordisError;
use crate::types::Value;

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

    /// B1：派生作用域实例（Cordis `Service[extend]`）。
    /// `None`（默认）= 恒等（`get_extended` 返回原实例）；`Some(derived)` = 返回绑定
    /// **访问方纤维的 ctx** 的派生实例（fork：callable → createCallable 重建；否则
    /// `Object.create(this)` + `Object.assign(props)`，DIV-7-3）。
    fn extend(&self, ctx: &Cordis) -> Option<Arc<dyn Service>> {
        let _ = ctx;
        None
    }

    /// B1：可调用服务主体（Cordis `Service[invoke]` 触发的 callable）。
    /// 默认不可调用；实现后 `ctx.call_service(name, args)` 可调用。
    fn invoke(&self, ctx: &Cordis, args: &[Value]) -> Result<Value, CordisError> {
        let _ = (ctx, args);
        Err(CordisError::Internal("service is not callable".to_string()))
    }
}
