use std::{ffi::c_void, mem::size_of, ptr, thread, time::Duration};

use libloading::Library;

use crate::{io_metrics::IoMetrics, temperature, win::system_dll};

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub cpu_percent: f32,
    pub cpu_temperature: Option<u32>,
    pub cpu_p_core_loads: Vec<f32>,
    pub cpu_e_core_loads: Vec<f32>,
    pub cpu_p_mhz: Option<u32>,
    pub cpu_e_mhz: Option<u32>,
    pub memory_used_gib: f32,
    pub memory_total_gib: f32,
    pub gpu_percent: Option<u32>,
    pub gpu_temperature: Option<u32>,
    pub gpu_power_w: Option<f32>,
    pub gpu_power_limit_w: Option<f32>,
    pub gpu_memory_used_bytes: Option<u64>,
    pub gpu_memory_total_bytes: Option<u64>,
    pub gpu_graphics_clock_mhz: Option<u32>,
    pub gpu_memory_clock_mhz: Option<u32>,
    pub gpu_performance_state: Option<u32>,
    pub gpu_fan_percent: Option<u32>,
    pub network_down_mib_s: Option<f32>,
    pub network_up_mib_s: Option<f32>,
    pub disk_read_mib_s: Option<f32>,
    pub disk_write_mib_s: Option<f32>,
}

pub struct Monitor {
    cpu_topology: Vec<CpuThread>,
    cpu_count: usize,
    previous_times: Option<Vec<ProcessorTimes>>,
    power: Option<PowerApi>,
    nvml: Option<Nvml>,
    io: IoMetrics,
}

impl Monitor {
    pub fn new() -> Self {
        let cpu_topology = cpu_topology();
        let cpu_count = if cpu_topology.is_empty() {
            active_processor_count()
        } else {
            cpu_topology.len()
        };
        Self {
            cpu_topology,
            cpu_count,
            previous_times: processor_times(cpu_count),
            power: PowerApi::load(),
            nvml: Nvml::load(),
            io: IoMetrics::new(),
        }
    }

    pub fn prime(&self) {
        thread::sleep(Duration::from_millis(250));
    }

    pub fn sample(&mut self) -> Snapshot {
        let logical_loads = self.sample_cpu_loads();
        let cpu_percent = average(&logical_loads).unwrap_or(0.0);
        let (cpu_p_core_loads, cpu_e_core_loads) =
            split_core_values(physical_core_averages(&self.cpu_topology, &logical_loads));

        let current_mhz = self
            .power
            .as_ref()
            .and_then(|power| power.processor_mhz(self.cpu_count));
        let (cpu_p_mhz, cpu_e_mhz) = current_mhz
            .map(|mhz| split_core_values(physical_core_averages(&self.cpu_topology, &mhz)))
            .map(|(p, e)| (average_u32(&p), average_u32(&e)))
            .unwrap_or_default();

        let memory = memory_usage().unwrap_or_default();
        let gpu = self.nvml.as_ref().map(Nvml::sample).unwrap_or_default();
        let io = self.io.sample();

        Snapshot {
            cpu_percent,
            cpu_temperature: temperature::service_sample().ok(),
            cpu_p_core_loads,
            cpu_e_core_loads,
            cpu_p_mhz,
            cpu_e_mhz,
            memory_used_gib: memory.0,
            memory_total_gib: memory.1,
            gpu_percent: gpu.percent,
            gpu_temperature: gpu.temperature,
            gpu_power_w: gpu.power_w,
            gpu_power_limit_w: gpu.power_limit_w,
            gpu_memory_used_bytes: gpu.memory_used_bytes,
            gpu_memory_total_bytes: gpu.memory_total_bytes,
            gpu_graphics_clock_mhz: gpu.graphics_clock_mhz,
            gpu_memory_clock_mhz: gpu.memory_clock_mhz,
            gpu_performance_state: gpu.performance_state,
            gpu_fan_percent: gpu.fan_percent,
            network_down_mib_s: io.network_down_mib_s,
            network_up_mib_s: io.network_up_mib_s,
            disk_read_mib_s: io.disk_read_mib_s,
            disk_write_mib_s: io.disk_write_mib_s,
        }
    }

