use std::{
    ffi::c_void,
    mem::{size_of, zeroed},
    path::PathBuf,
    ptr,
};

use anyhow::{Context, Result, bail, ensure};
use libloading::Library;
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Storage::FileSystem::{CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING},
    System::{
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        Pipes::{
            GetNamedPipeServerProcessId, PIPE_READMODE_MESSAGE, SetNamedPipeHandleState,
            TransactNamedPipe, WaitNamedPipeW,
        },
        Registry::HKEY_LOCAL_MACHINE,
        Services::{
            CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx,
            SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
            SERVICE_STATUS_PROCESS,
        },
        Threading::{CreateEventW, WaitForSingleObject},
    },
};

use crate::win::{registry_string, wide};

const IA32_TEMPERATURE_TARGET: u64 = 0x1A2;
const IA32_PACKAGE_THERM_STATUS: u64 = 0x1B1;
const INTEL_MSR_MODULE: &[u8] = include_bytes!("../vendor/pawnio-modules-0.2.10/IntelMSR.bin");
pub(crate) const ENDPOINT_KEY: &str = r"SOFTWARE\LoopPanel";
pub(crate) const ENDPOINT_VALUE: &str = "TemperatureEndpoint";
pub(crate) const PIPE_NAME_PREFIX: &str = r"\\.\pipe\LoopPanelTemperature-";
const REQUEST_TEMPERATURE: u8 = 1;
const RESPONSE_OK: u8 = 0;
const PIPE_CONNECT_TIMEOUT_MS: u32 = 100;
const PIPE_RESPONSE_TIMEOUT_MS: u32 = 250;

type Handle = *mut c_void;
type PawnOpen = unsafe extern "system" fn(*mut Handle) -> i32;
type PawnLoad = unsafe extern "system" fn(Handle, *const u8, usize) -> i32;
type PawnExecute = unsafe extern "system" fn(
    Handle,
    *const u8,
    *const u64,
    usize,
    *mut u64,
    usize,
    *mut usize,
) -> i32;
type PawnClose = unsafe extern "system" fn(Handle) -> i32;

pub struct CpuTemperature {
    pawn: PawnIo,
    tj_max: f32,
}

impl CpuTemperature {
    pub fn open() -> Result<Self> {
        let pawn = PawnIo::open()?;
        let target = pawn.read_msr(IA32_TEMPERATURE_TARGET)?;
        let tj_max = ((target >> 16) & 0xff) as f32;
        ensure!(tj_max > 0.0, "IA32_TEMPERATURE_TARGET 没有返回有效的 TjMax");
        Ok(Self { pawn, tj_max })
    }

    pub fn sample(&mut self) -> Result<f32> {
        let status = self.pawn.read_msr(IA32_PACKAGE_THERM_STATUS)?;
        package_temperature(self.tj_max, status)
    }
}

