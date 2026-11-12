//! 厂商无关的 GPU 传感器：没有 NVML 和 ADL 时（例如 Intel 显卡）使用 Windows 自身的接口。
//!
//! 占用率和显存用量来自任务管理器同源的 PDH 计数器；显存总量和温度通过 D3DKMT 向显卡驱动查询。
//! 功耗、风扇和时钟没有通用接口，保持 `none`。

use std::{
    collections::HashMap,
    ffi::c_void,
    mem::{size_of, zeroed},
    ptr,
};

use libloading::Library;

use crate::{
    monitor::GpuSample,
    win::{system_dll, wide},
};

const KMTQAITYPE_GETSEGMENTSIZE: u32 = 3;
const KMTQAITYPE_ADAPTERTYPE: u32 = 15;
const KMTQAITYPE_ADAPTERPERFDATA: u32 = 62;
const ADAPTER_TYPE_RENDER_SUPPORTED: u32 = 1 << 0;
const ADAPTER_TYPE_SOFTWARE_DEVICE: u32 = 1 << 2;
/// 专用显存不足这个数量的适配器按核显处理，改用共享显存的用量和总量。
const DISCRETE_MEMORY_THRESHOLD: u64 = 512 << 20;

const PDH_FMT_DOUBLE: u32 = 0x0000_0200;
const PDH_FMT_NOCAP100: u32 = 0x0000_8000;
const PDH_MORE_DATA: i32 = 0x8000_07D2_u32 as i32;
const PDH_CSTATUS_VALID_DATA: u32 = 0;
const PDH_CSTATUS_NEW_DATA: u32 = 1;

type D3dkmtCall<T> = unsafe extern "system" fn(*mut T) -> i32;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Luid {
    low: u32,
    high: i32,
}

/// `D3DKMT_ADAPTERINFO`。
#[repr(C)]
#[derive(Clone, Copy)]
struct AdapterInfo {
    handle: u32,
    luid: Luid,
    source_count: u32,
    precise_present_regions_preferred: i32,
}

/// `D3DKMT_ENUMADAPTERS2`。
#[repr(C)]
struct EnumAdapters2 {
    count: u32,
    adapters: *mut AdapterInfo,
}

/// `D3DKMT_QUERYADAPTERINFO`。
#[repr(C)]
struct QueryAdapterInfo {
    adapter: u32,
    kind: u32,
    data: *mut c_void,
    data_size: u32,
}

/// `D3DKMT_CLOSEADAPTER`。
#[repr(C)]
struct CloseAdapter {
    adapter: u32,
}

/// `D3DKMT_SEGMENTSIZEINFO`。
#[repr(C)]
#[derive(Default)]
struct SegmentSizeInfo {
    dedicated_video_memory: u64,
    dedicated_system_memory: u64,
    shared_system_memory: u64,
}

/// `D3DKMT_ADAPTER_PERFDATA`；`temperature` 的单位是 0.1°C。
#[repr(C)]
#[derive(Default)]
struct AdapterPerfData {
    physical_adapter_index: u32,
    memory_frequency: u64,
    max_memory_frequency: u64,
    max_memory_frequency_oc: u64,
    memory_bandwidth: u64,
    pcie_bandwidth: u64,
    fan_rpm: u32,
    power: u32,
    temperature: u32,
    power_state_override: u8,
}

const _: () = assert!(size_of::<AdapterInfo>() == 20);
const _: () = assert!(size_of::<EnumAdapters2>() == 16);
const _: () = assert!(size_of::<QueryAdapterInfo>() == 24);
const _: () = assert!(size_of::<SegmentSizeInfo>() == 24);
const _: () = assert!(size_of::<AdapterPerfData>() == 64);

type PdhQuery = *mut c_void;
type PdhCounter = *mut c_void;
type PdhOpenQuery = unsafe extern "system" fn(*const u16, usize, *mut PdhQuery) -> i32;
type PdhAddEnglishCounter =
    unsafe extern "system" fn(PdhQuery, *const u16, usize, *mut PdhCounter) -> i32;
type PdhCollectQueryData = unsafe extern "system" fn(PdhQuery) -> i32;
type PdhGetFormattedCounterArray =
    unsafe extern "system" fn(PdhCounter, u32, *mut u32, *mut u32, *mut CounterItem) -> i32;
