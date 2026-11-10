//! AMD Radeon 传感器：通过显卡驱动自带的 ADL（`atiadlxx.dll`）只读查询。

use std::{
    ffi::c_void,
    mem::{size_of, zeroed},
    ptr,
};

use libloading::Library;
use windows_sys::Win32::System::Memory::{GetProcessHeap, HeapAlloc};

use crate::{monitor::GpuSample, win::system_dll};

/// ADL 的负值是错误；0 以及正值（如 `ADL_OK_WARNING`）都表示调用成功。
const ADL_OK: i32 = 0;
const ADL_MAX_PATH: usize = 256;
const ADL_PMLOG_MAX_SENSORS: usize = 256;
/// ADL 在 Windows 上以十进制 1002 报告 AMD；同时接受 PCI 厂商号 0x1002。
const AMD_VENDOR_IDS: [i32; 2] = [1002, 0x1002];

// `ADLSensorType` 中用到的下标。
const PMLOG_CLK_GFXCLK: usize = 1;
const PMLOG_CLK_MEMCLK: usize = 2;
const PMLOG_TEMPERATURE_EDGE: usize = 8;
const PMLOG_FAN_PERCENTAGE: usize = 15;
const PMLOG_INFO_ACTIVITY_GFX: usize = 19;
const PMLOG_ASIC_POWER: usize = 23;
const PMLOG_TEMPERATURE_HOTSPOT: usize = 27;
const PMLOG_BOARD_POWER: usize = 73;

type AdlContext = *mut c_void;
type AdlMalloc = unsafe extern "system" fn(i32) -> *mut c_void;
type AdlCreate = unsafe extern "C" fn(AdlMalloc, i32, *mut AdlContext) -> i32;
type AdlDestroy = unsafe extern "C" fn(AdlContext) -> i32;
type AdlAdapterCount = unsafe extern "C" fn(AdlContext, *mut i32) -> i32;
type AdlAdapterInfo = unsafe extern "C" fn(AdlContext, *mut AdapterInfo, i32) -> i32;
type AdlQueryPmLog = unsafe extern "C" fn(AdlContext, i32, *mut PmLogData) -> i32;
type AdlGetI32 = unsafe extern "C" fn(AdlContext, i32, *mut i32) -> i32;
type AdlMemoryInfo = unsafe extern "C" fn(AdlContext, i32, *mut MemoryInfo2) -> i32;

