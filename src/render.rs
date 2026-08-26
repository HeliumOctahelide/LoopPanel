use std::{fs, path::Path};

use anyhow::{Context, Result};
use image::{Rgba, RgbaImage};
use minijinja::{AutoEscape, Environment, UndefinedBehavior};
use resvg::{tiny_skia, usvg};
use serde::Serialize;

use crate::{
    config::Config,
    monitor::Snapshot,
    protocol::{HEIGHT, WIDTH},
};

const AUDIO_LEFT: i32 = 10;
const AUDIO_TOP: i32 = 404;
const AUDIO_BAR_WIDTH: i32 = 14;
const AUDIO_BAR_STEP: f32 = 18.5;
const AUDIO_BLOCK_HEIGHT: i32 = 6;
const AUDIO_BLOCK_STEP: i32 = 8;
const AUDIO_BLOCK_COUNT: usize = 8;
const AUDIO_INACTIVE: Rgba<u8> = Rgba([231, 224, 238, 255]);
const AUDIO_ACTIVE: Rgba<u8> = Rgba([138, 88, 191, 255]);

pub struct Renderer {
    environment: Environment<'static>,
    svg_options: usvg::Options<'static>,
    overlay: RgbaImage,
    audio_enabled: bool,
}

impl Renderer {
    pub fn new(template_path: &Path, font_path: &Path) -> Result<Self> {
        let template = fs::read_to_string(template_path)
            .with_context(|| format!("无法读取 SVG 模板：{}", template_path.display()))?;
        let audio_enabled = template.contains(r#"id="audio-spectrum""#);
        let mut environment = Environment::new();
        environment.set_auto_escape_callback(|_| AutoEscape::Html);
        environment.set_undefined_behavior(UndefinedBehavior::Strict);
        environment
            .add_template_owned("dashboard.svg".to_owned(), template)
            .with_context(|| format!("无法解析 SVG 模板：{}", template_path.display()))?;

        let font_data = fs::read(font_path)
            .with_context(|| format!("无法读取字体：{}", font_path.display()))?;
        let mut svg_options = usvg::Options::default();
        let family = {
            let database = svg_options.fontdb_mut();
            database.load_font_data(font_data);
            let family = database
                .faces()
                .flat_map(|face| face.families.iter())
                .map(|(name, _)| name.clone())
                .next()
                .ok_or_else(|| anyhow::anyhow!("字体中没有可用字形面：{}", font_path.display()))?;
            database.set_sans_serif_family(family.clone());
            family
        };
        svg_options.font_family = family;
        svg_options.resources_dir = template_path.parent().map(Path::to_path_buf);

        Ok(Self {
            environment,
            svg_options,
            overlay: RgbaImage::new(WIDTH as u32, HEIGHT as u32),
            audio_enabled,
        })
    }

    pub fn audio_enabled(&self) -> bool {
        self.audio_enabled
    }

    pub fn update(&mut self, config: &Config, snapshot: &Snapshot) -> Result<()> {
        let svg = self.render_svg(config, snapshot)?;
        let tree = usvg::Tree::from_str(&svg, &self.svg_options).context("模板生成的 SVG 无效")?;
        let mut pixmap = tiny_skia::Pixmap::new(WIDTH as u32, HEIGHT as u32)
            .ok_or_else(|| anyhow::anyhow!("无法分配 {WIDTH}×{HEIGHT} RGBA 画布"))?;
        let size = tree.size();
        let transform = tiny_skia::Transform::from_scale(
            WIDTH as f32 / size.width(),
            HEIGHT as f32 / size.height(),
        );
        resvg::render(&tree, transform, &mut pixmap.as_mut());
        self.overlay = RgbaImage::from_raw(WIDTH as u32, HEIGHT as u32, pixmap.take_demultiplied())
            .ok_or_else(|| anyhow::anyhow!("SVG 渲染器返回了错误的像素缓冲区长度"))?;
        Ok(())
    }

    pub fn compose(&self, mut image: RgbaImage, audio_bands: &[f32]) -> RgbaImage {
        for (pixel, overlay) in image.pixels_mut().zip(self.overlay.pixels()) {
            blend(pixel, overlay.0);
        }
        if self.audio_enabled {
            draw_audio_spectrum(&mut image, audio_bands);
        }
        image
    }

    fn render_svg(&self, config: &Config, snapshot: &Snapshot) -> Result<String> {
        let custom_line_count = config.custom_lines.len().min(2);
        let context = DashboardContext {
            title: &config.title,
            time: local_time(),
            show_clock: config.show_clock,
            show_sensors: config.show_sensors,
            cpu: CpuView {
                load_percent: snapshot.cpu_percent,
                temperature_c: snapshot.cpu_temperature,
                performance_frequency_ghz: snapshot.cpu_p_mhz.map(mhz_to_ghz),
                efficiency_frequency_ghz: snapshot.cpu_e_mhz.map(mhz_to_ghz),
                performance_cores: core_views(&snapshot.cpu_p_core_loads, 8),
                efficiency_cores: core_views(&snapshot.cpu_e_core_loads, 16),
            },
            memory: MemoryView {
                used_gib: snapshot.memory_used_gib,
                total_gib: snapshot.memory_total_gib,
                bar_width: bar_width(snapshot.memory_used_gib, snapshot.memory_total_gib, 230.0),
                used_percent: bar_width(snapshot.memory_used_gib, snapshot.memory_total_gib, 100.0),
            },
            gpu: GpuView {
                load_percent: snapshot.gpu_percent,
                temperature_c: snapshot.gpu_temperature,
                power_w: snapshot.gpu_power_w,
                power_limit_w: snapshot.gpu_power_limit_w,
                memory: gpu_memory(snapshot),
                graphics_clock_mhz: snapshot.gpu_graphics_clock_mhz,
                memory_clock_mhz: snapshot.gpu_memory_clock_mhz,
                performance_state: snapshot.gpu_performance_state,
                fan_percent: snapshot.gpu_fan_percent,
            },
            io: IoView {
                network_down_mib_s: snapshot.network_down_mib_s,
                network_up_mib_s: snapshot.network_up_mib_s,
                disk_read_mib_s: snapshot.disk_read_mib_s,
                disk_write_mib_s: snapshot.disk_write_mib_s,
            },
            custom_lines: &config.custom_lines[..custom_line_count],
        };
        self.environment
            .get_template("dashboard.svg")?
            .render(context)
            .context("无法渲染 SVG 模板")
    }
}

#[derive(Serialize)]
struct DashboardContext<'a> {
    title: &'a str,
    time: String,
    show_clock: bool,
    show_sensors: bool,
    cpu: CpuView,
    memory: MemoryView,
    gpu: GpuView,
    io: IoView,
    custom_lines: &'a [String],
}

