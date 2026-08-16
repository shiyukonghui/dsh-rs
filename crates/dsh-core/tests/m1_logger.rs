//! §5.3 场景 8：Logger —— levels 过滤、Error/AggregateError 展开、printf 占位符。

mod common;
use common::*;

use std::collections::HashMap;

use dsh_core::*;

fn msg(name: &str, r#type: LoggerType, args: Vec<Value>) -> Message {
    Message {
        sn: 1,
        ts: 0,
        name: name.to_string(),
        r#type,
        level: r#type.level(),
        args,
        fiber: None,
    }
}

/// printf 占位符：%s %d %i %f %o %O，%% 转义，剩余参数追加。
#[test]
fn format_placeholders() {
    assert_eq!(
        format_message(&msg("app", LoggerType::Info, vec![json!("%s=%d%%"), json!("x"), json!(42)])),
        "x=42%"
    );
    assert_eq!(
        format_message(&msg("app", LoggerType::Info, vec![json!("%f"), json!(3.5)])),
        "3.5"
    );
    assert_eq!(
        format_message(&msg("app", LoggerType::Info, vec![json!("%o ok"), json!({"k": [1, 2]}), json!("tail")])),
        "{\"k\":[1,2]} ok tail"
    );
    // 首参非字符串 → 整体按 %o 输出
    assert_eq!(
        format_message(&msg("app", LoggerType::Info, vec![json!({"a": 1})])),
        "{\"a\":1}"
    );
}

/// exporter 级别过滤（Cordis 语义：`targetLevel < level` 跳过，阈值 = 最高显示级别）。
#[test]
fn level_filtering_via_exporter_config() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let host = FnPlugin::new("h", &[], move |ctx, _cfg| {
        let l = log2.clone();
        ctx.exporter(
            Box::new(move |m| push(&l, format!("{}:{}", m.name, m.level))),
            ExporterConfig {
                levels: HashMap::from([("app".to_string(), 2u8)]), // 阈值 WARN(2)：显示 0..=2
                default_level: None,
            },
        )
        .unwrap();
        let logger = ctx.logger("app");
        logger.error(vec![]); // 0
        logger.info(vec![]); // 1
        logger.warn(vec![]); // 2
        logger.debug(vec![]); // 3 > 2 → 过滤
        Ok(EffectOutcome::None)
    });
    cordis.plugin(host, json!({})).unwrap();
    assert_eq!(snapshot(&log), vec!["app:0", "app:1", "app:2"]);
}

/// 默认阈值 INFO(1)：error/info 显示，warn/debug 过滤（忠实 Cordis 行为）。
#[test]
fn default_threshold_hides_warn_and_debug() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let host = FnPlugin::new("h", &[], move |ctx, _cfg| {
        let l = log2.clone();
        ctx.exporter(
            Box::new(move |m| push(&l, format!("{}:{}", m.name, m.level))),
            ExporterConfig::default(),
        )
        .unwrap();
        let logger = ctx.logger("app");
        logger.error(vec![]);
        logger.info(vec![]);
        logger.warn(vec![]);
        logger.debug(vec![]);
        Ok(EffectOutcome::None)
    });
    cordis.plugin(host, json!({})).unwrap();
    assert_eq!(snapshot(&log), vec!["app:0", "app:1"]);
}

/// 自动命名：hyphenate(fiber.name)，intercept `logger` 配置可覆盖 name。
#[test]
fn logger_name_hyphenated_and_intercept_override() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let plugin = FnPlugin::new("myPlugin", &[], move |ctx, _cfg| {
        let l = log2.clone();
        ctx.exporter(
            Box::new(move |m| push(&l, m.name.clone())),
            ExporterConfig::default(),
        )
        .unwrap();
        ctx.logger_auto().info(vec![json!("x")]);
        ctx.intercept("logger", json!({"name": "custom", "level": 2})).unwrap();
        ctx.logger_auto().info(vec![json!("y")]);
        Ok(EffectOutcome::None)
    });
    cordis.plugin(plugin, json!({})).unwrap();
    assert_eq!(snapshot(&log), vec!["my-plugin", "custom"]);
}

/// AggregateError 展开：每个错误单独导出（Cordis Error 参数展开路径）。
#[test]
fn aggregate_error_expansion() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let plugin = FnPlugin::new("agg", &[], move |ctx, _cfg| {
        let l = log2.clone();
        ctx.exporter(
            Box::new(move |m| {
                let text = m.args.first().and_then(|a| a.as_str()).unwrap_or("");
                push(&l, format!("{}:{text}", m.r#type.as_str()));
            }),
            ExporterConfig::default(),
        )
        .unwrap();
        let logger = ctx.logger("agg");
        let agg = AggregateError {
            errors: vec![
                CordisError::Internal("e1".to_string()),
                CordisError::InactiveEffect,
            ],
        };
        logger.log_aggregate(&agg);
        Ok(EffectOutcome::None)
    });
    cordis.plugin(plugin, json!({})).unwrap();
    let s = snapshot(&log);
    assert_eq!(s.len(), 2);
    assert!(s[0].contains("e1"));
    assert!(s[1].contains("inactive"));
}

/// 默认 buffer 导出器接收消息；format 后内容正确。
#[test]
fn buffer_receives_messages() {
    let cordis = Cordis::new();
    let plugin = FnPlugin::new("buf", &[], |ctx, _cfg| {
        ctx.logger("buf").info(vec![json!("%s"), json!("careful")]);
        Ok(EffectOutcome::None)
    });
    cordis.plugin(plugin, json!({})).unwrap();
    let msgs = cordis.logger_buffer();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].name, "buf");
    assert_eq!(msgs[0].r#type, LoggerType::Info);
    assert_eq!(format_message(&msgs[0]), "careful");
}

/// exporter 随 fiber 卸载移除（注册是副作用）。
#[test]
fn exporter_removed_on_unload() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let plugin = FnPlugin::new("exp", &[], move |ctx, _cfg| {
        let l = log2.clone();
        ctx.exporter(
            Box::new(move |m| push(&l, format!("got:{}", m.name))),
            ExporterConfig::default(),
        )
        .unwrap();
        Ok(EffectOutcome::None)
    });
    let fid = cordis.plugin(plugin, json!({})).unwrap();
    cordis.unload(fid).unwrap();

    // 卸载后再记录日志：自定义 exporter 已移除，只剩默认 buffer
    let probe = FnPlugin::new("probe", &[], |ctx, _cfg| {
        ctx.logger("probe").info(vec![json!("x")]);
        Ok(EffectOutcome::None)
    });
    cordis.plugin(probe, json!({})).unwrap();
    assert_eq!(snapshot(&log), Vec::<String>::new());
}