/// `AdapterInfo` 的 Windows 布局。
#[repr(C)]
struct AdapterInfo {
    size: i32,
    adapter_index: i32,
    udid: [u8; ADL_MAX_PATH],
    bus_number: i32,
    device_number: i32,
    function_number: i32,
    vendor_id: i32,
    adapter_name: [u8; ADL_MAX_PATH],
    display_name: [u8; ADL_MAX_PATH],
    present: i32,
    exist: i32,
    driver_path: [u8; ADL_MAX_PATH],
    driver_path_ext: [u8; ADL_MAX_PATH],
    pnp_string: [u8; ADL_MAX_PATH],
    os_display_index: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SensorData {
    supported: i32,
    value: i32,
}

/// `ADLPMLogDataOutput`。
#[repr(C)]
struct PmLogData {
    size: i32,
    sensors: [SensorData; ADL_PMLOG_MAX_SENSORS],
}

/// `ADLMemoryInfo2`。
#[repr(C)]
struct MemoryInfo2 {
    memory_size: i64,
    memory_type: [u8; ADL_MAX_PATH],
    memory_bandwidth: i64,
    hyper_memory_size: i64,
    invisible_memory_size: i64,
    visible_memory_size: i64,
}

const _: () = assert!(size_of::<AdapterInfo>() == 1572);
const _: () = assert!(size_of::<PmLogData>() == 2052);
const _: () = assert!(size_of::<MemoryInfo2>() == 296);

pub(crate) struct Adl {
    _library: Library,
    context: AdlContext,
    adapter: i32,
    memory_total_bytes: Option<u64>,
    destroy: AdlDestroy,
    query_pm_log: AdlQueryPmLog,
    dedicated_vram_usage: Option<AdlGetI32>,
}

impl Adl {
    pub(crate) fn load() -> Option<Self> {
        unsafe {
            let library = Library::new(system_dll("atiadlxx.dll").ok()?).ok()?;
            let create = *library
                .get::<AdlCreate>(b"ADL2_Main_Control_Create\0")
                .ok()?;
            let destroy = *library
                .get::<AdlDestroy>(b"ADL2_Main_Control_Destroy\0")
                .ok()?;
            let adapter_count = *library
                .get::<AdlAdapterCount>(b"ADL2_Adapter_NumberOfAdapters_Get\0")
                .ok()?;
            let adapter_info = *library
                .get::<AdlAdapterInfo>(b"ADL2_Adapter_AdapterInfo_Get\0")
                .ok()?;
            let query_pm_log = *library
                .get::<AdlQueryPmLog>(b"ADL2_New_QueryPMLogData_Get\0")
                .ok()?;
            let memory_info = library
                .get::<AdlMemoryInfo>(b"ADL2_Adapter_MemoryInfo2_Get\0")
                .ok()
                .map(|symbol| *symbol);
            let dedicated_vram_usage = library
                .get::<AdlGetI32>(b"ADL2_Adapter_DedicatedVRAMUsage_Get\0")
                .ok()
                .map(|symbol| *symbol);

            let mut context = ptr::null_mut();
            if create(adl_malloc, 1, &mut context) < ADL_OK {
                return None;
            }
            // 从这里起由 Drop 负责销毁上下文。
            let mut adl = Self {
                _library: library,
                context,
                adapter: 0,
                memory_total_bytes: None,
                destroy,
                query_pm_log,
                dedicated_vram_usage,
            };

            let mut count = 0;
            if adapter_count(context, &mut count) < ADL_OK || count <= 0 {
                return None;
            }
            let byte_length = count.checked_mul(size_of::<AdapterInfo>() as i32)?;
            let mut adapters: Vec<AdapterInfo> = (0..count).map(|_| zeroed()).collect();
            if adapter_info(context, adapters.as_mut_ptr(), byte_length) < ADL_OK {
                return None;
            }

            let candidates: Vec<AdapterCandidate> = adapters
                .iter()
                .filter(|adapter| {
                    AMD_VENDOR_IDS.contains(&adapter.vendor_id)
                        && adapter.present != 0
                        && adapter.exist != 0
                })
                .filter(|adapter| {
                    // 只接受确实支持性能日志查询的适配器。
                    let mut log: PmLogData = zeroed();
                    query_pm_log(context, adapter.adapter_index, &mut log) >= ADL_OK
                })
                .map(|adapter| AdapterCandidate {
                    index: adapter.adapter_index,
                    memory_bytes: memory_info.and_then(|function| {
                        let mut memory: MemoryInfo2 = zeroed();
                        (function(context, adapter.adapter_index, &mut memory) >= ADL_OK)
                            .then_some(memory.memory_size)
                            .and_then(|bytes| u64::try_from(bytes).ok())
                            .filter(|bytes| *bytes > 0)
                    }),
                })
                .collect();
            let selected = select_adapter(&candidates)?;
            adl.adapter = selected.index;
            adl.memory_total_bytes = selected.memory_bytes;
            Some(adl)
        }
    }

    pub(crate) fn sample(&self) -> GpuSample {
        unsafe {
            let mut log: PmLogData = zeroed();
            if (self.query_pm_log)(self.context, self.adapter, &mut log) < ADL_OK {
                // 性能日志失败时各传感器记为不支持，显存信息仍单独上报。
                log = zeroed();
            }
            let memory_used_megabytes = self.dedicated_vram_usage.and_then(|function| {
                let mut megabytes = 0;
                (function(self.context, self.adapter, &mut megabytes) >= ADL_OK)
                    .then_some(megabytes)
            });
            gpu_sample(&log, memory_used_megabytes, self.memory_total_bytes)
        }
    }
}

impl Drop for Adl {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.context) };
    }
}