#[derive(Serialize)]
struct CpuView {
    load_percent: Option<f32>,
    temperature_c: Option<u32>,
    performance_frequency_ghz: Option<f32>,
    efficiency_frequency_ghz: Option<f32>,
    performance_cores: Vec<CoreView>,
    efficiency_cores: Vec<CoreView>,
}

#[derive(Serialize)]
struct CoreView {
    opacity: f32,
}

#[derive(Serialize)]
struct MemoryView {
    used_gib: f32,
    total_gib: f32,
    bar_width: f32,
    used_percent: f32,
}

#[derive(Serialize)]
struct GpuView {
    load_percent: Option<u32>,
    temperature_c: Option<u32>,
    power_w: Option<f32>,
    power_limit_w: Option<f32>,
    memory: Option<GpuMemoryView>,
    graphics_clock_mhz: Option<u32>,
    memory_clock_mhz: Option<u32>,
    performance_state: Option<u32>,
    fan_percent: Option<u32>,
}

#[derive(Serialize)]
struct GpuMemoryView {
    used_gib: f32,
    total_gib: f32,
    bar_width: f32,
}

#[derive(Serialize)]
struct IoView {
    network_down_mib_s: Option<f32>,
    network_up_mib_s: Option<f32>,
    disk_read_mib_s: Option<f32>,
    disk_write_mib_s: Option<f32>,
}

fn mhz_to_ghz(mhz: u32) -> f32 {
    mhz as f32 / 1_000.0
}

fn core_views(loads: &[f32], limit: usize) -> Vec<CoreView> {
    loads
        .iter()
        .take(limit)
        .map(|load| CoreView {
            opacity: 0.18 + 0.82 * load.clamp(0.0, 100.0) / 100.0,
        })
        .collect()
}

fn bar_width(used: f32, total: f32, maximum: f32) -> f32 {
    if total <= 0.0 {
        return 0.0;
    }
    (used / total * maximum).clamp(0.0, maximum)
}

fn gpu_memory(snapshot: &Snapshot) -> Option<GpuMemoryView> {
    let used = snapshot.gpu_memory_used_bytes? as f32 / 1024.0_f32.powi(3);
    let total = snapshot.gpu_memory_total_bytes? as f32 / 1024.0_f32.powi(3);
    (total > 0.0).then(|| GpuMemoryView {
        used_gib: used,
        total_gib: total,
        bar_width: bar_width(used, total, 230.0),
    })
}

fn blend(pixel: &mut Rgba<u8>, overlay: [u8; 4]) {
    let alpha = overlay[3] as u16;
    for channel in 0..3 {
        pixel[channel] =
            ((pixel[channel] as u16 * (255 - alpha) + overlay[channel] as u16 * alpha) / 255) as u8;
    }
}

