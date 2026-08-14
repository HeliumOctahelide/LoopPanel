use std::{
    ffi::{CStr, c_char, c_int, c_void},
    ptr, thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use libloading::Library;

use crate::protocol::{CHUNK_SIZE, ENDPOINT, INTERFACE, PID, VID};
use crate::win::system_dll;

const fn nibble(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        _ => panic!("invalid hexadecimal digit"),
    }
}

const fn report(hex: &[u8; 16]) -> [u8; 8] {
    let mut output = [0_u8; 8];
    let mut index = 0;
    while index < 8 {
        output[index] = nibble(hex[index * 2]) * 16 + nibble(hex[index * 2 + 1]);
        index += 1;
    }
    output
}

// Timings and reports are an exact replay of an ordinary startup captured on a
// 345F:9132, 480x480, UYVY, VIC-143 unit. Firmware-upgrade traffic is excluded.
// Source (MIT): https://github.com/Fakinvisibility/b360gt-driver/blob/94ae7f2a710123b582ca3fa806d85ee1c684e287/src/b360gt/device_init.py
const STARTUP_REPORTS: &[(u32, [u8; 8])] = &[
    (0, report(b"a603030000000000")),
    (21_837, report(b"b5c5580000000000")),
    (27_258, report(b"b6deee0100000000")),
    (30_239, report(b"a607010200000000")),
    (147_011, report(b"b5c4540000000000")),
    (152_241, report(b"a605000000000000")),
    (177_346, report(b"b5c5550000000000")),
    (183_259, report(b"b5f0040000000000")),
    (189_276, report(b"b6f0046d00000000")),
    (192_239, report(b"f5001fe000000000")),
    (198_236, report(b"f5001fe800000000")),
    (204_243, report(b"f5001ff000000000")),
    (210_241, report(b"f5001ff800000000")),
    (216_233, report(b"b5c4240000000000")),
    (222_230, report(b"b5c6500000000000")),
    (228_239, report(b"b5c30f0000000000")),
    (234_238, report(b"b5ff000000000000")),
    (240_290, report(b"b5f0000000000000")),
    (246_257, report(b"b500310000000000")),
    (252_254, report(b"b500300000000000")),
    (270_632, report(b"b500320000000000")),
    (1_262_877, report(b"b500320000000000")),
    (1_278_596, report(b"b5c0000000000000")),
    (1_284_263, report(b"b5c0040000000000")),
    (1_290_266, report(b"b5c0080000000000")),
    (1_296_260, report(b"b5c00c0000000000")),
    (1_302_277, report(b"b5c0100000000000")),
    (1_308_275, report(b"b5c0140000000000")),
    (1_314_273, report(b"b5c0180000000000")),
    (1_320_267, report(b"b5c01c0000000000")),
    (1_326_310, report(b"b5c0200000000000")),
    (1_332_273, report(b"b5c0240000000000")),
    (1_338_269, report(b"b5c0280000000000")),
    (1_344_275, report(b"b5c02c0000000000")),
    (1_350_267, report(b"b5c0300000000000")),
    (1_356_273, report(b"b5c0340000000000")),
    (1_362_265, report(b"b5c0380000000000")),
    (1_368_285, report(b"b5c03c0000000000")),
    (1_374_269, report(b"b5c0400000000000")),
    (1_380_343, report(b"b5c0440000000000")),
    (1_386_272, report(b"b5c0480000000000")),
    (1_392_274, report(b"b5c04c0000000000")),
    (1_398_274, report(b"b5c0500000000000")),
    (1_404_265, report(b"b5c0540000000000")),
    (1_410_274, report(b"b5c0580000000000")),
    (1_416_265, report(b"b5c05c0000000000")),
    (1_422_307, report(b"b5c0600000000000")),
    (1_428_268, report(b"b5c0640000000000")),
    (1_434_268, report(b"b5c0680000000000")),
    (1_440_273, report(b"b5c06c0000000000")),
    (1_446_266, report(b"b5c0700000000000")),
    (1_452_269, report(b"b5c0740000000000")),
    (1_458_271, report(b"b5c0780000000000")),
    (1_464_277, report(b"b5c07c0000000000")),
    (1_472_005, report(b"f5001fe000000000")),
    (1_477_268, report(b"f5001fe800000000")),
    (1_483_277, report(b"f5001ff000000000")),
    (1_489_317, report(b"f5001ff800000000")),
    (1_495_275, report(b"b500320000000000")),
    (1_512_517, report(b"a605000000000000")),
    (1_525_644, report(b"b5c5550000000000")),
    (1_531_284, report(b"b5f0050000000000")),
    (1_537_289, report(b"b6f0050000000000")),
    (1_540_264, report(b"a604000000000000")),
    (1_554_062, report(b"b5c5550000000000")),
    (1_682_472, report(b"b500320000000000")),
    (1_688_327, report(b"a603030000000000")),
    (1_713_905, report(b"b5c5580000000000")),
    (1_719_281, report(b"a60101e001e02200")),
    (1_745_227, report(b"b5c5550000000000")),
    (1_750_285, report(b"a6028f0001e001e0")),
    (1_776_715, report(b"b5c5570000000000")),
    (1_791_946, report(b"b5c5570000000000")),
    (1_807_475, report(b"b5c5570000000000")),
    (1_823_209, report(b"b5c5570000000000")),
    (1_838_572, report(b"b5c5570000000000")),
    (1_844_276, report(b"a604010000000000")),
    (1_869_866, report(b"b5c5550000000000")),
    (2_010_816, report(b"b6f24e0000000000")),
    (2_181_958, report(b"b500320000000000")),
    (2_524_434, report(b"b5f2420000000000")),
];

