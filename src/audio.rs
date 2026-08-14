use std::{f32::consts::PI, ffi::c_void, ptr, time::Duration};

use anyhow::{Result, bail, ensure};
use windows_sys::{
    Win32::{
        Media::Audio::{
            AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
            AUDCLNT_STREAMFLAGS_NOPERSIST, MMDeviceEnumerator, WAVEFORMATEX, eMultimedia, eRender,
        },
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_DISABLE_OLE1DDE, COINIT_MULTITHREADED, CoCreateInstance,
            CoInitializeEx, CoTaskMemFree, CoUninitialize,
        },
    },
    core::{GUID, HRESULT},
};

const IID_IMM_DEVICE_ENUMERATOR: GUID = GUID::from_u128(0xa95664d2_9614_4f35_a746_de8db63617e6);
const IID_I_AUDIO_CLIENT: GUID = GUID::from_u128(0x1cb9ad4c_dbfa_4c32_b178_c2f568a703b2);
const IID_I_AUDIO_CAPTURE_CLIENT: GUID = GUID::from_u128(0xc8adbd64_e71e_48a0_a4de_185c395cd317);

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xfffe;
const FFT_SIZE: usize = 1024;
pub const BAND_COUNT: usize = 16;
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(200);

pub struct OutputSpectrum {
    capture: ComPtr,
    client: ComPtr,
    format: SampleFormat,
    channels: usize,
    block_align: usize,
    sample_rate: u32,
    samples: [f32; FFT_SIZE],
    cursor: usize,
    filled: usize,
    levels: [f32; BAND_COUNT],
    _apartment: ComApartment,
}

impl OutputSpectrum {
    pub fn open() -> Result<Self> {
        let apartment = ComApartment::initialize()?;
        let enumerator = create_enumerator()?;
        let device = default_output_device(&enumerator)?;

        let mut client = ptr::null_mut();
        let device_interface = device.as_interface::<DeviceVTable>();
        check_hresult(
            unsafe {
                ((*(*device_interface).vtable).activate)(
                    device.as_raw(),
                    &IID_I_AUDIO_CLIENT,
                    CLSCTX_INPROC_SERVER,
                    ptr::null(),
                    &mut client,
                )
            },
            "无法创建 WASAPI 音频客户端",
        )?;
        let client = ComPtr::new(client, "WASAPI 音频客户端")?;
        let client_interface = client.as_interface::<AudioClientVTable>();

        let mut wave_format = ptr::null_mut();
        check_hresult(
            unsafe {
                ((*(*client_interface).vtable).get_mix_format)(client.as_raw(), &mut wave_format)
            },
            "无法取得默认输出设备的混音格式",
        )?;
        ensure!(!wave_format.is_null(), "WASAPI 返回了空混音格式");
        let format_info = unsafe { FormatInfo::from_wave_format(wave_format) };
        let initialize_result = unsafe {
            ((*(*client_interface).vtable).initialize)(
                client.as_raw(),
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_NOPERSIST,
                0,
                0,
                wave_format,
                ptr::null(),
            )
        };
        unsafe { CoTaskMemFree(wave_format.cast()) };
        let format_info = format_info?;
        check_hresult(initialize_result, "无法初始化 WASAPI 回环采集")?;

        let mut capture = ptr::null_mut();
        check_hresult(
            unsafe {
                ((*(*client_interface).vtable).get_service)(
                    client.as_raw(),
                    &IID_I_AUDIO_CAPTURE_CLIENT,
                    &mut capture,
                )
            },
            "无法取得 WASAPI 采集接口",
        )?;
        let capture = ComPtr::new(capture, "WASAPI 采集接口")?;
        check_hresult(
            unsafe { ((*(*client_interface).vtable).start)(client.as_raw()) },
            "无法启动 WASAPI 回环采集",
        )?;

        Ok(Self {
            capture,
            client,
            format: format_info.sample_format,
            channels: format_info.channels,
            block_align: format_info.block_align,
            sample_rate: format_info.sample_rate,
            samples: [0.0; FFT_SIZE],
            cursor: 0,
            filled: 0,
            levels: [0.0; BAND_COUNT],
            _apartment: apartment,
        })
    }

