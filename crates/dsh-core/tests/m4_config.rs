//! M4：插件 config_schema 接入 —— 加载校验、default 填充、更新校验、FAILED 状态。

mod common;
use common::*;

use std::collections::HashMap;
use std::sync::Arc;

use dsh_core::*;

/// 带 schema 的插件：{mode: string().default("native"), max: natural().default(10)}。
fn schema_plugin() -> SchemaPlugin {
    SchemaPlugin
}

struct SchemaPlugin;

impl Plugin for SchemaPlugin {
    fn name(&self) -> &'static str {
        "sp"
    }

    fn config_schema(&self) -> Option<dsh_schema::SchemaRef> {
        let mut dict = HashMap::new();
        dict.insert(
            "mode".to_string(),
            dsh_schema::Schema::with_default(&dsh_schema::Schema::string(), json!("native")),
        );
        dict.insert(
            "max".to_string(),
            dsh_schema::Schema::with_default(&dsh_schema::Schema::natural(), json!(10)),
        );
        Some(dsh_schema::Schema::object(dict))
    }

    fn apply(&self, ctx: &Cordis, config: Value) -> Result<EffectOutcome, CordisError> {
        // 记录收到的（校验+填充后）配置
        ctx.provide("seen", Arc::new(config)).unwrap();
        Ok(EffectOutcome::None)
    }
}

/// 合法配置：default 填充后传给 apply。
#[test]
fn valid_config_fills_defaults() {
    let cordis = Cordis::new();
    let fid = cordis.plugin(schema_plugin(), json!({"mode": "code"})).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));

    let consumer = FnPlugin::new("reader", &["seen"], |ctx, _cfg| {
        let v = ctx.get_typed::<Value>("seen").expect("seen");
        assert_eq!(*v, json!({"mode": "code", "max": 10}));
        Ok(EffectOutcome::None)
    });
    let cid = cordis.plugin(consumer, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(cid), Some(FiberState::Active));
}

/// 非法配置：fiber FAILED，错误为 Validation。
#[test]
fn invalid_config_fails_fiber() {
    let cordis = Cordis::new();
    let fid = cordis.plugin(schema_plugin(), json!({"mode": 123})).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Failed));
    let err = cordis.fiber_error(fid).expect("error set");
    assert!(matches!(err, CordisError::Validation(_)), "{err:?}");
    assert!(err.to_string().contains("expected string"), "{err}");
    assert!(err.to_string().contains("$.mode"), "{err}");
}

/// update：非法配置返回 Err（fiber 保持原配置）；合法配置热更并填充。
#[test]
fn update_validates_config() {
    let cordis = Cordis::new();
    let fid = cordis.plugin(schema_plugin(), json!({})).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));

    // 非法更新 → Err
    let err = cordis.update(fid, json!({"max": -5})).unwrap_err();
    assert!(matches!(err, CordisError::Validation(_)), "{err:?}");

    // 合法更新 → 重启并填充 default
    cordis.update(fid, json!({"mode": "both"})).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));
    let consumer = FnPlugin::new("reader2", &["seen"], |ctx, _cfg| {
        let v = ctx.get_typed::<Value>("seen").expect("seen");
        assert_eq!(*v, json!({"mode": "both", "max": 10}));
        Ok(EffectOutcome::None)
    });
    let cid = cordis.plugin(consumer, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(cid), Some(FiberState::Active));
}

/// 无 schema 插件：配置原样通过。
#[test]
fn no_schema_passes_through() {
    let cordis = Cordis::new();
    let log = log();
    let log2 = log.clone();
    let plugin = FnPlugin::new("plain", &[], move |_ctx, config| {
        push(&log2, format!("{:?}", config));
        Ok(EffectOutcome::None)
    });
    let fid = cordis.plugin(plugin, json!({"anything": true})).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));
    assert_eq!(snapshot(&log), vec![format!("{:?}", json!({"anything": true}))]);
}
