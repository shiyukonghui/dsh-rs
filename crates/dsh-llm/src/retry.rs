//! 重试策略：provider 拥有的请求重试策略配置与解析（对齐
//! `deepseek-harness/packages/llm/llm/src/retry-policy.ts` + `llm-retry/src/index.ts`
//! 的 `localDelay`）。
//!
//! 纯函数、可序列化、可差分：`resolve_retry_policy` 校验/默认/脱离配置，
//! `local_delay` 计算有界指数退避 + 对称抖动。

use serde::{Deserialize, Serialize};

/// 无内容但正常结束的响应的规范 code（`EMPTY_RESPONSE`）；重试策略视其可重试。
pub const EMPTY_RESPONSE_CODE: &str = "EMPTY_RESPONSE";

pub const DEFAULT_MAX_RETRIES: u64 = 5;
pub const DEFAULT_INITIAL_DELAY_MS: u64 = 500;
pub const DEFAULT_MAX_DELAY_MS: u64 = 10_000;
pub const DEFAULT_JITTER_RATIO: f64 = 0.1;
/// 默认可重试 transient code（对齐 TS `DEFAULT_RETRYABLE_CODES`）。
pub const DEFAULT_RETRYABLE_CODES: [&str; 5] =
    [EMPTY_RESPONSE_CODE, "RATE_LIMIT", "SERVER", "TIMEOUT", "TRANSPORT"];

/// 有界指数退避 + 对称抖动（`BackoffConfig`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BackoffConfig {
    /// 初始本地指数退避延迟（毫秒；缺省 500）。
    pub initial_delay_ms: u64,
    /// 本地调度或 provider 延迟的上限（毫秒；缺省 10000）。
    pub max_delay_ms: u64,
    /// 围绕 1 的对称随机乘子范围（缺省 0.1）。
    pub jitter_ratio: f64,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        BackoffConfig {
            initial_delay_ms: DEFAULT_INITIAL_DELAY_MS,
            max_delay_ms: DEFAULT_MAX_DELAY_MS,
            jitter_ratio: DEFAULT_JITTER_RATIO,
        }
    }
}

/// `NormalRetryPolicyConfig`：只对配置的 transient code 重试。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalRetryPolicyConfig {
    pub max_retries: u64,
    #[serde(default)]
    pub retryable_codes: Vec<String>,
    #[serde(default)]
    pub backoff: BackoffConfig,
}

/// `AlwaysRetryPolicyConfig`：对每次模型请求失败无界重试。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlwaysRetryPolicyConfig {
    #[serde(default)]
    pub backoff: BackoffConfig,
}

/// `RetryPolicyConfig`（provider 拥有的重试策略配置）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum RetryPolicyConfig {
    #[serde(rename = "normal")]
    Normal(NormalRetryPolicyConfig),
    #[serde(rename = "always")]
    Always(AlwaysRetryPolicyConfig),
}

/// 已解析的本地退避参数（normal/always 共享）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedRetryBackoff {
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub jitter_ratio: f64,
}

/// 已解析的有界 transient 重试策略。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedNormalRetryPolicy {
    pub backoff: ResolvedRetryBackoff,
    pub max_retries: u64,
    pub retryable_codes: Vec<String>,
}

/// 已解析的无界重试策略。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedAlwaysRetryPolicy {
    pub backoff: ResolvedRetryBackoff,
}

/// 不可变的 provider 已解析策略（注册路由时捕获）。
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedRetryPolicy {
    Normal(ResolvedNormalRetryPolicy),
    Always(ResolvedAlwaysRetryPolicy),
}

impl ResolvedRetryPolicy {
    pub fn mode(&self) -> &'static str {
        match self {
            ResolvedRetryPolicy::Normal(_) => "normal",
            ResolvedRetryPolicy::Always(_) => "always",
        }
    }
    pub fn backoff(&self) -> ResolvedRetryBackoff {
        match self {
            ResolvedRetryPolicy::Normal(p) => p.backoff,
            ResolvedRetryPolicy::Always(p) => p.backoff,
        }
    }
    /// 一次失败是否落在本 normal 策略的可重试集合内。
    pub fn is_retryable(&self, code: &str) -> bool {
        match self {
            ResolvedRetryPolicy::Normal(p) => p.retryable_codes.iter().any(|c| c == code),
            ResolvedRetryPolicy::Always(_) => true,
        }
    }
    /// layering key（对齐 `retryPolicyKey`）：normal 含 maxRetries/codes，always 仅退避。
    pub fn policy_key(&self) -> String {
        match self {
            ResolvedRetryPolicy::Always(p) => {
                format!("always,{},{},{}", p.backoff.initial_delay_ms, p.backoff.max_delay_ms, p.backoff.jitter_ratio)
            }
            ResolvedRetryPolicy::Normal(p) => {
                let mut codes = p.retryable_codes.clone();
                codes.sort();
                format!(
                    "normal,{},{},{},{},{}",
                    p.max_retries,
                    codes.join(","),
                    p.backoff.initial_delay_ms,
                    p.backoff.max_delay_ms,
                    p.backoff.jitter_ratio
                )
            }
        }
    }
}

