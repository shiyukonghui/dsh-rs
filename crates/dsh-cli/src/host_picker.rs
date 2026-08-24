//! M3a+（D-098）`host.pickDirectory` 原生目录选择——纯时序层。
//!
//! 结构与 DSH TS 的 `directory-picker-native/src/win32-dialog-logic.ts` 同构：
//! 「面向可注入 bindings 的**纯时序**（run_folder_dialog）+ 平台绑定」分离。时序层
//! 无任何平台/COM 依赖，全用假后端单测——选中/取消/失败/清理顺序全部可验证；
//! 真实 Windows 绑定在 `host_picker_windows`（cfg(windows)）里。
//!
//! 三态语义对齐 TS apiproxy 的 native 能力：`Ok(Some(path))` 选中 /
//! `Ok(None)` 用户取消（wire `{path:null}`）/ `Err(msg)` 初始化或对话框失败
//! （wire `directory-picker-unavailable`——**绝不**拿取消冒充不可用）。

/// `HRESULT_FROM_WIN32(ERROR_CANCELLED)`：用户关闭对话框。
pub const HRESULT_CANCELLED: i32 = 0x800704c7u32 as i32;

/// `FOS_PICKFOLDERS`：对话框只选目录、不选文件。
pub const FOS_PICKFOLDERS: u32 = 0x20;
/// `FOS_FORCEFILESYSTEM`：只允许带文件系统路径的结果。
pub const FOS_FORCEFILESYSTEM: u32 = 0x40;
/// `FOS_NOCHANGEDIR`：绝不改动进程当前工作目录。
pub const FOS_NOCHANGEDIR: u32 = 0x8;

/// 一个已创建的目录选择对话框（配置好 FOS 选项与标题）。
pub trait FolderDialog {
    /// 模态显示；`Ok(None)` = 用户取消。
    fn show(&mut self) -> Result<Option<String>, String>;
}

/// 对话框所在线程的 COM 公寓表面（真实 = Win32 COM；测试 = 脚本化假后端）。
pub trait DialogBindings {
    /// 本线程 COM 公寓初始化。成功包括 `S_OK` 与 `S_FALSE`（重入仍算成功，对齐 TS）。
    fn co_initialize_sta(&mut self) -> Result<(), String>;
    /// 与一次成功的 `co_initialize_sta` 恰配对一次。
    fn co_uninitialize(&mut self);
    /// 创建配置好的目录选择对话框（FOS 选项 + 标题在创建时设定）。
    fn create_folder_dialog(&mut self, title: &str) -> Result<Box<dyn FolderDialog>, String>;
}

/// 一次模态目录选择会话：公寓初始化 → 创建对话框 → 显示 → 提取结果，并在**每条**路径
/// 上恰配对一次 `co_uninitialize`（含失败路径，对齐 TS `runFolderDialog` 的
/// try/finally 语义）。
pub fn run_folder_dialog(
    bindings: &mut dyn DialogBindings,
    title: &str,
) -> Result<Option<String>, String> {
    bindings.co_initialize_sta()?;
    let result = (|| {
        let mut dialog = bindings.create_folder_dialog(title)?;
        dialog.show()
    })();
    bindings.co_uninitialize();
    result
}

/// 把 NUL 结尾的 UTF-16 指针解码为 `String`（带长度上限防失控，对齐 TS `readUtf16`
/// 的 32k 上限；丢失的代理对以 U+FFFD 顶替）。
///
/// # Safety
/// `data` 必须指向一个可达的、NUL 结尾的 UTF-16 缓冲区（长度 ≤ 32768），且在其被读取
/// 期间保持有效。
pub unsafe fn decode_wstring(data: *const u16) -> String {
    const MAX_UNITS: usize = 32768;
    let mut units = Vec::with_capacity(64);
    for i in 0..MAX_UNITS {
        let unit = *data.add(i);
        if unit == 0 {
            break;
        }
        units.push(unit);
    }
    String::from_utf16_lossy(&units)
}

/// 宿主目录选择器入口（web `Boot.host_picker` 装配对象）：Windows = 进程内原生对话框；
/// 非 Windows = 诚实不可用（wire `directory-picker-unavailable`，不 spawn 子进程）。
#[cfg(windows)]
pub fn pick_directory_native() -> Result<Option<String>, String> {
    crate::host_picker_windows::pick_directory_on_windows()
}
#[cfg(not(windows))]
pub fn pick_directory_native() -> Result<Option<String>, String> {
    Err("native directory picker is unsupported on this platform".to_string())
}

