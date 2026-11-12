//! 托盘菜单里的显示选项（布局、自动轮换、温度单位）：保存在当前用户的注册表中，运行时与显示线程共享。

use std::{
    mem::size_of,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
    time::Duration,
};

use anyhow::{Result, ensure};
use windows_sys::Win32::{
    Foundation::ERROR_SUCCESS,
    System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW},
};

use crate::{
    i18n::t,
    win::{registry_string, wide},
};

const SETTINGS_KEY: &str = r"Software\LoopPanel";
const LAYOUT_VALUE: &str = "Layout";
const TEMPERATURE_UNIT_VALUE: &str = "TemperatureUnit";
const SIMPLE_BACKGROUND_VALUE: &str = "SimpleBackground";
const ROTATION_VALUE: &str = "Rotation";
const ROTATION_INTERVAL_VALUE: &str = "RotationIntervalSeconds";
/// 自动轮换的默认间隔。
pub const DEFAULT_ROTATION_SECONDS: u32 = 30;
/// 注册表里手写的间隔也接受，但限制在这个范围内。
const ROTATION_SECONDS_RANGE: std::ops::RangeInclusive<u32> = 5..=3600;
const ON: &str = "on";
const OFF: &str = "off";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Layout {
    /// 配置文件里 `template` 指定的仪表盘。
    Default,
    /// 内置的大字布局：只有 CPU 温度和占用率。
    Simple,
    /// 内置的大字布局：CPU 与 GPU 各一行，温度加占用率。
    CpuGpu,
    /// 内置的大字布局：只有 CPU 温度和 GPU 温度。
    Temperatures,
    /// 内置的大字布局：时钟，下方是 CPU / GPU 温度。
    Clock,
}

impl Layout {
    /// 菜单顺序，也是自动轮换的顺序。
    pub const ALL: [Self; 5] = [
        Self::Default,
        Self::Simple,
        Self::CpuGpu,
        Self::Temperatures,
        Self::Clock,
    ];

    fn from_u8(value: u8) -> Self {
        Self::ALL
            .into_iter()
            .find(|layout| *layout as u8 == value)
            .unwrap_or(Self::Default)
    }

    fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Simple => "simple",
            Self::CpuGpu => "cpu_gpu",
            Self::Temperatures => "temperatures",
            Self::Clock => "clock",
        }
    }

    fn from_name(name: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|layout| layout.name() == name)
            .unwrap_or(Self::Default)
    }

    /// 自动轮换时排在后面的布局；最后一个之后回到第一个。
    pub fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|layout| *layout == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TemperatureUnit {
    #[default]
    Celsius,
    Fahrenheit,
}

impl TemperatureUnit {
    fn from_u8(value: u8) -> Self {
        if value == Self::Fahrenheit as u8 {
            Self::Fahrenheit
        } else {
            Self::Celsius
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Celsius => "celsius",
            Self::Fahrenheit => "fahrenheit",
        }
    }

    fn from_name(name: &str) -> Self {
        if name == Self::Fahrenheit.name() {
            Self::Fahrenheit
        } else {
            Self::Celsius
        }
    }

    /// 传感器读数一律是摄氏度；这里换算成要显示的整数值。
    pub fn convert(self, celsius: u32) -> i32 {
        match self {
            Self::Celsius => celsius as i32,
            Self::Fahrenheit => (f64::from(celsius) * 9.0 / 5.0 + 32.0).round() as i32,
        }
    }

    /// 单位字母，不含度数符号。
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Celsius => "C",
            Self::Fahrenheit => "F",
        }
    }

    /// 例如 `46°C`、`115°F`。
    pub fn format(self, celsius: u32) -> String {
        format!("{}°{}", self.convert(celsius), self.symbol())
    }
}

/// 托盘线程写、显示线程读的当前选项。
pub struct DisplaySettings {
    layout: AtomicU8,
    temperature_unit: AtomicU8,
    simple_background: AtomicBool,
    rotation: AtomicBool,
    rotation_seconds: AtomicU32,
    reload_requested: AtomicBool,
}