const POST_FIRST_FRAME_REPORTS: &[[u8; 8]] = &[
    report(b"b5f0050000000000"),
    report(b"b6f0051000000000"),
    report(b"a605010000000000"),
    report(b"b5c5550000000000"),
];
const _: [(); 81] = [(); STARTUP_REPORTS.len()];

pub fn diagnostic_lines() -> Vec<String> {
    let mut lines = Vec::new();
    match HidApi::load().and_then(|api| api.target_paths()) {
        Ok(paths) if paths.is_empty() => lines.push("HID 控制接口：未发现".to_owned()),
        Ok(paths) => lines.push(format!("HID 控制接口：已发现 {} 个匹配接口", paths.len())),
        Err(error) => lines.push(format!("HID 控制接口：枚举失败（{error:#}）")),
    }

    match LibUsb::load() {
        Ok(api) => match api.target_count() {
            Ok(0) => lines.push("libusb0 视频接口：未发现".to_owned()),
            Ok(count) => lines.push(format!(
                "libusb0 视频接口：已发现 {count} 个 345F:9132 设备"
            )),
            Err(error) => lines.push(format!("libusb0 视频接口：枚举失败（{error:#}）")),
        },
        Err(error) => lines.push(format!("libusb0：无法加载（{error:#}）")),
    }
    lines
}

pub struct Tm360 {
    bulk: LibUsb,
    control: HidControl,
}

impl Tm360 {
    pub fn open() -> Result<Self> {
        let hid_count = HidApi::load()?.target_paths()?.len();
        let bulk_count = LibUsb::load()?.target_count()?;
        if hid_count != 1 || bulk_count != 1 {
            bail!("要求恰好一台 TM-360，但发现 {hid_count} 个 HID 接口和 {bulk_count} 个视频设备");
        }
        let control = HidControl::open()
            .context("无法打开 TM-360 HID 控制接口；请先从托盘彻底退出 JONSBO-AIO，再重试")?;
        let bulk = LibUsb::load().context("无法加载系统 libusb0.dll")?;
        Ok(Self { bulk, control })
    }

    pub fn start(&mut self, first_packet: &[u8]) -> Result<()> {
        self.control.initialize()?;
        self.bulk
            .open_target()
            .context("无法 claim TM-360 视频接口；请先从托盘彻底退出 JONSBO-AIO，再重试")?;
        self.bulk.write_all(first_packet)?;
        self.bulk.write_all(first_packet)?;
        self.control.enable_after_first_frame()?;
        Ok(())
    }