type PdhCloseQuery = unsafe extern "system" fn(PdhQuery) -> i32;

/// `PDH_FMT_COUNTERVALUE_ITEM_W`，取值格式固定为 double。
#[repr(C)]
struct CounterItem {
    name: *const u16,
    status: u32,
    value: f64,
}

const _: () = assert!(size_of::<CounterItem>() == 24);

pub(crate) struct WindowsGpu {
    adapter: Adapter,
    counters: Option<Counters>,
    luid_tag: String,
    use_dedicated_memory: bool,
    memory_total_bytes: Option<u64>,
}

impl WindowsGpu {
    pub(crate) fn load() -> Option<Self> {
        let adapter = Adapter::open()?;
        let use_dedicated_memory = adapter.dedicated_bytes >= DISCRETE_MEMORY_THRESHOLD;
        let memory_total_bytes = Some(if use_dedicated_memory {
            adapter.dedicated_bytes
        } else {
            adapter.shared_bytes
        })
        .filter(|bytes| *bytes > 0);
        Some(Self {
            luid_tag: luid_tag(adapter.luid),
            counters: Counters::open(),
            adapter,
            use_dedicated_memory,
            memory_total_bytes,
        })
    }

    pub(crate) fn sample(&self) -> GpuSample {
        let mut sample = GpuSample {
            temperature: self.adapter.temperature(),
            memory_total_bytes: self.memory_total_bytes,
            ..GpuSample::default()
        };
        if let Some(counters) = &self.counters
            && counters.collect()
        {
            sample.percent = counters
                .values(counters.utilization)
                .and_then(|items| utilization_percent(&items, &self.luid_tag));
            let memory_counter = if self.use_dedicated_memory {
                counters.dedicated_usage
            } else {
                counters.shared_usage
            };
            sample.memory_used_bytes = memory_counter
                .and_then(|counter| counters.values(Some(counter)))
                .and_then(|items| memory_used_bytes(&items, &self.luid_tag));
        }
        sample
    }
}

struct Adapter {
    _library: Library,
    handle: u32,
    luid: Luid,
    dedicated_bytes: u64,
    shared_bytes: u64,
    query: D3dkmtCall<QueryAdapterInfo>,
    close: D3dkmtCall<CloseAdapter>,
}

impl Adapter {
    fn open() -> Option<Self> {
        unsafe {
            let library = Library::new(system_dll("gdi32.dll").ok()?).ok()?;
            let enumerate = *library
                .get::<D3dkmtCall<EnumAdapters2>>(b"D3DKMTEnumAdapters2\0")
                .ok()?;
            let query = *library
                .get::<D3dkmtCall<QueryAdapterInfo>>(b"D3DKMTQueryAdapterInfo\0")
                .ok()?;
            let close = *library
                .get::<D3dkmtCall<CloseAdapter>>(b"D3DKMTCloseAdapter\0")
                .ok()?;

            let mut request = EnumAdapters2 {
                count: 0,
                adapters: ptr::null_mut(),
            };
            if enumerate(&mut request) < 0 || request.count == 0 {
                return None;
            }
            let mut adapters: Vec<AdapterInfo> = (0..request.count).map(|_| zeroed()).collect();
            request.adapters = adapters.as_mut_ptr();
            if enumerate(&mut request) < 0 {
                return None;
            }
            adapters.truncate(request.count as usize);

            let candidates: Vec<AdapterCandidate> = adapters
                .iter()
                .enumerate()
                .filter_map(|(index, adapter)| {
                    let kind: u32 = query_adapter(query, adapter.handle, KMTQAITYPE_ADAPTERTYPE)?;
                    let sizes: SegmentSizeInfo =
                        query_adapter(query, adapter.handle, KMTQAITYPE_GETSEGMENTSIZE)?;
                    is_hardware_renderer(kind).then_some(AdapterCandidate {
                        index,
                        dedicated_bytes: sizes.dedicated_video_memory,
                        shared_bytes: sizes.shared_system_memory,
                    })
                })
                .collect();
            let selected = select_adapter(&candidates);
            // 枚举返回的每个句柄都要关闭；只留下选中的那个。
            for (index, adapter) in adapters.iter().enumerate() {
                if selected.is_none_or(|selected| selected.index != index) {
                    close(&mut CloseAdapter {
                        adapter: adapter.handle,
                    });
                }
            }
            let selected = selected?;
            let adapter = adapters[selected.index];
            Some(Self {
                _library: library,
                handle: adapter.handle,
                luid: adapter.luid,
                dedicated_bytes: selected.dedicated_bytes,
                shared_bytes: selected.shared_bytes,
                query,
                close,
            })
        }
    }

