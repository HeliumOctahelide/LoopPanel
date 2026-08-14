use std::mem::size_of;

use anyhow::{Result, bail};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle as WinCloseHandle, ERROR_ALREADY_EXISTS, ERROR_NO_MORE_FILES, GetLastError,
        HANDLE,
    },
    System::Threading::CreateMutexW,
};

type Handle = *mut std::ffi::c_void;
const INVALID_HANDLE_VALUE: Handle = -1_isize as Handle;
const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;

#[repr(C)]
struct ProcessEntry {
    size: u32,
    usage: u32,
    process_id: u32,
    default_heap_id: usize,
    module_id: u32,
    threads: u32,
    parent_process_id: u32,
    priority: i32,
    flags: u32,
    executable: [u16; 260],
}

impl Default for ProcessEntry {
    fn default() -> Self {
        Self {
            size: size_of::<Self>() as u32,
            usage: 0,
            process_id: 0,
            default_heap_id: 0,
            module_id: 0,
            threads: 0,
            parent_process_id: 0,
            priority: 0,
            flags: 0,
            executable: [0; 260],
        }
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> Handle;
    fn Process32FirstW(snapshot: Handle, entry: *mut ProcessEntry) -> i32;
    fn Process32NextW(snapshot: Handle, entry: *mut ProcessEntry) -> i32;
    fn CloseHandle(handle: Handle) -> i32;
}

pub fn official_app_running() -> Result<bool> {
    process_running("JONSBO-AIO.exe")
}

pub fn ensure_official_app_stopped() -> Result<()> {
    if official_app_running()? {
        bail!("JONSBO-AIO 仍在运行。请先从系统托盘彻底退出它，再启动 LoopPanel");
    }
    Ok(())
}

pub struct DisplayLock(HANDLE);

impl DisplayLock {
    pub fn acquire() -> Result<Self> {
        // Keep the pre-LoopPanel lock name so an older build cannot write the same screen at once.
        Self::acquire_named("Global\\TM360-Lite-Display")
    }

    fn acquire_named(name: &str) -> Result<Self> {
        let name = name.encode_utf16().chain([0]).collect::<Vec<_>>();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            bail!(
                "无法创建 LoopPanel 单实例锁：{}",
                std::io::Error::last_os_error()
            );
        }
        let error = unsafe { GetLastError() };
        if error == ERROR_ALREADY_EXISTS {
            unsafe { WinCloseHandle(handle) };
            bail!("已有一个 LoopPanel 实例正在控制屏幕");
        }
        Ok(Self(handle))
    }
}

impl Drop for DisplayLock {
    fn drop(&mut self) {
        unsafe { WinCloseHandle(self.0) };
    }
}

fn process_running(expected: &str) -> Result<bool> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        bail!(
            "无法检查正在运行的程序：{}",
            std::io::Error::last_os_error()
        );
    }

    let snapshot = SnapshotHandle(snapshot);
    let mut entry = ProcessEntry::default();
    if unsafe { Process32FirstW(snapshot.0, &mut entry) } == 0 {
        let error = unsafe { GetLastError() };
        if error == ERROR_NO_MORE_FILES {
            return Ok(false);
        }
        bail!(
            "无法读取进程列表：{}",
            std::io::Error::from_raw_os_error(error as i32)
        );
    }
    loop {
        let length = entry
            .executable
            .iter()
            .position(|character| *character == 0)
            .unwrap_or(entry.executable.len());
        let name = String::from_utf16_lossy(&entry.executable[..length]);
        if name.eq_ignore_ascii_case(expected) {
            return Ok(true);
        }
        entry = ProcessEntry::default();
        if unsafe { Process32NextW(snapshot.0, &mut entry) } == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_MORE_FILES {
                return Ok(false);
            }
            bail!(
                "读取进程列表时发生错误：{}",
                std::io::Error::from_raw_os_error(error as i32)
            );
        }
    }
}

struct SnapshotHandle(Handle);

impl Drop for SnapshotHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::DisplayLock;

    #[test]
    fn display_lock_rejects_a_second_instance() {
        let name = format!("Local\\LoopPanel-Test-{}", std::process::id());
        let first = DisplayLock::acquire_named(&name).unwrap();
        assert!(DisplayLock::acquire_named(&name).is_err());
        drop(first);
        assert!(DisplayLock::acquire_named(&name).is_ok());
    }
}
