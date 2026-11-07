//! 常规页「允许多个实例同时运行」关着时的单实例行为：第二次启动把文件交给
//! 已经开着的那个窗口，自己退出。
//!
//! 两件东西一起用。一个同名互斥体回答「是不是已经有另一个进程在跑」——它
//! 比窗口早建立，所以两次几乎同时的启动不会各自开一个窗；一个窗口属性回答
//! 「哪个窗口是这份 exe 的」，属性里带 exe 路径的指纹，另一个目录里的副本
//! 不会把文件抢走。

#![allow(unsafe_op_in_unsafe_fn)]

use super::window::CLASS_NAME;
use crate::launch::Launch;
use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};
use windows::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM, WPARAM,
};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcessId, Sleep};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, ChangeWindowMessageFilterEx, FindWindowExW, GetPropW,
    GetWindowThreadProcessId, IsIconic, SendMessageTimeoutW, SetForegroundWindow, SetPropW,
    ShowWindow, ASFW_ANY, MSGFLT_ALLOW, SMTO_ABORTIFHUNG, SW_RESTORE, WM_COPYDATA,
};
use windows::core::{PCWSTR, w};

/// `COPYDATASTRUCT.dwData`：一个只属于本程序的记号，免得把别的程序发来的
/// WM_COPYDATA 当成一份启动请求。
const COPYDATA_TAG: usize = 0x4447_4544;
const WINDOW_PROP: PCWSTR = w!("DogEdit.Instance");

/// 句柄留到进程结束：它一关，互斥体就可能被系统回收，下一次启动就认不出
/// 「还有一个进程在跑」了。
static INSTANCE_MUTEX: AtomicIsize = AtomicIsize::new(0);

/// exe 路径的指纹。大小写不敏感、按字节算，同一份 exe 的两个进程算出来一样。
fn instance_token() -> usize {
    let path = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    (hash as usize) | 1
}

/// 认领互斥体，返回「另一个进程已经认领过」。
unsafe fn claim_mutex() -> bool {
    let name: Vec<u16> = format!("DogEdit.Instance.{:016X}", instance_token())
        .encode_utf16()
        .chain([0])
        .collect();
    match CreateMutexW(None, false, PCWSTR(name.as_ptr())) {
        Ok(handle) => {
            INSTANCE_MUTEX.store(handle.0 as isize, Ordering::Relaxed);
            GetLastError() == ERROR_ALREADY_EXISTS
        }
        Err(_) => false,
    }
}

/// 找到这份 exe 的主窗口。同名类可能有好几个（选项开着时本来就能开多份），
/// 按 Z 序取最上面那个带对指纹的。
unsafe fn find_window() -> Option<HWND> {
    let token = instance_token();
    let self_pid = GetCurrentProcessId();
    let mut after: Option<HWND> = None;
    for _ in 0..64 {
        let hwnd = match FindWindowExW(None, after, CLASS_NAME, PCWSTR::null()) {
            Ok(h) => h,
            Err(_) => break,
        };
        after = Some(hwnd);
        if GetPropW(hwnd, WINDOW_PROP).0 as usize != token {
            continue;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != 0 && pid != self_pid {
            return Some(hwnd);
        }
    }
    None
}

/// 另一个进程可能刚起来、窗口还没建好。等它两秒，等不到就自己开窗，别把
/// 用户挡在门外。
unsafe fn wait_for_window() -> Option<HWND> {
    let start = GetTickCount();
    loop {
        if let Some(hwnd) = find_window() {
            return Some(hwnd);
        }
        if GetTickCount().wrapping_sub(start) >= 2000 {
            return None;
        }
        Sleep(100);
    }
}

unsafe fn send(hwnd: HWND, bytes: &[u8]) -> bool {
    let mut data = COPYDATASTRUCT {
        dwData: COPYDATA_TAG,
        cbData: bytes.len() as u32,
        lpData: bytes.as_ptr() as *mut c_void,
    };
    let mut result: usize = 0;
    let sent = SendMessageTimeoutW(
        hwnd,
        WM_COPYDATA,
        WPARAM(0),
        LPARAM(&mut data as *mut COPYDATASTRUCT as isize),
        SMTO_ABORTIFHUNG,
        2000,
        Some(&mut result),
    );
    sent.0 != 0 && result != 0
}

/// `[Setting] MulInst`：开着就是每次启动都另起一个进程。
fn multi_instance_allowed() -> bool {
    let Some(value) = crate::settings::section("Setting", "MulInst") else {
        return false;
    };
    bool_value(&value)
}

fn bool_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "y" | "yes" | "1" | "true" | "on"
    )
}

/// 建窗之后调一次：别的进程靠这个属性认人；同时放行 WM_COPYDATA，从资源
/// 管理器（普通权限）点开文件时，提权运行的窗口也收得到。
pub(super) unsafe fn mark_window(hwnd: HWND) {
    let _ = SetPropW(
        hwnd,
        WINDOW_PROP,
        Some(HANDLE(instance_token() as *mut c_void)),
    );
    let _ = ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, None);
}

/// 解一份别的进程送来的请求。只在 WM_COPYDATA 的调用栈里用：指针出了这个
/// 消息就失效了。
pub(super) unsafe fn decode(lparam: LPARAM) -> Option<Launch> {
    if lparam.0 == 0 {
        return None;
    }
    let data = &*(lparam.0 as *const COPYDATASTRUCT);
    if data.dwData != COPYDATA_TAG || data.lpData.is_null() || data.cbData == 0 {
        return None;
    }
    let bytes = std::slice::from_raw_parts(data.lpData as *const u8, data.cbData as usize);
    Launch::from_handoff(bytes)
}

/// 返回 true 表示这次启动已经交给别人，`run` 应当直接退出、不再建窗。
pub(super) unsafe fn try_handoff(launch: &Launch) -> bool {
    let another = claim_mutex();
    if !another || launch.new_instance || launch.safe_mode || multi_instance_allowed() {
        return false;
    }
    let Some(hwnd) = wait_for_window() else {
        return false;
    };
    let bytes = launch.handoff_bytes();
    if !send(hwnd, &bytes) {
        return false;
    }
    if IsIconic(hwnd).as_bool() {
        let _ = ShowWindow(hwnd, SW_RESTORE);
    }
    let _ = AllowSetForegroundWindow(ASFW_ANY);
    let _ = SetForegroundWindow(hwnd);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ini_switch_accepts_the_same_spellings_as_the_options_page() {
        for yes in ["Y", "y", "yes", "1", "TRUE", " on "] {
            assert!(bool_value(yes), "{yes}");
        }
        for no in ["N", "no", "0", "false", "off", ""] {
            assert!(!bool_value(no), "{no}");
        }
    }

    #[test]
    fn the_token_is_stable_and_not_a_null_property() {
        assert_eq!(instance_token(), instance_token());
        assert_ne!(instance_token(), 0);
    }
}