    fn sample_cpu_loads(&mut self) -> Vec<f32> {
        let Some(previous) = self.previous_times.as_ref() else {
            self.previous_times = processor_times(self.cpu_count);
            return Vec::new();
        };
        let Some(current) = processor_times(previous.len()) else {
            return Vec::new();
        };
        let loads = previous
            .iter()
            .zip(&current)
            .map(|(previous, current)| current.usage_since(*previous))
            .collect();
        self.previous_times = Some(current);
        loads
    }
}

impl Default for Monitor {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CpuThread {
    group: u16,
    logical_index: u8,
    core_index: u8,
    efficiency_class: u8,
}

#[derive(Clone, Copy)]
struct CoreValue {
    efficiency_class: u8,
    value: f32,
}

#[derive(Clone, Copy)]
struct CoreAccumulator {
    group: u16,
    core_index: u8,
    efficiency_class: u8,
    total: f64,
    count: u32,
}

fn physical_core_averages(topology: &[CpuThread], values: &[f32]) -> Vec<CoreValue> {
    let mut cores: Vec<CoreAccumulator> = Vec::new();
    for (thread, value) in topology.iter().zip(values) {
        if let Some(core) = cores
            .iter_mut()
            .find(|core| core.group == thread.group && core.core_index == thread.core_index)
        {
            core.total += f64::from(*value);
            core.count += 1;
        } else {
            cores.push(CoreAccumulator {
                group: thread.group,
                core_index: thread.core_index,
                efficiency_class: thread.efficiency_class,
                total: f64::from(*value),
                count: 1,
            });
        }
    }
    cores
        .into_iter()
        .map(|core| CoreValue {
            efficiency_class: core.efficiency_class,
            value: (core.total / f64::from(core.count)) as f32,
        })
        .collect()
}

fn split_core_values(values: Vec<CoreValue>) -> (Vec<f32>, Vec<f32>) {
    let Some(minimum) = values.iter().map(|core| core.efficiency_class).min() else {
        return (Vec::new(), Vec::new());
    };
    let maximum = values
        .iter()
        .map(|core| core.efficiency_class)
        .max()
        .unwrap_or(minimum);
    let mut performance = Vec::new();
    let mut efficiency = Vec::new();
    for core in values {
        if core.efficiency_class == maximum {
            performance.push(core.value);
        } else if core.efficiency_class == minimum {
            efficiency.push(core.value);
        }
    }
    (performance, efficiency)
}

fn average(values: &[f32]) -> Option<f32> {
    (!values.is_empty()).then(|| values.iter().sum::<f32>() / values.len() as f32)
}

fn average_u32(values: &[f32]) -> Option<u32> {
    average(values).map(|value| value.round() as u32)
}

fn cpu_topology() -> Vec<CpuThread> {
    let mut byte_length = 0;
    unsafe {
        GetSystemCpuSetInformation(ptr::null_mut(), 0, &mut byte_length, ptr::null_mut(), 0);
    }
    if byte_length == 0 {
        return Vec::new();
    }

    let mut buffer = vec![0_u8; byte_length as usize];
    let success = unsafe {
        GetSystemCpuSetInformation(
            buffer.as_mut_ptr().cast(),
            byte_length,
            &mut byte_length,
            ptr::null_mut(),
            0,
        )
    };
    if success == 0 {
        return Vec::new();
    }

    let mut threads = Vec::new();
    let mut offset = 0;
    let byte_length = byte_length as usize;
    while offset + 19 <= byte_length {
        let size = u32::from_ne_bytes(buffer[offset..offset + 4].try_into().unwrap()) as usize;
        if size < 19 || offset + size > byte_length {
            break;
        }
        let information_type =
            u32::from_ne_bytes(buffer[offset + 4..offset + 8].try_into().unwrap());
        if information_type == 0 {
            threads.push(CpuThread {
                group: u16::from_ne_bytes(buffer[offset + 12..offset + 14].try_into().unwrap()),
                logical_index: buffer[offset + 14],
                core_index: buffer[offset + 15],
                efficiency_class: buffer[offset + 18],
            });
        }
        offset += size;
    }
    threads.sort_unstable_by_key(|thread| (thread.group, thread.logical_index));
    threads
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ProcessorTimes {
    idle: i64,
    kernel: i64,
    user: i64,
    reserved: [i64; 2],
    interrupt_count: u32,
}

impl ProcessorTimes {
    fn usage_since(self, previous: Self) -> f32 {
        let idle = self.idle.saturating_sub(previous.idle).max(0) as f64;
        let total = self.kernel.saturating_sub(previous.kernel).max(0) as f64
            + self.user.saturating_sub(previous.user).max(0) as f64;
        if total == 0.0 {
            0.0
        } else {
            (100.0 * (1.0 - idle / total)).clamp(0.0, 100.0) as f32
        }
    }
}

fn processor_times(cpu_count: usize) -> Option<Vec<ProcessorTimes>> {
    if cpu_count == 0 {
        return None;
    }
    let mut times = vec![ProcessorTimes::default(); cpu_count];
    let byte_length = (times.len() * size_of::<ProcessorTimes>()) as u32;
    let status = unsafe {
        NtQuerySystemInformation(
            SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION,
            times.as_mut_ptr().cast(),
            byte_length,
            ptr::null_mut(),
        )
    };
    (status >= 0).then_some(times)
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ProcessorPowerInformation {
    number: u32,
    max_mhz: u32,
    current_mhz: u32,
    mhz_limit: u32,
    max_idle_state: u32,
    current_idle_state: u32,
}

type CallNtPowerInformation =
    unsafe extern "system" fn(i32, *const c_void, u32, *mut c_void, u32) -> i32;

struct PowerApi {
    _library: Library,
    call: CallNtPowerInformation,
}

impl PowerApi {
    fn load() -> Option<Self> {
        unsafe {
            let library = Library::new(system_dll("powrprof.dll").ok()?).ok()?;
            let call = *library
                .get::<CallNtPowerInformation>(b"CallNtPowerInformation\0")
                .ok()?;
            Some(Self {
                _library: library,
                call,
            })
        }
    }

    fn processor_mhz(&self, cpu_count: usize) -> Option<Vec<f32>> {
        if cpu_count == 0 {
            return None;
        }
        let mut information = vec![ProcessorPowerInformation::default(); cpu_count];
        let status = unsafe {
            (self.call)(
                PROCESSOR_INFORMATION,
                ptr::null(),
                0,
                information.as_mut_ptr().cast(),
                (information.len() * size_of::<ProcessorPowerInformation>()) as u32,
            )
        };
        (status >= 0).then(|| {
            information
                .into_iter()
                .map(|processor| processor.current_mhz as f32)
                .collect()
        })
    }
}

fn active_processor_count() -> usize {
    unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) as usize }
}

#[repr(C)]
struct MemoryStatus {
    length: u32,
    memory_load: u32,
    total_physical: u64,
    available_physical: u64,
    total_page_file: u64,
    available_page_file: u64,
    total_virtual: u64,
    available_virtual: u64,
    available_extended_virtual: u64,
}

impl Default for MemoryStatus {
    fn default() -> Self {
        Self {
            length: size_of::<Self>() as u32,
            memory_load: 0,
            total_physical: 0,
            available_physical: 0,
            total_page_file: 0,
            available_page_file: 0,
            total_virtual: 0,
            available_virtual: 0,
            available_extended_virtual: 0,
        }
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetActiveProcessorCount(group_number: u16) -> u32;
    fn GetSystemCpuSetInformation(
        information: *mut c_void,
        buffer_length: u32,
        returned_length: *mut u32,
        process: *mut c_void,
        flags: u32,
    ) -> i32;
    fn GlobalMemoryStatusEx(status: *mut MemoryStatus) -> i32;
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQuerySystemInformation(
        information_class: u32,
        information: *mut c_void,
        information_length: u32,
        return_length: *mut u32,
    ) -> i32;
}

const ALL_PROCESSOR_GROUPS: u16 = 0xffff;
const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION: u32 = 8;
const PROCESSOR_INFORMATION: i32 = 11;

fn memory_usage() -> Option<(f32, f32)> {
    let mut status = MemoryStatus::default();
    let success = unsafe { GlobalMemoryStatusEx(&mut status) };
    if success == 0 {
        return None;
    }
    const GIB: f32 = 1024.0 * 1024.0 * 1024.0;
    let used = status
        .total_physical
        .saturating_sub(status.available_physical);
    Some((used as f32 / GIB, status.total_physical as f32 / GIB))
}

type NvmlDevice = *mut c_void;
type NvmlInit = unsafe extern "C" fn() -> u32;
type NvmlShutdown = unsafe extern "C" fn() -> u32;
type NvmlGetDevice = unsafe extern "C" fn(u32, *mut NvmlDevice) -> u32;
type NvmlGetTemperature = unsafe extern "C" fn(NvmlDevice, u32, *mut u32) -> u32;
type NvmlGetUtilization = unsafe extern "C" fn(NvmlDevice, *mut NvmlUtilization) -> u32;
type NvmlGetU32 = unsafe extern "C" fn(NvmlDevice, *mut u32) -> u32;
type NvmlGetClock = unsafe extern "C" fn(NvmlDevice, u32, *mut u32) -> u32;
type NvmlGetMemory = unsafe extern "C" fn(NvmlDevice, *mut NvmlMemory) -> u32;

#[repr(C)]
#[derive(Default)]
struct NvmlUtilization {
    gpu: u32,
    memory: u32,
}

#[repr(C)]
#[derive(Default)]
struct NvmlMemory {
    total: u64,
    free: u64,
    used: u64,
}

#[derive(Default)]
struct GpuSample {
    percent: Option<u32>,
    temperature: Option<u32>,
    power_w: Option<f32>,
    power_limit_w: Option<f32>,
    memory_used_bytes: Option<u64>,
    memory_total_bytes: Option<u64>,
    graphics_clock_mhz: Option<u32>,
    memory_clock_mhz: Option<u32>,
    performance_state: Option<u32>,
    fan_percent: Option<u32>,
}

struct Nvml {
    _library: Library,
    device: NvmlDevice,
    shutdown: NvmlShutdown,
    get_temperature: NvmlGetTemperature,
    get_utilization: NvmlGetUtilization,
    get_power_usage: Option<NvmlGetU32>,
    get_power_limit: Option<NvmlGetU32>,
    get_memory: Option<NvmlGetMemory>,
    get_clock: Option<NvmlGetClock>,
    get_performance_state: Option<NvmlGetU32>,
    get_fan_speed: Option<NvmlGetU32>,
}

impl Nvml {
    fn load() -> Option<Self> {
        unsafe {
            let library = Library::new(system_dll("nvml.dll").ok()?).ok()?;
            let init = *library.get::<NvmlInit>(b"nvmlInit_v2\0").ok()?;
            let shutdown = *library.get::<NvmlShutdown>(b"nvmlShutdown\0").ok()?;
            let get_device = *library
                .get::<NvmlGetDevice>(b"nvmlDeviceGetHandleByIndex_v2\0")
                .ok()?;
            let get_temperature = *library
                .get::<NvmlGetTemperature>(b"nvmlDeviceGetTemperature\0")
                .ok()?;
            let get_utilization = *library
                .get::<NvmlGetUtilization>(b"nvmlDeviceGetUtilizationRates\0")
                .ok()?;
            let get_power_usage = optional_symbol(&library, b"nvmlDeviceGetPowerUsage\0");
            let get_power_limit = optional_symbol(&library, b"nvmlDeviceGetEnforcedPowerLimit\0");
            let get_memory = optional_symbol(&library, b"nvmlDeviceGetMemoryInfo\0");
            let get_clock = optional_symbol(&library, b"nvmlDeviceGetClockInfo\0");
            let get_performance_state =
                optional_symbol(&library, b"nvmlDeviceGetPerformanceState\0");
            let get_fan_speed = optional_symbol(&library, b"nvmlDeviceGetFanSpeed\0");

            if init() != 0 {
                return None;
            }
            let mut device = ptr::null_mut();
            if get_device(0, &mut device) != 0 {
                shutdown();
                return None;
            }
            Some(Self {
                _library: library,
                device,
                shutdown,
                get_temperature,
                get_utilization,
                get_power_usage,
                get_power_limit,
                get_memory,
                get_clock,
                get_performance_state,
                get_fan_speed,
            })
        }
    }