pub fn service_sample() -> Result<u32> {
    let service_pid = running_service_pid()?;
    let endpoint = registry_string(HKEY_LOCAL_MACHINE, ENDPOINT_KEY, ENDPOINT_VALUE)
        .context("CPU 温度服务尚未发布命名管道")?;
    let (registered_pid, pipe_name) = endpoint
        .split_once('|')
        .context("CPU 温度服务发布了无效的命名管道信息")?;
    let registered_pid = registered_pid
        .parse::<u32>()
        .context("CPU 温度服务发布了无效的进程编号")?;
    ensure!(
        registered_pid == service_pid,
        "CPU 温度服务正在切换实例，请稍后重试"
    );
    ensure!(
        pipe_name.starts_with(PIPE_NAME_PREFIX),
        "CPU 温度服务发布了无效的命名管道名称"
    );

    let pipe_name = wide(pipe_name);
    let available = unsafe { WaitNamedPipeW(pipe_name.as_ptr(), PIPE_CONNECT_TIMEOUT_MS) };
    if available == 0 {
        bail!(
            "CPU 温度服务暂时不可用：{}",
            std::io::Error::last_os_error()
        );
    }
    let pipe = unsafe {
        CreateFileW(
            pipe_name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            ptr::null_mut(),
        )
    };
    ensure!(
        pipe != INVALID_HANDLE_VALUE,
        "无法连接 CPU 温度服务：{}",
        std::io::Error::last_os_error()
    );
    let pipe = OwnedHandle(pipe);

    let mut actual_pid = 0_u32;
    ensure!(
        unsafe { GetNamedPipeServerProcessId(pipe.0, &mut actual_pid) } != 0,
        "无法验证 CPU 温度服务进程：{}",
        std::io::Error::last_os_error()
    );
    ensure!(
        actual_pid == service_pid,
        "命名管道并非由当前 CPU 温度服务创建"
    );
    let mode = PIPE_READMODE_MESSAGE;
    ensure!(
        unsafe { SetNamedPipeHandleState(pipe.0, &mode, ptr::null(), ptr::null()) } != 0,
        "无法设置 CPU 温度管道模式：{}",
        std::io::Error::last_os_error()
    );

    let event = OwnedHandle(unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) });
    ensure!(
        !event.0.is_null(),
        "无法创建 CPU 温度管道事件：{}",
        std::io::Error::last_os_error()
    );
    let mut operation: OVERLAPPED = unsafe { zeroed() };
    operation.hEvent = event.0;
    let request = [REQUEST_TEMPERATURE];
    let mut response = [0_u8; 3];
    let started = unsafe {
        TransactNamedPipe(
            pipe.0,
            request.as_ptr().cast(),
            request.len() as u32,
            response.as_mut_ptr().cast(),
            response.len() as u32,
            ptr::null_mut(),
            &mut operation,
        )
    };
    let read = if started != 0 {
        completed_bytes(pipe.0, &mut operation)?
    } else {
        let error = unsafe { GetLastError() };
        ensure!(
            error == ERROR_IO_PENDING,
            "CPU 温度服务请求失败：{}",
            std::io::Error::from_raw_os_error(error as i32)
        );
        match unsafe { WaitForSingleObject(event.0, PIPE_RESPONSE_TIMEOUT_MS) } {
            WAIT_OBJECT_0 => completed_bytes(pipe.0, &mut operation)?,
            WAIT_TIMEOUT => {
                cancel_and_drain(pipe.0, &mut operation);
                bail!("等待 CPU 温度服务响应超时");
            }
            _ => {
                cancel_and_drain(pipe.0, &mut operation);
                bail!(
                    "等待 CPU 温度服务响应失败：{}",
                    std::io::Error::last_os_error()
                );
            }
        }
    };
    decode_service_response(&response[..read as usize])
}

fn running_service_pid() -> Result<u32> {
    unsafe {
        let manager = OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_CONNECT);
        ensure!(
            !manager.is_null(),
            "无法打开服务控制管理器：{}",
            std::io::Error::last_os_error()
        );
        let manager = OwnedServiceHandle(manager);
        let name = wide("LoopPanelTemperature");
        let service = OpenServiceW(manager.0, name.as_ptr(), SERVICE_QUERY_STATUS);
        ensure!(
            !service.is_null(),
            "CPU 温度服务尚未安装或不可用：{}",
            std::io::Error::last_os_error()
        );
        let service = OwnedServiceHandle(service);
        let mut status: SERVICE_STATUS_PROCESS = zeroed();
        let mut needed = 0_u32;
        ensure!(
            QueryServiceStatusEx(
                service.0,
                SC_STATUS_PROCESS_INFO,
                (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
                size_of::<SERVICE_STATUS_PROCESS>() as u32,
                &mut needed,
            ) != 0,
            "无法查询 CPU 温度服务状态：{}",
            std::io::Error::last_os_error()
        );
        ensure!(
            status.dwCurrentState == SERVICE_RUNNING && status.dwProcessId != 0,
            "CPU 温度服务当前未运行"
        );
        Ok(status.dwProcessId)
    }
}

fn completed_bytes(handle: HANDLE, operation: &mut OVERLAPPED) -> Result<u32> {
    let mut transferred = 0_u32;
    let success = unsafe { GetOverlappedResult(handle, operation, &mut transferred, 0) };
    ensure!(
        success != 0,
        "CPU 温度管道操作失败：{}",
        std::io::Error::last_os_error()
    );
    Ok(transferred)
}

pub(crate) fn cancel_and_drain(handle: HANDLE, operation: &mut OVERLAPPED) {
    unsafe {
        CancelIoEx(handle, operation);
        let mut transferred = 0_u32;
        GetOverlappedResult(handle, operation, &mut transferred, 1);
    }
}

struct PawnIo {
    _library: Library,
    handle: Handle,
    execute: PawnExecute,
    close: PawnClose,
}

