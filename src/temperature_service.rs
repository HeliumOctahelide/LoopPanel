use std::{
    ffi::c_void,
    fmt::Write as _,
    fs,
    mem::{size_of, zeroed},
    os::windows::ffi::OsStrExt,
    path::PathBuf,
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use libloading::Library;
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING,
        ERROR_PIPE_CONNECTED, ERROR_SERVICE_CANNOT_ACCEPT_CTRL, ERROR_SERVICE_DOES_NOT_EXIST,
        ERROR_SERVICE_NOT_ACTIVE, ERROR_SERVICE_SPECIFIC_ERROR, ERROR_SUCCESS, GetLastError,
        HANDLE, INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
    },
    Storage::FileSystem::{
        DELETE, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX, ReadFile,
        SYNCHRONIZE, WriteFile,
    },
    System::{
        IO::{GetOverlappedResult, OVERLAPPED},
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_MESSAGE,
            PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_WAIT,
        },
        Registry::{
            HKEY_LOCAL_MACHINE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
            RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW,
        },
        Services::{
            CloseServiceHandle, ControlService, CreateServiceW, DeleteService, OpenSCManagerW,
            OpenServiceW, QueryServiceStatusEx, RegisterServiceCtrlHandlerExW, SC_MANAGER_CONNECT,
            SC_MANAGER_CREATE_SERVICE, SC_STATUS_PROCESS_INFO, SERVICE_ACCEPT_SHUTDOWN,
            SERVICE_ACCEPT_STOP, SERVICE_ALL_ACCESS, SERVICE_AUTO_START, SERVICE_CONTROL_SHUTDOWN,
            SERVICE_CONTROL_STOP, SERVICE_ERROR_NORMAL, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
            SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_HANDLE, SERVICE_STATUS_PROCESS,
            SERVICE_STOP, SERVICE_STOP_PENDING, SERVICE_STOPPED, SERVICE_TABLE_ENTRYW,
            SERVICE_WIN32_OWN_PROCESS, SetServiceStatus, StartServiceCtrlDispatcherW,
            StartServiceW,
        },
        Threading::{
            CreateEventW, GetCurrentProcessId, INFINITE, OpenProcess, ResetEvent, SetEvent,
            WaitForMultipleObjects, WaitForSingleObject,
        },
    },
};

use crate::{
    temperature::{
        CpuTemperature, ENDPOINT_KEY, ENDPOINT_VALUE, PIPE_NAME_PREFIX, cancel_and_drain,
        encode_service_response, service_sample,
    },
    win::{registry_string, system_dll, wide},
};

const SERVICE_NAME: &str = "LoopPanelTemperature";
const SERVICE_DISPLAY_NAME: &str = "LoopPanel CPU Temperature";
const INSTALLED_FILENAME: &str = "looppanel-temperature-service.exe";
const SERVICE_TRANSITION_TIMEOUT: Duration = Duration::from_secs(10);
const CLIENT_IO_TIMEOUT_MS: u32 = 1_000;
const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x00000002;

static SERVICE_STATUS_HANDLE_VALUE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static STOP_EVENT_VALUE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

type BCryptGenRandom = unsafe extern "system" fn(*mut c_void, *mut u8, u32, u32) -> i32;

pub fn run(arguments: &[String]) -> Result<()> {
    match arguments.first().map(String::as_str) {
        None => run_dispatcher(),
        Some("install") => install(),
        Some("uninstall") => uninstall(),
        Some(command) => bail!("未知温度服务命令：{command}"),
    }
}

pub fn is_installed() -> bool {
    unsafe {
        let manager = OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_CONNECT);
        if manager.is_null() {
            return false;
        }
        let name = wide(SERVICE_NAME);
        let service = OpenServiceW(manager, name.as_ptr(), SERVICE_QUERY_STATUS);
        let installed = !service.is_null();
        if installed {
            CloseServiceHandle(service);
        }
        CloseServiceHandle(manager);
        installed
    }
}