    fn temperature(&self) -> Option<u32> {
        let data: AdapterPerfData =
            unsafe { query_adapter(self.query, self.handle, KMTQAITYPE_ADAPTERPERFDATA) }?;
        temperature_celsius(data.temperature)
    }
}

impl Drop for Adapter {
    fn drop(&mut self) {
        unsafe {
            (self.close)(&mut CloseAdapter {
                adapter: self.handle,
            })
        };
    }
}

/// 向驱动查询一个定长结构；`T` 必须是全零有效的 `repr(C)` 类型。
unsafe fn query_adapter<T: Default>(
    query: D3dkmtCall<QueryAdapterInfo>,
    adapter: u32,
    kind: u32,
) -> Option<T> {
    let mut data = T::default();
    let mut request = QueryAdapterInfo {
        adapter,
        kind,
        data: (&raw mut data).cast(),
        data_size: size_of::<T>() as u32,
    };
    (unsafe { query(&mut request) } >= 0).then_some(data)
}

struct Counters {
    _library: Library,
    query: PdhQuery,
    utilization: Option<PdhCounter>,
    dedicated_usage: Option<PdhCounter>,
    shared_usage: Option<PdhCounter>,
    collect: PdhCollectQueryData,
    formatted_array: PdhGetFormattedCounterArray,
    close: PdhCloseQuery,
}

impl Counters {
    fn open() -> Option<Self> {
        unsafe {
            let library = Library::new(system_dll("pdh.dll").ok()?).ok()?;
            let open = *library.get::<PdhOpenQuery>(b"PdhOpenQueryW\0").ok()?;
            let add = *library
                .get::<PdhAddEnglishCounter>(b"PdhAddEnglishCounterW\0")
                .ok()?;
            let collect = *library
                .get::<PdhCollectQueryData>(b"PdhCollectQueryData\0")
                .ok()?;
            let formatted_array = *library
                .get::<PdhGetFormattedCounterArray>(b"PdhGetFormattedCounterArrayW\0")
                .ok()?;
            let close = *library.get::<PdhCloseQuery>(b"PdhCloseQuery\0").ok()?;

            let mut query = ptr::null_mut();
            if open(ptr::null(), 0, &mut query) != 0 || query.is_null() {
                return None;
            }
            let add_counter = |path: &str| {
                let path = wide(path);
                let mut counter = ptr::null_mut();
                (add(query, path.as_ptr(), 0, &mut counter) == 0 && !counter.is_null())
                    .then_some(counter)
            };
            let counters = Self {
                utilization: add_counter(r"\GPU Engine(*)\Utilization Percentage"),
                dedicated_usage: add_counter(r"\GPU Adapter Memory(*)\Dedicated Usage"),
                shared_usage: add_counter(r"\GPU Adapter Memory(*)\Shared Usage"),
                _library: library,
                query,
                collect,
                formatted_array,
                close,
            };
            // 占用率是差分计数器，需要先采集一次作为基准。
            counters.collect();
            Some(counters)
        }
    }

    fn collect(&self) -> bool {
        unsafe { (self.collect)(self.query) == 0 }
    }

    /// 读出通配计数器当前的全部实例：（实例名，数值）。
    fn values(&self, counter: Option<PdhCounter>) -> Option<Vec<(String, f64)>> {
        let counter = counter?;
        let format = PDH_FMT_DOUBLE | PDH_FMT_NOCAP100;
        unsafe {
            let mut byte_length = 0_u32;
            let mut count = 0_u32;
            let status = (self.formatted_array)(
                counter,
                format,
                &mut byte_length,
                &mut count,
                ptr::null_mut(),
            );
            if status != PDH_MORE_DATA || byte_length == 0 {
                return None;
            }
            // 缓冲区里先是条目数组，后面跟着各条目的名称字符串；按 8 字节对齐分配。
            let mut buffer = vec![0_u64; (byte_length as usize).div_ceil(size_of::<u64>())];
            let items = buffer.as_mut_ptr().cast::<CounterItem>();
            if (self.formatted_array)(counter, format, &mut byte_length, &mut count, items) != 0 {
                return None;
            }
            Some(
                std::slice::from_raw_parts(items, count as usize)
                    .iter()
                    .filter(|item| {
                        matches!(item.status, PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA)
                            && !item.name.is_null()
                    })
                    .map(|item| (wide_string(item.name), item.value))
                    .collect(),
            )
        }
    }
}

