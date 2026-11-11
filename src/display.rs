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
    Renderer::for_layout(layout, &config.template, &config.font)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct DisplayOptions {
    layout: Layout,
    unit: crate::settings::TemperatureUnit,
    background: bool,
}

impl DisplayOptions {
    fn requested(settings: &DisplaySettings, layout: Layout) -> Self {
        Self {
            layout,
            unit: settings.temperature_unit(),
            background: settings.simple_background(),
        }
    }
}

/// 已经通过加载、渲染与编码验证的显示内容；USB 只消费其帧包。
struct DisplayContent {
    config: Config,
    options: DisplayOptions,
    renderer: Renderer,
    background: Background,
    packet: Vec<u8>,
    next_sensor: Instant,
}

impl DisplayContent {
    fn prepare(
        config: Config,
        options: DisplayOptions,
        snapshot: &crate::monitor::Snapshot,
        now: Instant,
    ) -> Result<Self> {
        let mut renderer = renderer_for(&config, options.layout)?;
        renderer.set_temperature_unit(options.unit);
        renderer.update(&config, snapshot)?;
        let background = background_for(&config, options.layout, options.background)?;
        let image = renderer.compose(background.frame(0), &[0.0; BAND_COUNT]);
        let packet = frame_packet(&rgba_to_uyvy(&image, config.brightness)?, WIDTH, HEIGHT)?;
        let next_sensor = now + Duration::from_millis(config.sensor_interval_ms);
        Ok(Self {
            config,
            options,
            renderer,
            background,
            packet,
            next_sensor,
        })
    }

    fn load(
        path: Option<&str>,
        options: DisplayOptions,
        snapshot: &crate::monitor::Snapshot,
        now: Instant,
    ) -> Result<Self> {
        let (config, _) = Config::load(path)?;
        Self::prepare(config, options, snapshot, now)
    }

    fn reload(
        &mut self,
        path: Option<&str>,
        options: DisplayOptions,
        snapshot: &crate::monitor::Snapshot,
        now: Instant,
    ) -> Result<()> {
        let mut replacement = Self::load(path, options, snapshot, now)?;
        // 新配置立即获得一次新采样，不继续等待旧配置的期限。
        replacement.next_sensor = now;
        *self = replacement;
        Ok(())
    }

    fn switch(
        &mut self,
        options: DisplayOptions,
        snapshot: &crate::monitor::Snapshot,
        now: Instant,
    ) -> Result<()> {
        if options.layout == self.options.layout && options.background == self.options.background {
            // 单位切换只使用已接受的模板和字体，不顺带重载磁盘资源。
            self.renderer.set_temperature_unit(options.unit);
            if let Err(error) = self.renderer.update(&self.config, snapshot) {
                self.renderer.set_temperature_unit(self.options.unit);
                return Err(error);
            }
            self.options = options;
            return Ok(());
        }
        let mut replacement = Self::prepare(self.config.clone(), options, snapshot, now)?;
        replacement.next_sensor = self.next_sensor;
        *self = replacement;
        Ok(())
    }

    fn sensors_live(&self) -> bool {
        self.config.show_sensors || self.options.layout != Layout::Default
    }

    fn dashboard_live(&self) -> bool {
        self.config.show_clock || self.sensors_live()
    }

    fn encode(&mut self, frame: usize, audio_bands: &[f32]) -> Result<()> {
        let image = self
            .renderer
            .compose(self.background.frame(frame), audio_bands);
        self.packet = frame_packet(
            &rgba_to_uyvy(&image, self.config.brightness)?,
            WIDTH,
            HEIGHT,
        )?;
        Ok(())
    }
}

/// 自动轮换的状态：托盘里选中的布局是起点，开启轮换后每隔一段时间换到下一个布局。
struct Rotation {
    selected: Layout,
    shown: Layout,
    interval: Option<Duration>,
    next: Instant,
}

impl Rotation {
    fn new(selected: Layout, interval: Option<Duration>, now: Instant) -> Self {
        Self {
            selected,
            shown: selected,
            interval,
            next: now + interval.unwrap_or_default(),
        }
    }

    /// 返回现在应该显示的布局。在菜单里另选布局会立刻跳过去并重新计时；
    /// 改间隔或刚开启时重新计时；关闭轮换后回到选中的布局。
    fn layout(
        &mut self,
        selected: Layout,
        interval: Option<Duration>,
        selected_again: bool,
        now: Instant,
    ) -> Layout {
        if selected != self.selected || selected_again {
            self.selected = selected;
            self.shown = selected;
            self.next = now + interval.unwrap_or_default();
        }
        if interval != self.interval {
            self.interval = interval;
            self.next = now + interval.unwrap_or_default();
        }
        match interval {
            Some(interval) if now >= self.next => {
                self.shown = self.shown.next();
                self.next = now + interval;
            }
            Some(_) => {}
            None => self.shown = selected,
        }
        self.shown
    }