fn install() -> Result<()> {
    let mut sensor = CpuTemperature::open().context("安装前 PawnIO 自检失败")?;
    sensor.sample().context("安装前 CPU 温度自检失败")?;

    let destination = installed_executable()?;
    let source = std::env::current_exe().context("无法取得温度服务程序路径")?;
    ensure!(
        !is_installed(),
        "CPU 温度服务已经安装；更新前请先运行 uninstall"
    );
    let directory = destination
        .parent()
        .expect("installed service always has a parent directory");
    fs::create_dir_all(directory)
        .with_context(|| format!("无法创建服务目录：{}", directory.display()))?;
    let copied = source != destination;
    if copied {
        fs::copy(&source, &destination)
            .with_context(|| format!("无法复制温度服务到 {}", destination.display()))?;
    }
    let service_temperature = match create_start_and_verify_service(&destination) {
        Ok(value) => value,
        Err(error) => {
            if copied {
                let _ = fs::remove_file(&destination);
                let _ = fs::remove_dir(directory);
            }
            return Err(error);
        }
    };

    println!("CPU 温度服务已安装并启动；当前 CPU Package {service_temperature}°C。");
    Ok(())
}

fn uninstall() -> Result<()> {
    let destination = installed_executable()?;
    unsafe {
        let manager = OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_CONNECT);
        ensure!(
            !manager.is_null(),
            "无法打开服务控制管理器：{}",
            last_error()
        );
        let _manager = ServiceHandle(manager);
        let name = wide(SERVICE_NAME);
        let service = OpenServiceW(
            manager,
            name.as_ptr(),
            SERVICE_STOP | SERVICE_QUERY_STATUS | DELETE,
        );
        if service.is_null() {
            let error = GetLastError();
            ensure!(
                error == ERROR_SERVICE_DOES_NOT_EXIST,
                "无法打开 CPU 温度服务：{}",
                std::io::Error::from_raw_os_error(error as i32)
            );
        } else {
            let service = ServiceHandle(service);
            stop_service_and_wait(service.0)?;
            ensure!(
                DeleteService(service.0) != 0,
                "无法删除 CPU 温度服务：{}",
                last_error()
            );
        }
    }

    clear_pipe_endpoint()?;

    if destination.exists() {
        fs::remove_file(&destination)
            .with_context(|| format!("无法删除 {}", destination.display()))?;
    }
    if let Some(directory) = destination.parent() {
        let _ = fs::remove_dir(directory);
    }
    println!("CPU 温度服务已卸载。");
    Ok(())
}

fn create_start_and_verify_service(destination: &std::path::Path) -> Result<u32> {
    unsafe {
        let manager = OpenSCManagerW(
            ptr::null(),
            ptr::null(),
            SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE,
        );
        ensure!(
            !manager.is_null(),
            "无法打开服务控制管理器：{}",
            last_error()
        );
        let _manager = ServiceHandle(manager);

        let name = wide(SERVICE_NAME);
        let display_name = wide(SERVICE_DISPLAY_NAME);
        let binary_path = quoted_path(destination);
        let dependencies = "PawnIO\0\0".encode_utf16().collect::<Vec<_>>();
        let service = CreateServiceW(
            manager,
            name.as_ptr(),
            display_name.as_ptr(),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            binary_path.as_ptr(),
            ptr::null(),
            ptr::null_mut(),
            dependencies.as_ptr(),
            ptr::null(),
            ptr::null(),
        );
        ensure!(
            !service.is_null(),
            "无法创建 CPU 温度服务：{}",
            last_error()
        );
        let service = ServiceHandle(service);
        if StartServiceW(service.0, 0, ptr::null()) == 0 {
            let error = last_error();
            DeleteService(service.0);
            bail!("CPU 温度服务已创建但无法启动：{error}");
        }

        match wait_for_service_ready(service.0) {
            Ok(temperature) => Ok(temperature),
            Err(error) => {
                let _ = stop_service_and_wait(service.0);
                DeleteService(service.0);
                Err(error.context("CPU 温度服务未能完成启动"))
            }
        }
    }
}