/// ADL 要求调用方提供分配器；这里用到的查询都写入调用方缓冲区，不会经它返回内存。
unsafe extern "system" fn adl_malloc(size: i32) -> *mut c_void {
    match usize::try_from(size) {
        Ok(size) => unsafe { HeapAlloc(GetProcessHeap(), 0, size) },
        Err(_) => ptr::null_mut(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AdapterCandidate {
    index: i32,
    memory_bytes: Option<u64>,
}

/// 核显与独显并存时取专用显存最大的一个；相同时保留 ADL 给出的第一个。
fn select_adapter(candidates: &[AdapterCandidate]) -> Option<AdapterCandidate> {
    candidates.iter().copied().reduce(|best, candidate| {
        if candidate.memory_bytes > best.memory_bytes {
            candidate
        } else {
            best
        }
    })
}

fn sensor(log: &PmLogData, index: usize) -> Option<u32> {
    let sensor = log.sensors[index];
    (sensor.supported != 0)
        .then_some(sensor.value)
        .and_then(|value| u32::try_from(value).ok())
}

fn gpu_sample(
    log: &PmLogData,
    memory_used_megabytes: Option<i32>,
    memory_total_bytes: Option<u64>,
) -> GpuSample {
    GpuSample {
        percent: sensor(log, PMLOG_INFO_ACTIVITY_GFX).map(|percent| percent.min(100)),
        temperature: sensor(log, PMLOG_TEMPERATURE_EDGE)
            .or_else(|| sensor(log, PMLOG_TEMPERATURE_HOTSPOT)),
        // 与 NVML 的整卡功耗口径一致：优先整卡功耗，旧型号退回芯片功耗。
        power_w: sensor(log, PMLOG_BOARD_POWER)
            .or_else(|| sensor(log, PMLOG_ASIC_POWER))
            .map(|watts| watts as f32),
        power_limit_w: None,
        memory_used_bytes: memory_used_megabytes
            .and_then(|megabytes| u64::try_from(megabytes).ok())
            .map(|megabytes| megabytes << 20),
        memory_total_bytes,
        graphics_clock_mhz: sensor(log, PMLOG_CLK_GFXCLK),
        memory_clock_mhz: sensor(log, PMLOG_CLK_MEMCLK),
        performance_state: None,
        fan_percent: sensor(log, PMLOG_FAN_PERCENTAGE).map(|percent| percent.min(100)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(values: &[(usize, i32)]) -> PmLogData {
        let mut log: PmLogData = unsafe { zeroed() };
        for (index, value) in values {
            log.sensors[*index] = SensorData {
                supported: 1,
                value: *value,
            };
        }
        log
    }

    #[test]
    fn maps_supported_sensors_from_a_radeon_performance_log() {
        // 取自 RX 9070 XT 的一次真实查询：没有 ASIC 功耗，只有整卡功耗。
        let log = log(&[
            (PMLOG_CLK_GFXCLK, 40),
            (PMLOG_CLK_MEMCLK, 2505),
            (PMLOG_TEMPERATURE_EDGE, 33),
            (PMLOG_FAN_PERCENTAGE, 20),
            (PMLOG_INFO_ACTIVITY_GFX, 3),
            (PMLOG_TEMPERATURE_HOTSPOT, 34),
            (PMLOG_BOARD_POWER, 49),
        ]);
        let sample = gpu_sample(&log, Some(3307), Some(17_095_983_104));
        assert_eq!(sample.percent, Some(3));
        assert_eq!(sample.temperature, Some(33));
        assert_eq!(sample.power_w, Some(49.0));
        assert_eq!(sample.power_limit_w, None);
        assert_eq!(sample.memory_used_bytes, Some(3307 << 20));
        assert_eq!(sample.memory_total_bytes, Some(17_095_983_104));
        assert_eq!(sample.graphics_clock_mhz, Some(40));
        assert_eq!(sample.memory_clock_mhz, Some(2505));
        assert_eq!(sample.performance_state, None);
        assert_eq!(sample.fan_percent, Some(20));
    }

    #[test]
    fn unsupported_or_negative_sensors_are_none_and_fallbacks_apply() {
        let mut log = log(&[
            (PMLOG_TEMPERATURE_HOTSPOT, 61),
            (PMLOG_ASIC_POWER, 120),
            (PMLOG_INFO_ACTIVITY_GFX, 250),
            (PMLOG_FAN_PERCENTAGE, -1),
        ]);
        // 有数值但未标记为支持的传感器不能被采用。
        log.sensors[PMLOG_TEMPERATURE_EDGE].value = 99;
        let sample = gpu_sample(&log, Some(-1), None);
        assert_eq!(sample.temperature, Some(61));
        assert_eq!(sample.power_w, Some(120.0));
        assert_eq!(sample.percent, Some(100));
        assert_eq!(sample.fan_percent, None);
        assert_eq!(sample.graphics_clock_mhz, None);
        assert_eq!(sample.memory_used_bytes, None);
        assert_eq!(sample.memory_total_bytes, None);
    }

    #[test]
    fn selects_the_adapter_with_the_most_dedicated_memory() {
        let integrated = AdapterCandidate {
            index: 0,
            memory_bytes: Some(512 << 20),
        };
        let discrete = AdapterCandidate {
            index: 4,
            memory_bytes: Some(16 << 30),
        };
        let unknown = AdapterCandidate {
            index: 7,
            memory_bytes: None,
        };
        assert_eq!(select_adapter(&[]), None);
        assert_eq!(select_adapter(&[unknown]), Some(unknown));
        assert_eq!(
            select_adapter(&[integrated, unknown, discrete]),
            Some(discrete)
        );
        // 同一块显卡的多个输出给出相同显存：保留第一个。
        let second_output = AdapterCandidate {
            index: 5,
            ..discrete
        };
        assert_eq!(select_adapter(&[discrete, second_output]), Some(discrete));
    }
}