impl Drop for Counters {
    fn drop(&mut self) {
        unsafe { (self.close)(self.query) };
    }
}

unsafe fn wide_string(pointer: *const u16) -> String {
    let mut length = 0;
    while unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(pointer, length) })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AdapterCandidate {
    index: usize,
    dedicated_bytes: u64,
    shared_bytes: u64,
}

/// 排除“Microsoft 基本呈现驱动程序”这类软件设备和不能渲染的适配器。
fn is_hardware_renderer(adapter_type: u32) -> bool {
    adapter_type & ADAPTER_TYPE_RENDER_SUPPORTED != 0
        && adapter_type & ADAPTER_TYPE_SOFTWARE_DEVICE == 0
}

/// 取专用显存最大的适配器；都没有专用显存时（只有核显）取共享显存最大的。
fn select_adapter(candidates: &[AdapterCandidate]) -> Option<AdapterCandidate> {
    candidates.iter().copied().reduce(|best, candidate| {
        if (candidate.dedicated_bytes, candidate.shared_bytes)
            > (best.dedicated_bytes, best.shared_bytes)
        {
            candidate
        } else {
            best
        }
    })
}

/// PDH 实例名里标识适配器的片段，例如 `luid_0x00000000_0x00014265`。
fn luid_tag(luid: Luid) -> String {
    format!("luid_0x{:08x}_0x{:08x}", luid.high as u32, luid.low)
}

fn belongs_to(instance: &str, luid_tag: &str) -> bool {
    instance.to_ascii_lowercase().contains(luid_tag)
}

/// 与任务管理器相同的口径：同一类引擎把各进程的占用相加，再取最忙的一类。
fn utilization_percent(items: &[(String, f64)], luid_tag: &str) -> Option<u32> {
    let mut by_engine: HashMap<String, f64> = HashMap::new();
    for (instance, value) in items {
        if !belongs_to(instance, luid_tag) || !value.is_finite() {
            continue;
        }
        let instance = instance.to_ascii_lowercase();
        let engine = instance
            .split_once("engtype_")
            .map_or("", |(_, engine)| engine);
        *by_engine.entry(engine.to_owned()).or_default() += value.max(0.0);
    }
    by_engine
        .into_values()
        .reduce(f64::max)
        .map(|percent| percent.round().clamp(0.0, 100.0) as u32)
}

fn memory_used_bytes(items: &[(String, f64)], luid_tag: &str) -> Option<u64> {
    let mut matched = false;
    let mut total = 0.0;
    for (instance, value) in items {
        if belongs_to(instance, luid_tag) && value.is_finite() {
            matched = true;
            total += value.max(0.0);
        }
    }
    matched.then_some(total as u64)
}

