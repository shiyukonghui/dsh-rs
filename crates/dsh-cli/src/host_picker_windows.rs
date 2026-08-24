//! Windows 原生目录选择绑定（`cfg(windows)`，D-098）——**进程内** IFileDialog/COM。
//!
//! 对应 DSH TS `directory-picker-native/src/win32-dialog-bindings.ts`：IFileOpenDialog/
//! IShellItem 的 COM 会话，但用新版 `windows` crate（0.62）的成熟类型化绑定，而非
//! 自搓 vtable。零子进程——不 spawn powershell / 任何外部进程，杀软不涉。
//!
//! 结论自 `run_folder_dialog`（host_picker.rs 纯时序）+ 本绑定：`WindowsDialogBindings`
//! 只做真正的 COM 调用，选中/取消/失败语义由时序层统一。

use crate::host_picker::{DialogBindings, FolderDialog, HRESULT_CANCELLED};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, IShellItem, FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR,
    FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};

/// 在调用线程执行一次真实的原生目录选择（Windows，进程内）。
///
/// 直接跑在 RPC 线程上：该线程此前未初始化 COM，本函数以 STA 初始化、显示模态对话框
/// （阻塞至用户选择/取消——native 能力的 user-paced 语义）、收尾。若调用线程已有 COM
/// 公寓，`CoInitializeEx` 返回 `S_FALSE` 仍是成功（对齐 TS 处理）。
pub fn pick_directory_on_windows() -> Result<Option<String>, String> {
    run_folder_dialog_windows("Select a folder")
}

fn run_folder_dialog_windows(title: &str) -> Result<Option<String>, String> {
    crate::host_picker::run_folder_dialog(&mut WindowsDialogBindings, title)
}

/// 真实 Win32 COM 绑定：掩藏平台类型，仅暴露 `DialogBindings`/`FolderDialog` 表面。
pub struct WindowsDialogBindings;

impl DialogBindings for WindowsDialogBindings {
    fn co_initialize_sta(&mut self) -> Result<(), String> {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr.is_err() {
            return Err(format!(
                "CoInitializeEx failed: HRESULT 0x{:08x}",
                hr.0 as u32
            ));
        }
        Ok(())
    }

    fn co_uninitialize(&mut self) {
        unsafe { CoUninitialize() }
    }

    fn create_folder_dialog(&mut self, title: &str) -> Result<Box<dyn FolderDialog>, String> {
        let dialog: IFileOpenDialog =
            unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| format!("CoCreateInstance(FileOpenDialog) failed: {e}"))?;
        unsafe {
            dialog
                .SetOptions(FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR)
                .map_err(|e| format!("IFileDialog::SetOptions failed: {e}"))?;
            let title16: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
            dialog
                .SetTitle(PCWSTR(title16.as_ptr()))
                .map_err(|e| format!("IFileDialog::SetTitle failed: {e}"))?;
        }
        Ok(Box::new(Win32FolderDialog { dialog }))
    }
}

struct Win32FolderDialog {
    dialog: IFileOpenDialog,
}

impl FolderDialog for Win32FolderDialog {
    fn show(&mut self) -> Result<Option<String>, String> {
        match unsafe { self.dialog.Show(None) } {
            Ok(()) => {}
            // HRESULT_CANCELLED = 用户关闭 → 取消（Ok(None)）。
            Err(e) if e.code().0 == HRESULT_CANCELLED => return Ok(None),
            Err(e) => return Err(format!("IModalWindow::Show failed: {e}")),
        }
        let item: IShellItem = unsafe { self.dialog.GetResult() }
            .map_err(|e| format!("IFileDialog::GetResult failed: {e}"))?;
        let name: PWSTR = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }
            .map_err(|e| format!("IShellItem::GetDisplayName failed: {e}"))?;
        let path = unsafe { crate::host_picker::decode_wstring(name.0) };
        unsafe { CoTaskMemFree(Some(name.0 as *const core::ffi::c_void)) };
        Ok(Some(path))
    }
}