    pub fn send_packet(&self, packet: &[u8]) -> Result<()> {
        self.bulk.write_all(packet)
    }
}

type Handle = *mut c_void;
const INVALID_HANDLE_VALUE: Handle = -1_isize as Handle;
const DIGCF_PRESENT: u32 = 0x0000_0002;
const DIGCF_DEVICEINTERFACE: u32 = 0x0000_0010;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const OPEN_EXISTING: u32 = 3;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[repr(C)]
struct DeviceInterfaceData {
    size: u32,
    interface_class_guid: Guid,
    flags: u32,
    reserved: usize,
}

impl Default for DeviceInterfaceData {
    fn default() -> Self {
        Self {
            size: std::mem::size_of::<Self>() as u32,
            interface_class_guid: Guid::default(),
            flags: 0,
            reserved: 0,
        }
    }
}

type GetHidGuid = unsafe extern "system" fn(*mut Guid);
type GetClassDevs = unsafe extern "system" fn(*const Guid, *const u16, Handle, u32) -> Handle;
type EnumDeviceInterfaces = unsafe extern "system" fn(
    Handle,
    *mut c_void,
    *const Guid,
    u32,
    *mut DeviceInterfaceData,
) -> i32;
type GetDeviceInterfaceDetail = unsafe extern "system" fn(
    Handle,
    *mut DeviceInterfaceData,
    *mut c_void,
    u32,
    *mut u32,
    *mut c_void,
) -> i32;
type DestroyDeviceInfoList = unsafe extern "system" fn(Handle) -> i32;
type CreateFile =
    unsafe extern "system" fn(*const u16, u32, u32, *mut c_void, u32, u32, Handle) -> Handle;
type CloseHandle = unsafe extern "system" fn(Handle) -> i32;
type SetFeature = unsafe extern "system" fn(Handle, *mut c_void, u32) -> i32;
type GetFeature = unsafe extern "system" fn(Handle, *mut c_void, u32) -> i32;

struct HidApi {
    _hid: Library,
    _setupapi: Library,
    _kernel32: Library,
    get_hid_guid: GetHidGuid,
    get_class_devs: GetClassDevs,
    enum_device_interfaces: EnumDeviceInterfaces,
    get_device_interface_detail: GetDeviceInterfaceDetail,
    destroy_device_info_list: DestroyDeviceInfoList,
    create_file: CreateFile,
    close_handle: CloseHandle,
    set_feature: SetFeature,
    get_feature: GetFeature,
}

impl HidApi {
    fn load() -> Result<Self> {
        unsafe {
            let hid = Library::new(system_dll("hid.dll")?)?;
            let setupapi = Library::new(system_dll("setupapi.dll")?)?;
            let kernel32 = Library::new(system_dll("kernel32.dll")?)?;
            let get_hid_guid = *hid.get::<GetHidGuid>(b"HidD_GetHidGuid\0")?;
            let set_feature = *hid.get::<SetFeature>(b"HidD_SetFeature\0")?;
            let get_feature = *hid.get::<GetFeature>(b"HidD_GetFeature\0")?;
            let get_class_devs = *setupapi.get::<GetClassDevs>(b"SetupDiGetClassDevsW\0")?;
            let enum_device_interfaces =
                *setupapi.get::<EnumDeviceInterfaces>(b"SetupDiEnumDeviceInterfaces\0")?;
            let get_device_interface_detail =
                *setupapi.get::<GetDeviceInterfaceDetail>(b"SetupDiGetDeviceInterfaceDetailW\0")?;
            let destroy_device_info_list =
                *setupapi.get::<DestroyDeviceInfoList>(b"SetupDiDestroyDeviceInfoList\0")?;
            let create_file = *kernel32.get::<CreateFile>(b"CreateFileW\0")?;
            let close_handle = *kernel32.get::<CloseHandle>(b"CloseHandle\0")?;
            Ok(Self {
                _hid: hid,
                _setupapi: setupapi,
                _kernel32: kernel32,
                get_hid_guid,
                get_class_devs,
                enum_device_interfaces,
                get_device_interface_detail,
                destroy_device_info_list,
                create_file,
                close_handle,
                set_feature,
                get_feature,
            })
        }
    }

