//! dsh-subprocess：有界收集 spill 落盘（M5-DESIGN §2.2/§2.3）。
//!
//! 参考 `subprocess/subprocess/src/types.ts`：`StdoutMode::Collect{ maxBytes, spill? }`——
//! 带 spill = 超出 max_bytes 时完整流落盘且 `spillPath` 可恢复；不带 spill = 仅内存
//! tail（诊断形）。本测试驱动 spill 路径真实写盘 + readFrom(0) 仍返回批结果。

use dsh_subprocess::{
    ChildStdio, StdinMode, StdoutMode, SubprocessCollect, SubprocessSpawnSpec, SubprocessSpill,
};

#[test]
fn collect_with_spill_writes_spill_file_when_over_budget() {
    let dir = std::env::temp_dir().join(format!("dshspill-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let spec = SubprocessSpawnSpec {
        argv: vec!["cmd".to_string(), "/c".to_string(), "for".to_string(), "/l".to_string(),
                   "%i".to_string(), "in".to_string(), "(1,1,5000)".to_string(), "do".to_string(),
                   "echo".to_string(), "hello".to_string()],
        cwd: std::env::current_dir().expect("cwd"),
        stdio: ChildStdio {
            stdin: StdinMode::Ignore,
            stdout: StdoutMode::Collect(SubprocessCollect {
                max_bytes: 200, // 远小于 5000 行输出 → 必然溢出
                spill: Some(SubprocessSpill { max_bytes: 1024 * 1024, dir: dir.clone() }),
            }),
            stderr: StdoutMode::Collect(SubprocessCollect { max_bytes: 4096, spill: None }),
        },
        grace_ms: 1000,
        signal: None,
        env: None,
    };

    let mut handle = dsh_subprocess::spawn(&spec).expect("spawn ok");
    // 不再持有 stdout 读端时 wait 仍应能回收（收集在线程 drain）
    let outcome = handle.wait();
    assert_eq!(
        outcome.exit_code, Some(0),
        "for-loop exits 0, stderr={:?}",
        handle.collected_stderr()
    );

    let out = handle.collected_stdout();
    // 内存 tail 被截到 max_bytes；spill 文件存在且可恢复完整量
    let _ = std::fs::read_dir(&dir).expect("spill dir exists");
    let spill_files: Vec<_> = std::fs::read_dir(&dir)
        .expect("spill dir exists")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    assert!(!spill_files.is_empty(), "spill file should be created");
    let spill_path = &spill_files[0];
    let recovered = std::fs::read_to_string(spill_path).unwrap_or_default();
    assert!(recovered.contains("hello"), "spill recovers full stream");
    assert!(out.len() <= 200, "in-memory tail capped at max_bytes, got {}", out.len());

    let _ = std::fs::remove_dir_all(&dir);
}