    pub fn bands(&mut self) -> Option<[f32; BAND_COUNT]> {
        self.drain_packets().ok()?;
        let mut ordered = [0.0; FFT_SIZE];
        if self.filled < FFT_SIZE {
            ordered[FFT_SIZE - self.filled..].copy_from_slice(&self.samples[..self.filled]);
        } else {
            let tail = FFT_SIZE - self.cursor;
            ordered[..tail].copy_from_slice(&self.samples[self.cursor..]);
            ordered[tail..].copy_from_slice(&self.samples[..self.cursor]);
        }
        self.levels = spectrum(&ordered, self.sample_rate, self.levels);
        Some(self.levels)
    }

    fn drain_packets(&mut self) -> Result<()> {
        let capture = self.capture.as_interface::<AudioCaptureClientVTable>();
        loop {
            let mut packet_frames = 0;
            check_hresult(
                unsafe {
                    ((*(*capture).vtable).get_next_packet_size)(
                        self.capture.as_raw(),
                        &mut packet_frames,
                    )
                },
                "无法读取 WASAPI 缓冲区大小",
            )?;
            if packet_frames == 0 {
                return Ok(());
            }

            let mut data = ptr::null_mut();
            let mut frames = 0;
            let mut flags = 0;
            check_hresult(
                unsafe {
                    ((*(*capture).vtable).get_buffer)(
                        self.capture.as_raw(),
                        &mut data,
                        &mut frames,
                        &mut flags,
                        ptr::null_mut(),
                        ptr::null_mut(),
                    )
                },
                "无法读取 WASAPI 音频数据",
            )?;

            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT as u32 != 0;
            for frame in 0..frames as usize {
                let mono = if silent {
                    0.0
                } else {
                    let mut sum = 0.0;
                    for channel in 0..self.channels {
                        sum += unsafe {
                            self.format.read(
                                data,
                                frame * self.block_align
                                    + channel * (self.block_align / self.channels),
                            )
                        };
                    }
                    sum / self.channels as f32
                };
                self.samples[self.cursor] = mono.clamp(-1.0, 1.0);
                self.cursor = (self.cursor + 1) % FFT_SIZE;
                self.filled = (self.filled + 1).min(FFT_SIZE);
            }
            check_hresult(
                unsafe { ((*(*capture).vtable).release_buffer)(self.capture.as_raw(), frames) },
                "无法释放 WASAPI 音频缓冲区",
            )?;
        }
    }
}

impl Drop for OutputSpectrum {
    fn drop(&mut self) {
        let client = self.client.as_interface::<AudioClientVTable>();
        unsafe {
            ((*(*client).vtable).stop)(self.client.as_raw());
        }
    }
}

fn create_enumerator() -> Result<ComPtr> {
    let mut enumerator = ptr::null_mut();
    check_hresult(
        unsafe {
            CoCreateInstance(
                &MMDeviceEnumerator,
                ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_IMM_DEVICE_ENUMERATOR,
                &mut enumerator,
            )
        },
        "无法创建音频设备枚举器",
    )?;
    ComPtr::new(enumerator, "音频设备枚举器")
}

fn default_output_device(enumerator: &ComPtr) -> Result<ComPtr> {
    let mut device = ptr::null_mut();
    let interface = enumerator.as_interface::<DeviceEnumeratorVTable>();
    check_hresult(
        unsafe {
            ((*(*interface).vtable).get_default_audio_endpoint)(
                enumerator.as_raw(),
                eRender,
                eMultimedia,
                &mut device,
            )
        },
        "无法取得默认音频输出设备",
    )?;
    ComPtr::new(device, "默认音频输出设备")
}