    fn target_paths(&self) -> Result<Vec<Vec<u16>>> {
        let mut guid = Guid::default();
        unsafe { (self.get_hid_guid)(&mut guid) };
        let devices = unsafe {
            (self.get_class_devs)(
                &guid,
                ptr::null(),
                ptr::null_mut(),
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            )
        };
        if devices == INVALID_HANDLE_VALUE {
            bail!(
                "SetupDiGetClassDevsW 失败：{}",
                std::io::Error::last_os_error()
            );
        }

        let result = self.collect_target_paths(devices, &guid);
        unsafe { (self.destroy_device_info_list)(devices) };
        result
    }

    fn collect_target_paths(&self, devices: Handle, guid: &Guid) -> Result<Vec<Vec<u16>>> {
        let mut paths = Vec::new();
        for index in 0.. {
            let mut interface = DeviceInterfaceData::default();
            let found = unsafe {
                (self.enum_device_interfaces)(devices, ptr::null_mut(), guid, index, &mut interface)
            };
            if found == 0 {
                break;
            }

            let mut required = 0;
            unsafe {
                (self.get_device_interface_detail)(
                    devices,
                    &mut interface,
                    ptr::null_mut(),
                    0,
                    &mut required,
                    ptr::null_mut(),
                );
            }
            if required < 8 {
                continue;
            }
            let mut detail = vec![0_u8; required as usize];
            detail[..4].copy_from_slice(&8_u32.to_ne_bytes());
            let read = unsafe {
                (self.get_device_interface_detail)(
                    devices,
                    &mut interface,
                    detail.as_mut_ptr() as *mut c_void,
                    required,
                    &mut required,
                    ptr::null_mut(),
                )
            };
            if read == 0 {
                continue;
            }
            // DevicePath begins after the 4-byte cbSize field. cbSize itself must be
            // 8 on x64 because the variable-length structure has 8-byte alignment.
            let path_start = unsafe { detail.as_ptr().add(4) as *const u16 };
            let capacity = (detail.len() - 4) / 2;
            let length = (0..capacity)
                .find(|offset| unsafe { *path_start.add(*offset) } == 0)
                .unwrap_or(capacity);
            let path = unsafe { std::slice::from_raw_parts(path_start, length) };
            let lower = String::from_utf16_lossy(path).to_ascii_lowercase();
            if lower.contains("vid_345f&pid_9132") {
                let mut owned = path.to_vec();
                owned.push(0);
                paths.push(owned);
            }
        }
        Ok(paths)
    }
}

struct HidControl {
    api: HidApi,
    handle: Handle,
}

impl HidControl {
    fn open() -> Result<Self> {
        let api = HidApi::load().context("加载 Windows HID/SetupAPI 失败")?;
        let path = api
            .target_paths()?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("找不到 345F:9132 的 HID 接口"))?;
        let handle = unsafe {
            (api.create_file)(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null_mut(),
                OPEN_EXISTING,
                0,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            bail!(
                "CreateFileW 打开 HID 接口失败：{}",
                std::io::Error::last_os_error()
            );
        }
        Ok(Self { api, handle })
    }

    fn send(&self, report: &[u8; 9]) -> Result<()> {
        let success = unsafe {
            (self.api.set_feature)(
                self.handle,
                report.as_ptr() as *mut c_void,
                report.len() as u32,
            )
        };
        if success == 0 {
            bail!(
                "发送 HID 指令 {:02X?} 失败：{}",
                &report[1..],
                std::io::Error::last_os_error(),
            );
        }
        Ok(())
    }

    fn get(&self) -> Result<[u8; 9]> {
        let mut report = [0_u8; 9];
        let success = unsafe {
            (self.api.get_feature)(
                self.handle,
                report.as_mut_ptr() as *mut c_void,
                report.len() as u32,
            )
        };
        if success == 0 {
            bail!(
                "读取 HID feature report 失败：{}",
                std::io::Error::last_os_error()
            );
        }
        Ok(report)
    }

