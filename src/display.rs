use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;

use crate::{
    audio::{BAND_COUNT, OutputSpectrum, SAMPLE_INTERVAL},
    config::Config,
    media::{Background, Placement},
    monitor::Monitor,
    process,
    protocol::{HEIGHT, WIDTH, frame_packet, rgba_to_uyvy},
    render::Renderer,
    settings::{DisplaySettings, Layout},
    transport::Tm360,
};

fn renderer_for(config: &Config, layout: Layout) -> Result<Renderer> {
    match layout {
        Layout::Default => Renderer::new(&config.template, &config.font),
        Layout::Simple => Renderer::simple(&config.font),
    }
}

/// 单纯布局关闭背景时使用的底色，与模板里压暗背景的颜色相同。
const SIMPLE_PLAIN_BACKGROUND: [u8; 4] = [14, 17, 20, 255];

fn background_for(config: &Config, layout: Layout, simple_background: bool) -> Result<Background> {
    match layout {
        Layout::Default => Background::load(config.background.as_deref(), Placement::CenterBand),
        Layout::Simple if simple_background => {
            Background::load(config.background.as_deref(), Placement::FullScreen)
        }
        Layout::Simple => Ok(Background::solid(SIMPLE_PLAIN_BACKGROUND)),
    }
}

pub fn run_until(
    config_path: Option<&str>,
    stop: &AtomicBool,
    settings: &DisplaySettings,
    on_started: impl FnOnce(),
    on_reload_failed: impl Fn(&anyhow::Error),
) -> Result<()> {
    let (mut config, _) = Config::load(config_path)?;
    let mut active_layout = settings.layout();
    let mut active_unit = settings.temperature_unit();
    let mut renderer = renderer_for(&config, active_layout)?;
    renderer.set_temperature_unit(active_unit);
    let mut active_simple_background = settings.simple_background();
    let mut background = background_for(&config, active_layout, active_simple_background)?;
    let mut monitor = Monitor::new();
    monitor.prime();
    let mut snapshot = monitor.sample();
    let mut audio_spectrum = if renderer.audio_enabled() {
        OutputSpectrum::open().ok()
    } else {
        None
    };
    let mut audio_bands = [0.0; BAND_COUNT];
    match audio_spectrum.as_mut().and_then(OutputSpectrum::bands) {
        Some(bands) => audio_bands = bands,
        None => audio_spectrum = None,
    }

    renderer.update(&config, &snapshot)?;
    let first = renderer.compose(background.frame(0), &audio_bands);
    let mut current_packet =
        frame_packet(&rgba_to_uyvy(&first, config.brightness)?, WIDTH, HEIGHT)?;
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }

    process::ensure_official_app_stopped()?;
    let mut device = Tm360::open()?;
    device.start(&current_packet)?;
    process::ensure_official_app_stopped()?;
    on_started();

    let started = Instant::now();
    let mut animated = background.is_animated();
    let mut sensor_interval = Duration::from_millis(config.sensor_interval_ms);
    let mut next_sensor = started + sensor_interval;
    let mut frame_index = 0_usize;
    let mut next_content = started + background.delay(0, config.fps);
    let repeat_interval = Duration::from_secs(1);
    let mut next_repeat = started + repeat_interval;
    let dashboard_interval = Duration::from_secs(1);
    let mut next_dashboard = started + dashboard_interval;
    let mut next_audio = started + SAMPLE_INTERVAL;

    while !stop.load(Ordering::Relaxed) {
        let mut wake_at = next_repeat;
        if animated {
            wake_at = wake_at.min(next_content);
        }
        if config.show_sensors {
            wake_at = wake_at.min(next_sensor);
        }
        if config.show_clock || config.show_sensors {
            wake_at = wake_at.min(next_dashboard);
        }
        if audio_spectrum.is_some() {
            wake_at = wake_at.min(next_audio);
        }
        thread::park_timeout(wake_at.saturating_duration_since(Instant::now()));
        if stop.load(Ordering::Relaxed) {
            break;
        }

        // 托盘菜单改动显示选项后会唤醒本线程：立即重画，不重新初始化设备。
        let requested_layout = settings.layout();
        let requested_unit = settings.temperature_unit();
        let requested_simple_background = settings.simple_background();

        // 托盘“重新载入设置”：先把新配置、模板和背景完整载入并试渲染一次，全部成功才替换；
        // 任何一步失败都保留当前画面，只把原因报告给托盘。设备不重新初始化。
        let mut reloaded = false;
        if settings.take_reload_request() {
            let loaded = Config::load(config_path).and_then(|(config, _)| {
                let mut renderer = renderer_for(&config, requested_layout)?;
                renderer.set_temperature_unit(requested_unit);
                renderer.update(&config, &snapshot)?;
                let background =
                    background_for(&config, requested_layout, requested_simple_background)?;
                Ok((config, renderer, background))
            });
            match loaded {
                Ok((new_config, new_renderer, new_background)) => {
                    config = new_config;
                    renderer = new_renderer;
                    background = new_background;
                    active_layout = requested_layout;
                    active_unit = requested_unit;
                    active_simple_background = requested_simple_background;
                    audio_spectrum = if renderer.audio_enabled() {
                        OutputSpectrum::open().ok()
                    } else {
                        None
                    };
                    audio_bands = [0.0; BAND_COUNT];
                    animated = background.is_animated();
                    frame_index = 0;
                    next_content = Instant::now() + background.delay(0, config.fps);
                    sensor_interval = Duration::from_millis(config.sensor_interval_ms);
                    reloaded = true;
                }
                Err(error) => on_reload_failed(&error),
            }
        }

        let layout_changed = requested_layout != active_layout;
        let background_changed =
            layout_changed || requested_simple_background != active_simple_background;
        let settings_changed = background_changed || requested_unit != active_unit;
        if layout_changed {
            active_layout = requested_layout;
            renderer = renderer_for(&config, active_layout)?;
            audio_spectrum = if renderer.audio_enabled() {
                OutputSpectrum::open().ok()
            } else {
                None
            };
            audio_bands = [0.0; BAND_COUNT];
        }
        if background_changed {
            // 两种布局摆放背景的方式不同，背景开关也会换素材：按当前选项重新载入。
            active_simple_background = requested_simple_background;
            background = background_for(&config, active_layout, active_simple_background)?;
            animated = background.is_animated();
            frame_index = 0;
            next_content = Instant::now() + background.delay(0, config.fps);
        }
        if settings_changed {
            active_unit = requested_unit;
            renderer.set_temperature_unit(active_unit);
            renderer.update(&config, &snapshot)?;
        }

        let now = Instant::now();
        let repeat_due = now >= next_repeat;
        let content_due = animated && now >= next_content;
        let audio_due = audio_spectrum.is_some() && now >= next_audio;
        if config.show_sensors && now >= next_sensor {
            snapshot = monitor.sample();
            next_sensor = now + sensor_interval;
        }
        let dashboard_changed = (config.show_clock || config.show_sensors) && now >= next_dashboard;
        if dashboard_changed {
            while next_dashboard <= now {
                next_dashboard += dashboard_interval;
            }
            renderer.update(&config, &snapshot)?;
        }
        let mut audio_changed = false;
        if audio_due {
            next_audio = now + SAMPLE_INTERVAL;
            match audio_spectrum.as_mut().and_then(OutputSpectrum::bands) {
                Some(bands) => {
                    audio_bands = bands;
                    audio_changed = true;
                }
                None => {
                    audio_spectrum = None;
                    audio_changed = audio_bands.iter().any(|level| *level > 0.0);
                    audio_bands = [0.0; BAND_COUNT];
                }
            }
        }
        let mut content_changed = audio_changed || settings_changed || reloaded;
        if content_due {
            frame_index = frame_index.wrapping_add(1);
            next_content = now + background.delay(frame_index, config.fps);
            content_changed = true;
        }
        if dashboard_changed && !animated {
            content_changed = true;
        }
        if content_changed {
            let image = renderer.compose(background.frame(frame_index), &audio_bands);
            current_packet =
                frame_packet(&rgba_to_uyvy(&image, config.brightness)?, WIDTH, HEIGHT)?;
        }
        if content_changed || repeat_due {
            device.send_packet(&current_packet)?;
            next_repeat = now + repeat_interval;
        }
    }
    Ok(())
}