fn draw_audio_spectrum(image: &mut RgbaImage, bands: &[f32]) {
    for index in 0..16 {
        let x = AUDIO_LEFT + (index as f32 * AUDIO_BAR_STEP).round() as i32;
        let level = bands.get(index).copied().unwrap_or(0.0);
        let level = if level.is_finite() {
            level.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let active = (level * AUDIO_BLOCK_COUNT as f32).ceil() as usize;
        for block in 0..AUDIO_BLOCK_COUNT {
            let y = AUDIO_TOP + block as i32 * AUDIO_BLOCK_STEP;
            let color = if block >= AUDIO_BLOCK_COUNT - active {
                AUDIO_ACTIVE
            } else {
                AUDIO_INACTIVE
            };
            fill_rect(image, x, y, AUDIO_BAR_WIDTH, AUDIO_BLOCK_HEIGHT, color);
        }
    }
}

fn fill_rect(image: &mut RgbaImage, x: i32, y: i32, width: i32, height: i32, color: Rgba<u8>) {
    for row in y..y + height {
        for column in x..x + width {
            image.put_pixel(column as u32, row as u32, color);
        }
    }
}

#[repr(C)]
#[derive(Default)]
struct SystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLocalTime(system_time: *mut SystemTime);
}

fn local_time() -> String {
    let mut value = SystemTime::default();
    unsafe { GetLocalTime(&mut value) };
    format!("{:02}:{:02}:{:02}", value.hour, value.minute, value.second)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        process,
    };

    use image::{Rgba, RgbaImage};

    use super::{
        AUDIO_ACTIVE, AUDIO_INACTIVE, Renderer, bar_width, core_views, draw_audio_spectrum,
    };
    use crate::{config::Config, monitor::Snapshot};

    #[test]
    fn renders_default_dashboard_around_a_transparent_image_slot() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut renderer = Renderer::new(
            &root.join("dashboard.svg.jinja"),
            Path::new(r"C:\Windows\Fonts\segoeui.ttf"),
        )
        .unwrap();
        let config = Config::default();
        let snapshot = Snapshot::default();

        assert!(renderer.audio_enabled());
        let svg = renderer.render_svg(&config, &snapshot).unwrap();
        assert!(svg.contains("width=\"480\" height=\"480\""));
        assert!(svg.contains(">CPU<"));
        assert!(svg.contains(">GPU<"));
        assert!(svg.contains(">AUDIO<"));
        assert!(svg.contains(">NETWORK<"));
        assert!(svg.contains(">DISK<"));
        assert!(svg.contains("id=\"audio-spectrum\""));
        renderer.update(&config, &snapshot).unwrap();
        assert_eq!(renderer.overlay.get_pixel(0, 0).0, [250, 250, 248, 255]);
        assert_eq!(renderer.overlay.get_pixel(240, 240).0, [0, 0, 0, 0]);
        assert_eq!(renderer.overlay.get_pixel(0, 479).0, [250, 250, 248, 255]);

        let base = RgbaImage::from_pixel(480, 480, Rgba([11, 22, 33, 255]));
        let composed = renderer.compose(base, &[]);
        assert_eq!(composed.get_pixel(240, 240).0, [11, 22, 33, 255]);
    }

    #[test]
    fn renders_full_default_dashboard_values() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let renderer = Renderer::new(
            &root.join("dashboard.svg.jinja"),
            Path::new(r"C:\Windows\Fonts\segoeui.ttf"),
        )
        .unwrap();
        let config = Config::default();
        let snapshot = Snapshot {
            cpu_percent: Some(23.0),
            cpu_temperature: Some(47),
            cpu_p_core_loads: vec![10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0],
            cpu_e_core_loads: vec![
                1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0,
                16.0,
            ],
            cpu_p_mhz: Some(4_800),
            cpu_e_mhz: Some(3_400),
            memory_used_gib: 31.0,
            memory_total_gib: 64.0,
            gpu_percent: Some(35),
            gpu_temperature: Some(63),
            gpu_power_w: Some(116.0),
            gpu_power_limit_w: Some(400.0),
            gpu_memory_used_bytes: Some(6_u64 << 30),
            gpu_memory_total_bytes: Some(16_u64 << 30),
            gpu_graphics_clock_mhz: Some(2_505),
            gpu_memory_clock_mhz: Some(15_001),
            gpu_performance_state: Some(2),
            gpu_fan_percent: Some(41),
            network_down_mib_s: Some(12.3),
            network_up_mib_s: Some(4.5),
            disk_read_mib_s: Some(67.8),
            disk_write_mib_s: Some(9.1),
        };
        let svg = renderer.render_svg(&config, &snapshot).unwrap();
        assert!(svg.contains(">23%<"));
        assert!(svg.contains(">47°<"));
        assert!(svg.contains(">35%<"));
        assert!(svg.contains(">63°<"));
        assert!(svg.contains(">31 GB<"));
        assert!(svg.contains(">64 GB<"));
        assert!(svg.contains(">6.0 GB<"));
        assert!(svg.contains(">16.0 GB<"));
        assert!(svg.contains(">12.3<"));
        assert!(svg.contains(">4.5<"));
        assert!(svg.contains(">68<"));
        assert!(svg.contains(">9<"));
        assert!(svg.contains("M332 402 V415"));
        assert!(svg.contains("M408 459 V446"));
    }

    #[test]
    fn default_dashboard_marks_missing_cpu_utility() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let renderer = Renderer::new(
            &root.join("dashboard.svg.jinja"),
            Path::new(r"C:\Windows\Fonts\segoeui.ttf"),
        )
        .unwrap();
        let svg = renderer
            .render_svg(&Config::default(), &Snapshot::default())
            .unwrap();

        assert!(svg.contains(r#"<text x="48" y="40" font-size="29">--</text>"#));
        assert!(!svg.contains("cx=\"48\" cy=\"50\" r=\"38\" stroke=\"#2477d4\""));
    }

    #[test]
    fn custom_templates_keep_the_complete_typed_context() {
        let path = std::env::temp_dir().join(format!(
            "looppanel-context-test-{}.svg.jinja",
            process::id()
        ));
        fs::write(
            &path,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="480" height="480"><text>{{ title }}|{{ custom_lines[0] }}|{{ "%.1f"|format(cpu.performance_frequency_ghz) }}|{{ cpu.performance_cores|length }}|{{ "%.1f"|format(gpu.power_w) }}|{{ "%.1f"|format(gpu.memory.used_gib) }}|{{ "%.1f"|format(io.network_down_mib_s) }}|{{ "%.1f"|format(memory.bar_width) }}|{{ "%.1f"|format(memory.used_percent) }}</text></svg>"#,
        )
        .unwrap();
        let renderer = Renderer::new(&path, Path::new(r"C:\Windows\Fonts\segoeui.ttf")).unwrap();
        fs::remove_file(path).unwrap();
        assert!(!renderer.audio_enabled());
        let config = Config {
            title: "A&B".to_owned(),
            custom_lines: vec!["custom".to_owned()],
            ..Config::default()
        };
        let snapshot = Snapshot {
            cpu_p_mhz: Some(4_800),
            cpu_p_core_loads: vec![25.0],
            memory_used_gib: 32.0,
            memory_total_gib: 64.0,
            gpu_power_w: Some(120.0),
            gpu_memory_used_bytes: Some(6_u64 << 30),
            gpu_memory_total_bytes: Some(12_u64 << 30),
            network_down_mib_s: Some(3.5),
            ..Snapshot::default()
        };

        let svg = renderer.render_svg(&config, &snapshot).unwrap();
        assert!(svg.contains("A&amp;B|custom|4.8|1|120.0|6.0|3.5|115.0|50.0"));

        let background = RgbaImage::from_pixel(480, 480, Rgba([250, 250, 248, 255]));
        let composed = renderer.compose(background, &[1.0]);
        assert_eq!(composed.get_pixel(456, 405).0, [250, 250, 248, 255]);
    }

    #[test]
    fn draws_sixteen_clamped_audio_bands() {
        let background = Rgba([250, 250, 248, 255]);
        let mut image = RgbaImage::from_pixel(480, 480, background);
        let mut bands = [0.0; 16];
        bands[0] = 1.5;
        bands[1] = f32::NAN;
        draw_audio_spectrum(&mut image, &bands);
        assert_eq!(*image.get_pixel(10, 404), AUDIO_ACTIVE);
        assert_eq!(*image.get_pixel(10, 465), AUDIO_ACTIVE);
        assert_eq!(*image.get_pixel(29, 404), AUDIO_INACTIVE);
        assert_eq!(*image.get_pixel(29, 465), AUDIO_INACTIVE);
    }

    #[test]
    fn clamps_core_opacity_and_bar_width() {
        let cores = core_views(&[-20.0, 0.0, 100.0, 120.0], 4);
        assert!((cores[0].opacity - 0.18).abs() < 0.001);
        assert!((cores[1].opacity - 0.18).abs() < 0.001);
        assert!((cores[2].opacity - 1.0).abs() < 0.001);
        assert!((cores[3].opacity - 1.0).abs() < 0.001);
        assert_eq!(bar_width(10.0, 0.0, 230.0), 0.0);
        assert_eq!(bar_width(5.0, 10.0, 230.0), 115.0);
        assert_eq!(bar_width(20.0, 10.0, 230.0), 230.0);
    }
}