    fn issue(&self, payload: [u8; 8]) -> Result<Option<[u8; 9]>> {
        let mut report = [0_u8; 9];
        report[1..].copy_from_slice(&payload);
        self.send(&report)?;
        if matches!(payload[0], 0xb5 | 0xf5) {
            Ok(Some(self.get()?))
        } else {
            Ok(None)
        }
    }

    fn initialize(&self) -> Result<()> {
        let started = Instant::now();
        for &(target_micros, payload) in STARTUP_REPORTS {
            let target = Duration::from_micros(target_micros as u64);
            thread::sleep(target.saturating_sub(started.elapsed()));
            self.issue(payload)?;
        }
        Ok(())
    }

    fn enable_after_first_frame(&self) -> Result<()> {
        for &payload in POST_FIRST_FRAME_REPORTS {
            self.issue(payload)?;
            thread::sleep(Duration::from_millis(3));
        }
        Ok(())
    }
}

impl Drop for HidControl {
    fn drop(&mut self) {
        unsafe { (self.api.close_handle)(self.handle) };
        self.handle = INVALID_HANDLE_VALUE;
    }
}

#[repr(C)]
struct UsbBus {
    next: *mut UsbBus,
    prev: *mut UsbBus,
    dirname: [c_char; 512],
    devices: *mut UsbDevice,
}

#[repr(C)]
struct UsbDevice {
    next: *mut UsbDevice,
    prev: *mut UsbDevice,
    filename: [c_char; 512],
    bus: *mut UsbBus,
    descriptor: [u8; 18],
}

enum UsbDevHandle {}

type UsbInit = unsafe extern "C" fn();
type UsbFind = unsafe extern "C" fn() -> c_int;
type UsbGetBusses = unsafe extern "C" fn() -> *mut UsbBus;
type UsbOpen = unsafe extern "C" fn(*mut UsbDevice) -> *mut UsbDevHandle;
type UsbClaim = unsafe extern "C" fn(*mut UsbDevHandle, c_int) -> c_int;
type UsbBulkWrite =
    unsafe extern "C" fn(*mut UsbDevHandle, c_int, *mut c_char, c_int, c_int) -> c_int;
type UsbRelease = unsafe extern "C" fn(*mut UsbDevHandle, c_int) -> c_int;
type UsbClose = unsafe extern "C" fn(*mut UsbDevHandle) -> c_int;
type UsbStrError = unsafe extern "C" fn() -> *const c_char;

struct LibUsb {
    _library: Library,
    find_busses: UsbFind,
    find_devices: UsbFind,
    get_busses: UsbGetBusses,
    open: UsbOpen,
    claim_interface: UsbClaim,
    bulk_write: UsbBulkWrite,
    release_interface: UsbRelease,
    close: UsbClose,
    strerror: UsbStrError,
    handle: *mut UsbDevHandle,
}

impl LibUsb {
    fn load() -> Result<Self> {
        unsafe {
            let library = Library::new(system_dll("libusb0.dll")?)
                .context("系统 System32 中没有可用的 libusb0.dll")?;
            let init = *library.get::<UsbInit>(b"usb_init\0")?;
            let find_busses = *library.get::<UsbFind>(b"usb_find_busses\0")?;
            let find_devices = *library.get::<UsbFind>(b"usb_find_devices\0")?;
            let get_busses = *library.get::<UsbGetBusses>(b"usb_get_busses\0")?;
            let open = *library.get::<UsbOpen>(b"usb_open\0")?;
            let claim_interface = *library.get::<UsbClaim>(b"usb_claim_interface\0")?;
            let bulk_write = *library.get::<UsbBulkWrite>(b"usb_bulk_write\0")?;
            let release_interface = *library.get::<UsbRelease>(b"usb_release_interface\0")?;
            let close = *library.get::<UsbClose>(b"usb_close\0")?;
            let strerror = *library.get::<UsbStrError>(b"usb_strerror\0")?;
            init();
            Ok(Self {
                _library: library,
                find_busses,
                find_devices,
                get_busses,
                open,
                claim_interface,
                bulk_write,
                release_interface,
                close,
                strerror,
                handle: ptr::null_mut(),
            })
        }
    }

