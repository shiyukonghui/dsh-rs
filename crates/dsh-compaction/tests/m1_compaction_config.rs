//! M1c：配置解析/目标政策/紧凑规格（对齐 config.ts + index.ts 的 policy 换算）。

use dsh_compaction::{
    BasicCompactionConfig, CompactionPolicyConfig, ModelCompactPolicyConfig,
    ResolvedRetention, resolve_config, resolve_compact_spec, resolve_target_policy,
};

fn default_config() -> BasicCompactionConfig {
    BasicCompactionConfig::default()
}

// ---- resolve_config ----

#[test]
fn resolve_defaults() {
    let resolved = resolve_config(&default_config()).unwrap();
    assert_eq!(resolved.threshold_ratio, 0.8);
    assert_eq!(
        resolved.retention,
        ResolvedRetention::RetainRatio { retain_ratio: 0.16 }
    );
    assert_eq!(resolved.max_tokens, 8192);
    assert_eq!(resolved.compaction_retries, 1);
    assert_eq!(resolved.max_overflow_retries, 1);
    assert!(resolved.auto);
}

#[test]
fn resolve_ratio_retention_override() {
    let cfg = BasicCompactionConfig {
        policy: CompactionPolicyConfig {
            threshold_ratio: Some(0.9),
            retain_ratio: Some(0.2),
            ..Default::default()
        },
        ..Default::default()
    };
    let resolved = resolve_config(&cfg).unwrap();
    assert_eq!(resolved.threshold_ratio, 0.9);
    assert_eq!(resolved.retention, ResolvedRetention::RetainRatio { retain_ratio: 0.2 });
}

#[test]
fn resolve_exact_retain_tokens() {
    let cfg = BasicCompactionConfig {
        policy: CompactionPolicyConfig {
            retain_tokens: Some(4096),
            ..Default::default()
        },
        ..Default::default()
    };
    let resolved = resolve_config(&cfg).unwrap();
    assert_eq!(resolved.retention, ResolvedRetention::RetainTokens { retain_tokens: 4096 });
}

#[test]
fn reject_ratio_out_of_range() {
    for bad in [0.0, -0.1, 1.5, f64::NAN, f64::INFINITY] {
        let cfg = BasicCompactionConfig {
            policy: CompactionPolicyConfig { threshold_ratio: Some(bad), ..Default::default() },
            ..Default::default()
        };
        assert!(resolve_config(&cfg).is_err(), "ratio {bad} should be rejected");
    }
}

#[test]
fn reject_retain_ratio_at_or_above_threshold() {
    let cfg = BasicCompactionConfig {
        policy: CompactionPolicyConfig {
            threshold_ratio: Some(0.8),
            retain_ratio: Some(0.8),
            ..Default::default()
        },
        ..Default::default()
    };
    let err = resolve_config(&cfg).unwrap_err();
    assert!(err.contains("retainRatio"));
}

#[test]
fn reject_retain_ratio_and_tokens_together() {
    let cfg = BasicCompactionConfig {
        policy: CompactionPolicyConfig {
            retain_ratio: Some(0.2),
            retain_tokens: Some(100),
            ..Default::default()
        },
        ..Default::default()
    };
    let err = resolve_config(&cfg).unwrap_err();
    assert!(err.contains("mutually exclusive"));
}

#[test]
fn reject_zero_max_tokens() {
    let cfg = BasicCompactionConfig {
        policy: CompactionPolicyConfig { max_tokens: Some(0), ..Default::default() },
        ..Default::default()
    };
    assert!(resolve_config(&cfg).is_err());
}