/// 校验、默认化并脱离一份 provider 拥有的重试策略。
/// `path` 命名拥有该配置的 provider 段（诊断用）；缺省采用 normal 默认值。
pub fn resolve_retry_policy(
    config: Option<&RetryPolicyConfig>,
    path: &str,
) -> Result<ResolvedRetryPolicy, String> {
    let backoff = ResolvedRetryBackoff {
        initial_delay_ms: DEFAULT_INITIAL_DELAY_MS,
        max_delay_ms: DEFAULT_MAX_DELAY_MS,
        jitter_ratio: DEFAULT_JITTER_RATIO,
    };
    match config {
        None => Ok(ResolvedRetryPolicy::Normal(ResolvedNormalRetryPolicy {
            backoff,
            max_retries: DEFAULT_MAX_RETRIES,
            retryable_codes: DEFAULT_RETRYABLE_CODES.iter().map(|s| s.to_string()).collect(),
        })),
        Some(RetryPolicyConfig::Normal(cfg)) => {
            let resolved_backoff = resolve_backoff(&cfg.backoff, &format!("{path}.backoff"))?;
            if cfg.retryable_codes.is_empty() {
                return Err(format!("{path}.retryableCodes must not be empty"));
            }
            if cfg.retryable_codes.iter().any(|c| c.is_empty()) {
                return Err(format!("{path}.retryableCodes must contain only non-empty strings"));
            }
            let mut seen = std::collections::HashSet::new();
            if cfg.retryable_codes.iter().any(|c| !seen.insert(c.clone())) {
                return Err(format!("{path}.retryableCodes must not contain duplicates"));
            }
            Ok(ResolvedRetryPolicy::Normal(ResolvedNormalRetryPolicy {
                backoff: resolved_backoff,
                max_retries: cfg.max_retries,
                retryable_codes: cfg.retryable_codes.clone(),
            }))
        }
        Some(RetryPolicyConfig::Always(cfg)) => Ok(ResolvedRetryPolicy::Always(
            ResolvedAlwaysRetryPolicy {
                backoff: resolve_backoff(&cfg.backoff, &format!("{path}.backoff"))?,
            },
        )),
    }
}

fn resolve_backoff(config: &BackoffConfig, path: &str) -> Result<ResolvedRetryBackoff, String> {
    let BackoffConfig { initial_delay_ms, max_delay_ms, jitter_ratio } = *config;
    if initial_delay_ms == 0 {
        return Err(format!("{path}.initialDelayMs must be a positive finite number"));
    }
    if max_delay_ms == 0 {
        return Err(format!("{path}.maxDelayMs must be a positive finite number"));
    }
    if initial_delay_ms > max_delay_ms {
        return Err(format!("{path}.initialDelayMs must be less than or equal to maxDelayMs"));
    }
    if !jitter_ratio.is_finite() || !(0.0..=1.0).contains(&jitter_ratio) {
        return Err(format!("{path}.jitterRatio must be between 0 and 1"));
    }
    Ok(ResolvedRetryBackoff { initial_delay_ms, max_delay_ms, jitter_ratio })
}

