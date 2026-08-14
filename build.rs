use std::{env, fs, path::PathBuf};

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const LANGUAGE_EN_US: u16 = 0x0409;

fn main() {
    println!("cargo:rerun-if-changed=assets/looppanel.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let icon = fs::read("assets/looppanel.ico").expect("failed to read LoopPanel icon");
    let resource = icon_resource(&icon);
    let output =
        PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is not set")).join("looppanel.res");
    fs::write(&output, resource).expect("failed to write Windows icon resource");
    println!("cargo:rustc-link-arg-bin=looppanel={}", output.display());
}

fn icon_resource(icon: &[u8]) -> Vec<u8> {
    assert!(icon.len() >= 6, "invalid ICO header");
    assert_eq!(read_u16(icon, 0), 0, "invalid ICO reserved field");
    assert_eq!(read_u16(icon, 2), 1, "file is not an ICO");
    let count = read_u16(icon, 4) as usize;
    assert!(count > 0, "ICO has no images");
    assert!(icon.len() >= 6 + count * 16, "truncated ICO directory");

    let mut output = Vec::new();
    write_null_resource(&mut output);
    let mut group = Vec::with_capacity(6 + count * 14);
    push_u16(&mut group, 0);
    push_u16(&mut group, 1);
    push_u16(&mut group, count as u16);

    for index in 0..count {
        let entry = 6 + index * 16;
        let size = read_u32(icon, entry + 8) as usize;
        let offset = read_u32(icon, entry + 12) as usize;
        let end = offset.checked_add(size).expect("ICO image size overflow");
        assert!(end <= icon.len(), "truncated ICO image");
        let id = (index + 1) as u16;
        write_resource(&mut output, RT_ICON, id, LANGUAGE_EN_US, &icon[offset..end]);
        group.extend_from_slice(&icon[entry..entry + 12]);
        push_u16(&mut group, id);
    }

    write_resource(&mut output, RT_GROUP_ICON, 1, LANGUAGE_EN_US, &group);
    output
}

fn write_null_resource(output: &mut Vec<u8>) {
    push_u32(output, 0);
    push_u32(output, 32);
    push_u16(output, 0xffff);
    push_u16(output, 0);
    push_u16(output, 0xffff);
    push_u16(output, 0);
    output.extend_from_slice(&[0; 16]);
}

fn write_resource(output: &mut Vec<u8>, resource_type: u16, name: u16, language: u16, data: &[u8]) {
    pad_to_dword(output);
    push_u32(output, data.len() as u32);
    push_u32(output, 32);
    push_u16(output, 0xffff);
    push_u16(output, resource_type);
    push_u16(output, 0xffff);
    push_u16(output, name);
    push_u32(output, 0);
    push_u16(output, 0x0030);
    push_u16(output, language);
    push_u32(output, 0);
    push_u32(output, 0);
    output.extend_from_slice(data);
    pad_to_dword(output);
}

fn pad_to_dword(output: &mut Vec<u8>) {
    while !output.len().is_multiple_of(4) {
        output.push(0);
    }
}

fn read_u16(input: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(input[offset..offset + 2].try_into().unwrap())
}

fn read_u32(input: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(input[offset..offset + 4].try_into().unwrap())
}

fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}