    fn refresh(&self) -> Result<()> {
        let busses = unsafe { (self.find_busses)() };
        if busses < 0 {
            bail!("usb_find_busses 失败：{}", self.error_text());
        }
        let devices = unsafe { (self.find_devices)() };
        if devices < 0 {
            bail!("usb_find_devices 失败：{}", self.error_text());
        }
        Ok(())
    }

    fn target_devices(&self) -> Result<Vec<*mut UsbDevice>> {
        self.refresh()?;
        let mut matches = Vec::new();
        unsafe {
            let mut bus = (self.get_busses)();
            while !bus.is_null() {
                let mut device = (*bus).devices;
                while !device.is_null() {
                    let descriptor = &(*device).descriptor;
                    let vid = u16::from_le_bytes([descriptor[8], descriptor[9]]);
                    let pid = u16::from_le_bytes([descriptor[10], descriptor[11]]);
                    if vid == VID && pid == PID {
                        matches.push(device);
                    }
                    device = (*device).next;
                }
                bus = (*bus).next;
            }
        }
        Ok(matches)
    }

    fn target_count(&self) -> Result<usize> {
        Ok(self.target_devices()?.len())
    }

    fn open_target(&mut self) -> Result<()> {
        let device = self
            .target_devices()?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("libusb0 未发现 345F:9132"))?;
        let handle = unsafe { (self.open)(device) };
        if handle.is_null() {
            bail!("usb_open 失败：{}", self.error_text());
        }
        let result = unsafe { (self.claim_interface)(handle, INTERFACE) };
        if result < 0 {
            unsafe { (self.close)(handle) };
            bail!(
                "usb_claim_interface({INTERFACE}) 失败：{}",
                self.error_text()
            );
        }
        self.handle = handle;
        Ok(())
    }

    fn write_all(&self, data: &[u8]) -> Result<()> {
        if self.handle.is_null() {
            bail!("libusb0 视频接口尚未打开");
        }
        for chunk in data.chunks(CHUNK_SIZE) {
            let mut offset = 0;
            while offset < chunk.len() {
                let remaining = &chunk[offset..];
                let written = unsafe {
                    (self.bulk_write)(
                        self.handle,
                        ENDPOINT,
                        remaining.as_ptr() as *mut c_void as *mut c_char,
                        remaining.len() as c_int,
                        5_000,
                    )
                };
                if written <= 0 {
                    bail!("usb_bulk_write 失败：{}", self.error_text());
                }
                offset += written as usize;
            }
        }
        Ok(())
    }

    fn error_text(&self) -> String {
        unsafe {
            let pointer = (self.strerror)();
            if pointer.is_null() {
                "未知 libusb0 错误".to_owned()
            } else {
                CStr::from_ptr(pointer).to_string_lossy().into_owned()
            }
        }
    }
}

impl Drop for LibUsb {
    fn drop(&mut self) {
        if self.handle.is_null() {
            return;
        }
        unsafe {
            (self.release_interface)(self.handle, INTERFACE);
            (self.close)(self.handle);
        }
        self.handle = ptr::null_mut();
    }
}

#[cfg(test)]
mod tests {
    use std::mem::offset_of;

    use super::*;

    #[test]
    fn libusb_win32_prefix_layout_matches_usb_h() {
        assert_eq!(offset_of!(UsbBus, devices), 528);
        assert_eq!(offset_of!(UsbDevice, descriptor), 536);
    }

    #[test]
    fn captured_startup_has_tm360_mode_and_enable_reports() {
        assert_eq!(STARTUP_REPORTS.len(), 81);
        assert!(
            STARTUP_REPORTS
                .iter()
                .any(|entry| { entry.1 == report(b"a60101e001e02200") })
        );
        assert!(
            STARTUP_REPORTS
                .iter()
                .any(|entry| { entry.1 == report(b"a6028f0001e001e0") })
        );
        assert_eq!(
            POST_FIRST_FRAME_REPORTS,
            &[
                report(b"b5f0050000000000"),
                report(b"b6f0051000000000"),
                report(b"a605010000000000"),
                report(b"b5c5550000000000"),
            ]
        );
    }
}
