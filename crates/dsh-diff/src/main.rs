//! dsh-diff CLI：读取场景 JSON → 执行 → 输出规范化 trace / 校验 golden。
//!
//! 用法：
//! - `dsh-diff <scenario.json>`：执行并打印 trace。
//! - `dsh-diff <scenario.json> --golden <file>`：执行并逐行对比 golden。
//! - `dsh-diff <scenario.json> --record <file>`：执行并写入 golden。
//! - `--async`：用 M7 异步编排执行（真实微任务让出；深嵌套场景与 TS 一致）。

use std::path::PathBuf;

use dsh_diff::{Runner, diff_trace};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: dsh-diff <scenario.json> [--golden <file> | --record <file>] [--async]");
        std::process::exit(2);
    }
    let scenario_path = PathBuf::from(&args[1]);
    let mut golden: Option<PathBuf> = None;
    let mut record = false;
    let mut use_async = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--golden" => {
                i += 1;
                golden = Some(PathBuf::from(&args[i]));
            }
            "--record" => {
                record = true;
                i += 1;
                golden = Some(PathBuf::from(&args[i]));
            }
            "--async" => {
                use_async = true;
            }
            other => {
                eprintln!("unknown arg {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let text = std::fs::read_to_string(&scenario_path).expect("read scenario");
    let scenario_json: serde_json::Value =
        serde_json::from_str(&text).expect("parse scenario JSON");

    // M63：include 差分场景（顶层含 `data`/`patches`，无 `steps`）→ 纯函数级。
    let trace: Vec<String> = if scenario_json.get("patches").is_some() {
        dsh_diff::run_include(&text).expect("run include scenario")
    } else {
        let scenario: dsh_diff::Scenario = serde_json::from_value(scenario_json)
            .unwrap_or_else(|e| panic!("parse scenario: {e}"));
        if use_async {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build tokio runtime");
            rt.block_on(async {
                let local = tokio::task::LocalSet::new();
                local
                    .run_until(async {
                        let mut runner = Runner::new();
                        runner.run_async(&scenario).await
                    })
                    .await
            })
            .expect("run scenario async")
        } else {
            let mut runner = Runner::new();
            runner.run(&scenario).expect("run scenario")
        }
    };
    let trace_text = trace.join("\n") + "\n";

    match (golden, record) {
        (Some(path), true) => {
            std::fs::write(&path, &trace_text).expect("write golden");
            println!("recorded {} lines -> {}", trace.len(), path.display());
        }
        (Some(path), false) => {
            let golden_text = std::fs::read_to_string(&path).expect("read golden");
            let golden_lines: Vec<String> = golden_text
                .lines()
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let diffs = diff_trace(&trace, &golden_lines);
            if diffs.is_empty() {
                println!("PASS: {} lines match {}", trace.len(), path.display());
            } else {
                eprintln!("FAIL: {} diffs vs {}", diffs.len(), path.display());
                for d in diffs {
                    eprintln!("  {d}");
                }
                std::process::exit(1);
            }
        }
        (None, _) => {
            print!("{trace_text}");
        }
    }
}
