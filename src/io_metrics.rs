use std::{
    collections::HashMap,
    ffi::c_void,
    ptr, slice,
    time::{Duration, Instant},
};

use libloading::Library;

use crate::win::{system_dll, wide};

const MIB: f64 = 1024.0 * 1024.0;
const IF_OPER_STATUS_UP: u32 = 1;
const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;

#[derive(Clone, Copy, Debug, Default)]
pub struct IoSnapshot {
    pub network_down_mib_s: Option<f32>,
    pub network_up_mib_s: Option<f32>,
    pub disk_read_mib_s: Option<f32>,
    pub disk_write_mib_s: Option<f32>,
}

pub struct IoMetrics {
    network: Option<NetworkSampler>,
    disk: Option<DiskSampler>,
}

impl IoMetrics {
    pub fn new() -> Self {
        Self {
            network: NetworkSampler::load(),
            disk: DiskSampler::load(),
        }
    }

    pub fn sample(&mut self) -> IoSnapshot {
        let (network_down_mib_s, network_up_mib_s) = self
            .network
            .as_mut()
            .map(NetworkSampler::sample)
            .unwrap_or((None, None));
        let (disk_read_mib_s, disk_write_mib_s) = self
            .disk
            .as_ref()
            .map(DiskSampler::sample)
            .unwrap_or((None, None));

        IoSnapshot {
            network_down_mib_s,
            network_up_mib_s,
            disk_read_mib_s,
            disk_write_mib_s,
        }
    }
}

impl Default for IoMetrics {
    fn default() -> Self {
        Self::new()
    }
}

type GetIfTable2 = unsafe extern "system" fn(*mut *mut MibIfTable2) -> u32;
type FreeMibTable = unsafe extern "system" fn(*mut c_void);

// The application has a fixed x64 Windows target. Keep the native row stride while naming only
// the fields used here; the layout test below guards the SDK-defined 1,352-byte ABI.
#[repr(C, align(8))]
struct MibIfRow2 {
    interface_luid: u64,
    _before_type: [u8; 1120],
    interface_type: u32,
    _after_type: [u8; 20],
    interface_flags: u8,
    _status_padding: [u8; 3],
    oper_status: u32,
    _before_in_octets: [u8; 48],
    in_octets: u64,
    _before_out_octets: [u8; 64],
    out_octets: u64,
    _tail: [u8; 64],
}

#[repr(C)]
struct MibIfTable2 {
    num_entries: u32,
    table: [MibIfRow2; 1],
}

struct IpHelper {
    _library: Library,
    get_if_table: GetIfTable2,
    free_table: FreeMibTable,
}

impl IpHelper {
    fn load() -> Option<Self> {
        unsafe {
            let library = Library::new(system_dll("iphlpapi.dll").ok()?).ok()?;
            let get_if_table = *library.get::<GetIfTable2>(b"GetIfTable2\0").ok()?;
            let free_table = *library.get::<FreeMibTable>(b"FreeMibTable\0").ok()?;
            Some(Self {
                _library: library,
                get_if_table,
                free_table,
            })
        }
    }

    fn counters(&self) -> Option<HashMap<u64, InterfaceCounters>> {
        let mut table = ptr::null_mut();
        let status = unsafe { (self.get_if_table)(&mut table) };
        if status != 0 || table.is_null() {
            return None;
        }

        let counters = unsafe {
            let rows =
                slice::from_raw_parts((*table).table.as_ptr(), (*table).num_entries as usize);
            rows.iter()
                .filter(|row| {
                    row.oper_status == IF_OPER_STATUS_UP
                        && row.interface_type != IF_TYPE_SOFTWARE_LOOPBACK
                        && row.interface_flags & 1 != 0
                })
                .map(|row| {
                    (
                        row.interface_luid,
                        InterfaceCounters {
                            received: row.in_octets,
                            sent: row.out_octets,
                        },
                    )
                })
                .collect()
        };
        unsafe { (self.free_table)(table.cast()) };
        Some(counters)
    }
}