fn wait_for_service_ready(service: windows_sys::Win32::System::Services::SC_HANDLE) -> Result<u32> {
    let deadline = Instant::now() + SERVICE_TRANSITION_TIMEOUT;
    let mut last_pipe_error = None;
    loop {
        let status = query_service_status(service)?;
        if status.dwCurrentState == SERVICE_STOPPED {
            bail!(
                "CPU 温度服务在启动期间停止（Win32={}，服务={}）",
                status.dwWin32ExitCode,
                status.dwServiceSpecificExitCode
            );
        }
        if status.dwCurrentState == SERVICE_RUNNING {
            match service_sample() {
                Ok(temperature) => return Ok(temperature),
                Err(error) => last_pipe_error = Some(format!("{error:#}")),
            }
        }
        if Instant::now() >= deadline {
            if let Some(error) = last_pipe_error {
                bail!("等待 CPU 温度管道就绪超时：{error}");
            }
            bail!("等待 CPU 温度服务进入运行状态超时");
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn stop_service_and_wait(service: windows_sys::Win32::System::Services::SC_HANDLE) -> Result<()> {
    let deadline = Instant::now() + SERVICE_TRANSITION_TIMEOUT;
    let initial = query_service_status(service)?;
    let process = if initial.dwProcessId == 0 {
        None
    } else {
        let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, initial.dwProcessId) };
        (!handle.is_null()).then_some(OwnedHandle(handle))
    };
    let mut status = initial;
    let mut stop_sent = status.dwCurrentState == SERVICE_STOP_PENDING;
    while status.dwCurrentState != SERVICE_STOPPED {
        if !stop_sent && status.dwCurrentState != SERVICE_START_PENDING {
            let mut reported: SERVICE_STATUS = unsafe { zeroed() };
            if unsafe { ControlService(service, SERVICE_CONTROL_STOP, &mut reported) } != 0 {
                stop_sent = true;
            } else {
                let error = unsafe { GetLastError() };
                if error == ERROR_SERVICE_NOT_ACTIVE {
                    break;
                }
                ensure!(
                    error == ERROR_SERVICE_CANNOT_ACCEPT_CTRL,
                    "无法停止 CPU 温度服务：{}",
                    std::io::Error::from_raw_os_error(error as i32)
                );
            }
        }
        ensure!(Instant::now() < deadline, "等待 CPU 温度服务停止超时");
        thread::sleep(Duration::from_millis(100));
        status = query_service_status(service)?;
    }

    if let Some(process) = process {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let wait = unsafe {
            WaitForSingleObject(
                process.0,
                remaining.as_millis().min(u32::MAX as u128) as u32,
            )
        };
        ensure!(wait == WAIT_OBJECT_0, "等待 CPU 温度服务进程退出超时");
    }
    Ok(())
}

fn query_service_status(
    service: windows_sys::Win32::System::Services::SC_HANDLE,
) -> Result<SERVICE_STATUS_PROCESS> {
    let mut status: SERVICE_STATUS_PROCESS = unsafe { zeroed() };
    let mut needed = 0_u32;
    let success = unsafe {
        QueryServiceStatusEx(
            service,
            SC_STATUS_PROCESS_INFO,
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
            size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut needed,
        )
    };
    ensure!(success != 0, "无法查询 CPU 温度服务状态：{}", last_error());
    Ok(status)
}

fn run_dispatcher() -> Result<()> {
    let mut name = wide(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: ptr::null_mut(),
            lpServiceProc: None,
        },
    ];
    let success = unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) };
    ensure!(success != 0, "无法进入 Windows 服务模式：{}", last_error());
    Ok(())
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
    let name = wide(SERVICE_NAME);
    let status_handle = unsafe {
        RegisterServiceCtrlHandlerExW(
            name.as_ptr(),
            Some(service_control_handler),
            ptr::null_mut(),
        )
    };
    if status_handle.is_null() {
        return;
    }
    SERVICE_STATUS_HANDLE_VALUE.store(status_handle.cast(), Ordering::Release);
    unsafe { report_status(status_handle, SERVICE_START_PENDING, 0, 0) };

    let stop_event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
    if stop_event.is_null() {
        unsafe { report_status(status_handle, SERVICE_STOPPED, GetLastError(), 0) };
        return;
    }
    STOP_EVENT_VALUE.store(stop_event, Ordering::Release);
    let stop_event = OwnedHandle(stop_event);

    let result = CpuTemperature::open()
        .and_then(|mut sensor| run_pipe_server(&mut sensor, stop_event.0, status_handle));

    STOP_EVENT_VALUE.store(ptr::null_mut(), Ordering::Release);
    SERVICE_STATUS_HANDLE_VALUE.store(ptr::null_mut(), Ordering::Release);
    match result {
        Ok(()) => unsafe { report_status(status_handle, SERVICE_STOPPED, 0, 0) },
        Err(_) => unsafe {
            report_status(
                status_handle,
                SERVICE_STOPPED,
                ERROR_SERVICE_SPECIFIC_ERROR,
                1,
            )
        },
    }
}

