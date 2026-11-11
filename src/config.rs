use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Deserialize;
use toml_parser::{Source, lexer::TokenKind};
use windows_sys::Win32::Storage::FileSystem::CopyFileW;

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
    replace_config(path, |file| file.write_all(localized.as_bytes()))
        .with_context(|| t!("config.write_failed", path = path.display()))?;
    Ok(true)
}

fn localized_comments(text: &str, translate: impl Fn(&str) -> Option<String>) -> Option<String> {
    let mut output = String::with_capacity(text.len());
    let mut copied = 0;
    for token in Source::new(text).lex() {
        if token.kind() != TokenKind::Comment {
            continue;
        }
        let span = token.span();
        let line_start = text[..span.start()]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        // 只翻译独占一行的注释，保留值后面的注释。
        if !text[line_start..span.start()].trim().is_empty() {
            continue;
        }
        let comment = text[span.start() + 1..span.end()].trim();
        if let Some(translated) = translate(comment).filter(|new| new != comment) {
            output.push_str(&text[copied..span.start()]);
            output.push_str("# ");
            output.push_str(&translated);
            copied = span.end();
        }
    }
    if copied == 0 {
        return None;
    }
    output.push_str(&text[copied..]);
    Some(output)
}

/// 完整写入同目录临时文件后，用单次重命名替换原文件。
fn replace_config(
    path: &Path,
    write: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    if fs::metadata(path)?.permissions().readonly() {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(".looppanel-tmp");
    let temporary = PathBuf::from(temporary);
    let source: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
    // Windows 10 的原生复制保留文件权限；拒绝覆盖已有临时文件。
    if unsafe { CopyFileW(source.as_ptr(), destination.as_ptr(), 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&temporary)?;
        write(&mut file)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
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
    fn regression_localization_preserves_multiline_string_values() {
        for quote in ["\"\"\"", "'''"] {
            let text = format!("# Known comment\r\ntitle = {quote}\n# Known comment\n{quote}\n");
            let translated = super::localized_comments(&text, |comment| {
                (comment == "Known comment").then(|| "Translated comment".to_owned())
            })
            .unwrap();
            assert!(translated.starts_with("# Translated comment\r\n"));
            assert_eq!(
                toml::from_str::<toml::Table>(&text).unwrap(),
                toml::from_str::<toml::Table>(&translated).unwrap()
            );
        }
    }

    #[test]
    fn regression_partial_write_preserves_original_config() {
        use std::io::{self, Write};
        let path = std::env::temp_dir().join(format!("looppanel-atomic-{}.toml", process::id()));
        fs::write(&path, "title = 'original'\n").unwrap();
        let result = super::replace_config(&path, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("simulated disk write failure"))
        });
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "title = 'original'\n");
        super::replace_config(&path, |file| file.write_all(b"title = 'translated'\n")).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "title = 'translated'\n");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn regression_config_replacement_failure_and_readonly_preserve_original() {
        use std::{io::Write, os::windows::fs::OpenOptionsExt};
        let path = std::env::temp_dir().join(format!("looppanel-locked-{}.toml", process::id()));
        fs::write(&path, "title = 'original'\n").unwrap();
        let locked = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        assert!(super::replace_config(&path, |file| file.write_all(b"changed")).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "title = 'original'\n");
        drop(locked);
        let original_permissions = fs::metadata(&path).unwrap().permissions();
        let mut readonly = original_permissions.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&path, readonly).unwrap();
        let result = super::replace_config(&path, |file| file.write_all(b"changed"));
        fs::set_permissions(&path, original_permissions).unwrap();
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "title = 'original'\n");
        fs::remove_file(path).unwrap();
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
