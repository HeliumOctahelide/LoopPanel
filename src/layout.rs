//! 屏幕布局选择：由托盘菜单切换，保存在当前用户的注册表中。

use std::mem::size_of;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Layout {
    /// 配置文件里 `template` 指定的仪表盘。
    Default,
    /// 内置的大字布局：只有 CPU 温度和占用率。
    Simple,
}

impl Layout {
    pub fn from_u8(value: u8) -> Self {
        if value == Self::Simple as u8 {
            Self::Simple
        } else {
            Self::Default
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Simple => "simple",
        }
    }

    fn from_name(name: &str) -> Self {
        if name == Self::Simple.name() {
            Self::Simple
        } else {
            Self::Default
        }
    }

    /// 没有保存过或读取失败时使用默认布局。
    pub fn load() -> Self {
        registry_string(HKEY_CURRENT_USER, SETTINGS_KEY, LAYOUT_VALUE)
            .map_or(Self::Default, |name| Self::from_name(&name))
    }

    pub fn save(self) -> Result<()> {
        let subkey = wide(SETTINGS_KEY);
        let value_name = wide(LAYOUT_VALUE);
        let data = wide(self.name());
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
                "layout.save_failed",
                error = std::io::Error::from_raw_os_error(result as i32)
            )
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Layout;

    #[test]
    fn layout_round_trips_and_unknown_values_fall_back_to_default() {
        for layout in [Layout::Default, Layout::Simple] {
            assert_eq!(Layout::from_u8(layout as u8), layout);
            assert_eq!(Layout::from_name(layout.name()), layout);
        }
        assert_eq!(Layout::from_u8(200), Layout::Default);
        assert_eq!(Layout::from_name("no-such-layout"), Layout::Default);
    }
}
