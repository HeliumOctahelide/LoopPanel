use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};

use anyhow::{Result, ensure};
use windows_sys::Win32::{
    Foundation::ERROR_SUCCESS,
    System::Registry::{HKEY, RRF_RT_REG_SZ, RegGetValueW},
};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
}

pub fn system_dll(name: &str) -> Result<PathBuf> {
    let mut buffer = vec![0_u16; 32_768];
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
    ensure!(
        length > 0 && length < buffer.len() as u32,
        "无法取得 Windows 系统目录"
    );
    Ok(PathBuf::from(OsString::from_wide(&buffer[..length as usize])).join(name))
}

pub fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

pub(crate) fn registry_string(root: HKEY, subkey: &str, value: &str) -> Result<String> {
    let subkey = wide(subkey);
    let value = wide(value);
    let mut bytes = 0_u32;
    let result = unsafe {
        RegGetValueW(
            root,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut bytes,
        )
    };
    ensure!(
        result == ERROR_SUCCESS,
        "无法读取注册表字符串（Win32 错误 {result}）"
    );
    let mut buffer = vec![0_u16; bytes as usize / 2];
    let result = unsafe {
        RegGetValueW(
            root,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    ensure!(
        result == ERROR_SUCCESS,
        "无法读取注册表字符串（Win32 错误 {result}）"
    );
    let length = buffer
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(buffer.len());
    Ok(String::from_utf16(&buffer[..length])?)
}