    /// 下一次轮换的时刻；未开启时为 `None`。
    fn deadline(&self) -> Option<Instant> {
        self.interval.map(|_| self.next)
    }
}

/// 内置大字布局关闭背景时使用的底色，与模板里压暗背景的颜色相同。
const SIMPLE_PLAIN_BACKGROUND: [u8; 4] = [14, 17, 20, 255];

fn background_for(config: &Config, layout: Layout, simple_background: bool) -> Result<Background> {
    match layout {
        Layout::Default => Background::load(config.background.as_deref(), Placement::CenterBand),
        _ if simple_background => {
            Background::load(config.background.as_deref(), Placement::FullScreen)
        }
        _ => Ok(Background::solid(SIMPLE_PLAIN_BACKGROUND)),
    }
}

pub fn run_until(
    config_path: Option<&str>,
    stop: &AtomicBool,
    settings: &DisplaySettings,
    on_started: impl FnOnce(),
    on_reload_failed: impl Fn(&anyhow::Error),
) -> Result<()> {
    let mut monitor = Monitor::new();
    monitor.prime();
    let mut snapshot = monitor.sample();
    let mut rotation = Rotation::new(settings.layout(), settings.rotation(), Instant::now());
    let mut last_requested = DisplayOptions::requested(settings, rotation.shown);
    let mut content = DisplayContent::load(config_path, last_requested, &snapshot, Instant::now())?;
    let mut audio_spectrum = if content.renderer.audio_enabled() {
        OutputSpectrum::open().ok()
    } else {
        None
    };
    let mut audio_bands = [0.0; BAND_COUNT];
    match audio_spectrum.as_mut().and_then(OutputSpectrum::bands) {
        Some(bands) => audio_bands = bands,
        None => audio_spectrum = None,
    }
    content.encode(0, &audio_bands)?;
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    process::ensure_official_app_stopped()?;
    let mut device = Tm360::open()?;
    device.start(&content.packet)?;
    process::ensure_official_app_stopped()?;
    on_started();

    let started = Instant::now();
    let mut animated = content.background.is_animated();
    let mut frame_index = 0_usize;
    let mut next_content = started + content.background.delay(0, content.config.fps);
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
        if content.sensors_live() {
            wake_at = wake_at.min(content.next_sensor);
        }
        if content.dashboard_live() {
            wake_at = wake_at.min(next_dashboard);
        }
        if let Some(deadline) = rotation.deadline() {
            wake_at = wake_at.min(deadline);
        }
        if audio_spectrum.is_some() {
            wake_at = wake_at.min(next_audio);
        }
        thread::park_timeout(wake_at.saturating_duration_since(Instant::now()));
        if stop.load(Ordering::Relaxed) {
            break;
        }

        let selected_again = settings.take_layout_request();
        let requested_layout = rotation.layout(
            settings.layout(),
            settings.rotation(),
            selected_again,
            Instant::now(),
        );
        let requested = DisplayOptions::requested(settings, requested_layout);
        // 失败的切换只报告一次，下一次菜单操作或轮换再尝试，当前有效内容继续工作。
        let options_changed = requested != last_requested || selected_again;
        last_requested = requested;
        let reload_requested = settings.take_reload_request();
        let restart_background = reload_requested
            || requested.layout != content.options.layout
            || requested.background != content.options.background;
        let was_audio_enabled = content.renderer.audio_enabled();
        let mut replaced = false;
        if reload_requested || options_changed {
            let result = if reload_requested {
                content.reload(config_path, requested, &snapshot, Instant::now())
            } else {
                content.switch(requested, &snapshot, Instant::now())
            };
            match result {
                Ok(()) => replaced = true,
                Err(error) => on_reload_failed(&error),
            }
        }
        if replaced {
            if reload_requested || was_audio_enabled != content.renderer.audio_enabled() {
                audio_spectrum = if content.renderer.audio_enabled() {
                    OutputSpectrum::open().ok()
                } else {
                    None
                };
                audio_bands = [0.0; BAND_COUNT];
            }
            if restart_background {
                animated = content.background.is_animated();
                frame_index = 0;
                next_content = Instant::now() + content.background.delay(0, content.config.fps);
            }
        }

        let now = Instant::now();
        let repeat_due = now >= next_repeat;
        let content_due = animated && now >= next_content;
        let audio_due = audio_spectrum.is_some() && now >= next_audio;
        if content.sensors_live() && now >= content.next_sensor {
            snapshot = monitor.sample();
            content.next_sensor = now + Duration::from_millis(content.config.sensor_interval_ms);
        }
        let dashboard_changed = content.dashboard_live() && now >= next_dashboard;
        if dashboard_changed {
            while next_dashboard <= now {
                next_dashboard += dashboard_interval;
            }
            content.renderer.update(&content.config, &snapshot)?;
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
        let mut content_changed = audio_changed || replaced;
        if content_due {
            frame_index = frame_index.wrapping_add(1);
            next_content = now + content.background.delay(frame_index, content.config.fps);
            content_changed = true;
        }
        if dashboard_changed && !animated {
            content_changed = true;
        }
        if content_changed {
            content.encode(frame_index, &audio_bands)?;
        }
        if content_changed || repeat_due {
            device.send_packet(&content.packet)?;
            next_repeat = now + repeat_interval;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::Rotation;
    use crate::settings::Layout;

    #[test]
    fn regression_reselecting_the_same_layout_jumps_back_and_restarts_timer() {
        let start = Instant::now();
        let interval = Some(Duration::from_secs(30));
        let mut rotation = Rotation::new(Layout::Simple, interval, start);
        assert_eq!(
            rotation.layout(
                Layout::Simple,
                interval,
                false,
                start + Duration::from_secs(30)
            ),
            Layout::CpuGpu
        );
        assert_eq!(
            rotation.layout(
                Layout::Simple,
                interval,
                true,
                start + Duration::from_secs(35)
            ),
            Layout::Simple
        );
        assert_eq!(rotation.deadline(), Some(start + Duration::from_secs(65)));
        assert_eq!(
            rotation.layout(
                Layout::Simple,
                interval,
                false,
                start + Duration::from_secs(64)
            ),
            Layout::Simple
        );
        assert_eq!(
            rotation.layout(
                Layout::Simple,
                interval,
                false,
                start + Duration::from_secs(65)
            ),
            Layout::CpuGpu
        );
    }

    struct Fixture(std::path::PathBuf);

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("looppanel-display-{name}-{}", std::process::id()));
            std::fs::create_dir(&root).unwrap();
            let fixture = Self(root);
            fixture.template("red");
            fixture.config("sensor_interval_ms = 600000\n");
            fixture
        }

        fn config(&self, extra: &str) {
            std::fs::write(self.0.join("looppanel.toml"), format!("font = 'C:\\Windows\\Fonts\\segoeui.ttf'\ntemplate = 'dashboard.svg.jinja'\n{extra}")).unwrap();
        }

        fn template(&self, color: &str) {
            std::fs::write(self.0.join("dashboard.svg.jinja"), format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="480" height="480"><rect width="480" height="480" fill="{color}"/></svg>"#)).unwrap();
        }

        fn path(&self) -> String {
            self.0.join("looppanel.toml").to_str().unwrap().to_owned()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn options(layout: Layout) -> super::DisplayOptions {
        super::DisplayOptions {
            layout,
            unit: crate::settings::TemperatureUnit::Celsius,
            background: true,
        }
    }

    #[test]
    fn regression_reload_replaces_content_and_reschedules_sensor_immediately() {
        let fixture = Fixture::new("success");
        let snapshot = crate::monitor::Snapshot::default();
        let now = Instant::now();
        let mut content = super::DisplayContent::load(
            Some(&fixture.path()),
            options(Layout::Default),
            &snapshot,
            now,
        )
        .unwrap();
        assert_eq!(content.next_sensor, now + Duration::from_secs(600));
        let original_packet = content.packet.clone();
        fixture.template("blue");
        fixture.config("sensor_interval_ms = 250\nbrightness = 0.2\nfps = 20\nshow_sensors = false\nshow_clock = false\n");
        let reload_at = now + Duration::from_secs(60);
        content
            .reload(
                Some(&fixture.path()),
                options(Layout::Default),
                &snapshot,
                reload_at,
            )
            .unwrap();
        assert_eq!(content.next_sensor, reload_at);
        assert_eq!(content.config.sensor_interval_ms, 250);
        assert_eq!(content.config.fps, 20);
        assert_eq!(content.config.brightness, 0.2);
        assert_ne!(content.packet, original_packet);
        assert_eq!(content.packet.len(), 480 * 480 * 2 + 16);
        assert!(!content.sensors_live());
        for layout in Layout::ALL
            .into_iter()
            .filter(|layout| *layout != Layout::Default)
        {
            content
                .switch(options(layout), &snapshot, reload_at)
                .unwrap();
            assert!(content.sensors_live() && content.dashboard_live());
        }
    }

    #[test]
    fn regression_unit_change_uses_accepted_resources() {
        let fixture = Fixture::new("unit");
        let snapshot = crate::monitor::Snapshot::default();
        let now = Instant::now();
        let mut content = super::DisplayContent::load(
            Some(&fixture.path()),
            options(Layout::Default),
            &snapshot,
            now,
        )
        .unwrap();
        let original_packet = content.packet.clone();
        std::fs::write(fixture.0.join("dashboard.svg.jinja"), "{% invalid").unwrap();
        let mut fahrenheit = options(Layout::Default);
        fahrenheit.unit = crate::settings::TemperatureUnit::Fahrenheit;
        content.switch(fahrenheit, &snapshot, now).unwrap();
        content.encode(0, &[]).unwrap();
        // 固定红色模板仍是旧版本，不会读取磁盘上未接受的模板。
        assert_eq!(content.packet, original_packet);
        assert!(content.options.unit == crate::settings::TemperatureUnit::Fahrenheit);
    }

    #[test]
    fn regression_failed_reload_and_rotation_keep_valid_display_content() {
        let fixture = Fixture::new("failure");
        let snapshot = crate::monitor::Snapshot::default();
        let now = Instant::now();
        let mut content = super::DisplayContent::load(
            Some(&fixture.path()),
            options(Layout::Default),
            &snapshot,
            now,
        )
        .unwrap();
        let original_packet = content.packet.clone();
        for broken_config in [
            "title = [",
            "background = 'missing.png'",
            "font = 'missing.ttf'\n",
            "template = 'missing.svg'",
        ] {
            // 字段覆盖要保持其余配置合法，确保错误来自目标资源。
            let text = format!(
                "font = 'C:\\Windows\\Fonts\\segoeui.ttf'\ntemplate = 'dashboard.svg.jinja'\n{broken_config}"
            );
            let text = if broken_config.starts_with("font") {
                format!("template = 'dashboard.svg.jinja'\n{broken_config}")
            } else if broken_config.starts_with("template") {
                format!("font = 'C:\\Windows\\Fonts\\segoeui.ttf'\n{broken_config}")
            } else {
                text
            };
            std::fs::write(fixture.path(), text).unwrap();
            assert!(
                content
                    .reload(
                        Some(&fixture.path()),
                        options(Layout::Default),
                        &snapshot,
                        now
                    )
                    .is_err()
            );
            assert_eq!(content.packet, original_packet);
            assert_eq!(content.config.sensor_interval_ms, 600000);
        }
        fixture.config("");
        for bad_template in ["{% if", "not an SVG"] {
            std::fs::write(fixture.0.join("dashboard.svg.jinja"), bad_template).unwrap();
            assert!(
                content
                    .reload(
                        Some(&fixture.path()),
                        options(Layout::Default),
                        &snapshot,
                        now
                    )
                    .is_err()
            );
            for layout in [
                Layout::Simple,
                Layout::CpuGpu,
                Layout::Temperatures,
                Layout::Clock,
            ] {
                content.switch(options(layout), &snapshot, now).unwrap();
            }
            let clock_packet = content.packet.clone();
            assert!(
                content
                    .switch(options(Layout::Default), &snapshot, now)
                    .is_err()
            );
            assert!(content.options.layout == Layout::Clock);
            assert_eq!(content.packet, clock_packet);
            // 下一轮仍能正常切换，不会因为刚才失败而结束显示。
            content
                .switch(options(Layout::Simple), &snapshot, now)
                .unwrap();
        }
    }

    #[test]
    fn rotation_cycles_layouts_and_follows_menu_changes() {
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        let interval = Some(Duration::from_secs(30));

        // 关闭时始终显示选中的布局，也不需要定时唤醒。
        let mut rotation = Rotation::new(Layout::Simple, None, start);
        assert_eq!(rotation.deadline(), None);
        assert_eq!(
            rotation.layout(Layout::Simple, None, false, at(100)),
            Layout::Simple
        );

        // 开启后从选中的布局开始，满一个间隔才换下一个。
        let mut shown =
            |selected, interval, seconds| rotation.layout(selected, interval, false, at(seconds));
        assert_eq!(shown(Layout::Simple, interval, 100), Layout::Simple);
        assert_eq!(shown(Layout::Simple, interval, 129), Layout::Simple);
        assert_eq!(shown(Layout::Simple, interval, 130), Layout::CpuGpu);
        assert_eq!(shown(Layout::Simple, interval, 160), Layout::Temperatures);

        // 在菜单里另选布局：立刻跳过去并重新计时。
        assert_eq!(shown(Layout::Clock, interval, 170), Layout::Clock);
        assert_eq!(shown(Layout::Clock, interval, 199), Layout::Clock);
        assert_eq!(shown(Layout::Clock, interval, 200), Layout::Default);

        // 改间隔重新计时；关闭后回到选中的布局。
        let shorter = Some(Duration::from_secs(10));
        assert_eq!(shown(Layout::Clock, shorter, 201), Layout::Default);
        assert_eq!(shown(Layout::Clock, shorter, 211), Layout::Simple);
        assert_eq!(shown(Layout::Clock, None, 212), Layout::Clock);
        assert_eq!(rotation.deadline(), None);
    }
}