#[derive(Clone, Copy)]
struct InterfaceCounters {
    received: u64,
    sent: u64,
}

struct NetworkSampler {
    api: IpHelper,
    previous: Option<HashMap<u64, InterfaceCounters>>,
    previous_at: Option<Instant>,
}

impl NetworkSampler {
    fn load() -> Option<Self> {
        let api = IpHelper::load()?;
        let previous = api.counters();
        let previous_at = previous.as_ref().map(|_| Instant::now());
        Some(Self {
            api,
            previous,
            previous_at,
        })
    }

    fn sample(&mut self) -> (Option<f32>, Option<f32>) {
        let current = match self.api.counters() {
            Some(current) => current,
            None => return (None, None),
        };
        let now = Instant::now();
        let rates = match (&self.previous, self.previous_at) {
            (Some(previous), Some(previous_at)) => {
                network_rates(previous, &current, now.duration_since(previous_at))
            }
            _ => (None, None),
        };
        self.previous = Some(current);
        self.previous_at = Some(now);
        rates
    }
}

fn network_rates(
    previous: &HashMap<u64, InterfaceCounters>,
    current: &HashMap<u64, InterfaceCounters>,
    elapsed: Duration,
) -> (Option<f32>, Option<f32>) {
    let mut received = 0_u64;
    let mut sent = 0_u64;
    for (interface, current) in current {
        let Some(previous) = previous.get(interface) else {
            continue;
        };
        received = received.saturating_add(current.received.saturating_sub(previous.received));
        sent = sent.saturating_add(current.sent.saturating_sub(previous.sent));
    }
    (
        mib_per_second(received, elapsed),
        mib_per_second(sent, elapsed),
    )
}

type PdhQuery = *mut c_void;
type PdhCounter = *mut c_void;
type PdhOpenQuery = unsafe extern "system" fn(*const u16, usize, *mut PdhQuery) -> i32;
type PdhAddEnglishCounter =
    unsafe extern "system" fn(PdhQuery, *const u16, usize, *mut PdhCounter) -> i32;
type PdhCollectQueryData = unsafe extern "system" fn(PdhQuery) -> i32;
type PdhGetFormattedCounterValue =
    unsafe extern "system" fn(PdhCounter, u32, *mut u32, *mut PdhFormattedValue) -> i32;
type PdhCloseQuery = unsafe extern "system" fn(PdhQuery) -> i32;

const PDH_FMT_DOUBLE: u32 = 0x0000_0200;
const PDH_CSTATUS_VALID_DATA: u32 = 0;
const PDH_CSTATUS_NEW_DATA: u32 = 1;

#[repr(C)]
union PdhValue {
    double_value: f64,
}

#[repr(C)]
struct PdhFormattedValue {
    status: u32,
    value: PdhValue,
}

impl Default for PdhFormattedValue {
    fn default() -> Self {
        Self {
            status: 0,
            value: PdhValue { double_value: 0.0 },
        }
    }
}

struct DiskSampler {
    _library: Library,
    query: PdhQuery,
    read_counter: Option<PdhCounter>,
    write_counter: Option<PdhCounter>,
    collect: PdhCollectQueryData,
    formatted_value: PdhGetFormattedCounterValue,
    close: PdhCloseQuery,
}