#[test]
fn reject_single_summarization_target() {
    let cfg = BasicCompactionConfig {
        policy: CompactionPolicyConfig {
            summarization_provider: Some("deepseek".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(resolve_config(&cfg).is_err());
}

#[test]
fn accept_empty_summarization_pair() {
    let cfg = BasicCompactionConfig {
        policy: CompactionPolicyConfig {
            summarization_provider: Some(String::new()),
            summarization_model: Some(String::new()),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(resolve_config(&cfg).is_ok());
}

#[test]
fn reject_duplicate_model_policy() {
    let cfg = BasicCompactionConfig {
        model_policies: Some(vec![
            ModelCompactPolicyConfig {
                provider: "deepseek".into(),
                model: "chat".into(),
                policy: CompactionPolicyConfig::default(),
            },
            ModelCompactPolicyConfig {
                provider: "deepseek".into(),
                model: "chat".into(),
                policy: CompactionPolicyConfig::default(),
            },
        ]),
        ..Default::default()
    };
    let err = resolve_config(&cfg).unwrap_err();
    assert!(err.contains("duplicate"));
}

#[test]
fn reject_empty_model_policy_target() {
    let cfg = BasicCompactionConfig {
        model_policies: Some(vec![ModelCompactPolicyConfig {
            provider: String::new(),
            model: "chat".into(),
            policy: CompactionPolicyConfig::default(),
        }]),
        ..Default::default()
    };
    assert!(resolve_config(&cfg).is_err());
}

// ---- resolve_target_policy ----

#[test]
fn target_policy_inherits_default() {
    let resolved = resolve_config(&default_config()).unwrap();
    let policy = resolve_target_policy(&resolved, "deepseek", "chat");
    assert_eq!(policy.target, ("deepseek".to_string(), "chat".to_string()));
    assert_eq!(policy.threshold_ratio, 0.8);
    assert_eq!(policy.retention, ResolvedRetention::RetainRatio { retain_ratio: 0.16 });
    assert_eq!(policy.max_tokens, 8192);
}

#[test]
fn target_policy_applies_override() {
    let cfg = BasicCompactionConfig {
        model_policies: Some(vec![ModelCompactPolicyConfig {
            provider: "deepseek".into(),
            model: "chat".into(),
            policy: CompactionPolicyConfig {
                threshold_ratio: Some(0.5),
                retain_tokens: Some(1000),
                summarization_provider: Some("deepseek".into()),
                summarization_model: Some("deepseek-r1".into()),
                max_tokens: Some(2048),
                compaction_retries: Some(3),
                ..Default::default()
            },
        }]),
        ..Default::default()
    };
    let resolved = resolve_config(&cfg).unwrap();
    let policy = resolve_target_policy(&resolved, "deepseek", "chat");
    assert_eq!(policy.threshold_ratio, 0.5);
    assert_eq!(policy.retention, ResolvedRetention::RetainTokens { retain_tokens: 1000 });
    assert_eq!(policy.summarization_model, "deepseek-r1");
    assert_eq!(policy.max_tokens, 2048);
    assert_eq!(policy.compaction_retries, 3);

    // 其他 target 继承默认
    let other = resolve_target_policy(&resolved, "anthropic", "claude");
    assert_eq!(other.threshold_ratio, 0.8);
}

// ---- resolve_compact_spec ----

#[test]
fn compact_spec_derives_threshold_and_retain() {
    let resolved = resolve_config(&default_config()).unwrap();
    let policy = resolve_target_policy(&resolved, "deepseek", "chat");
    let spec = resolve_compact_spec(&policy, 128_000).unwrap();
    assert_eq!(spec.context_window, 128_000);
    assert_eq!(spec.threshold_tokens, 102_400); // 0.8 * 128k
    assert_eq!(spec.retain_tokens, 20_480); // 0.16 * 128k
}

#[test]
fn compact_spec_exact_retain_tokens() {
    let cfg = BasicCompactionConfig {
        policy: CompactionPolicyConfig { retain_tokens: Some(5000), ..Default::default() },
        ..Default::default()
    };
    let resolved = resolve_config(&cfg).unwrap();
    let policy = resolve_target_policy(&resolved, "deepseek", "chat");
    let spec = resolve_compact_spec(&policy, 100_000).unwrap();
    assert_eq!(spec.retain_tokens, 5000);
}

#[test]
fn compact_spec_rejects_zero_context_window() {
    let resolved = resolve_config(&default_config()).unwrap();
    let policy = resolve_target_policy(&resolved, "deepseek", "chat");
    let err = resolve_compact_spec(&policy, 0).unwrap_err();
    assert!(err.message.contains("positive integer"));
}

#[test]
fn compact_spec_rejects_retain_at_threshold() {
    // resolve_config 已拒绝 retain >= threshold；直接构造非法 policy 测试 spec 级守卫
    let policy = dsh_compaction::ResolvedTargetPolicy {
        target: ("deepseek".into(), "chat".into()),
        threshold_ratio: 0.5,
        retention: ResolvedRetention::RetainTokens { retain_tokens: 5000 },
        summarization_provider: String::new(),
        summarization_model: String::new(),
        max_tokens: 8192,
        compaction_retries: 1,
        max_overflow_retries: 1,
    };
    let err = resolve_compact_spec(&policy, 10_000).unwrap_err();
    assert!(err.message.contains("must be less than"));
}