unsafe extern "system" fn service_control_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    if control == SERVICE_CONTROL_STOP || control == SERVICE_CONTROL_SHUTDOWN {
        let status_handle = SERVICE_STATUS_HANDLE_VALUE.load(Ordering::Acquire);
        if !status_handle.is_null() {
            unsafe { report_status(status_handle.cast(), SERVICE_STOP_PENDING, 0, 0) };
        }
        let stop_event = STOP_EVENT_VALUE.load(Ordering::Acquire);
        if !stop_event.is_null() {
            unsafe { SetEvent(stop_event) };
        }
    }
    ERROR_SUCCESS
}

unsafe fn report_status(
    handle: SERVICE_STATUS_HANDLE,
    state: u32,
    win32_exit_code: u32,
    service_exit_code: u32,
) {
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        dwWin32ExitCode: win32_exit_code,
        dwServiceSpecificExitCode: service_exit_code,
        dwCheckPoint: u32::from(state == SERVICE_START_PENDING || state == SERVICE_STOP_PENDING),
        dwWaitHint: if state == SERVICE_START_PENDING || state == SERVICE_STOP_PENDING {
            5_000
        } else {
            0
        },
    };
    unsafe { SetServiceStatus(handle, &status) };
}

fn run_pipe_server(
    sensor: &mut CpuTemperature,
    stop_event: HANDLE,
    status_handle: SERVICE_STATUS_HANDLE,
) -> Result<()> {
    let descriptor = PipeSecurity::new()?;
    let pipe_name = random_pipe_name()?;
    let pipe_name_wide = wide(&pipe_name);
    let pipe = unsafe {
        CreateNamedPipeW(
            pipe_name_wide.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            64,
            64,
            0,
            &descriptor.attributes,
        )
    };
    ensure!(
        pipe != INVALID_HANDLE_VALUE,
        "无法创建 CPU 温度命名管道：{}",
        last_error()
    );
    let pipe = OwnedHandle(pipe);
    let event = OwnedHandle(unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) });
    ensure!(!event.0.is_null(), "无法创建管道事件：{}", last_error());
    let mut operation: OVERLAPPED = unsafe { zeroed() };
    operation.hEvent = event.0;

    publish_pipe_endpoint(&pipe_name)?;
    unsafe { report_status(status_handle, SERVICE_RUNNING, 0, 0) };
    let result = (|| -> Result<()> {
        loop {
            if !connect_pipe(pipe.0, stop_event, &mut operation)? {
                return Ok(());
            }
            let _ = serve_client(pipe.0, stop_event, &mut operation, sensor);
            unsafe { DisconnectNamedPipe(pipe.0) };
        }
    })();
    let clear_result = clear_pipe_endpoint();
    result?;
    clear_result
}

enum IoWait {
    Completed(u32),
    Stopped,
    TimedOut,
}