impl PawnIo {
    fn open() -> Result<Self> {
        let library_path = pawnio_install_directory()?.join("PawnIOLib.dll");
        let library = unsafe { Library::new(&library_path) }
            .with_context(|| format!("无法加载 {}", library_path.display()))?;
        unsafe {
            let open = *library
                .get::<PawnOpen>(b"pawnio_open\0")
                .context("PawnIOLib.dll 缺少 pawnio_open")?;
            let load = *library
                .get::<PawnLoad>(b"pawnio_load\0")
                .context("PawnIOLib.dll 缺少 pawnio_load")?;
            let execute = *library
                .get::<PawnExecute>(b"pawnio_execute\0")
                .context("PawnIOLib.dll 缺少 pawnio_execute")?;
            let close = *library
                .get::<PawnClose>(b"pawnio_close\0")
                .context("PawnIOLib.dll 缺少 pawnio_close")?;

            let mut handle = ptr::null_mut();
            check_hresult(open(&mut handle), "无法打开 PawnIO")?;
            if handle.is_null() {
                bail!("PawnIO 返回了空执行器句柄");
            }
            if let Err(error) = check_hresult(
                load(handle, INTEL_MSR_MODULE.as_ptr(), INTEL_MSR_MODULE.len()),
                "无法加载官方 IntelMSR 模块",
            ) {
                close(handle);
                return Err(error);
            }
            Ok(Self {
                _library: library,
                handle,
                execute,
                close,
            })
        }
    }

    fn read_msr(&self, address: u64) -> Result<u64> {
        let input = [address];
        let mut output = [0_u64];
        let mut returned = 0_usize;
        let result = unsafe {
            (self.execute)(
                self.handle,
                c"ioctl_read_msr".as_ptr().cast(),
                input.as_ptr(),
                input.len(),
                output.as_mut_ptr(),
                output.len(),
                &mut returned,
            )
        };
        check_hresult(result, &format!("无法读取 MSR 0x{address:X}"))?;
        ensure!(
            returned == 1,
            "PawnIO 返回了错误的 MSR 结果长度：{returned}"
        );
        Ok(output[0])
    }
}

impl Drop for PawnIo {
    fn drop(&mut self) {
        unsafe {
            (self.close)(self.handle);
        }
    }
}

fn pawnio_install_directory() -> Result<PathBuf> {
    let directory = registry_string(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\PawnIO",
        "InstallLocation",
    )
    .context("未找到 PawnIO 2.x 安装信息")?;
    ensure!(!directory.is_empty(), "PawnIO 安装目录为空");
    Ok(PathBuf::from(directory))
}

fn package_temperature(tj_max: f32, status: u64) -> Result<f32> {
    ensure!(status & (1 << 31) != 0, "CPU Package 温度读数当前无效");
    let distance = ((status >> 16) & 0x7f) as f32;
    Ok(tj_max - distance)
}

pub(crate) fn encode_service_response(temperature: Result<f32>) -> [u8; 3] {
    match temperature {
        Ok(value) => {
            let value = value.round().clamp(0.0, u16::MAX as f32) as u16;
            [RESPONSE_OK, value as u8, (value >> 8) as u8]
        }
        Err(_) => [1, 0, 0],
    }
}

fn decode_service_response(response: &[u8]) -> Result<u32> {
    ensure!(response.len() == 3, "CPU 温度服务返回了错误的响应长度");
    ensure!(response[0] == RESPONSE_OK, "CPU 温度服务当前没有有效读数");
    Ok(u16::from_le_bytes([response[1], response[2]]) as u32)
}

fn check_hresult(result: i32, action: &str) -> Result<()> {
    if result < 0 {
        bail!("{action}（HRESULT 0x{:08X}）", result as u32);
    }
    Ok(())
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0) };
        }
    }
}

struct OwnedServiceHandle(windows_sys::Win32::System::Services::SC_HANDLE);

impl Drop for OwnedServiceHandle {
    fn drop(&mut self) {
        unsafe { CloseServiceHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_service_response, encode_service_response, package_temperature};

    #[test]
    fn decodes_package_temperature() {
        let status = (1_u64 << 31) | (42_u64 << 16);
        assert_eq!(package_temperature(105.0, status).unwrap(), 63.0);
    }

    #[test]
    fn rejects_invalid_package_temperature() {
        assert!(package_temperature(105.0, 42_u64 << 16).is_err());
    }

    #[test]
    fn service_protocol_has_one_fixed_temperature_response() {
        let encoded = encode_service_response(Ok(63.4));
        assert_eq!(encoded, [0, 63, 0]);
        assert_eq!(decode_service_response(&encoded).unwrap(), 63);
        assert!(decode_service_response(&[0, 63]).is_err());
        assert!(decode_service_response(&[1, 0, 0]).is_err());
    }
}
