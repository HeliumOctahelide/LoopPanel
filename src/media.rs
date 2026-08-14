use std::{fs::File, io::BufReader, path::Path, time::Duration};

use anyhow::{Context, Result};
use image::{
    AnimationDecoder, ImageReader, Rgba, RgbaImage,
    codecs::gif::GifDecoder,
    imageops::{self, FilterType},
};

use crate::protocol::{HEIGHT, WIDTH};

const MAX_GIF_FRAMES: usize = 120;
const BACKGROUND_TOP: i64 = 105;
const BACKGROUND_HEIGHT: u32 = 270;

pub struct MediaFrame {
    pub image: RgbaImage,
    pub delay: Duration,
}

pub enum Background {
    Procedural,
    Frames(Vec<MediaFrame>),
}

impl Background {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let Some(path) = path else {
            return Ok(Self::Procedural);
        };

        let is_gif = path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("gif"));

        if is_gif {
            let file =
                File::open(path).with_context(|| format!("无法打开 GIF：{}", path.display()))?;
            let decoder = GifDecoder::new(BufReader::new(file))
                .with_context(|| format!("无法解码 GIF：{}", path.display()))?;
            let mut frames = Vec::new();
            for frame in decoder.into_frames() {
                if frames.len() == MAX_GIF_FRAMES {
                    anyhow::bail!(
                        "GIF 超过 {MAX_GIF_FRAMES} 帧；请缩短动画以控制常驻内存：{}",
                        path.display()
                    );
                }
                let frame =
                    frame.with_context(|| format!("无法读取 GIF 帧：{}", path.display()))?;
                let (numerator, denominator) = frame.delay().numer_denom_ms();
                let millis = (numerator as f64 / denominator as f64).max(10.0);
                frames.push(MediaFrame {
                    image: background_frame(frame.into_buffer()),
                    delay: Duration::from_secs_f64(millis / 1_000.0),
                });
            }
            if frames.is_empty() {
                anyhow::bail!("GIF 不含可显示的帧：{}", path.display());
            }
            return Ok(Self::Frames(frames));
        }

        let image = ImageReader::open(path)
            .with_context(|| format!("无法打开图片：{}", path.display()))?
            .decode()
            .with_context(|| format!("无法解码图片：{}", path.display()))?
            .to_rgba8();
        Ok(Self::Frames(vec![MediaFrame {
            image: background_frame(image),
            delay: Duration::from_secs(1),
        }]))
    }

    pub fn frame(&self, index: usize) -> RgbaImage {
        match self {
            Self::Frames(frames) => frames[index % frames.len()].image.clone(),
            Self::Procedural => {
                let mut image = procedural_base();
                let phase = (index % 120) as i32;
                let x = -100 + phase * 6;
                for offset in 0..150 {
                    let alpha = (90_i32 - (offset - 75_i32).abs()).clamp(0, 72) as u8;
                    let stripe_x = x + offset;
                    if (0..WIDTH as i32).contains(&stripe_x) {
                        for y in 0..HEIGHT as u32 {
                            blend(
                                image.get_pixel_mut(stripe_x as u32, y),
                                [64, 210, 255, alpha],
                            );
                        }
                    }
                }
                image
            }
        }
    }

    pub fn delay(&self, index: usize, fps_limit: u32) -> Duration {
        let limit = Duration::from_secs_f64(1.0 / fps_limit as f64);
        match self {
            Self::Frames(frames) => frames[index % frames.len()].delay.max(limit),
            Self::Procedural => limit,
        }
    }

    pub fn is_animated(&self) -> bool {
        match self {
            Self::Procedural => true,
            Self::Frames(frames) => frames.len() > 1,
        }
    }
}

pub fn load_first(path: &Path) -> Result<RgbaImage> {
    let is_gif = path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("gif"));
    if !is_gif {
        let image = ImageReader::open(path)
            .with_context(|| format!("无法打开图片：{}", path.display()))?
            .decode()
            .with_context(|| format!("无法解码图片：{}", path.display()))?
            .to_rgba8();
        return Ok(cover(image));
    }

    let file = File::open(path).with_context(|| format!("无法打开 GIF：{}", path.display()))?;
    let decoder = GifDecoder::new(BufReader::new(file))
        .with_context(|| format!("无法解码 GIF：{}", path.display()))?;
    let frame = decoder
        .into_frames()
        .next()
        .transpose()
        .with_context(|| format!("无法读取 GIF 首帧：{}", path.display()))?
        .ok_or_else(|| anyhow::anyhow!("GIF 不含可显示的帧：{}", path.display()))?;
    Ok(cover(frame.into_buffer()))
}

