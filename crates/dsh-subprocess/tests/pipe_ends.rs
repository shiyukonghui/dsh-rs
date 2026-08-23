//! dsh-subprocess：Pipe 端暴露原语测试（协议式交互后端用：stdin 持续写 + stdout 裸读）。

use dsh_subprocess::{
    spawn, ChildStdio, StdoutMode, StdinMode, SubprocessSpawnSpec, SubprocessCollect,
};
use std::io::{Read, Write};
use std::path::PathBuf;

/// 回显子进程：读 stdin 直至 EOF，原样写 stdout（Windows `more` / Unix `cat` 语义）。
fn echor() -> (String, PathBuf) {
    #[cfg(windows)]
    { ("cmd".to_string(), PathBuf::from(".")) }
    #[cfg(not(windows))]
    { ("cat".to_string(), PathBuf::from(".")) }
}

fn echor_spec(stdio: ChildStdio) -> SubprocessSpawnSpec {
    let (prog, cwd) = echor();
    let mut argv = vec![prog];
    #[cfg(windows)]
    argv.extend(["/c".into(), "more".into()]);
    SubprocessSpawnSpec { argv, cwd, stdio, grace_ms: 3_000, signal: None, env: None }
}

#[test]
fn pipe_write_take_stdin_then_read_stdout() {
    let stdio = ChildStdio {
        stdin: StdinMode::Pipe,
        stdout: StdoutMode::Pipe,
        stderr: StdoutMode::Collect(SubprocessCollect { max_bytes: 4096, spill: None }),
    };
    let mut handle = spawn(&echor_spec(stdio)).expect("spawn");
    {
        let writer = handle.stdin_writer().expect("stdin end held");
        writer.write_all(b"hello-pipe\n").expect("write");
        writer.flush().expect("flush");
    }
    // 关闭写端 → 子进程 stdin EOF → 退出
    drop(handle.take_stdin().expect("take stdin"));
    let outcome = handle.wait();
    assert_eq!(outcome.exit_code, Some(0));

    let mut reader = handle.take_stdout_reader().expect("stdout bare reader");
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).expect("read stdout");
    let text = String::from_utf8_lossy(&buf);
    assert!(text.contains("hello-pipe"), "echoed: {text:?}");
}

#[test]
fn pipe_stdin_writer_absent_for_non_pipe_modes() {
    let stdio = ChildStdio {
        stdin: StdinMode::Ignore,
        stdout: StdoutMode::Pipe,
        stderr: StdoutMode::Inherit,
    };
    let mut handle = spawn(&echor_spec(stdio)).expect("spawn");
    assert!(handle.stdin_writer().is_none(), "Ignore → 无写端");
    let mut reader = handle.take_stdout_reader().expect("stdout bare reader");
    let mut buf = Vec::new();
    let _ = reader.read_to_end(&mut buf);
    let _ = handle.wait();
}
