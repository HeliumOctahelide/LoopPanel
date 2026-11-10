use std::{
    arch::x86_64::__cpuid,
    ffi::{CStr, c_void},
    mem::{size_of, zeroed},
    path::PathBuf,
    ptr,
};

use crate::i18n::t;
use anyhow::{Context, Result, bail, ensure};
use libloading::Library;
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE, LocalFree, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
    },
    Storage::FileSystem::{CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, SYNCHRONIZE},
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
        Threading::{
            CreateEventW, CreateMutexExW, MUTEX_MODIFY_STATE, ReleaseMutex, WaitForSingleObject,
        },
    },
};

use crate::win::{registry_string, wide};

const IA32_TEMPERATURE_TARGET: u64 = 0x1A2;
const IA32_PACKAGE_THERM_STATUS: u64 = 0x1B1;
const INTEL_MSR_MODULE: &[u8] = include_bytes!("../vendor/pawnio-modules-0.2.10/IntelMSR.bin");
/// AMD 17h–1Ah 的 SMN 报告温度寄存器 `THM_TCON_CUR_TMP`，与 Linux k10temp 使用的地址相同。
const AMD_THM_TCON_CUR_TMP: u64 = 0x0005_9800;
const AMD_FAMILY17_MODULE: &[u8] =
    include_bytes!("../vendor/pawnio-modules-0.2.10/AMDFamily17.bin");
/// 硬件监控软件之间约定的 PCI 配置空间互斥量；SMN 读取是“写索引、读数据”两步，必须串行。
const PCI_MUTEX_NAME: &str = r"Global\Access_PCI";
const PCI_MUTEX_TIMEOUT_MS: u32 = 100;
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
    sensor: Sensor,
}

enum Sensor {
    /// Intel：`TjMax - Package 数字读数`。
    IntelPackage { tj_max: f32 },
    /// AMD Zen：SMN 报告温度（Tctl），减去个别型号的固定偏移后即 Tdie。
    AmdReported { tctl_offset: f32, pci: PciMutex },
}

impl CpuTemperature {
    pub fn open() -> Result<Self> {
        let cpu = CpuIdentity::read();
        match cpu_kind(&cpu.vendor, cpu.family)? {
            CpuKind::Intel => {
                let pawn = PawnIo::open(INTEL_MSR_MODULE, "IntelMSR")?;
                let target = pawn.execute(c"ioctl_read_msr", IA32_TEMPERATURE_TARGET)?;
                let tj_max = ((target >> 16) & 0xff) as f32;
                ensure!(tj_max > 0.0, "{}", t!("temperature.tj_max_invalid"));
                Ok(Self {
                    pawn,
                    sensor: Sensor::IntelPackage { tj_max },
                })
            }
            CpuKind::AmdZen => {
                let pawn = PawnIo::open(AMD_FAMILY17_MODULE, "AMDFamily17")?;
                Ok(Self {
                    pawn,
                    sensor: Sensor::AmdReported {
                        tctl_offset: amd_tctl_offset(cpu.family, &cpu.brand),
                        pci: PciMutex::open()?,
                    },
                })
            }
        }
    }