fn wait_for_io(
    handle: HANDLE,
    stop_event: HANDLE,
    operation: &mut OVERLAPPED,
    timeout_ms: u32,
) -> Result<IoWait> {
    let handles = [stop_event, operation.hEvent];
    let wait =
        unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, timeout_ms) };
    if wait == WAIT_OBJECT_0 + 1 {
        return completed_bytes(handle, operation).map(IoWait::Completed);
    }
    if wait == WAIT_OBJECT_0 {
        cancel_and_drain(handle, operation);
        return Ok(IoWait::Stopped);
    }
    if wait == WAIT_TIMEOUT {
        cancel_and_drain(handle, operation);
        return Ok(IoWait::TimedOut);
    }
    cancel_and_drain(handle, operation);
    bail!("等待 CPU 温度管道操作失败：{}", last_error())
}

fn connect_pipe(pipe: HANDLE, stop_event: HANDLE, operation: &mut OVERLAPPED) -> Result<bool> {
    unsafe { ResetEvent(operation.hEvent) };
    if unsafe { ConnectNamedPipe(pipe, operation) } != 0 {
        return Ok(true);
    }
    match unsafe { GetLastError() } {
        ERROR_PIPE_CONNECTED => Ok(true),
        ERROR_IO_PENDING => match wait_for_io(pipe, stop_event, operation, INFINITE)? {
            IoWait::Completed(_) => Ok(true),
            IoWait::Stopped => Ok(false),
            IoWait::TimedOut => unreachable!("无限等待不会超时"),
        },
        error => bail!(
            "等待 CPU 温度客户端失败：{}",
            std::io::Error::from_raw_os_error(error as i32)
        ),
    }
}

fn serve_client(
    pipe: HANDLE,
    stop_event: HANDLE,
    operation: &mut OVERLAPPED,
    sensor: &mut CpuTemperature,
) -> Result<()> {
    let mut request = [0_u8; 1];
    unsafe { ResetEvent(operation.hEvent) };
    let started = unsafe {
        ReadFile(
            pipe,
            request.as_mut_ptr().cast(),
            request.len() as u32,
            ptr::null_mut(),
            operation,
        )
    };
    let read = if started != 0 {
        completed_bytes(pipe, operation)?
    } else {
        let error = unsafe { GetLastError() };
        if error == ERROR_BROKEN_PIPE {
            return Ok(());
        }
        ensure!(
            error == ERROR_IO_PENDING,
            "读取温度请求失败：{}",
            last_error()
        );
        match wait_for_io(pipe, stop_event, operation, CLIENT_IO_TIMEOUT_MS)? {
            IoWait::Completed(read) => read,
            IoWait::Stopped | IoWait::TimedOut => return Ok(()),
        }
    };
    if read != 1 || request[0] != 1 {
        return Ok(());
    }

    let response = encode_service_response(sensor.sample());
    unsafe { ResetEvent(operation.hEvent) };
    let started = unsafe {
        WriteFile(
            pipe,
            response.as_ptr().cast(),
            response.len() as u32,
            ptr::null_mut(),
            operation,
        )
    };
    let written = if started != 0 {
        completed_bytes(pipe, operation)?
    } else {
        let error = unsafe { GetLastError() };
        ensure!(
            error == ERROR_IO_PENDING,
            "写入温度响应失败：{}",
            last_error()
        );
        match wait_for_io(pipe, stop_event, operation, CLIENT_IO_TIMEOUT_MS)? {
            IoWait::Completed(written) => written,
            IoWait::Stopped | IoWait::TimedOut => return Ok(()),
        }
    };
    ensure!(written == response.len() as u32, "CPU 温度响应未完整写入");
    Ok(())
}

fn completed_bytes(handle: HANDLE, operation: &mut OVERLAPPED) -> Result<u32> {
    let mut transferred = 0_u32;
    let success = unsafe { GetOverlappedResult(handle, operation, &mut transferred, 0) };
    ensure!(success != 0, "CPU 温度管道操作失败：{}", last_error());
    Ok(transferred)
}

