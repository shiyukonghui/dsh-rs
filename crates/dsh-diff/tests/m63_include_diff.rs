//! M63：include 差分场景——`run_include`（纯函数级 `apply_entry_patches` 对比）。
//! 覆盖 insert 进 group / 顶层追加 / 嵌套命中 / 各 warn 诊断，与 TS 宿主
//! `include-host.mjs` 逐行一致的 trace 形态（golden 由 verify-diff 维护）。

use dsh_diff::run_include;

#[test]
fn run_include_apply_patches_matches_ts_shape() {
    let json = r#"{
      "name": "include-01-apply-patches-full",
      "data": [
        { "id": "a", "name": "a", "config": { "k": 1 } },
        { "id": "g", "name": "g", "config": [
          { "id": "c1", "name": "c1", "config": { "inner": 0 } }
        ], "group": true }
      ],
      "patches": [
        { "id": "a", "config": { "k": 2 } },
        { "insert": [ { "id": "x", "name": "x" } ] },
        { "id": "g", "insert": [ { "id": "c2", "name": "c2", "config": { "inner": 9 } } ] },
        { "id": "a", "disabled": true },
        { "id": "ghost", "config": {} },
        { "id": "a", "insert": [ { "id": "e", "name": "e" } ] },
        { "id": "nope", "insert": [ { "id": "e2", "name": "e2" } ] },
        { "config": {} },
        { "id": "a", "name": "WRONG", "config": { "z": 1 } },
        { "id": "c1", "config": { "nested": true } }
      ]
    }"#;

    let lines = run_include(json).unwrap();

    // data 行（输入 entry 列表，按键序）
    assert!(lines[0].starts_with("include-data:"), "{}", lines[0]);
    assert!(lines[0].contains("\"id\":\"a\""));
    assert!(lines[0].contains("\"id\":\"g\""));

    // warn 按序（对齐 TS printf 展开）
    let warns: Vec<&str> = lines
        .iter()
        .filter(|l| l.starts_with("include-warn:"))
        .map(|l| l.trim_start_matches("include-warn:"))
        .collect();
    assert_eq!(
        warns,
        vec![
            "patch: entry ghost not found",
            "patch insert: entry a is not a group",
            "patch insert: entry nope not found",
            "patch: id is required for non-insert patches",
            "patch: name mismatch for a (expected a, got WRONG), skipping",
        ]
    );

    // result 行：a.config 覆盖、a.disabled=true、顶层追加 x、group 插入 c2、嵌套命中 c1
    let result = lines
        .iter()
        .find(|l| l.starts_with("include-result:"))
        .expect("result line");
    assert!(result.contains("\"id\":\"a\",\"inject\":[],\"intercept\":{},\"isolate\":{},\"name\":\"a\""));
    assert!(result.contains("\"k\":2"));
    assert!(result.contains("\"disabled\":true"));
    assert!(result.contains("\"id\":\"x\""));
    assert!(result.contains("\"id\":\"c2\""));
    assert!(result.contains("\"nested\":true"));
}