impl DisplaySettings {
    /// 没有保存过或读取失败的项使用默认值。
    pub fn load() -> Self {
        let layout =
            load_value(LAYOUT_VALUE).map_or(Layout::Default, |name| Layout::from_name(&name));
        let temperature_unit = load_value(TEMPERATURE_UNIT_VALUE)
            .map_or(TemperatureUnit::Celsius, |name| {
                TemperatureUnit::from_name(&name)
            });
        // 单纯布局默认把配置的背景铺在下面。
        let simple_background =
            load_value(SIMPLE_BACKGROUND_VALUE).is_none_or(|value| value != OFF);
        let settings = Self::new(layout, temperature_unit, simple_background);
        settings.rotation.store(
            load_value(ROTATION_VALUE).is_some_and(|value| value == ON),
            Ordering::Relaxed,
        );
        if let Some(seconds) = load_value(ROTATION_INTERVAL_VALUE)
            .and_then(|value| value.parse().ok())
            .filter(|seconds| ROTATION_SECONDS_RANGE.contains(seconds))
        {
            settings.rotation_seconds.store(seconds, Ordering::Relaxed);
        }
        settings
    }

    pub fn new(layout: Layout, temperature_unit: TemperatureUnit, simple_background: bool) -> Self {
        Self {
            layout: AtomicU8::new(layout as u8),
            temperature_unit: AtomicU8::new(temperature_unit as u8),
            simple_background: AtomicBool::new(simple_background),
            rotation: AtomicBool::new(false),
            rotation_seconds: AtomicU32::new(DEFAULT_ROTATION_SECONDS),
            reload_requested: AtomicBool::new(false),
        }
    }

    /// 内置大字布局下是否显示配置的背景图 / GIF。
    pub fn simple_background(&self) -> bool {
        self.simple_background.load(Ordering::Relaxed)
    }

    /// 托盘请求显示线程重新读取配置文件和模板。
    pub fn request_reload(&self) {
        self.reload_requested.store(true, Ordering::Relaxed);
    }

    /// 取走一次重新载入请求；没有请求时返回 `false`。
    pub fn take_reload_request(&self) -> bool {
        self.reload_requested.swap(false, Ordering::Relaxed)
    }

    pub fn set_simple_background(&self, enabled: bool) -> Result<()> {
        save_value(SIMPLE_BACKGROUND_VALUE, if enabled { ON } else { OFF })?;
        self.simple_background.store(enabled, Ordering::Relaxed);
        Ok(())
    }

    /// 开启自动轮换时返回切换间隔，关闭时返回 `None`。
    pub fn rotation(&self) -> Option<Duration> {
        self.rotation_enabled()
            .then(|| Duration::from_secs(u64::from(self.rotation_seconds())))
    }

    pub fn rotation_enabled(&self) -> bool {
        self.rotation.load(Ordering::Relaxed)
    }

    pub fn rotation_seconds(&self) -> u32 {
        self.rotation_seconds.load(Ordering::Relaxed)
    }

    pub fn set_rotation_enabled(&self, enabled: bool) -> Result<()> {
        save_value(ROTATION_VALUE, if enabled { ON } else { OFF })?;
        self.rotation.store(enabled, Ordering::Relaxed);
        Ok(())
    }

    pub fn set_rotation_seconds(&self, seconds: u32) -> Result<()> {
        let seconds = seconds.clamp(
            *ROTATION_SECONDS_RANGE.start(),
            *ROTATION_SECONDS_RANGE.end(),
        );
        save_value(ROTATION_INTERVAL_VALUE, &seconds.to_string())?;
        self.rotation_seconds.store(seconds, Ordering::Relaxed);
        Ok(())
    }

    pub fn layout(&self) -> Layout {
        Layout::from_u8(self.layout.load(Ordering::Relaxed))
    }