#[cfg(test)]
mod tests {
    use super::{run_folder_dialog, DialogBindings, FolderDialog, HRESULT_CANCELLED};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// 脚本化假后端：只记录 `co_uninitialize` 配对次数，可装配创建/显示结果。
    /// （真实对话框的 release 由其 Drop 自动完成；时序层的义务是 init 后必 uninit 恰一次。）
    struct Scripted {
        uninit: Rc<RefCell<u32>>,
        dialog: Box<dyn FolderDialog>,
        create_error: Option<String>,
    }
    impl Scripted {
        fn show_result(result: Result<Option<String>, String>) -> Self {
            Scripted {
                uninit: Rc::new(RefCell::new(0)),
                dialog: Box::new(ShowScript { result }),
                create_error: None,
            }
        }
    }
    impl DialogBindings for Scripted {
        fn co_initialize_sta(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn co_uninitialize(&mut self) {
            *self.uninit.borrow_mut() += 1;
        }
        fn create_folder_dialog(&mut self, _title: &str) -> Result<Box<dyn FolderDialog>, String> {
            if let Some(err) = &self.create_error {
                return Err(err.clone());
            }
            let dlg =
                std::mem::replace(&mut self.dialog, Box::new(ShowScript { result: Ok(None) }));
            Ok(dlg)
        }
    }

    struct ShowScript {
        result: Result<Option<String>, String>,
    }
    impl FolderDialog for ShowScript {
        fn show(&mut self) -> Result<Option<String>, String> {
            self.result.clone()
        }
    }

    /// 选中 → Ok(Some(path))。
    #[test]
    fn selected_path_returns_some() {
        let mut b = Scripted::show_result(Ok(Some("C:\\proj".to_string())));
        let out = run_folder_dialog(&mut b, "Select a folder").unwrap();
        assert_eq!(out, Some("C:\\proj".to_string()));
        assert_eq!(*b.uninit.borrow(), 1, "co_uninitialize called exactly once");
    }

    /// 取消 → Ok(None)（HRESULT_CANCELLED 在绑定层翻译为 Ok(None)）。
    #[test]
    fn cancel_returns_none() {
        let mut b = Scripted::show_result(Ok(None));
        let out = run_folder_dialog(&mut b, "Select a folder").unwrap();
        assert_eq!(out, None);
        assert_eq!(*b.uninit.borrow(), 1);
    }

    /// 显示失败 → Err（不冒充取消）。
    #[test]
    fn show_failure_is_err_and_still_uninit() {
        let mut b = Scripted::show_result(Err("Show failed".to_string()));
        let err = run_folder_dialog(&mut b, "Select a folder").unwrap_err();
        assert!(err.contains("Show failed"), "{err}");
        assert_eq!(*b.uninit.borrow(), 1, "uninit on failure path too");
    }

    /// 创建失败 → Err（初始化后仍未成对——创建失败发生在 init 之后，须 uninit）。
    #[test]
    fn create_failure_is_err_and_uninit() {
        let mut b = Scripted::show_result(Ok(None));
        b.create_error = Some("CoCreateInstance failed".to_string());
        let err = run_folder_dialog(&mut b, "Select a folder").unwrap_err();
        assert!(err.contains("CoCreateInstance failed"), "{err}");
        assert_eq!(
            *b.uninit.borrow(),
            1,
            "uninit even when create fails after init"
        );
    }

    /// 常量防漂移：HRESULT_CANCELLED 必须等于 `HRESULT_FROM_WIN32(ERROR_CANCELLED)`。
    #[test]
    fn cancelled_const_never_drifts() {
        // 0x800704c7 = 0x80070000(severity+facility) | 0x04c7(1259 = ERROR_CANCELLED)。
        assert_eq!(HRESULT_CANCELLED, 0x800704c7u32 as i32);
        // FOS 常量对齐 Vista 起的稳定 ABI（TS 同值）。
        assert_eq!(super::FOS_PICKFOLDERS, 0x20);
        assert_eq!(super::FOS_FORCEFILESYSTEM, 0x40);
        assert_eq!(super::FOS_NOCHANGEDIR, 0x8);
    }

    /// `decode_wstring`：NUL 结尾 UTF-16 → String；空串与非 ASCII 均正确。
    #[test]
    fn decode_wstring_handles_ascii_and_cjk() {
        let data: [u16; 5] = ['C' as u16, ':' as u16, '\\' as u16, 0, 0];
        assert_eq!(unsafe { super::decode_wstring(data.as_ptr()) }, "C:\\");
        // 「选择目录」的 UTF-16 -> 「选择目录」（CJK 无代理对，lossy 不变）。
        let cjk: Vec<u16> = "选择目录"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        assert_eq!(unsafe { super::decode_wstring(cjk.as_ptr()) }, "选择目录");
        // 代理对（emoji）也完整。
        let emoji: Vec<u16> = "盘😀".encode_utf16().chain(std::iter::once(0)).collect();
        assert_eq!(unsafe { super::decode_wstring(emoji.as_ptr()) }, "盘😀");
    }
}