/// 有界指数退避 + 对称抖动（对齐 llm-retry `localDelay`）。
/// `retry` 从 1 起（首次重试）；`random` 返回 [0,1)，测试用确定性钩子。
pub fn local_delay(config: ResolvedRetryBackoff, retry: u64, random: f64) -> u64 {
    let exponent = (retry - 1).min(1024);
    let exponential = (config.initial_delay_ms as f64 * 2f64.powi(exponent as i32)).min(config.max_delay_ms as f64);
    let jitter = 1.0 - config.jitter_ratio + 2.0 * config.jitter_ratio * random;
    (exponential * jitter).min(config.max_delay_ms as f64) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normal() -> BackoffConfig {
        BackoffConfig { initial_delay_ms: 500, max_delay_ms: 10_000, jitter_ratio: 0.1 }
    }

    #[test]
    fn default_resolution_is_normal_with_defaults() {
        let policy = resolve_retry_policy(None, "llm: test").unwrap();
        match &policy {
            ResolvedRetryPolicy::Normal(p) => {
                assert_eq!(p.max_retries, 5);
                assert_eq!(p.retryable_codes.len(), 5);
                assert!(p.retryable_codes.contains(&"RATE_LIMIT".to_string()));
                assert_eq!(p.backoff.initial_delay_ms, 500);
                assert_eq!(p.backoff.max_delay_ms, 10_000);
                assert_eq!(p.backoff.jitter_ratio, 0.1);
            }
            ResolvedRetryPolicy::Always(_) => panic!("default must be normal"),
        }
    }

    #[test]
    fn always_mode_resolves_backoff() {
        let cfg = RetryPolicyConfig::Always(AlwaysRetryPolicyConfig { backoff: normal() });
        match resolve_retry_policy(Some(&cfg), "p").unwrap() {
            ResolvedRetryPolicy::Always(p) => {
                assert_eq!(p.backoff.initial_delay_ms, 500);
            }
            _ => panic!("expected always"),
        }
    }

    #[test]
    fn empty_retryable_codes_rejected() {
        let cfg = RetryPolicyConfig::Normal(NormalRetryPolicyConfig {
            max_retries: 3,
            retryable_codes: vec![],
            backoff: normal(),
        });
        let err = resolve_retry_policy(Some(&cfg), "p").unwrap_err();
        assert!(err.contains("must not be empty"));
    }

    #[test]
    fn duplicate_retryable_codes_rejected() {
        let cfg = RetryPolicyConfig::Normal(NormalRetryPolicyConfig {
            max_retries: 3,
            retryable_codes: vec!["RATE_LIMIT".into(), "RATE_LIMIT".into()],
            backoff: normal(),
        });
        let err = resolve_retry_policy(Some(&cfg), "p").unwrap_err();
        assert!(err.contains("must not contain duplicates"));
    }

    #[test]
    fn inverse_backoff_rejected() {
        let cfg = RetryPolicyConfig::Normal(NormalRetryPolicyConfig {
            max_retries: 3,
            retryable_codes: vec!["SERVER".into()],
            backoff: BackoffConfig { initial_delay_ms: 100, max_delay_ms: 10, jitter_ratio: 0.0 },
        });
        let err = resolve_retry_policy(Some(&cfg), "p").unwrap_err();
        assert!(err.contains("less than or equal to maxDelayMs"));
    }

    #[test]
    fn out_of_range_jitter_rejected() {
        let cfg = RetryPolicyConfig::Always(AlwaysRetryPolicyConfig {
            backoff: BackoffConfig { ..normal() },
        });
        let bad = RetryPolicyConfig::Always(AlwaysRetryPolicyConfig {
            backoff: BackoffConfig { jitter_ratio: 1.5, ..normal() },
        });
        assert!(resolve_retry_policy(Some(&cfg), "p").is_ok());
        assert!(resolve_retry_policy(Some(&bad), "p").unwrap_err().contains("between 0 and 1"));
    }

    #[test]
    fn retryable_predicate_respects_mode_and_set() {
        let normal_policy = resolve_retry_policy(None, "p").unwrap();
        assert!(normal_policy.is_retryable("RATE_LIMIT"));
        assert!(!normal_policy.is_retryable("AUTH"));
        let always = resolve_retry_policy(
            Some(&RetryPolicyConfig::Always(AlwaysRetryPolicyConfig { backoff: normal() })),
            "p",
        )
        .unwrap();
        assert!(always.is_retryable("AUTH"));
    }

    #[test]
    fn local_delay_zero_jitter_is_exponential_capped() {
        let backoff = ResolvedRetryBackoff { initial_delay_ms: 100, max_delay_ms: 1000, jitter_ratio: 0.0 };
        // retry 1: 100 * 2^0 = 100
        assert_eq!(local_delay(backoff, 1, 0.5), 100);
        // retry 4: 100 * 2^3 = 800
        assert_eq!(local_delay(backoff, 4, 0.5), 800);
        // retry 5: 100 * 2^4 = 1600 → 封顶 1000
        assert_eq!(local_delay(backoff, 5, 0.5), 1000);
    }

    #[test]
    fn local_delay_jitter_scales_symmetrically() {
        let backoff = ResolvedRetryBackoff { initial_delay_ms: 100, max_delay_ms: 10_000, jitter_ratio: 0.1 };
        // retry 1: exponential 100; jitter = 0.9 + 0.2*random
        let low = local_delay(backoff, 1, 0.0);
        let mid = local_delay(backoff, 1, 0.5);
        let high = local_delay(backoff, 1, 1.0);
        assert_eq!(low, 90);
        assert_eq!(mid, 100);
        assert_eq!(high, 110);
    }

    #[test]
    fn policy_key_distinguishes_modes_and_options() {
        let a = resolve_retry_policy(None, "p").unwrap();
        let b = resolve_retry_policy(
            Some(&RetryPolicyConfig::Normal(NormalRetryPolicyConfig {
                max_retries: 5,
                retryable_codes: DEFAULT_RETRYABLE_CODES.iter().map(|s| s.to_string()).collect(),
                backoff: normal(),
            })),
            "p",
        )
        .unwrap();
        assert_eq!(a.policy_key(), b.policy_key());
        let always = resolve_retry_policy(
            Some(&RetryPolicyConfig::Always(AlwaysRetryPolicyConfig { backoff: normal() })),
            "p",
        )
        .unwrap();
        assert_ne!(a.policy_key(), always.policy_key());
    }
}
