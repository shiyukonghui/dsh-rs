//! §M7：async 编排路径的差分验证——深嵌套（3 层）场景与 TS golden 逐行一致。
//!
//! 同步路径（两阶段延迟）在 3 层嵌套上与 TS 存在顺序偏差（HANDOFF §6 记录）；
//! `plugin_arc_async` 用真实 `yield_now` 微任务队列复刻 Cordis `_reload` 的两个
//! 让出点，深嵌套应与 TS 逐行一致。

use dsh_diff::{Runner, diff_trace};

const SCENARIO: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scenarios/09-deep-nesting-3-levels.json"
);
const GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scenarios/09-deep-nesting-3-levels.golden"
);

/// async 路径：3 层嵌套 trace 与 TS golden 逐行一致。
#[tokio::test]
async fn async_deep_nesting_matches_ts_golden() {
    let text = std::fs::read_to_string(SCENARIO).expect("read scenario");
    let scenario: dsh_diff::Scenario = serde_json::from_str(&text).expect("parse scenario JSON");

    let mut runner = Runner::new();
    let trace = runner.run_async(&scenario).await.expect("run async scenario");

    let golden_text = std::fs::read_to_string(GOLDEN).expect("read golden");
    let golden_lines: Vec<String> = golden_text
        .lines()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let diffs = diff_trace(&trace, &golden_lines);
    if !diffs.is_empty() {
        eprintln!("--- rust (async) ---");
        for l in &trace {
            eprintln!("  {l}");
        }
        eprintln!("--- diffs ---");
        for d in &diffs {
            eprintln!("  {d}");
        }
    }
    assert!(diffs.is_empty(), "async deep-nesting diverges from TS golden");
}