fn cover(image: RgbaImage) -> RgbaImage {
    cover_to(image, WIDTH as u32, HEIGHT as u32)
}

fn background_frame(image: RgbaImage) -> RgbaImage {
    let content = cover_to(image, WIDTH as u32, BACKGROUND_HEIGHT);
    let mut canvas = RgbaImage::from_pixel(WIDTH as u32, HEIGHT as u32, Rgba([250, 250, 248, 255]));
    imageops::replace(&mut canvas, &content, 0, BACKGROUND_TOP);
    canvas
}

fn cover_to(image: RgbaImage, target_width: u32, target_height: u32) -> RgbaImage {
    let (width, height) = image.dimensions();
    let scale = (target_width as f32 / width as f32).max(target_height as f32 / height as f32);
    let scaled_width = (width as f32 * scale).ceil() as u32;
    let scaled_height = (height as f32 * scale).ceil() as u32;
    let resized = imageops::resize(&image, scaled_width, scaled_height, FilterType::Lanczos3);
    let x = (scaled_width - target_width) / 2;
    let y = (scaled_height - target_height) / 2;
    imageops::crop_imm(&resized, x, y, target_width, target_height).to_image()
}

fn procedural_base() -> RgbaImage {
    RgbaImage::from_fn(WIDTH as u32, HEIGHT as u32, |x, y| {
        let r = 8 + (x * 18 / WIDTH as u32) as u8;
        let g = 14 + (y * 24 / HEIGHT as u32) as u8;
        let b = 32 + ((x + y) * 36 / (WIDTH as u32 + HEIGHT as u32)) as u8;
        Rgba([r, g, b, 255])
    })
}

fn blend(pixel: &mut Rgba<u8>, overlay: [u8; 4]) {
    let alpha = overlay[3] as u16;
    for channel in 0..3 {
        pixel[channel] =
            ((pixel[channel] as u16 * (255 - alpha) + overlay[channel] as u16 * alpha) / 255) as u8;
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        process,
        time::Duration,
    };

    use image::{
        Delay, Frame, Rgba, RgbaImage,
        codecs::gif::{GifEncoder, Repeat},
    };

    use super::{Background, load_first};

    #[test]
    fn classifies_procedural_and_frame_backgrounds() {
        let frame = || super::MediaFrame {
            image: RgbaImage::new(1, 1),
            delay: Duration::from_secs(1),
        };
        assert!(Background::Procedural.is_animated());
        assert!(!Background::Frames(vec![frame()]).is_animated());
        assert!(Background::Frames(vec![frame(), frame()]).is_animated());
    }

    #[test]
    fn loads_and_resizes_animated_gif() {
        let path = std::env::temp_dir().join(format!("looppanel-gif-test-{}.gif", process::id()));
        let file = File::create(&path).unwrap();
        let mut encoder = GifEncoder::new(file);
        encoder.set_repeat(Repeat::Infinite).unwrap();
        for color in [[255, 0, 0, 255], [0, 255, 0, 255]] {
            encoder
                .encode_frame(Frame::from_parts(
                    RgbaImage::from_pixel(2, 2, Rgba(color)),
                    0,
                    0,
                    Delay::from_numer_denom_ms(80, 1),
                ))
                .unwrap();
        }
        drop(encoder);

        let first = load_first(&path).unwrap();
        assert_eq!(first.dimensions(), (480, 480));
        assert_eq!(first.get_pixel(0, 0).0, [255, 0, 0, 255]);

        let background = Background::load(Some(&path)).unwrap();
        match background {
            Background::Frames(frames) => {
                assert_eq!(frames.len(), 2);
                assert_eq!(frames[0].image.dimensions(), (480, 480));
                assert_eq!(frames[0].delay, Duration::from_millis(80));
                assert_eq!(frames[0].image.get_pixel(0, 104).0, [250, 250, 248, 255]);
                assert_eq!(frames[0].image.get_pixel(0, 105).0, [255, 0, 0, 255]);
                assert_eq!(frames[0].image.get_pixel(479, 374).0, [255, 0, 0, 255]);
                assert_eq!(frames[0].image.get_pixel(479, 375).0, [250, 250, 248, 255]);
            }
            Background::Procedural => panic!("expected GIF frames"),
        }
        fs::remove_file(path).unwrap();
    }
}
