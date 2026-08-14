use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub background: Option<PathBuf>,
    pub template: PathBuf,
    pub font: PathBuf,
    pub title: String,
    pub custom_lines: Vec<String>,
    pub fps: u32,
    pub sensor_interval_ms: u64,
    pub brightness: f32,
    pub show_clock: bool,
    pub show_sensors: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            background: None,
            template: PathBuf::from("dashboard.svg.jinja"),
            font: PathBuf::from(r"C:\Windows\Fonts\segoeui.ttf"),
            title: "LoopPanel".to_owned(),
            custom_lines: vec!["Native display".to_owned()],
            fps: 10,
            sensor_interval_ms: 1_000,
            brightness: 1.0,
            show_clock: true,
            show_sensors: true,
        }
    }
}

impl Config {
    pub fn load(argument: Option<&str>) -> Result<(Self, PathBuf)> {
        let explicit = argument.map(PathBuf::from);
        let executable_directory = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        let path = explicit
            .clone()
            .unwrap_or_else(|| executable_directory.join("looppanel.toml"));

        if !path.exists() {
            if explicit.is_some() {
                anyhow::bail!("找不到配置文件：{}", path.display());
            }
            let mut config = Self::default();
            config.resolve(&executable_directory);
            return Ok((config, executable_directory));
        }

        let text = fs::read_to_string(&path)
            .with_context(|| format!("无法读取配置文件：{}", path.display()))?;
        let mut config: Self = toml::from_str(&text)
            .with_context(|| format!("配置文件格式错误：{}", path.display()))?;
        let base = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .canonicalize()
            .with_context(|| format!("无法解析配置目录：{}", path.display()))?;

        config.resolve(&base);
        config.fps = config.fps.clamp(1, 30);
        config.sensor_interval_ms = config.sensor_interval_ms.max(250);
        config.brightness = config.brightness.clamp(0.05, 1.0);
        Ok((config, base))
    }

    fn resolve(&mut self, base: &Path) {
        if let Some(background) = &self.background
            && background.is_relative()
        {
            self.background = Some(base.join(background));
        }
        if self.template.is_relative() {
            self.template = base.join(&self.template);
        }
        if self.font.is_relative() {
            self.font = base.join(&self.font);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, process};

    use super::Config;

    #[test]
    fn loads_explicit_config_with_a_bare_filename() {
        let filename = format!("looppanel-config-test-{}.toml", process::id());
        fs::write(&filename, "title = \"Bare filename\"\n").unwrap();

        let (config, base) = Config::load(Some(&filename)).unwrap();

        assert_eq!(config.title, "Bare filename");
        assert_eq!(
            base,
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
        assert_eq!(config.template, base.join("dashboard.svg.jinja"));
        fs::remove_file(filename).unwrap();
    }
}