impl DiskSampler {
    fn load() -> Option<Self> {
        unsafe {
            let library = Library::new(system_dll("pdh.dll").ok()?).ok()?;
            let open = *library.get::<PdhOpenQuery>(b"PdhOpenQueryW\0").ok()?;
            let add = *library
                .get::<PdhAddEnglishCounter>(b"PdhAddEnglishCounterW\0")
                .ok()?;
            let collect = *library
                .get::<PdhCollectQueryData>(b"PdhCollectQueryData\0")
                .ok()?;
            let formatted_value = *library
                .get::<PdhGetFormattedCounterValue>(b"PdhGetFormattedCounterValue\0")
                .ok()?;
            let close = *library.get::<PdhCloseQuery>(b"PdhCloseQuery\0").ok()?;

            let mut query = ptr::null_mut();
            if open(ptr::null(), 0, &mut query) != 0 || query.is_null() {
                return None;
            }

            let read_counter =
                add_counter(add, query, r"\PhysicalDisk(_Total)\Disk Read Bytes/sec");
            let write_counter =
                add_counter(add, query, r"\PhysicalDisk(_Total)\Disk Write Bytes/sec");
            if read_counter.is_none() && write_counter.is_none() {
                close(query);
                return None;
            }
            if collect(query) != 0 {
                close(query);
                return None;
            }

            // The first collection primes rate counters. The monitor's normal delay supplies the
            // second sample used by the first displayed value.
            Some(Self {
                _library: library,
                query,
                read_counter,
                write_counter,
                collect,
                formatted_value,
                close,
            })
        }
    }

    fn sample(&self) -> (Option<f32>, Option<f32>) {
        if unsafe { (self.collect)(self.query) } != 0 {
            return (None, None);
        }
        (
            self.read_counter.and_then(|counter| self.value(counter)),
            self.write_counter.and_then(|counter| self.value(counter)),
        )
    }

    fn value(&self, counter: PdhCounter) -> Option<f32> {
        let mut value = PdhFormattedValue::default();
        let status =
            unsafe { (self.formatted_value)(counter, PDH_FMT_DOUBLE, ptr::null_mut(), &mut value) };
        if status != 0 || !matches!(value.status, PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA) {
            return None;
        }
        let bytes_per_second = unsafe { value.value.double_value };
        let mib_per_second = (bytes_per_second / MIB) as f32;
        (mib_per_second.is_finite() && mib_per_second >= 0.0).then_some(mib_per_second)
    }
}

impl Drop for DiskSampler {
    fn drop(&mut self) {
        unsafe {
            (self.close)(self.query);
        }
    }
}

fn add_counter(add: PdhAddEnglishCounter, query: PdhQuery, path: &str) -> Option<PdhCounter> {
    let path = wide(path);
    let mut counter = ptr::null_mut();
    (unsafe { add(query, path.as_ptr(), 0, &mut counter) } == 0 && !counter.is_null())
        .then_some(counter)
}

fn mib_per_second(bytes: u64, elapsed: Duration) -> Option<f32> {
    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        return None;
    }
    let rate = (bytes as f64 / MIB / seconds) as f32;
    rate.is_finite().then_some(rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mib_rate_uses_the_actual_interval() {
        assert_eq!(
            mib_per_second(1024 * 1024, Duration::from_millis(500)),
            Some(2.0)
        );
        assert_eq!(mib_per_second(1, Duration::ZERO), None);
    }

    #[test]
    fn network_delta_ignores_new_removed_and_reset_interfaces() {
        let previous = HashMap::from([
            (
                1,
                InterfaceCounters {
                    received: 100,
                    sent: 200,
                },
            ),
            (
                2,
                InterfaceCounters {
                    received: 900,
                    sent: 900,
                },
            ),
            (
                4,
                InterfaceCounters {
                    received: 500,
                    sent: 500,
                },
            ),
        ]);
        let current = HashMap::from([
            (
                1,
                InterfaceCounters {
                    received: 100 + 1024 * 1024,
                    sent: 200 + 2 * 1024 * 1024,
                },
            ),
            (
                3,
                InterfaceCounters {
                    received: u64::MAX,
                    sent: u64::MAX,
                },
            ),
            (
                4,
                InterfaceCounters {
                    received: 10,
                    sent: 10,
                },
            ),
        ]);

        assert_eq!(
            network_rates(&previous, &current, Duration::from_secs(1)),
            (Some(1.0), Some(2.0))
        );
    }

    #[test]
    fn mib_if_row_matches_the_x64_windows_layout() {
        assert_eq!(std::mem::size_of::<MibIfRow2>(), 1352);
        assert_eq!(std::mem::size_of::<MibIfTable2>(), 1360);
    }
}