struct FormatInfo {
    sample_format: SampleFormat,
    channels: usize,
    block_align: usize,
    sample_rate: u32,
}

impl FormatInfo {
    unsafe fn from_wave_format(format: *const WAVEFORMATEX) -> Result<Self> {
        let wave = unsafe { ptr::read_unaligned(format) };
        ensure!(wave.nChannels > 0, "默认音频格式没有声道");
        ensure!(wave.nBlockAlign > 0, "默认音频格式的帧宽度无效");
        let bits_per_sample = wave.wBitsPerSample;
        let tag = if wave.wFormatTag == WAVE_FORMAT_EXTENSIBLE {
            ensure!(wave.cbSize >= 22, "WAVEFORMATEXTENSIBLE 长度不足");
            unsafe { ptr::read_unaligned(format.cast::<u8>().add(24).cast::<u32>()) as u16 }
        } else {
            wave.wFormatTag
        };
        let sample_format = match (tag, bits_per_sample) {
            (WAVE_FORMAT_IEEE_FLOAT, 32) => SampleFormat::Float32,
            (WAVE_FORMAT_IEEE_FLOAT, 64) => SampleFormat::Float64,
            (WAVE_FORMAT_PCM, 16) => SampleFormat::Pcm16,
            (WAVE_FORMAT_PCM, 24) => SampleFormat::Pcm24,
            (WAVE_FORMAT_PCM, 32) => SampleFormat::Pcm32,
            _ => bail!(
                "不支持默认音频格式：tag=0x{tag:04X}，{} bit",
                bits_per_sample
            ),
        };
        Ok(Self {
            sample_format,
            channels: wave.nChannels as usize,
            block_align: wave.nBlockAlign as usize,
            sample_rate: wave.nSamplesPerSec,
        })
    }
}

#[derive(Clone, Copy)]
enum SampleFormat {
    Float32,
    Float64,
    Pcm16,
    Pcm24,
    Pcm32,
}

impl SampleFormat {
    unsafe fn read(self, data: *const u8, offset: usize) -> f32 {
        let sample = unsafe { data.add(offset) };
        match self {
            Self::Float32 => unsafe { ptr::read_unaligned(sample.cast::<f32>()) },
            Self::Float64 => unsafe { ptr::read_unaligned(sample.cast::<f64>()) as f32 },
            Self::Pcm16 => unsafe { ptr::read_unaligned(sample.cast::<i16>()) as f32 / 32768.0 },
            Self::Pcm24 => {
                let bytes = unsafe { std::slice::from_raw_parts(sample, 3) };
                let value =
                    ((bytes[0] as i32) | ((bytes[1] as i32) << 8) | ((bytes[2] as i32) << 16)) << 8
                        >> 8;
                value as f32 / 8_388_608.0
            }
            Self::Pcm32 => unsafe {
                ptr::read_unaligned(sample.cast::<i32>()) as f32 / 2_147_483_648.0
            },
        }
    }
}

fn spectrum(
    samples: &[f32; FFT_SIZE],
    sample_rate: u32,
    previous: [f32; BAND_COUNT],
) -> [f32; BAND_COUNT] {
    let mut real = [0.0; FFT_SIZE];
    let mut imaginary = [0.0; FFT_SIZE];
    for (index, sample) in samples.iter().enumerate() {
        let window = 0.5 - 0.5 * (2.0 * PI * index as f32 / (FFT_SIZE - 1) as f32).cos();
        real[index] = sample * window;
    }
    fft(&mut real, &mut imaginary);

    let mut peaks = [0.0_f32; BAND_COUNT];
    for (band, (start, end)) in band_bin_ranges(sample_rate).into_iter().enumerate() {
        for bin in start..end {
            let magnitude = (real[bin] * real[bin] + imaginary[bin] * imaginary[bin]).sqrt() * 2.0
                / FFT_SIZE as f32;
            peaks[band] = peaks[band].max(magnitude);
        }
    }

    let mut levels = [0.0; BAND_COUNT];
    for index in 0..BAND_COUNT {
        let decibels = 20.0 * peaks[index].max(0.000_001).log10();
        let target = ((decibels + 60.0) / 50.0).clamp(0.0, 1.0);
        let smoothing = if target > previous[index] { 0.65 } else { 0.18 };
        levels[index] = previous[index] + (target - previous[index]) * smoothing;
    }
    levels
}

