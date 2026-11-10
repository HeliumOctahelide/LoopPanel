use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::i18n::{message_key, retranslate, t, tr};

/// 随包配置文件里各条注释对应的消息键。
const COMMENT_KEYS: [&str; 5] = [
    message_key("config.comment.background"),
    message_key("config.comment.paths"),
    message_key("config.comment.font"),
    message_key("config.comment.fps"),
    message_key("config.comment.sensor_interval"),
];
/// 旧版本随包配置里、现已改写的注释。
const LEGACY_COMMENTS: [(&str, &str); 1] = [(
    "默认模板按 Agency FB 调整；也可换成其他本地 TTF/TTC 字体。",
    "config.comment.font",
)];

/// 把配置文件里随包自带的注释换成当前 Windows 显示语言。
///
/// 只改与已知注释完全相同的行；用户自己写的注释、所有配置值和换行方式都保持原样。
/// 文件有改动时返回 `true`。
pub fn localize_comments(path: &Path) -> Result<bool> {
    let text = fs::read_to_string(path)
        .with_context(|| t!("config.read_failed", path = path.display()))?;
    let Some(localized) = localized_comments(&text, |comment| {
        LEGACY_COMMENTS
            .iter()
            .find(|(legacy, _)| *legacy == comment)
            .map(|(_, key)| tr(key))
            .or_else(|| retranslate(&COMMENT_KEYS, comment))
    }) else {
        return Ok(false);
    };
    fs::write(path, localized).with_context(|| t!("config.write_failed", path = path.display()))?;
    Ok(true)
}

fn localized_comments(text: &str, translate: impl Fn(&str) -> Option<String>) -> Option<String> {
    let mut changed = false;
    let mut output = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        let ending = &line[body.len()..];
        let content = body.trim_start();
        let indent = &body[..body.len() - content.len()];
        let comment = content.strip_prefix('#').map(str::trim);
        match comment.and_then(|comment| translate(comment).filter(|new| new != comment)) {
            Some(translated) => {
                changed = true;
                output.push_str(indent);
                output.push_str("# ");
                output.push_str(&translated);
                output.push_str(ending);
            }
            None => output.push_str(line),
        }
    }
    changed.then_some(output)
}

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
                anyhow::bail!("{}", t!("config.not_found", path = path.display()));
            }
            let mut config = Self::default();
            config.resolve(&executable_directory);
            return Ok((config, executable_directory));
        }

        let text = fs::read_to_string(&path)
            .with_context(|| t!("config.read_failed", path = path.display()))?;
        let mut config: Self = toml::from_str(&text)
            .with_context(|| t!("config.format_invalid", path = path.display()))?;
        let base = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .canonicalize()
            .with_context(|| t!("config.directory_resolve_failed", path = path.display()))?;

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
    fn localizes_only_known_comments_and_keeps_everything_else() {
        let translate = |comment: &str| match comment {
            "已知注释" | "旧注释" => Some("Known comment".to_owned()),
            "Known comment" => Some("Known comment".to_owned()),
            _ => None,
        };
        let text =
            "# 已知注释\r\nfont = 'a#b.ttf'  # 已知注释\r\n  #旧注释\n# 我自己的备注\nfps = 5";
        assert_eq!(
            super::localized_comments(text, translate).unwrap(),
            "# Known comment\r\nfont = 'a#b.ttf'  # 已知注释\r\n  # Known comment\n# 我自己的备注\nfps = 5"
        );
        // 已经是目标语言时不重写文件。
        assert_eq!(
            super::localized_comments("# Known comment\nfps = 5\n", translate),
            None
        );
    }

    #[test]
    fn bundled_config_comments_are_all_in_the_catalog() {
        let bundled = include_str!("../looppanel.toml");
        let comments = bundled
            .lines()
            .filter_map(|line| line.strip_prefix("# "))
            .collect::<Vec<_>>();
        assert_eq!(comments.len(), super::COMMENT_KEYS.len());
        for comment in comments {
            assert!(
                super::retranslate(&super::COMMENT_KEYS, comment).is_some(),
                "{comment}"
            );
        }
        for (legacy, key) in super::LEGACY_COMMENTS {
            assert!(super::COMMENT_KEYS.contains(&key), "{legacy}");
        }
    }

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