/// 驱动不提供温度时上报 0。
fn temperature_celsius(deci_celsius: u32) -> Option<u32> {
    (deci_celsius > 0).then(|| (deci_celsius + 5) / 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RADEON: &str = "luid_0x00000000_0x00014265";

    fn items(values: &[(&str, f64)]) -> Vec<(String, f64)> {
        values
            .iter()
            .map(|(name, value)| ((*name).to_owned(), *value))
            .collect()
    }

    #[test]
    fn formats_the_luid_the_way_pdh_instance_names_do() {
        let luid = Luid {
            low: 0x0001_4265,
            high: 0,
        };
        assert_eq!(luid_tag(luid), RADEON);
        assert!(belongs_to(
            "pid_6284_luid_0x00000000_0x00014265_phys_0_eng_7_engtype_Video Codec Engine",
            RADEON
        ));
        // PDH 可能以大写十六进制给出实例名。
        assert!(belongs_to(
            "luid_0x00000000_0x0001426A_phys_0",
            "luid_0x00000000_0x0001426a"
        ));
        assert!(!belongs_to("luid_0x00000000_0x00016657_phys_0", RADEON));
    }

    #[test]
    fn utilization_sums_processes_per_engine_and_takes_the_busiest_engine() {
        let items = items(&[
            (
                "pid_1_luid_0x00000000_0x00014265_phys_0_eng_0_engtype_3D",
                30.4,
            ),
            (
                "pid_2_luid_0x00000000_0x00014265_phys_0_eng_0_engtype_3D",
                12.0,
            ),
            (
                "pid_2_luid_0x00000000_0x00014265_phys_0_eng_7_engtype_Video Codec Engine",
                25.0,
            ),
            // 另一块适配器不计入。
            (
                "pid_3_luid_0x00000000_0x00016657_phys_0_eng_0_engtype_3D",
                99.0,
            ),
        ]);
        assert_eq!(utilization_percent(&items, RADEON), Some(42));
        assert_eq!(
            utilization_percent(&items, "luid_0x00000000_0x00016657"),
            Some(99)
        );
        assert_eq!(
            utilization_percent(&items, "luid_0x00000000_0x0000ffff"),
            None
        );

        let saturated = super::tests::items(&[
            (
                "pid_1_luid_0x00000000_0x00014265_phys_0_eng_0_engtype_3D",
                80.0,
            ),
            (
                "pid_2_luid_0x00000000_0x00014265_phys_0_eng_0_engtype_3D",
                70.0,
            ),
        ]);
        assert_eq!(utilization_percent(&saturated, RADEON), Some(100));
    }

    #[test]
    fn memory_usage_sums_the_matching_adapter_only() {
        let items = items(&[
            ("luid_0x00000000_0x00014265_phys_0", 4_457_684_992.0),
            ("luid_0x00000000_0x00016657_phys_0", 8_192.0),
        ]);
        assert_eq!(memory_used_bytes(&items, RADEON), Some(4_457_684_992));
        assert_eq!(
            memory_used_bytes(&items, "luid_0x00000000_0x0000ffff"),
            None
        );
    }

    #[test]
    fn selects_hardware_adapters_by_dedicated_then_shared_memory() {
        // 渲染 + 显示的硬件适配器；软件设备；只能显示不能渲染的适配器。
        assert!(is_hardware_renderer(0b011));
        assert!(!is_hardware_renderer(0b101));
        assert!(!is_hardware_renderer(0b010));

        let integrated = AdapterCandidate {
            index: 0,
            dedicated_bytes: 128 << 20,
            shared_bytes: 16 << 30,
        };
        let discrete = AdapterCandidate {
            index: 1,
            dedicated_bytes: 8 << 30,
            shared_bytes: 16 << 30,
        };
        assert_eq!(select_adapter(&[]), None);
        assert_eq!(select_adapter(&[integrated]), Some(integrated));
        assert_eq!(select_adapter(&[integrated, discrete]), Some(discrete));
        assert_eq!(select_adapter(&[discrete, integrated]), Some(discrete));
    }

    /// 在当前机器上真实走一遍系统调用；没有可用适配器的环境（如无显卡的 CI）直接跳过断言。
    #[test]
    fn queries_the_local_adapter_without_inconsistent_results() {
        let Some(gpu) = WindowsGpu::load() else {
            return;
        };
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let sample = gpu.sample();
        println!(
            "adapter {} dedicated={} percent={:?} temperature={:?} memory={:?}/{:?}",
            gpu.luid_tag,
            gpu.use_dedicated_memory,
            sample.percent,
            sample.temperature,
            sample.memory_used_bytes,
            sample.memory_total_bytes
        );
        assert!(sample.percent.is_none_or(|percent| percent <= 100));
        assert!(
            sample
                .temperature
                .is_none_or(|temperature| temperature < 150)
        );
        assert!(sample.memory_total_bytes.is_some_and(|total| total > 0));
        assert!(sample.power_w.is_none() && sample.fan_percent.is_none());
    }

    #[test]
    fn temperature_is_rounded_from_tenths_and_zero_means_unsupported() {
        assert_eq!(temperature_celsius(0), None);
        assert_eq!(temperature_celsius(344), Some(34));
        assert_eq!(temperature_celsius(345), Some(35));
        assert_eq!(temperature_celsius(1000), Some(100));
    }
}