fn band_bin_ranges(sample_rate: u32) -> [(usize, usize); BAND_COUNT] {
    let bin_width = sample_rate as f32 / FFT_SIZE as f32;
    let first = (40.0 / bin_width).ceil().max(1.0) as usize;
    let maximum = 16_000.0_f32.min(sample_rate as f32 / 2.0);
    let end = ((maximum / bin_width).floor() as usize + 1).min(FFT_SIZE / 2);
    let logarithmic_range = (end as f32 / first as f32).ln();

    let mut ranges = [(0, 0); BAND_COUNT];
    let mut start = first;
    for (band, range) in ranges.iter_mut().enumerate() {
        let remaining = BAND_COUNT - band - 1;
        let desired = if remaining == 0 {
            end
        } else {
            (first as f32 * (logarithmic_range * (band + 1) as f32 / BAND_COUNT as f32).exp())
                .round() as usize
        };
        let band_end = desired.clamp(start + 1, end - remaining);
        *range = (start, band_end);
        start = band_end;
    }
    ranges
}

fn fft(real: &mut [f32; FFT_SIZE], imaginary: &mut [f32; FFT_SIZE]) {
    let mut reversed = 0;
    for index in 1..FFT_SIZE {
        let mut bit = FFT_SIZE >> 1;
        while reversed & bit != 0 {
            reversed ^= bit;
            bit >>= 1;
        }
        reversed ^= bit;
        if index < reversed {
            real.swap(index, reversed);
            imaginary.swap(index, reversed);
        }
    }

    let mut length = 2;
    while length <= FFT_SIZE {
        let angle = -2.0 * PI / length as f32;
        let step_real = angle.cos();
        let step_imaginary = angle.sin();
        for start in (0..FFT_SIZE).step_by(length) {
            let mut twiddle_real = 1.0;
            let mut twiddle_imaginary = 0.0;
            for offset in 0..length / 2 {
                let even = start + offset;
                let odd = even + length / 2;
                let odd_real = real[odd] * twiddle_real - imaginary[odd] * twiddle_imaginary;
                let odd_imaginary = real[odd] * twiddle_imaginary + imaginary[odd] * twiddle_real;
                real[odd] = real[even] - odd_real;
                imaginary[odd] = imaginary[even] - odd_imaginary;
                real[even] += odd_real;
                imaginary[even] += odd_imaginary;
                let next_real = twiddle_real * step_real - twiddle_imaginary * step_imaginary;
                twiddle_imaginary = twiddle_real * step_imaginary + twiddle_imaginary * step_real;
                twiddle_real = next_real;
            }
        }
        length *= 2;
    }
}

struct ComApartment;

