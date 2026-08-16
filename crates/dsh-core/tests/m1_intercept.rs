//! §5.3 场景 4：intercept 配置合并（resolveConfig 语义）。

mod common;
use common::*;

use dsh_core::*;

/// 父层 {a:1}，子层 {a:9, b:2}：合并后子层覆盖 a，且 base/head 分别垫底/置顶。
#[test]
fn intercept_merges_parent_then_child() {
    let cordis = Cordis::new();
    let parent = FnPlugin::new("parent", &[], |ctx, _cfg| {
        ctx.intercept("srv", json!({"a": 1})).unwrap();
        let child = FnPlugin::new("child", &[], |ctx, _cfg| {
            // 只有父层
            assert_eq!(ctx.resolve_config("srv", None, None), json!({"a": 1}));
            // 加一层子层
            ctx.intercept("srv", json!({"a": 9, "b": 2})).unwrap();
            assert_eq!(ctx.resolve_config("srv", None, None), json!({"a": 9, "b": 2}));
            // base 最低优先级、head 最高优先级
            let merged = ctx.resolve_config("srv", Some(json!({"z": 0, "a": 100})), Some(json!({"h": 1, "a": 999})));
            assert_eq!(merged, json!({"a": 999, "b": 2, "z": 0, "h": 1}));
            Ok(EffectOutcome::None)
        });
        ctx.plugin(child, json!({})).unwrap();
        Ok(EffectOutcome::None)
    });
    let pid = cordis.plugin(parent, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(pid), Some(FiberState::Active));
}

/// 同层同名后者覆盖（等价 Object.assign 逐层语义）。
#[test]
fn intercept_same_layer_later_wins() {
    let cordis = Cordis::new();
    let plugin = FnPlugin::new("p", &[], |ctx, _cfg| {
        ctx.intercept("srv", json!({"k": 1})).unwrap();
        ctx.intercept("srv", json!({"k": 2, "extra": true})).unwrap();
        assert_eq!(ctx.resolve_config("srv", None, None), json!({"k": 2, "extra": true}));
        Ok(EffectOutcome::None)
    });
    let fid = cordis.plugin(plugin, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));
}

/// intercept 随 fiber 卸载移除。
#[test]
fn intercept_disposed_with_fiber() {
    let cordis = Cordis::new();
    let parent = FnPlugin::new("parent", &[], |ctx, _cfg| {
        ctx.intercept("srv", json!({"a": 1})).unwrap();
        Ok(EffectOutcome::None)
    });
    let pid = cordis.plugin(parent, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(pid), Some(FiberState::Active));

    // 卸载后：新插件的 resolve_config 不再看到 {a:1}
    cordis.unload(pid).unwrap();
    let probe = FnPlugin::new("probe", &[], |ctx, _cfg| {
        assert_eq!(ctx.resolve_config("srv", None, None), json!({}));
        Ok(EffectOutcome::None)
    });
    let prid = cordis.plugin(probe, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(prid), Some(FiberState::Active));
}
