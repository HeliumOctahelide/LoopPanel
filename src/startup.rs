use std::{mem::size_of, os::windows::ffi::OsStrExt, path::Path, ptr};

use anyhow::{Result, ensure};
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
    System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
        RegSetValueExW,
    },
};

use crate::win::wide;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "LoopPanel";

pub fn is_enabled(executable: &Path) -> Result<bool> {
    let expected = startup_command(executable);
    let subkey = wide(RUN_KEY);
    let name = wide(VALUE_NAME);
    let mut key = ptr::null_mut();
    let result = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut key,
        )
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    check_registry(result, "无法读取登录启动设置")?;
    let key = RegistryKey(key);
    let mut kind = 0_u32;
    let mut buffer = [0_u16; 260];
    let mut bytes = (buffer.len() * size_of::<u16>()) as u32;
    let result = unsafe {
        RegQueryValueExW(
            key.0,
            name.as_ptr(),
            ptr::null(),
            &mut kind,
            buffer.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    check_registry(result, "无法读取登录启动设置")?;
    if kind != REG_SZ {
        return Ok(false);
    }
    let length = bytes as usize / size_of::<u16>();
    let length = length.min(buffer.len());
    let length = buffer[..length]
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(length);
    Ok(buffer[..length] == expected[..expected.len() - 1])
}

pub fn set_enabled(executable: &Path, enabled: bool) -> Result<()> {
    let subkey = wide(RUN_KEY);
    let name = wide(VALUE_NAME);
    if !enabled {
        let mut key = ptr::null_mut();
        let result = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                KEY_SET_VALUE,
                &mut key,
            )
        };
        if result == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        check_registry(result, "无法打开登录启动设置")?;
        let key = RegistryKey(key);
        let result = unsafe { RegDeleteValueW(key.0, name.as_ptr()) };
        if result != ERROR_FILE_NOT_FOUND {
            check_registry(result, "无法关闭登录启动")?;
        }
        return Ok(());
    }

    let command = startup_command(executable);
    ensure!(
        command.len() <= 260,
        "登录启动命令超过 Windows 的 260 字符限制"
    );
    let mut key = ptr::null_mut();
    let result = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
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
    check_registry(result, "无法打开登录启动设置")?;
    let key = RegistryKey(key);
    let result = unsafe {
        RegSetValueExW(
            key.0,
            name.as_ptr(),
            0,
            REG_SZ,
            command.as_ptr().cast(),
            (command.len() * size_of::<u16>()) as u32,
        )
    };
    check_registry(result, "无法启用登录启动")
}

fn startup_command(executable: &Path) -> Vec<u16> {
    let mut command = vec![b'"' as u16];
    command.extend(executable.as_os_str().encode_wide());
    command.push(b'"' as u16);
    command.push(0);
    command
}

fn check_registry(result: u32, action: &str) -> Result<()> {
    ensure!(
        result == ERROR_SUCCESS,
        "{action}：{}",
        std::io::Error::from_raw_os_error(result as i32)
    );
    Ok(())
}

struct RegistryKey(HKEY);

impl Drop for RegistryKey {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::startup_command;

    #[test]
    fn quotes_executable_path() {
        let command = startup_command(Path::new(r"C:\Program Files\LoopPanel\LoopPanel.exe"));
        let text = String::from_utf16(&command[..command.len() - 1]).unwrap();
        assert_eq!(text, r#""C:\Program Files\LoopPanel\LoopPanel.exe""#);
    }
}