impl ComApartment {
    fn initialize() -> Result<Self> {
        let result = unsafe {
            CoInitializeEx(
                ptr::null(),
                (COINIT_MULTITHREADED | COINIT_DISABLE_OLE1DDE) as u32,
            )
        };
        check_hresult(result, "无法初始化 Core Audio 所需的 COM")?;
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct ComPtr(*mut c_void);

impl ComPtr {
    fn new(pointer: *mut c_void, name: &str) -> Result<Self> {
        ensure!(!pointer.is_null(), "{name}返回了空接口");
        Ok(Self(pointer))
    }

    fn as_raw(&self) -> *mut c_void {
        self.0
    }

    fn as_interface<V>(&self) -> *mut ComInterface<V> {
        self.0.cast()
    }
}

impl Drop for ComPtr {
    fn drop(&mut self) {
        let interface = self.0.cast::<ComInterface<UnknownVTable>>();
        unsafe { ((*(*interface).vtable).release)(self.0) };
    }
}

#[repr(C)]
struct ComInterface<V> {
    vtable: *const V,
}

#[repr(C)]
struct UnknownVTable {
    _query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    _add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
struct DeviceEnumeratorVTable {
    _unknown: UnknownVTable,
    _enum_audio_endpoints: *const c_void,
    get_default_audio_endpoint:
        unsafe extern "system" fn(*mut c_void, i32, i32, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
struct DeviceVTable {
    _unknown: UnknownVTable,
    activate: unsafe extern "system" fn(
        *mut c_void,
        *const GUID,
        u32,
        *const c_void,
        *mut *mut c_void,
    ) -> HRESULT,
}

#[repr(C)]
struct AudioClientVTable {
    _unknown: UnknownVTable,
    initialize: unsafe extern "system" fn(
        *mut c_void,
        i32,
        u32,
        i64,
        i64,
        *const WAVEFORMATEX,
        *const GUID,
    ) -> HRESULT,
    _get_buffer_size: *const c_void,
    _get_stream_latency: *const c_void,
    _get_current_padding: *const c_void,
    _is_format_supported: *const c_void,
    get_mix_format: unsafe extern "system" fn(*mut c_void, *mut *mut WAVEFORMATEX) -> HRESULT,
    _get_device_period: *const c_void,
    start: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    stop: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    _reset: *const c_void,
    _set_event_handle: *const c_void,
    get_service: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
struct AudioCaptureClientVTable {
    _unknown: UnknownVTable,
    get_buffer: unsafe extern "system" fn(
        *mut c_void,
        *mut *mut u8,
        *mut u32,
        *mut u32,
        *mut u64,
        *mut u64,
    ) -> HRESULT,
    release_buffer: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
    get_next_packet_size: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
}

fn check_hresult(result: HRESULT, context: &str) -> Result<()> {
    ensure!(result >= 0, "{context}（HRESULT 0x{:08X}）", result as u32);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{BAND_COUNT, FFT_SIZE, band_bin_ranges, spectrum};

    #[test]
    fn silence_produces_empty_bands() {
        assert_eq!(
            spectrum(&[0.0; FFT_SIZE], 48_000, [0.0; BAND_COUNT]),
            [0.0; BAND_COUNT]
        );
    }

    #[test]
    fn sine_wave_activates_its_logarithmic_band() {
        let mut samples = [0.0; FFT_SIZE];
        for (index, sample) in samples.iter_mut().enumerate() {
            *sample = (2.0 * std::f32::consts::PI * 1_000.0 * index as f32 / 48_000.0).sin();
        }
        let bands = spectrum(&samples, 48_000, [0.0; BAND_COUNT]);
        let strongest = bands
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(right.1))
            .unwrap()
            .0;
        assert!((7..=10).contains(&strongest));
        assert!(bands[strongest] > 0.5);
    }

    #[test]
    fn second_band_contains_and_uses_an_fft_bin() {
        let ranges = band_bin_ranges(48_000);
        assert!(ranges.iter().all(|(start, end)| start < end));

        let frequency = 2.0 * 48_000.0 / FFT_SIZE as f32;
        let mut samples = [0.0; FFT_SIZE];
        for (index, sample) in samples.iter_mut().enumerate() {
            *sample = (2.0 * std::f32::consts::PI * frequency * index as f32 / 48_000.0).sin();
        }
        let bands = spectrum(&samples, 48_000, [0.0; BAND_COUNT]);
        let strongest = bands
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(right.1))
            .unwrap()
            .0;
        assert_eq!(strongest, 1);
        assert!(bands[1] > 0.5);
    }
}
