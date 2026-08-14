use anyhow::{Result, ensure};
use image::RgbaImage;

pub const WIDTH: u16 = 480;
pub const HEIGHT: u16 = 480;
pub const VIC: u8 = 143;
pub const VID: u16 = 0x345f;
pub const PID: u16 = 0x9132;
pub const INTERFACE: i32 = 3;
pub const ENDPOINT: i32 = 0x04;
pub const CHUNK_SIZE: usize = 65_536;

// Integer coefficients and UYVY ordering match the real-device USB capture in:
// https://github.com/Fakinvisibility/b360gt-driver/blob/94ae7f2a710123b582ca3fa806d85ee1c684e287/src/b360gt/protocol.py
pub fn rgba_to_uyvy(image: &RgbaImage, brightness: f32) -> Result<Vec<u8>> {
    let (width, height) = image.dimensions();
    ensure!(width % 2 == 0, "UYVY422 要求图片宽度为偶数");
    let scale = (brightness.clamp(0.05, 1.0) * 256.0).round() as i32;
    let source = image.as_raw();
    let mut output = Vec::with_capacity(width as usize * height as usize * 2);

    for pair in source.chunks_exact(8) {
        let r1 = (pair[0] as i32 * scale) >> 8;
        let g1 = (pair[1] as i32 * scale) >> 8;
        let b1 = (pair[2] as i32 * scale) >> 8;
        let r2 = (pair[4] as i32 * scale) >> 8;
        let g2 = (pair[5] as i32 * scale) >> 8;
        let b2 = (pair[6] as i32 * scale) >> 8;

        let y1 = ((257 * r1 + 504 * g1 + 98 * b1) / 1_000 + 16).clamp(16, 235);
        let y2 = ((257 * r2 + 504 * g2 + 98 * b2) / 1_000 + 16).clamp(16, 235);
        let u1 = div_floor(-148 * r1 - 291 * g1 + 439 * b1, 1_000) + 128;
        let u2 = div_floor(-148 * r2 - 291 * g2 + 439 * b2, 1_000) + 128;
        let v1 = div_floor(439 * r1 - 368 * g1 - 71 * b1, 1_000) + 128;
        let v2 = div_floor(439 * r2 - 368 * g2 - 71 * b2, 1_000) + 128;
        let u1 = u1.clamp(16, 240);
        let u2 = u2.clamp(16, 240);
        let v1 = v1.clamp(16, 240);
        let v2 = v2.clamp(16, 240);

        output.extend_from_slice(&[byte((u1 + u2) / 2), byte(y1), byte((v1 + v2) / 2), byte(y2)]);
    }
    Ok(output)
}

pub fn frame_packet(uyvy: &[u8], width: u16, height: u16) -> Result<Vec<u8>> {
    ensure!(width.is_multiple_of(16), "MS9132 全帧宽度必须是 16 的倍数");
    let payload_length = width as usize * height as usize * 2;
    ensure!(uyvy.len() == payload_length, "像素数据长度不正确");

    let mut packet = Vec::with_capacity(payload_length + 16);
    packet.extend_from_slice(&[
        0xff,
        0x00,
        0x00,
        0x00,
        0x00,
        (width / 16) as u8,
        (height >> 8) as u8,
        height as u8,
    ]);
    packet.extend_from_slice(uyvy);
    packet.extend_from_slice(&[0xff, 0xc0, 0, 0, 0, 0, 0, 0]);
    Ok(packet)
}

fn byte(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

fn div_floor(numerator: i32, denominator: i32) -> i32 {
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    if remainder != 0 && numerator < 0 {
        quotient - 1
    } else {
        quotient
    }
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    use super::*;

    #[test]
    fn black_pair_is_limited_range_uyvy() {
        let image = RgbaImage::from_pixel(2, 1, Rgba([0, 0, 0, 255]));
        assert_eq!(rgba_to_uyvy(&image, 1.0).unwrap(), [128, 16, 128, 16]);
    }

    #[test]
    fn red_pair_matches_captured_uyvy() {
        let image = RgbaImage::from_pixel(2, 1, Rgba([255, 0, 0, 255]));
        assert_eq!(rgba_to_uyvy(&image, 1.0).unwrap(), [90, 81, 239, 81]);
    }

    #[test]
    fn tm360_packet_header_and_trailer_are_exact() {
        let pixels = vec![0; WIDTH as usize * HEIGHT as usize * 2];
        let packet = frame_packet(&pixels, WIDTH, HEIGHT).unwrap();
        assert_eq!(packet.len(), 480 * 480 * 2 + 16);
        assert_eq!(&packet[..8], &[0xff, 0x00, 0, 0, 0, 0x1e, 0x01, 0xe0]);
        assert_eq!(&packet[packet.len() - 8..], &[0xff, 0xc0, 0, 0, 0, 0, 0, 0]);
    }
}