    fn sample(&self) -> GpuSample {
        unsafe {
            let mut utilization = NvmlUtilization::default();
            let percent = ((self.get_utilization)(self.device, &mut utilization) == 0)
                .then_some(utilization.gpu);
            let mut temperature = 0;
            let temperature = ((self.get_temperature)(self.device, 0, &mut temperature) == 0)
                .then_some(temperature);

            let power_w = self
                .get_power_usage
                .and_then(|function| query_u32(function, self.device))
                .map(|milliwatts| milliwatts as f32 / 1000.0);
            let power_limit_w = self
                .get_power_limit
                .and_then(|function| query_u32(function, self.device))
                .map(|milliwatts| milliwatts as f32 / 1000.0);

            let memory = self.get_memory.and_then(|function| {
                let mut memory = NvmlMemory::default();
                (function(self.device, &mut memory) == 0).then_some(memory)
            });
            let graphics_clock_mhz = self
                .get_clock
                .and_then(|function| query_clock(function, self.device, NVML_CLOCK_GRAPHICS));
            let memory_clock_mhz = self
                .get_clock
                .and_then(|function| query_clock(function, self.device, NVML_CLOCK_MEMORY));
            let performance_state = self
                .get_performance_state
                .and_then(|function| query_u32(function, self.device));
            let fan_percent = self
                .get_fan_speed
                .and_then(|function| query_u32(function, self.device));

            GpuSample {
                percent,
                temperature,
                power_w,
                power_limit_w,
                memory_used_bytes: memory.as_ref().map(|memory| memory.used),
                memory_total_bytes: memory.as_ref().map(|memory| memory.total),
                graphics_clock_mhz,
                memory_clock_mhz,
                performance_state,
                fan_percent,
            }
        }
    }
}

unsafe fn optional_symbol<T: Copy>(library: &Library, name: &[u8]) -> Option<T> {
    unsafe { library.get::<T>(name).ok().map(|symbol| *symbol) }
}

unsafe fn query_u32(function: NvmlGetU32, device: NvmlDevice) -> Option<u32> {
    let mut value = 0;
    unsafe { (function(device, &mut value) == 0).then_some(value) }
}

unsafe fn query_clock(function: NvmlGetClock, device: NvmlDevice, clock: u32) -> Option<u32> {
    let mut value = 0;
    unsafe { (function(device, clock, &mut value) == 0).then_some(value) }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        unsafe { (self.shutdown)() };
    }
}

const NVML_CLOCK_GRAPHICS: u32 = 0;
const NVML_CLOCK_MEMORY: u32 = 2;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_cores_average_sibling_threads_and_split_by_efficiency() {
        let topology = [
            CpuThread {
                group: 0,
                logical_index: 0,
                core_index: 0,
                efficiency_class: 8,
            },
            CpuThread {
                group: 0,
                logical_index: 1,
                core_index: 0,
                efficiency_class: 8,
            },
            CpuThread {
                group: 0,
                logical_index: 2,
                core_index: 1,
                efficiency_class: 0,
            },
        ];
        let cores = physical_core_averages(&topology, &[20.0, 40.0, 60.0]);
        let (performance, efficiency) = split_core_values(cores);
        assert_eq!(performance, [30.0]);
        assert_eq!(efficiency, [60.0]);
    }

    #[test]
    fn homogeneous_cpu_is_reported_once() {
        let cores = vec![
            CoreValue {
                efficiency_class: 0,
                value: 10.0,
            },
            CoreValue {
                efficiency_class: 0,
                value: 20.0,
            },
        ];
        let (performance, efficiency) = split_core_values(cores);
        assert_eq!(performance, [10.0, 20.0]);
        assert!(efficiency.is_empty());
    }
}