fn random_pipe_name() -> Result<String> {
    let mut random = [0_u8; 16];
    let library_path = system_dll("bcrypt.dll")?;
    let library = unsafe { Library::new(&library_path) }
        .with_context(|| format!("无法加载 {}", library_path.display()))?;
    let generate = unsafe {
        *library
            .get::<BCryptGenRandom>(b"BCryptGenRandom\0")
            .context("bcrypt.dll 缺少 BCryptGenRandom")?
    };
    let status = unsafe {
        generate(
            ptr::null_mut(),
            random.as_mut_ptr(),
            random.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    ensure!(
        status >= 0,
        "无法生成温度管道名称（NTSTATUS 0x{:08X}）",
        status as u32
    );
    let mut name = String::from(PIPE_NAME_PREFIX);
    for byte in random {
        write!(&mut name, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(name)
}

fn publish_pipe_endpoint(pipe_name: &str) -> Result<()> {
    let subkey = wide(ENDPOINT_KEY);
    let value_name = wide(ENDPOINT_VALUE);
    let endpoint = wide(&format!("{}|{pipe_name}", unsafe { GetCurrentProcessId() }));
    let mut key = ptr::null_mut();
    let result = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            0,
            ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            ptr::null(),
            &mut key,
            ptr::null_mut(),
        )
    };
    ensure!(
        result == ERROR_SUCCESS,
        "无法创建温度服务注册表项：{}",
        std::io::Error::from_raw_os_error(result as i32)
    );
    let key = RegistryKey(key);
    let result = unsafe {
        RegSetValueExW(
            key.0,
            value_name.as_ptr(),
            0,
            REG_SZ,
            endpoint.as_ptr().cast(),
            (endpoint.len() * size_of::<u16>()) as u32,
        )
    };
    ensure!(
        result == ERROR_SUCCESS,
        "无法发布温度服务命名管道：{}",
        std::io::Error::from_raw_os_error(result as i32)
    );
    Ok(())
}

fn clear_pipe_endpoint() -> Result<()> {
    let subkey = wide(ENDPOINT_KEY);
    let value_name = wide(ENDPOINT_VALUE);
    let mut key = ptr::null_mut();
    let result = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut key,
        )
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    ensure!(
        result == ERROR_SUCCESS,
        "无法打开温度服务注册表项：{}",
        std::io::Error::from_raw_os_error(result as i32)
    );
    let key = RegistryKey(key);
    let result = unsafe { RegDeleteValueW(key.0, value_name.as_ptr()) };
    ensure!(
        result == ERROR_SUCCESS || result == ERROR_FILE_NOT_FOUND,
        "无法清除温度服务命名管道：{}",
        std::io::Error::from_raw_os_error(result as i32)
    );
    Ok(())
}

fn installed_executable() -> Result<PathBuf> {
    let program_files = registry_string(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Windows\CurrentVersion",
        "ProgramFilesDir",
    )?;
    Ok(PathBuf::from(program_files)
        .join("LoopPanel")
        .join(INSTALLED_FILENAME))
}

fn quoted_path(path: &std::path::Path) -> Vec<u16> {
    let mut value = vec![b'"' as u16];
    value.extend(path.as_os_str().encode_wide());
    value.extend([b'"' as u16, 0]);
    value
}

fn last_error() -> std::io::Error {
    std::io::Error::last_os_error()
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0) };
        }
    }
}

struct ServiceHandle(windows_sys::Win32::System::Services::SC_HANDLE);

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        unsafe { CloseServiceHandle(self.0) };
    }
}

struct RegistryKey(windows_sys::Win32::System::Registry::HKEY);

impl Drop for RegistryKey {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

struct PipeSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

impl PipeSecurity {
    fn new() -> Result<Self> {
        let sddl = wide("D:P(A;;GA;;;SY)(A;;GRGW;;;AU)S:(ML;;NW;;;ME)");
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        let success = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        };
        ensure!(success != 0, "无法创建命名管道安全描述符：{}", last_error());
        Ok(Self {
            descriptor,
            attributes: SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            },
        })
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        unsafe { LocalFree(self.descriptor.cast()) };
    }
}
