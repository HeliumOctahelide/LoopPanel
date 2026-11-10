//! 界面文字的消息目录：每种语言一个 `locales/<语言>.toml`，按键查找，缺失时回退到英文。
//!
//! 新增语言只需添加目录文件并在 [`LOCALES`] 登记一行；调用方只使用消息键。

use std::{collections::HashMap, sync::OnceLock};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetUserDefaultUILanguage() -> u16;
}

struct Locale {
    tag: &'static str,
    /// Windows LANGID 的主语言号（低 10 位）。
    primary_language: u16,
    source: &'static str,
}

const FALLBACK_TAG: &str = "en";

const LOCALES: [Locale; 3] = [
    Locale {
        tag: "en",
        primary_language: 0x09,
        source: include_str!("../locales/en.toml"),
    },
    Locale {
        tag: "ko",
        primary_language: 0x12,
        source: include_str!("../locales/ko.toml"),
    },
    Locale {
        tag: "zh-CN",
        primary_language: 0x04,
        source: include_str!("../locales/zh-CN.toml"),
    },
];

type Messages = HashMap<String, String>;

struct Catalog {
    active: Messages,
    fallback: Messages,
}

impl Catalog {
    fn for_language_id(language_id: u16) -> Self {
        let tag = locale_tag(language_id);
        Self {
            active: messages(tag),
            fallback: if tag == FALLBACK_TAG {
                Messages::new()
            } else {
                messages(FALLBACK_TAG)
            },
        }
    }

    /// 找不到时返回键本身，界面上能直接看出缺了哪一条。
    fn message<'a>(&'a self, key: &'a str) -> &'a str {
        self.active
            .get(key)
            .or_else(|| self.fallback.get(key))
            .map_or(key, String::as_str)
    }
}

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| Catalog::for_language_id(unsafe { GetUserDefaultUILanguage() }))
}

/// 按 Windows 显示语言取一条界面文字。
pub(crate) fn tr(key: &str) -> String {
    catalog().message(key).to_owned()
}

/// 取一条界面文字并替换其中的 `{name}` 占位符。
pub(crate) fn tr_with(key: &str, arguments: &[(&str, &str)]) -> String {
    substitute(catalog().message(key), arguments)
}

fn locale_tag(language_id: u16) -> &'static str {
    let primary_language = language_id & 0x3ff;
    LOCALES
        .iter()
        .find(|locale| locale.primary_language == primary_language)
        .map_or(FALLBACK_TAG, |locale| locale.tag)
}

fn messages(tag: &str) -> Messages {
    let mut messages = Messages::new();
    if let Some(locale) = LOCALES.iter().find(|locale| locale.tag == tag)
        && let Ok(table) = locale.source.parse::<toml::Table>()
    {
        flatten("", &table, &mut messages);
    }
    messages
}

/// 把 `[tray.menu] exit = "…"` 展开成键 `tray.menu.exit`。
fn flatten(prefix: &str, table: &toml::Table, messages: &mut Messages) {
    for (name, value) in table {
        let key = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        match value {
            toml::Value::String(text) => {
                messages.insert(key, text.clone());
            }
            toml::Value::Table(table) => flatten(&key, table, messages),
            _ => {}
        }
    }
}

fn substitute(template: &str, arguments: &[(&str, &str)]) -> String {
    arguments
        .iter()
        .fold(template.to_owned(), |text, (name, value)| {
            text.replace(&format!("{{{name}}}"), value)
        })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn placeholders(text: &str) -> BTreeSet<String> {
        text.split('{')
            .skip(1)
            .filter_map(|part| part.split_once('}'))
            .map(|(name, _)| name.to_owned())
            .collect()
    }

    #[test]
    fn selects_a_locale_from_the_primary_language_id() {
        // zh-CN、zh-TW、ko-KR、en-US、en-GB；未登记的 ja-JP 回退到英文。
        assert_eq!(locale_tag(0x0804), "zh-CN");
        assert_eq!(locale_tag(0x0404), "zh-CN");
        assert_eq!(locale_tag(0x0412), "ko");
        assert_eq!(locale_tag(0x0409), "en");
        assert_eq!(locale_tag(0x0809), "en");
        assert_eq!(locale_tag(0x0411), "en");
    }

    #[test]
    fn every_locale_file_is_valid_and_matches_the_fallback() {
        let fallback = messages(FALLBACK_TAG);
        assert!(!fallback.is_empty());
        for locale in &LOCALES {
            locale
                .source
                .parse::<toml::Table>()
                .unwrap_or_else(|error| panic!("{}: {error}", locale.tag));
            let translated = messages(locale.tag);
            let keys = |messages: &Messages| messages.keys().cloned().collect::<BTreeSet<_>>();
            assert_eq!(keys(&translated), keys(&fallback), "{}", locale.tag);
            for (key, text) in &translated {
                assert_eq!(
                    placeholders(text),
                    placeholders(&fallback[key]),
                    "{}: {key}",
                    locale.tag
                );
            }
        }
    }

    #[test]
    fn looks_up_messages_and_substitutes_placeholders() {
        let korean = Catalog::for_language_id(0x0412);
        assert_eq!(korean.message("tray.menu.exit"), "종료");
        assert_eq!(
            substitute(
                korean.message("tray.temperature.value"),
                &[("celsius", "54")]
            ),
            "CPU 온도: 54°C"
        );
        let chinese = Catalog::for_language_id(0x0804);
        assert_eq!(
            substitute(chinese.message("tray.status.failed"), &[("error", "x")]),
            "LoopPanel：连接失败（x）"
        );
        assert_eq!(
            Catalog::for_language_id(0x0411).message("tray.menu.exit"),
            "Exit"
        );
    }

    #[test]
    fn missing_keys_fall_back_to_english_and_then_to_the_key() {
        let mut catalog = Catalog::for_language_id(0x0412);
        catalog.active.remove("tray.menu.exit");
        assert_eq!(catalog.message("tray.menu.exit"), "Exit");
        assert_eq!(catalog.message("tray.no_such_key"), "tray.no_such_key");
    }
}