    pub fn sample(&mut self) -> Result<f32> {
        match &self.sensor {
            Sensor::IntelPackage { tj_max } => {
                let status = self
                    .pawn
                    .execute(c"ioctl_read_msr", IA32_PACKAGE_THERM_STATUS)?;
                package_temperature(*tj_max, status)
            }
            Sensor::AmdReported { tctl_offset, pci } => {
                let register = {
                    let _guard = pci.lock()?;
                    self.pawn.execute(c"ioctl_read_smn", AMD_THM_TCON_CUR_TMP)?
                };
                amd_reported_temperature(register as u32, *tctl_offset)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CpuKind {
    Intel,
    AmdZen,
}

struct CpuIdentity {
    vendor: [u8; 12],
    family: u32,
    brand: String,
}

impl CpuIdentity {
    fn read() -> Self {
        let leaf0 = __cpuid(0);
        let mut vendor = [0_u8; 12];
        vendor[..4].copy_from_slice(&leaf0.ebx.to_le_bytes());
        vendor[4..8].copy_from_slice(&leaf0.edx.to_le_bytes());
        vendor[8..].copy_from_slice(&leaf0.ecx.to_le_bytes());

        let mut brand = Vec::with_capacity(48);
        if __cpuid(0x8000_0000).eax >= 0x8000_0004 {
            for leaf in 0x8000_0002..=0x8000_0004_u32 {
                let registers = __cpuid(leaf);
                for register in [registers.eax, registers.ebx, registers.ecx, registers.edx] {
                    brand.extend_from_slice(&register.to_le_bytes());
                }
            }
        }
        let length = brand
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(brand.len());
        Self {
            vendor,
            family: cpu_family(__cpuid(1).eax),
            brand: String::from_utf8_lossy(&brand[..length]).trim().to_owned(),
        }
    }
}

fn cpu_family(leaf1_eax: u32) -> u32 {
    let base = (leaf1_eax >> 8) & 0xf;
    if base == 0xf {
        base + ((leaf1_eax >> 20) & 0xff)
    } else {
        base
    }
}

fn cpu_kind(vendor: &[u8; 12], family: u32) -> Result<CpuKind> {
    match vendor {
        b"GenuineIntel" => Ok(CpuKind::Intel),
        // 与官方 AMDFamily17 模块接受的范围一致：Zen 至 Zen 5。
        b"AuthenticAMD" if (0x17..=0x1a).contains(&family) => Ok(CpuKind::AmdZen),
        b"AuthenticAMD" => bail!(
            "{}",
            t!(
                "temperature.amd_family_unsupported",
                family = format!("{family:X}")
            )
        ),
        _ => bail!(
            "{}",
            t!(
                "temperature.cpu_vendor_unsupported",
                vendor = String::from_utf8_lossy(vendor)
            )
        ),
    }
}

pub fn service_sample() -> Result<u32> {
    let service_pid = running_service_pid()?;
    let endpoint = registry_string(HKEY_LOCAL_MACHINE, ENDPOINT_KEY, ENDPOINT_VALUE)
        .with_context(|| t!("temperature.service_pipe_not_published"))?;
    let (registered_pid, pipe_name) = endpoint
        .split_once('|')
        .with_context(|| t!("temperature.service_pipe_info_invalid"))?;
    let registered_pid = registered_pid
        .parse::<u32>()
        .with_context(|| t!("temperature.service_pid_invalid"))?;
    ensure!(
        registered_pid == service_pid,
        "{}",
        t!("temperature.service_switching_instance")
    );
    ensure!(
        pipe_name.starts_with(PIPE_NAME_PREFIX),
        "{}",
        t!("temperature.service_pipe_name_invalid")
    );

    let pipe_name = wide(pipe_name);
    let available = unsafe { WaitNamedPipeW(pipe_name.as_ptr(), PIPE_CONNECT_TIMEOUT_MS) };
    if available == 0 {
        bail!(
            "{}",
            t!(
                "temperature.service_unavailable",
                error = std::io::Error::last_os_error()
            )
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
        "{}",
        t!(
            "temperature.service_connect_failed",
            error = std::io::Error::last_os_error()
        )
    );
    let pipe = OwnedHandle(pipe);

    let mut actual_pid = 0_u32;
    ensure!(
        unsafe { GetNamedPipeServerProcessId(pipe.0, &mut actual_pid) } != 0,
        "{}",
        t!(
            "temperature.service_verify_failed",
            error = std::io::Error::last_os_error()
        )
    );
    ensure!(
        actual_pid == service_pid,
        "{}",
        t!("temperature.pipe_not_from_service")
    );
    let mode = PIPE_READMODE_MESSAGE;
    ensure!(
        unsafe { SetNamedPipeHandleState(pipe.0, &mode, ptr::null(), ptr::null()) } != 0,
        "{}",
        t!(
            "temperature.pipe_mode_failed",
            error = std::io::Error::last_os_error()
        )
    );

    let event = OwnedHandle(unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) });
    ensure!(
        !event.0.is_null(),
        "{}",
        t!(
            "temperature.pipe_event_failed",
            error = std::io::Error::last_os_error()
        )
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
            "{}",
            t!(
                "temperature.service_request_failed",
                error = std::io::Error::from_raw_os_error(error as i32)
            )
        );
        match unsafe { WaitForSingleObject(event.0, PIPE_RESPONSE_TIMEOUT_MS) } {
            WAIT_OBJECT_0 => completed_bytes(pipe.0, &mut operation)?,
            WAIT_TIMEOUT => {
                cancel_and_drain(pipe.0, &mut operation);
                bail!("{}", t!("temperature.service_response_timeout"));
            }
            _ => {
                cancel_and_drain(pipe.0, &mut operation);
                bail!(
                    "{}",
                    t!(
                        "temperature.service_response_wait_failed",
                        error = std::io::Error::last_os_error()
                    )
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
            "{}",
            t!(
                "temperature.scm_open_failed",
                error = std::io::Error::last_os_error()
            )
        );
        let manager = OwnedServiceHandle(manager);
        let name = wide("LoopPanelTemperature");
        let service = OpenServiceW(manager.0, name.as_ptr(), SERVICE_QUERY_STATUS);
        ensure!(
            !service.is_null(),
            "{}",
            t!(
                "temperature.service_not_installed",
                error = std::io::Error::last_os_error()
            )
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
            "{}",
            t!(
                "temperature.service_query_failed",
                error = std::io::Error::last_os_error()
            )
        );
        ensure!(
            status.dwCurrentState == SERVICE_RUNNING && status.dwProcessId != 0,
            "{}",
            t!("temperature.service_not_running")
        );
        Ok(status.dwProcessId)
    }
}

fn completed_bytes(handle: HANDLE, operation: &mut OVERLAPPED) -> Result<u32> {
    let mut transferred = 0_u32;
    let success = unsafe { GetOverlappedResult(handle, operation, &mut transferred, 0) };
    ensure!(
        success != 0,
        "{}",
        t!(
            "temperature.pipe_operation_failed",
            error = std::io::Error::last_os_error()
        )
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
    fn open(module: &[u8], module_name: &str) -> Result<Self> {
        let library_path = pawnio_install_directory()?.join("PawnIOLib.dll");
        let library = unsafe { Library::new(&library_path) }.with_context(|| {
            t!(
                "temperature.library_load_failed",
                path = library_path.display()
            )
        })?;
        unsafe {
            let open = *library.get::<PawnOpen>(b"pawnio_open\0").with_context(|| {
                t!(
                    "temperature.library_missing_function",
                    function = "pawnio_open"
                )
            })?;
            let load = *library.get::<PawnLoad>(b"pawnio_load\0").with_context(|| {
                t!(
                    "temperature.library_missing_function",
                    function = "pawnio_load"
                )
            })?;
            let execute = *library
                .get::<PawnExecute>(b"pawnio_execute\0")
                .with_context(|| {
                    t!(
                        "temperature.library_missing_function",
                        function = "pawnio_execute"
                    )
                })?;
            let close = *library
                .get::<PawnClose>(b"pawnio_close\0")
                .with_context(|| {
                    t!(
                        "temperature.library_missing_function",
                        function = "pawnio_close"
                    )
                })?;

            let mut handle = ptr::null_mut();
            check_hresult(open(&mut handle), &t!("temperature.pawnio_open_failed"))?;
            if handle.is_null() {
                bail!("{}", t!("temperature.pawnio_null_handle"));
            }
            if let Err(error) = check_hresult(
                load(handle, module.as_ptr(), module.len()),
                &t!(
                    "temperature.pawnio_module_load_failed",
                    module = module_name
                ),
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

    /// 调用模块中“一个输入、一个输出”的只读 ioctl；函数名和地址只来自本文件的常量。
    fn execute(&self, function: &CStr, address: u64) -> Result<u64> {
        let input = [address];
        let mut output = [0_u64];
        let mut returned = 0_usize;
        let result = unsafe {
            (self.execute)(
                self.handle,
                function.as_ptr().cast(),
                input.as_ptr(),
                input.len(),
                output.as_mut_ptr(),
                output.len(),
                &mut returned,
            )
        };
        check_hresult(
            result,
            &t!(
                "temperature.pawnio_read_failed",
                function = function.to_string_lossy(),
                address = format!("{address:X}")
            ),
        )?;
        ensure!(
            returned == 1,
            "{}",
            t!(
                "temperature.pawnio_result_length_invalid",
                length = returned
            )
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
    .with_context(|| t!("temperature.pawnio_not_installed"))?;
    ensure!(
        !directory.is_empty(),
        "{}",
        t!("temperature.pawnio_directory_empty")
    );
    Ok(PathBuf::from(directory))
}

fn package_temperature(tj_max: f32, status: u64) -> Result<f32> {
    ensure!(
        status & (1 << 31) != 0,
        "{}",
        t!("temperature.package_temperature_invalid")
    );
    let distance = ((status >> 16) & 0x7f) as f32;
    Ok(tj_max - distance)
}

/// 解码 `THM_TCON_CUR_TMP`，规则与 Linux k10temp 一致：高 11 位以 0.125°C 为单位，
/// 量程选择位（bit 19）或 Tj 选择位（bit 17:16 全 1）置位时读数带 49°C 偏置。
fn amd_reported_temperature(register: u32, tctl_offset: f32) -> Result<f32> {
    ensure!(
        register != 0 && register != u32::MAX,
        "{}",
        t!("temperature.amd_register_invalid")
    );
    let mut temperature = (register >> 21) as f32 * 0.125;
    if register & (1 << 19) != 0 || register & (0b11 << 16) == (0b11 << 16) {
        temperature -= 49.0;
    }
    let temperature = temperature - tctl_offset;
    // 不遵守 PCI 互斥量的软件可能改写索引寄存器，使这次读到的是别的寄存器。
    ensure!(
        (-40.0..=150.0).contains(&temperature),
        "{}",
        t!(
            "temperature.amd_temperature_out_of_range",
            temperature = temperature
        )
    );
    Ok(temperature)
}

/// 个别 Zen/Zen+ 型号的 Tctl 相对 Tdie 有固定偏移；数据取自 Linux k10temp 的偏移表。
fn amd_tctl_offset(family: u32, brand: &str) -> f32 {
    const OFFSETS: [(&str, f32); 6] = [
        ("AMD Ryzen 5 1600X", 20.0),
        ("AMD Ryzen 7 1700X", 20.0),
        ("AMD Ryzen 7 1800X", 20.0),
        ("AMD Ryzen 7 2700X", 10.0),
        ("AMD Ryzen Threadripper 19", 27.0),
        ("AMD Ryzen Threadripper 29", 27.0),
    ];
    if family != 0x17 {
        return 0.0;
    }
    OFFSETS
        .iter()
        .find(|(model, _)| brand.contains(model))
        .map_or(0.0, |(_, offset)| *offset)
}

struct PciMutex(HANDLE);

impl PciMutex {
    fn open() -> Result<Self> {
        // 服务开机即创建并长期持有这个互斥量，因此这里的访问规则对所有协作软件生效：
        // 其他账户也必须能等待和释放它，否则它们会放弃加锁、直接访问 SMN。
        let sddl = wide("D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x00100001;;;WD)");
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        ensure!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    ptr::null_mut(),
                )
            } != 0,
            "{}",
            t!(
                "temperature.pci_mutex_descriptor_failed",
                error = std::io::Error::last_os_error()
            )
        );
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let name = wide(PCI_MUTEX_NAME);
        // 只申请等待与释放权限，这样也能打开其他软件以较窄权限创建的同名互斥量。
        let handle = unsafe {
            CreateMutexExW(
                &attributes,
                name.as_ptr(),
                0,
                SYNCHRONIZE | MUTEX_MODIFY_STATE,
            )
        };
        let error = std::io::Error::last_os_error();
        unsafe { LocalFree(descriptor.cast()) };
        ensure!(
            !handle.is_null(),
            "{}",
            t!("temperature.pci_mutex_open_failed", error = error)
        );
        Ok(Self(handle))
    }

    fn lock(&self) -> Result<PciMutexGuard<'_>> {
        match unsafe { WaitForSingleObject(self.0, PCI_MUTEX_TIMEOUT_MS) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(PciMutexGuard(self)),
            WAIT_TIMEOUT => bail!("{}", t!("temperature.pci_mutex_timeout")),
            _ => bail!(
                "{}",
                t!(
                    "temperature.pci_mutex_wait_failed",
                    error = std::io::Error::last_os_error()
                )
            ),
        }
    }
}

impl Drop for PciMutex {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

struct PciMutexGuard<'a>(&'a PciMutex);

impl Drop for PciMutexGuard<'_> {
    fn drop(&mut self) {
        unsafe { ReleaseMutex(self.0.0) };
    }
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
    ensure!(
        response.len() == 3,
        "{}",
        t!("temperature.response_length_invalid")
    );
    ensure!(
        response[0] == RESPONSE_OK,
        "{}",
        t!("temperature.no_valid_reading")
    );
    Ok(u16::from_le_bytes([response[1], response[2]]) as u32)
}

fn check_hresult(result: i32, action: &str) -> Result<()> {
    if result < 0 {
        bail!(
            "{}",
            t!(
                "temperature.hresult_failed",
                action = action,
                code = format!("{:08X}", result as u32)
            )
        );
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
    use super::{
        CpuKind, amd_reported_temperature, amd_tctl_offset, cpu_family, cpu_kind,
        decode_service_response, encode_service_response, package_temperature,
    };

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
    fn decodes_amd_reported_temperature() {
        // 45.5°C，无量程偏置。
        assert_eq!(amd_reported_temperature(364 << 21, 0.0).unwrap(), 45.5);
        // 量程选择位：读数带 49°C 偏置。
        let ranged = (((45 + 49) * 8) << 21) | (1 << 19);
        assert_eq!(amd_reported_temperature(ranged, 0.0).unwrap(), 45.0);
        // Tj 选择位全 1 时同样带偏置；只置一位则不带。
        let tj_selected = (((45 + 49) * 8) << 21) | (0b11 << 16);
        assert_eq!(amd_reported_temperature(tj_selected, 0.0).unwrap(), 45.0);
        assert_eq!(
            amd_reported_temperature((360 << 21) | (0b01 << 16), 0.0).unwrap(),
            45.0
        );
        // Tctl 偏移型号换算为 Tdie。
        assert_eq!(amd_reported_temperature(520 << 21, 20.0).unwrap(), 45.0);
    }

    #[test]
    fn rejects_invalid_amd_reported_temperature() {
        assert!(amd_reported_temperature(0, 0.0).is_err());
        assert!(amd_reported_temperature(u32::MAX, 0.0).is_err());
        // 255.875°C 与 -49°C 都不可能是真实读数。
        assert!(amd_reported_temperature(2047 << 21, 0.0).is_err());
        assert!(amd_reported_temperature(1 << 19, 0.0).is_err());
    }

    #[test]
    fn tctl_offset_applies_only_to_listed_family_17h_models() {
        assert_eq!(
            amd_tctl_offset(0x17, "AMD Ryzen 7 1800X Eight-Core Processor"),
            20.0
        );
        assert_eq!(
            amd_tctl_offset(0x17, "AMD Ryzen 7 2700X Eight-Core Processor"),
            10.0
        );
        assert_eq!(
            amd_tctl_offset(0x17, "AMD Ryzen Threadripper 2950X 16-Core Processor"),
            27.0
        );
        assert_eq!(
            amd_tctl_offset(0x17, "AMD Ryzen 7 3700X 8-Core Processor"),
            0.0
        );
        assert_eq!(
            amd_tctl_offset(0x19, "AMD Ryzen 7 7800X3D 8-Core Processor"),
            0.0
        );
    }

    #[test]
    fn identifies_supported_cpu_families() {
        // 7800X3D：Family 19h Model 61h Stepping 2。
        assert_eq!(cpu_family(0x00A6_0F12), 0x19);
        // Intel Family 6 不叠加扩展族号。
        assert_eq!(cpu_family(0x000B_06F2), 6);
        assert_eq!(cpu_kind(b"GenuineIntel", 6).unwrap(), CpuKind::Intel);
        assert_eq!(cpu_kind(b"AuthenticAMD", 0x17).unwrap(), CpuKind::AmdZen);
        assert_eq!(cpu_kind(b"AuthenticAMD", 0x1a).unwrap(), CpuKind::AmdZen);
        assert!(cpu_kind(b"AuthenticAMD", 0x15).is_err());
        assert!(cpu_kind(b"AuthenticAMD", 0x1b).is_err());
        assert!(cpu_kind(b"HygonGenuine", 0x18).is_err());
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
