//! B1 Service 派生作用域实例 + 可调用服务：`extend`（None=恒等 / Some=绑定访问方 ctx 的
//! 派生）、`invoke`（可调用服务）、`ctx.get_extended`/`ctx.call_service`（srv 通道）。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::sync::{Arc, Mutex};

use dsh_core::*;

/// 自定义派生服务：extend 记录**访问方纤维名**到共享日志，返回派生实例（观察通道）。
struct ExtSvc {
    log: Arc<Mutex<Vec<String>>>,
}

impl Service for ExtSvc {
    fn service_name(&self) -> &'static str {
        "svcx"
    }
    fn extend(&self, ctx: &Cordis) -> Option<Arc<dyn Service>> {
        let caller = ctx
            .current_fiber()
            .and_then(|fid| ctx.fiber_name(fid))
            .unwrap_or_else(|| "?".to_string());
        self.log.lock().unwrap().push(format!("derived:{caller}"));
        Some(Arc::new(DerivedSvc))
    }
}

/// 派生实例（同一服务名下；type 不同 = 派生）。
struct DerivedSvc;
impl Service for DerivedSvc {
    fn service_name(&self) -> &'static str {
        "svcx"
    }
}

/// 可调用服务（invoke = 数字加和）。
struct CalcSvc;
impl Service for CalcSvc {
    fn service_name(&self) -> &'static str {
        "calc"
    }
    fn invoke(&self, _ctx: &Cordis, args: &[Value]) -> Result<Value, CordisError> {
        let a = args.first().and_then(|v| v.as_f64()).unwrap_or(0.0);
        let b = args.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0);
        Ok(json!(a + b))
    }
}

/// 普通服务（默认 extend=恒等 / invoke=不可调）。
struct PlainSvc;
impl Service for PlainSvc {
    fn service_name(&self) -> &'static str {
        "plain"
    }
}

/// T1：自定义 extend → 派生实例绑定**访问方纤维**的 ctx（观察日志含调用者纤维名）。
#[test]
fn extend_produces_derived_bound_to_accessing_fiber() {
    let derived_log = Arc::new(Mutex::new(Vec::new()));
    let cordis = Cordis::new();
    let parent = FnPlugin::new("parent", &[], {
        let dl = derived_log.clone();
        move |ctx, _cfg| {
            ctx.provide_service(Arc::new(ExtSvc { log: dl.clone() }))
                .unwrap();
            let child = FnPlugin::new("child", &[], move |c2, _cfg2| {
                let ext = c2.get_extended("svcx");
                assert!(ext.is_some(), "get_extended returns a derived instance");
                Ok(EffectOutcome::None)
            });
            ctx.plugin(child, json!({})).unwrap();
            Ok(EffectOutcome::None)
        }
    });
    cordis.plugin(parent, json!({})).unwrap();
    let guard = derived_log.lock().unwrap();
    assert!(
        guard.iter().any(|s| s == "derived:child"),
        "extend must run with the accessing (child) fiber ctx: {guard:?}"
    );
}

/// T2：默认 extend → 恒等（同一 Arc，ptr_eq）。
#[test]
fn default_extend_returns_identity() {
    let cordis = Cordis::new();
    let parent = FnPlugin::new("parent", &[], {
        move |ctx, _cfg| {
            ctx.provide_service(Arc::new(PlainSvc)).unwrap();
            let child = FnPlugin::new("child", &[], move |c2, _cfg2| {
                let base = c2.srv_lookup("plain").expect("srv registered");
                let ext = c2.get_extended("plain").expect("get_extended");
                assert!(
                    Arc::ptr_eq(&base, &ext),
                    "default extend must be identity (same Arc)"
                );
                Ok(EffectOutcome::None)
            });
            ctx.plugin(child, json!({})).unwrap();
            Ok(EffectOutcome::None)
        }
    });
    cordis.plugin(parent, json!({})).unwrap();
}

/// T3：可调用服务 invoke → 结果值。
#[test]
fn callable_service_invokes_with_args() {
    let cordis = Cordis::new();
    let parent = FnPlugin::new("parent", &[], {
        move |ctx, _cfg| {
            ctx.provide_service(Arc::new(CalcSvc)).unwrap();
            let child = FnPlugin::new("child", &[], move |c2, _cfg2| {
                let r = c2.call_service("calc", &[json!(1), json!(2)]).unwrap();
                assert_eq!(r.as_f64(), Some(3.0), "call_service invokes the service: {r}");
                Ok(EffectOutcome::None)
            });
            ctx.plugin(child, json!({})).unwrap();
            Ok(EffectOutcome::None)
        }
    });
    cordis.plugin(parent, json!({})).unwrap();
}

/// T4：不可调用服务 → 明确错误（非静默）。
#[test]
fn non_callable_service_errors_clearly() {
    let cordis = Cordis::new();
    let parent = FnPlugin::new("parent", &[], {
        move |ctx, _cfg| {
            ctx.provide_service(Arc::new(PlainSvc)).unwrap();
            let child = FnPlugin::new("child", &[], move |c2, _cfg2| {
                let err = c2.call_service("plain", &[]).unwrap_err();
                assert!(
                    err.to_string().contains("not callable"),
                    "non-callable service must error clearly: {err}"
                );
                Ok(EffectOutcome::None)
            });
            ctx.plugin(child, json!({})).unwrap();
            Ok(EffectOutcome::None)
        }
    });
    cordis.plugin(parent, json!({})).unwrap();
}
