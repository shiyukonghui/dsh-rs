//! M3a+（D-096）`host.pickDirectory` 原生目录选择后端。
//!
//! 用 `powershell.exe -STA` + `System.Windows.Forms.FolderBrowserDialog` 弹出系统的
//! 文件夹选择框（桌面会话）。三态语义对齐 TS apiproxy seam（native 能力）：
//! - `Ok(Some(path))` —— 用户选中；
//! - `Ok(None)` —— 用户取消（wire `{path: null}`）；
//! - `Err(msg)` —— 无法打开/失败（wire `directory-picker-unavailable`，**绝不**拿
//!   「取消」冒充不可用——修复 D-096 前这里老实返回 `{path:null}`，前端无弹窗）。
//!
//! 评估（D-096）：TS 原生实现是 IFileDialog/COM 后台 worker（
//! `deepseek-harness/packages/host/directory-picker-native/src/win32-dialog-worker.ts`）；
//! 我们的务实等价是子进程 + 经典 FolderBrowserDialog（XP+ 至 Win11 均可用），换系统级
//! 现代 IFileDialog 留作后续（成本/收益当前不成比例）。`interpret` 独立可测；弹框本身
//! 交互由用户驱动，单测不触发。

use std::process::Command;

/// 打开原生文件夹选择；三态见模块注释。
pub fn pick_directory_native() -> Result<Option<String>, String> {
    let script = [
        "$OutputEncoding=[Console]::OutputEncoding=[System.Text.Encoding]::UTF8",
        "Add-Type -AssemblyName System.Windows.Forms",
        "$d=New-Object System.Windows.Forms.FolderBrowserDialog",
        "$d.Description='Select a folder'",
        "$o=New-Object System.Windows.Forms.Form",
        "$o.TopMost=$true",
        "$r=$d.ShowDialog($o)",
        "if($r -eq [System.Windows.Forms.DialogResult]::OK){ $d.SelectedPath }",
    ]
    .join("; ");
    let output = Command::new("powershell.exe")
        .arg("-NoProfile")
        .arg("-STA")
        .arg("-WindowStyle")
        .arg("Hidden")
        .arg("-Command")
        .arg(&script)
        .output()
        .map_err(|e| format!("cannot spawn native folder picker (powershell.exe): {e}"))?;
    interpret(
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// 解释子进程输出：无 stdout+无 stderr = 取消；无 stdout+有 stderr = 失败；
/// 有 stdout = 选中路径（末行）。
fn interpret(
    _status: Option<i32>,
    stdout: String,
    stderr: String,
) -> Result<Option<String>, String> {
    let path = stdout
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string());
    if let Some(path) = path {
        return Ok(Some(path));
    }
    if let Some(err) = stderr.lines().map(str::trim).find(|l| !l.is_empty()) {
        return Err(format!("native folder picker failed: {err}"));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::interpret;

    #[test]
    fn interprets_selected_path_as_some() {
        assert_eq!(
            interpret(Some(0), "C:\\proj\n".into(), "".into()).unwrap(),
            Some("C:\\proj".into())
        );
        // CRLF + 尾随空行也归一到末行有效路径。
        assert_eq!(
            interpret(Some(0), "C:\\proj\r\n\r\n".into(), "".into()).unwrap(),
            Some("C:\\proj".into())
        );
    }

    #[test]
    fn interprets_cancel_as_none() {
        // 取消：无输出、无 stderr → Ok(None)（wire {path:null}）。
        assert_eq!(interpret(Some(0), "".into(), "".into()).unwrap(), None);
        assert_eq!(
            interpret(Some(0), "\r\n  \r\n".into(), "".into()).unwrap(),
            None
        );
    }

    #[test]
    fn interprets_failure_as_unavailable() {
        // 脚本末行失败：stderr 非空 → Err（不冒充取消）。
        let err = interpret(
            Some(1),
            "".into(),
            "Add-Type : The name 'System.Windows.Forms' is not available\n".into(),
        )
        .unwrap_err();
        assert!(err.contains("native folder picker failed"), "{err}");
    }
}
