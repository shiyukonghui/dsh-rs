//! Windows 平台：Job Object 树级终止（M5-DESIGN §2.5「Windows 平台面」）。
//!
//! `taskkill /T /F` 在受限环境（无创建/终止远进程特权）会被拒（Access denied），且存在
//! PID 复用竞态。Job Object 是更稳健的原语：① spawn 后立即 AssignProcessToJobObject，
//! 其后代自动继承 job 成员资格；② 设 `KILL_ON_JOB_CLOSE` 保证句柄关闭即整树终止；
//! ③ `TerminateJobObject` 显式整树终止。仅在可用时使用，失败静默降级（返回 None，
//! 调用方回退 `child.kill()`），绝不因环境问题牺牲确定性终止。

#![cfg(windows)]

use std::mem::size_of;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

/// Job Object 句柄：Drop 时随 `KILL_ON_JOB_CLOSE` 整树终止。
pub struct Job {
    handle: HANDLE,
}

unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    /// 尝试创建一个带 `KILL_ON_JOB_CLOSE` 的 Job Object。
    pub fn new() -> Option<Job> {
        // SAFETY: 空安全属性 + 匿名 job；失败返回 NULL/INVALID。
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: info 指向有效的扩展信息结构，大小精确。
        let ok = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            // SAFETY: 关闭创建的句柄。
            unsafe { CloseHandle(handle) };
            return None;
        }
        Some(Job { handle })
    }

    /// 把进程（按 pid）加入 job；失败（已在其它 job / 权限不足）返回 false。
    pub fn add_pid(&self, pid: u32) -> bool {
        // SAFETY: 打开进程句柄（设置配额 + 终止），随后赋入 job。
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            return false;
        }
        // SAFETY: 有效进程句柄；assign 失败仅返回 0。
        let ok = unsafe { AssignProcessToJobObject(self.handle, process) };
        // SAFETY: 关闭打开的进程句柄。
        unsafe { CloseHandle(process) };
        ok != 0
    }

    /// 显式整树终止（等价 `taskkill /T /F` 的确定性实现）。
    pub fn kill(&self) {
        // SAFETY: 有效 job 句柄；退出码 1。
        unsafe { TerminateJobObject(self.handle, 1) };
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: 关闭 job 句柄；若进程还活着则 KILL_ON_JOB_CLOSE 触发整树终止。
        unsafe { CloseHandle(self.handle) };
    }
}