    pub fn temperature_unit(&self) -> TemperatureUnit {
        TemperatureUnit::from_u8(self.temperature_unit.load(Ordering::Relaxed))
    }

    pub fn set_layout(&self, layout: Layout) -> Result<()> {
        save_value(LAYOUT_VALUE, layout.name())?;
        self.layout.store(layout as u8, Ordering::Relaxed);
        Ok(())
    }

    pub fn set_temperature_unit(&self, unit: TemperatureUnit) -> Result<()> {
        save_value(TEMPERATURE_UNIT_VALUE, unit.name())?;
        self.temperature_unit.store(unit as u8, Ordering::Relaxed);
        Ok(())
    }
}

fn load_value(name: &str) -> Option<String> {
    registry_string(HKEY_CURRENT_USER, SETTINGS_KEY, name).ok()
}

fn save_value(name: &str, value: &str) -> Result<()> {
    let subkey = wide(SETTINGS_KEY);
    let value_name = wide(name);
    let data = wide(value);
    let result = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value_name.as_ptr(),
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * size_of::<u16>()) as u32,
        )
    };
    ensure!(
        result == ERROR_SUCCESS,
        "{}",
        t!(
            "settings.save_failed",
            error = std::io::Error::from_raw_os_error(result as i32)
        )
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{DisplaySettings, Layout, TemperatureUnit};

    #[test]
    fn options_round_trip_and_unknown_values_fall_back_to_defaults() {
        let mut visited = Layout::Default;
        for layout in Layout::ALL {
            assert_eq!(Layout::from_u8(layout as u8), layout);
            assert_eq!(Layout::from_name(layout.name()), layout);
            // 轮换按菜单顺序走完所有布局。
            assert_eq!(visited, layout);
            visited = visited.next();
        }
        assert_eq!(visited, Layout::Default);
        assert_eq!(Layout::from_u8(200), Layout::Default);
        assert_eq!(Layout::from_name("no-such-layout"), Layout::Default);

        for unit in [TemperatureUnit::Celsius, TemperatureUnit::Fahrenheit] {
            assert_eq!(TemperatureUnit::from_u8(unit as u8), unit);
            assert_eq!(TemperatureUnit::from_name(unit.name()), unit);
        }
        assert_eq!(TemperatureUnit::from_u8(200), TemperatureUnit::Celsius);
        assert_eq!(
            TemperatureUnit::from_name("kelvin"),
            TemperatureUnit::Celsius
        );

        let settings = DisplaySettings::new(Layout::Simple, TemperatureUnit::Fahrenheit, false);
        assert!(!settings.simple_background());
        // 自动轮换默认关闭，间隔默认 30 秒。
        assert_eq!(settings.rotation(), None);
        assert_eq!(settings.rotation_seconds(), 30);

        // 重新载入请求只被取走一次。
        assert!(!settings.take_reload_request());
        settings.request_reload();
        assert!(settings.take_reload_request());
        assert!(!settings.take_reload_request());
        assert_eq!(settings.layout(), Layout::Simple);
        assert_eq!(settings.temperature_unit(), TemperatureUnit::Fahrenheit);
    }

    #[test]
    fn converts_celsius_readings_for_display() {
        assert_eq!(TemperatureUnit::Celsius.convert(46), 46);
        assert_eq!(TemperatureUnit::Fahrenheit.convert(0), 32);
        assert_eq!(TemperatureUnit::Fahrenheit.convert(46), 115);
        assert_eq!(TemperatureUnit::Fahrenheit.convert(100), 212);
        // 37°C = 98.6°F，四舍五入。
        assert_eq!(TemperatureUnit::Fahrenheit.convert(37), 99);
        assert_eq!(TemperatureUnit::Celsius.format(46), "46°C");
        assert_eq!(TemperatureUnit::Fahrenheit.format(46), "115°F");
    }
}
