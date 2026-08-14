use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;

use crate::{
    audio::{BAND_COUNT, OutputSpectrum, SAMPLE_INTERVAL},
    config::Config,
    media::Background,
    monitor::Monitor,
    process,
    protocol::{HEIGHT, WIDTH, frame_packet, rgba_to_uyvy},
    render::Renderer,
    transport::Tm360,
};

pub fn run_until(
    config_path: Option<&str>,
    stop: &AtomicBool,
    on_started: impl FnOnce(),
) -> Result<()> {
    let (config, _) = Config::load(config_path)?;
    let background = Background::load(config.background.as_deref())?;
    let mut renderer = Renderer::new(&config.template, &config.font)?;
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
    let animated = background.is_animated();
    let sensor_interval = Duration::from_millis(config.sensor_interval_ms);
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
        let mut content_changed = audio_changed;
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
